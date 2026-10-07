use super::{AdmissionChange, StorageUpdatePlan};
use crate::config::{
    ConfigFile, LocalStorageConfig, S3AddressingStyle, S3Provider, S3StorageConfig,
    StorageBackendConfig, StorageInstanceConfig,
};

fn local() -> StorageInstanceConfig {
    ConfigFile::with_test_storage().storage_instances.remove(0)
}

fn s3() -> StorageInstanceConfig {
    let mut instance = local();
    instance.backend = StorageBackendConfig::S3(S3StorageConfig {
        provider: S3Provider::S3Compatible,
        endpoint: "http://127.0.0.1:9".into(),
        bucket: "test-bucket".into(),
        region: "test-region".into(),
        prefix: "files/".into(),
        addressing_style: S3AddressingStyle::Path,
        access_key_id: "test-key".into(),
        secret_access_key: "test-secret".into(),
        relay_upload: false,
        capacity_limit_bytes: None,
    });
    instance
}

#[test]
fn metadata_and_looser_policy_reuse_both_backend_kinds_without_interruption() {
    for mut previous in [local(), s3()] {
        previous.allow_guest_access = false;
        previous.allow_guest_download = Some(false);
        let mut candidate = previous.clone();
        candidate.name = "New name".into();
        candidate.allow_guest_access = true;
        candidate.allow_guest_download = Some(true);
        match &mut candidate.backend {
            StorageBackendConfig::Local(settings) => settings.capacity_limit_bytes = Some(4096),
            StorageBackendConfig::S3(settings) => settings.capacity_limit_bytes = Some(4096),
        }
        let plan = StorageUpdatePlan::new(&previous, &candidate, true);
        assert!(plan.reuse_backend());
        assert_eq!(plan.admission, AdmissionChange::Unchanged);
        assert!(!plan.invalidate_uploads);
    }
}

#[test]
fn disable_and_guest_restrictions_interrupt_without_replacing_connections() {
    for previous in [local(), s3()] {
        for candidate in [
            StorageInstanceConfig {
                enabled: false,
                ..previous.clone()
            },
            StorageInstanceConfig {
                allow_guest_access: false,
                ..previous.clone()
            },
            StorageInstanceConfig {
                allow_guest_download: Some(false),
                ..previous.clone()
            },
        ] {
            let plan = StorageUpdatePlan::new(&previous, &candidate, true);
            assert!(plan.reuse_backend());
            assert_eq!(plan.admission, AdmissionChange::InterruptPolicy);
            assert!(plan.invalidate_uploads);
        }
    }
}

#[test]
fn reenable_requires_readiness_even_when_the_edit_also_restricts_guests() {
    for mut previous in [local(), s3()] {
        previous.enabled = false;
        let mut candidate = previous.clone();
        candidate.enabled = true;
        let unchanged = StorageUpdatePlan::new(&previous, &candidate, true);
        assert_eq!(unchanged.admission, AdmissionChange::Reenable);
        assert!(!unchanged.invalidate_uploads);
        candidate.allow_guest_access = false;
        let restricted = StorageUpdatePlan::new(&previous, &candidate, true);
        assert_eq!(restricted.admission, AdmissionChange::Reenable);
        assert!(restricted.invalidate_uploads);
        assert!(restricted.reuse_backend());
    }
}

#[test]
fn both_relay_mode_changes_interrupt_but_reuse_the_s3_connection() {
    for relay in [false, true] {
        let mut previous = s3();
        let StorageBackendConfig::S3(settings) = &mut previous.backend else {
            unreachable!()
        };
        settings.relay_upload = relay;
        let mut candidate = previous.clone();
        let StorageBackendConfig::S3(settings) = &mut candidate.backend else {
            unreachable!()
        };
        settings.relay_upload = !relay;
        let plan = StorageUpdatePlan::new(&previous, &candidate, true);
        assert_eq!(plan.admission, AdmissionChange::InterruptPolicy);
        assert!(plan.invalidate_uploads);
        assert!(plan.reuse_backend());
    }
}

#[test]
fn each_s3_connection_field_change_requires_draining_and_replacement() {
    let previous = s3();
    let edits: [fn(&mut S3StorageConfig); 8] = [
        |settings| settings.provider = S3Provider::Minio,
        |settings| settings.endpoint = "http://127.0.0.1:10".into(),
        |settings| settings.bucket = "other-bucket".into(),
        |settings| settings.region = "other-region".into(),
        |settings| settings.prefix = "other/".into(),
        |settings| settings.addressing_style = S3AddressingStyle::VirtualHosted,
        |settings| settings.access_key_id = "other-key".into(),
        |settings| settings.secret_access_key = "other-secret".into(),
    ];
    for edit in edits {
        let mut candidate = previous.clone();
        let StorageBackendConfig::S3(settings) = &mut candidate.backend else {
            unreachable!()
        };
        edit(settings);
        let plan = StorageUpdatePlan::new(&previous, &candidate, true);
        assert_eq!(plan.admission, AdmissionChange::ReplaceConnection);
        assert!(plan.invalidate_uploads);
        assert!(!plan.reuse_backend());
    }
}

#[test]
fn changed_mount_backend_kind_or_missing_cache_cannot_reuse() {
    let previous = local();
    let mut candidate = previous.clone();
    candidate.backend = StorageBackendConfig::Local(LocalStorageConfig {
        mount_id: "other-mount".into(),
        capacity_limit_bytes: None,
    });
    for (candidate, cached) in [(candidate, true), (s3(), true), (previous.clone(), false)] {
        let plan = StorageUpdatePlan::new(&previous, &candidate, cached);
        assert_eq!(plan.admission, AdmissionChange::ReplaceConnection);
        assert!(plan.invalidate_uploads);
        assert!(!plan.reuse_backend());
    }
}

#[tokio::test]
async fn failed_candidate_initialization_keeps_old_configuration_and_owner() {
    use crate::{
        state::{AppState, LocalStorageEdit},
        storage_catalog::{DeploymentLocalMount, LocalMountCatalog},
    };
    let directory = crate::test_support::TestDirectory::new("storage-edit-init-failure");
    let mut runtime = crate::test_support::runtime_config(directory.path());
    let replacement = directory.path().join("replacement");
    tokio::fs::create_dir(&replacement).await.unwrap();
    // An ordinary filesystem failure during layout initialization, not a
    // partially constructed candidate that may be published anyway.
    tokio::fs::write(replacement.join(".ycloud-system"), b"blocked")
        .await
        .unwrap();
    runtime.local_mounts = LocalMountCatalog::new(
        runtime.storage_path.clone(),
        vec![DeploymentLocalMount {
            id: "replacement".into(),
            name: "Replacement".into(),
            path: replacement.clone(),
        }],
    )
    .unwrap();
    let previous = ConfigFile::with_test_storage();
    crate::config::save_config(&runtime.config_path, &previous)
        .await
        .unwrap();
    let state = AppState::new(
        runtime,
        std::sync::Arc::new(tokio::sync::RwLock::new(previous.clone())),
    )
    .await
    .unwrap();
    let old = state.backends.cached("primary").await.unwrap();
    let result = state
        .update_local_storage(
            "primary",
            LocalStorageEdit {
                name: "Replacement".into(),
                path: replacement.to_string_lossy().into_owned(),
                capacity_limit_bytes: None,
                enabled: true,
                guest_access: true.into(),
                expected_revision: None,
            },
        )
        .await;
    assert!(result.is_err());
    crate::test_support::wait_storage_settled(&old).await;
    let fresh = state.storage_backend("primary").await.unwrap();
    assert_eq!(old.instance_key(), fresh.instance_key());
    assert!(fresh.metadata("").await.unwrap().is_dir);
    assert_eq!(
        state.config_file.read().await.storage_instances,
        previous.storage_instances
    );
    assert_eq!(
        crate::config::load_config(&state.config.config_path)
            .await
            .unwrap()
            .storage_instances,
        previous.storage_instances
    );
}

#[tokio::test]
async fn local_edit_reenable_keeps_instance_without_reviving_old_requests() {
    let directory = crate::test_support::TestDirectory::new("storage-edit-reenable");
    let state = crate::test_support::app_state(&directory, ConfigFile::with_test_storage()).await;
    let old_request = state.storage_backend("primary").await.unwrap();
    let edit = |enabled| crate::state::LocalStorageEdit {
        name: "Local".into(),
        path: state.config.storage_path.to_string_lossy().into_owned(),
        capacity_limit_bytes: None,
        enabled,
        guest_access: true.into(),
        expected_revision: None,
    };
    state
        .update_local_storage("primary", edit(false))
        .await
        .unwrap();
    assert!(state.storage_backend("primary").await.is_err());
    crate::test_support::wait_storage_settled(&old_request).await;
    state
        .update_local_storage("primary", edit(true))
        .await
        .unwrap();
    let fresh = state.storage_backend("primary").await.unwrap();
    assert_eq!(old_request.instance_key(), fresh.instance_key());
    assert!(old_request
        .upload_new_file(
            "stale.bin",
            axum::body::Body::from("x"),
            Some(1),
            1024,
            None
        )
        .await
        .is_err());
    fresh
        .upload_new_file(
            "fresh.bin",
            axum::body::Body::from("ok"),
            Some(2),
            1024,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(state.config.storage_path.join("fresh.bin"))
            .await
            .unwrap(),
        b"ok"
    );
    assert!(!state.config.storage_path.join("stale.bin").exists());
}

#[tokio::test]
async fn failed_policy_save_reopens_same_owner_without_publishing_limits() {
    let directory = crate::test_support::TestDirectory::new("storage-edit-save-failure");
    let previous = ConfigFile::with_test_storage();
    let state = crate::test_support::app_state(&directory, previous.clone()).await;
    let old = state.backends.cached("primary").await.unwrap();
    tokio::fs::rename(
        &state.config.config_path,
        directory.path().join("saved-config.json"),
    )
    .await
    .unwrap();
    tokio::fs::create_dir(&state.config.config_path)
        .await
        .unwrap();
    let result = state
        .update_local_storage(
            "primary",
            crate::state::LocalStorageEdit {
                name: "Must not publish".into(),
                path: state.config.storage_path.to_string_lossy().into_owned(),
                capacity_limit_bytes: Some(4 * 1024 * 1024),
                enabled: true,
                guest_access: false.into(),
                expected_revision: None,
            },
        )
        .await;
    assert!(result.is_err());
    crate::test_support::wait_storage_settled(&old).await;
    let fresh = state.storage_backend("primary").await.unwrap();
    assert_eq!(old.instance_key(), fresh.instance_key());
    assert_eq!(fresh.capacity_status().limit, None);
    assert!(fresh.metadata("").await.unwrap().is_dir);
    assert_eq!(
        state.config_file.read().await.storage_instances,
        previous.storage_instances
    );
}
