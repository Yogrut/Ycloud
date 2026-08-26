use anyhow::{bail, Context};
use serde::Deserialize;
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

pub const PRIMARY_LOCAL_MOUNT_ID: &str = "primary";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeploymentLocalMount {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct LocalMountCatalog {
    mounts: Vec<DeploymentLocalMount>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LocalMountStatus {
    pub ready: bool,
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
}

impl LocalMountCatalog {
    pub fn new(
        primary_path: PathBuf,
        additional: Vec<DeploymentLocalMount>,
    ) -> anyhow::Result<Self> {
        let mut mounts = Vec::with_capacity(additional.len() + 1);
        mounts.push(DeploymentLocalMount {
            id: PRIMARY_LOCAL_MOUNT_ID.into(),
            name: "Local storage".into(),
            path: primary_path,
        });
        mounts.extend(additional);

        let mut ids = HashSet::new();
        let mut path_keys = Vec::<String>::new();
        for mount in &mut mounts {
            validate_mount_id(&mount.id)?;
            let name = mount.name.trim();
            if name.is_empty() || name.chars().count() > 64 {
                bail!("local mount names must contain 1-64 characters");
            }
            mount.name = name.to_string();
            mount.path = normalize_absolute_path(&mount.path)?;
            let key = comparable_path(&mount.path);
            if !ids.insert(mount.id.as_str()) {
                bail!("local mount IDs must be unique");
            }
            if path_keys
                .iter()
                .any(|existing| paths_overlap(existing, &key))
            {
                bail!("local mount paths must be unique and cannot be nested");
            }
            path_keys.push(key);
        }
        Ok(Self { mounts })
    }

    pub fn from_json(primary_path: PathBuf, json: Option<&str>) -> anyhow::Result<Self> {
        let additional = match json.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => serde_json::from_str::<Vec<DeploymentLocalMount>>(value)
                .context("LOCAL_STORAGE_MOUNTS must be a JSON array")?,
            None => Vec::new(),
        };
        Self::new(primary_path, additional)
    }

    pub fn all(&self) -> &[DeploymentLocalMount] {
        &self.mounts
    }

    pub fn resolve(&self, mount_id: &str) -> Option<&DeploymentLocalMount> {
        self.mounts.iter().find(|mount| mount.id == mount_id)
    }

    /// Resolve administrator-entered paths to a directory explicitly exposed
    /// to Ycloud by deployment configuration. The UI accepts a path, while the
    /// persisted storage instance keeps the stable deployment mount identity.
    pub fn resolve_path(&self, path: &Path) -> anyhow::Result<Option<&DeploymentLocalMount>> {
        let normalized = normalize_absolute_path(path)?;
        let key = comparable_path(&normalized);
        Ok(self
            .mounts
            .iter()
            .find(|mount| comparable_path(&mount.path) == key))
    }

    pub async fn status(&self, mount_id: &str) -> LocalMountStatus {
        let Some(mount) = self.resolve(mount_id) else {
            return LocalMountStatus::default();
        };
        let Ok(metadata) = tokio::fs::symlink_metadata(&mount.path).await else {
            return LocalMountStatus::default();
        };
        if !metadata.is_dir() || crate::storage::is_link_or_reparse_point(&metadata) {
            return LocalMountStatus::default();
        }
        let path = mount.path.clone();
        tokio::task::spawn_blocking(move || LocalMountStatus {
            ready: true,
            total_bytes: fs4::total_space(&path).ok(),
            available_bytes: fs4::available_space(&path).ok(),
        })
        .await
        .unwrap_or_default()
    }
}

fn validate_mount_id(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("local mount IDs must use 1-64 ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

fn normalize_absolute_path(path: &Path) -> anyhow::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        bail!("local mount paths cannot be empty");
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve the current directory")?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => bail!("local mount paths cannot contain '..'"),
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

fn comparable_path(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    let value = value.to_lowercase();
    value.trim_end_matches('/').to_string()
}

fn paths_overlap(first: &str, second: &str) -> bool {
    first == second
        || second
            .strip_prefix(first)
            .is_some_and(|rest| rest.starts_with('/'))
        || first
            .strip_prefix(second)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::{DeploymentLocalMount, LocalMountCatalog};
    use std::path::PathBuf;

    #[test]
    fn deployment_mount_catalog_rejects_duplicate_or_nested_roots() {
        let root = std::env::temp_dir().join("ycloud-mount-catalog");
        let primary = root.join("primary");
        let archive = root.join("archive");
        let catalog = LocalMountCatalog::new(
            primary.clone(),
            vec![DeploymentLocalMount {
                id: "archive-disk".into(),
                name: "Archive disk".into(),
                path: archive.clone(),
            }],
        )
        .unwrap();

        assert_eq!(catalog.resolve("primary").unwrap().path, primary);
        assert_eq!(catalog.resolve("archive-disk").unwrap().path, archive);

        assert!(LocalMountCatalog::new(
            PathBuf::from(&catalog.resolve("primary").unwrap().path),
            vec![DeploymentLocalMount {
                id: "duplicate".into(),
                name: "Duplicate".into(),
                path: catalog.resolve("primary").unwrap().path.clone(),
            }],
        )
        .is_err());
        assert!(LocalMountCatalog::new(
            catalog.resolve("primary").unwrap().path.clone(),
            vec![DeploymentLocalMount {
                id: "nested".into(),
                name: "Nested".into(),
                path: catalog.resolve("primary").unwrap().path.join("nested"),
            }],
        )
        .is_err());
    }
}
