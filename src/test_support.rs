//! Isolated filesystem fixtures shared by behavior tests, never real app data.
use std::path::{Path, PathBuf};

pub(crate) struct TestDirectory(PathBuf);

impl TestDirectory {
    pub(crate) fn new(label: &str) -> Self {
        assert!(label
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-'));
        let root = std::env::temp_dir()
            .canonicalize()
            .expect("temporary directory exists");
        let path = root.join(format!("ycloud-test-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).expect("create isolated test directory");
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        // Delete only the exact, uniquely created fixture directory. Refuse to
        // follow a replacement symlink and never operate on the temp root.
        let Ok(metadata) = std::fs::symlink_metadata(&self.0) else {
            return;
        };
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

pub(crate) fn runtime_config(root: &Path) -> crate::config::Config {
    use crate::config;
    let storage = root.join("storage");
    std::fs::create_dir_all(&storage).expect("create fixture storage");
    config::Config {
        bind_address: "127.0.0.1".parse().unwrap(),
        port: 18473,
        storage_path: storage.clone(),
        local_mounts: crate::storage_catalog::LocalMountCatalog::new(storage, vec![]).unwrap(),
        config_path: root.join("config.json"),
        max_upload_bytes: config::DEFAULT_DEPLOYMENT_MAX_UPLOAD_BYTES,
        max_upload_batch_bytes: config::DEFAULT_DEPLOYMENT_MAX_UPLOAD_BATCH_BYTES,
        max_upload_batch_entries: config::DEFAULT_DEPLOYMENT_MAX_UPLOAD_BATCH_ENTRIES,
        max_archive_bytes: config::DEFAULT_DEPLOYMENT_MAX_ARCHIVE_BYTES,
        max_archive_entries: config::DEFAULT_DEPLOYMENT_MAX_ARCHIVE_ENTRIES,
        io_concurrency: 1,
        max_list_entries: 100,
        request_timeout_secs: 30,
        upload_timeout_secs: 300,
        disk_reserve_bytes: 0,
        secure_cookies: false,
        allow_lan_http: false,
        public_base_url: None,
        public_host: None,
        trusted_proxy_ips: Default::default(),
        allowed_hosts: Default::default(),
        s3_allowed_endpoints: Default::default(),
        transaction_auth_key: [0x31; 32],
    }
}

pub(crate) async fn app_state(
    directory: &TestDirectory,
    persisted: crate::config::ConfigFile,
) -> crate::state::AppState {
    let runtime = runtime_config(directory.path());
    crate::config::save_config(&runtime.config_path, &persisted)
        .await
        .unwrap();
    crate::state::AppState::new(
        runtime,
        std::sync::Arc::new(tokio::sync::RwLock::new(persisted)),
    )
    .await
    .unwrap()
}
