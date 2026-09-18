use std::{net::SocketAddr, sync::Arc};

use tokio::sync::{watch, RwLock};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use crate::{app, config, state::AppState};

pub async fn run() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let mut runtime = config::Config::from_env()?;
    let config_file = config::load_config(&runtime.config_path).await?;
    runtime.initialize_transaction_auth_key().await?;
    let config_file = Arc::new(RwLock::new(config_file));
    let state = AppState::new(runtime.clone(), config_file).await?;

    let (cleanup_stop, cleanup_signal) = watch::channel(false);
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
    result?;
    Ok(())
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
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    state.sessions.cleanup().await;
                    state.gate_access.cleanup().await;
                    state.folder_access.cleanup().await;
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
