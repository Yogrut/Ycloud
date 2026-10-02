use std::{net::SocketAddr, sync::Arc};

use tokio::sync::{watch, RwLock};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use crate::{app, config, state::AppState};

const RECOVERY_PASS_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

pub async fn run() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let mut runtime = config::Config::from_env()?;
    // Hold this before configuration bootstrap or any storage recovery can run.
    let _instance_lock = crate::instance_lock::InstanceLock::acquire(&runtime.config_path)?;
    let config_file = config::load_config_for_runtime(&runtime).await?;
    runtime.initialize_transaction_auth_key().await?;
    let config_file = Arc::new(RwLock::new(config_file));
    let state = AppState::new(runtime.clone(), config_file).await?;
    tracing::info!(
        bytes = crate::relay_budget::bytes(),
        "global S3 relay payload memory budget"
    );

    let (cleanup_stop, cleanup_signal) = watch::channel(false);
    let health_task = spawn_health_task(state.clone(), cleanup_signal.clone());
    let retired_task = spawn_retired_cleanup_task(state.clone(), cleanup_signal.clone());
    let cleanup_task = spawn_cleanup_task(state.clone(), cleanup_signal);
    let router = app::build_router(state);
    let address = SocketAddr::new(runtime.bind_address, runtime.port);
    let listener = tokio::net::TcpListener::bind(address).await?;

    tracing::info!(
        %address,
        storage = %runtime.storage_path.display(),
        config = %runtime.config_path.display(),
        "Ycloud is ready"
    );

    let result = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await;

    let _ = cleanup_stop.send(true);
    let _ = cleanup_task.await;
    let _ = health_task.await;
    let _ = retired_task.await;
    result?;
    Ok(())
}

fn spawn_retired_cleanup_task(
    state: AppState,
    mut stop: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut cursor = 0_usize;
        loop {
            tokio::select! {
                _ = stop.changed() => return,
                _ = interval.tick() => {}
            }
            // One pass, no initialization loop or concurrent workers. Neither
            // config publication nor unrelated administrator edits wait here.
            let entries = state.retired_storage.snapshot().await;
            let selected = if entries.is_empty() {
                None
            } else {
                let entry = entries[cursor % entries.len()].clone();
                cursor = cursor.wrapping_add(1);
                Some(entry)
            };
            if let Some(entry) = selected {
                let deadline = tokio::time::Instant::now() + RECOVERY_PASS_BUDGET;
                let current = state.config_file.read().await.clone();
                let live = current.storage_instances.iter().find(|instance| matches!(&instance.backend,
                    config::StorageBackendConfig::S3(settings) if config::retired_storage_namespace_matches(settings, &entry.settings)));
                let cancellation = tokio_util::sync::CancellationToken::new();
                let work = async {
                    if let Some(live) = live {
                        let Some(backend) = state.backends.cached(&live.id).await else {
                            return Ok(false);
                        };
                        // Never scan a live namespace using an independent
                        // client: the backend gate prevents adopting uploads.
                        match backend
                            .recover_abandoned_uploads_before(
                                Some(deadline),
                                Some(cancellation.clone()),
                            )
                            .await
                        {
                            Ok(_guard) => Ok(true),
                            Err(crate::error::AppError::Conflict(_)) => Ok(false),
                            Err(error) => Err(error),
                        }
                    } else if let Some(backend) = &entry.runtime {
                        match backend
                            .recover_abandoned_uploads_before(
                                Some(deadline),
                                Some(cancellation.clone()),
                            )
                            .await
                        {
                            Ok(_guard) => Ok(true),
                            Err(crate::error::AppError::Conflict(_)) => Ok(false),
                            Err(error) => Err(error),
                        }
                    } else {
                        let backend =
                            crate::s3_backend::S3Backend::new(&entry.settings, &state.config)?;
                        backend
                            .scoped_work(Some(deadline), Some(cancellation.clone()))
                            .recover_transactions()
                            .await
                            .map(|_| true)
                    }
                };
                let mut work = Box::pin(work);
                let result = tokio::select! {
                    _ = stop.changed() => {
                        cancellation.cancel();
                        // Keep the original client and gates until an issued
                        // formal write returns. The next read/admission exits.
                        let _ = work.await;
                        return;
                    },
                    result = &mut work => result,
                };
                match result {
                    Ok(true) => {
                        let settled = tokio::select! {
                            _ = stop.changed() => return,
                            result = tokio::time::timeout_at(deadline, settle_retired_uploads(&state, &entry)) => result,
                        };
                        match settled {
                            Ok(true) => {
                                if let Err(error) = state.retired_storage.finish(&entry).await {
                                    tracing::warn!(storage_id = %entry.id, %error, "old storage recovery record remains durable");
                                }
                            }
                            Ok(false) => {}
                            Err(_) => {
                                tracing::warn!(storage_id = %entry.id, "old upload verification timed out; retaining connection record")
                            }
                        }
                    }
                    Ok(false) => {}
                    _ => {
                        tracing::warn!(storage_id = %entry.id, "old storage cleanup unavailable; retaining encrypted responsibility for the next bounded attempt")
                    }
                }
            }
        }
    })
}

/// A removed S3 connection remains encrypted until upload tickets tied to its
/// namespace are settled. A positive operation marker proves success; absence
/// alone does not prove that an earlier success was never overwritten.
async fn settle_retired_uploads(state: &AppState, entry: &config::RetiredStorageEntry) -> bool {
    let backend_config = config::StorageBackendConfig::S3(entry.settings.clone());
    let Ok(namespace) = crate::upload_batch::namespace_id(&state.config, &backend_config) else {
        return false;
    };
    let pending = state
        .upload_batches
        .unknown_items()
        .await
        .into_iter()
        .filter(|item| {
            item.storage_id == entry.id && item.namespace_id.as_deref() == Some(&namespace)
        })
        .take(32)
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return !state
            .upload_batches
            .has_unsettled_namespace(&entry.id, &namespace)
            .await;
    }
    let detached = if entry.runtime.is_none() {
        match crate::s3_backend::S3Backend::new(&entry.settings, &state.config) {
            Ok(backend) => Some(backend),
            Err(error) => {
                tracing::warn!(storage_id = %entry.id, %error, "old upload verification connection is unavailable");
                return false;
            }
        }
    } else {
        None
    };
    for item in pending {
        let operation = crate::upload_batch::operation_id(&item.ticket, &item.path);
        let result = if let Some(runtime) = &entry.runtime {
            runtime
                .recovered_upload_committed(&item.path, item.size, &operation)
                .await
        } else {
            detached
                .as_ref()
                .expect("detached connection was constructed")
                .upload_committed(&item.path, item.size, &operation)
                .await
        };
        if matches!(result, Ok(true)) {
            let _ = state.upload_batches.confirm_unknown_committed(&item).await;
        }
    }
    !state
        .upload_batches
        .has_unsettled_namespace(&entry.id, &namespace)
        .await
}

fn spawn_health_task(
    state: AppState,
    mut stop: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let ids = state.config_file.read().await.storage_instances.iter()
                        .map(|storage| storage.id.clone()).collect::<Vec<_>>();
                    for id in ids {
                        if let Some(backend) = state.backends.cached(&id).await {
                            tokio::select! {
                                _ = backend.check_health() => {},
                                _ = stop.changed() => return,
                            }
                        }
                    }
                }
                _ = stop.changed() => return,
            }
        }
    })
}

fn spawn_cleanup_task(
    state: AppState,
    mut stop: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(600));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut capacity_interval =
            tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
        capacity_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // `interval` ticks immediately; consume the first capacity tick because
        // backend startup already loads or starts an initial reconciliation.
        capacity_interval.tick().await;
        let mut upload_interval = tokio::time::interval(std::time::Duration::from_secs(15));
        upload_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut upload_recovery = crate::upload_recovery::UploadRecovery::default();
        loop {
            tokio::select! {
                _ = upload_interval.tick() => {
                    let cancellation = tokio_util::sync::CancellationToken::new();
                    let mut recovery = Box::pin(upload_recovery.recover_with_budget(&state, Some(RECOVERY_PASS_BUDGET), cancellation.clone()));
                    tokio::select! {
                        _ = &mut recovery => {},
                        _ = stop.changed() => {
                            cancellation.cancel();
                            recovery.await;
                            break;
                        },
                    }
                }
                _ = interval.tick() => {
                    state.sessions.cleanup().await;
                    state.gate_access.cleanup().await;
                    state.folder_access.cleanup().await;
                    state.upload_batches.sweep_expired().await;
                }
                _ = capacity_interval.tick() => {
                    state.backends.reconcile_capacities().await;
                }
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                }
            }
        }
    })
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received; draining active requests");
}
