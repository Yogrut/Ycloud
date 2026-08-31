use std::path::{Path, PathBuf};

use tokio::fs;

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use super::StorageService;
use crate::{
    error::{AppError, AppResult},
    storage_transaction::SYSTEM_DIR,
};

#[derive(Clone, Debug)]
pub struct ResolvedPath {
    pub(super) relative: String,
    pub(super) absolute: PathBuf,
}

impl ResolvedPath {
    pub fn relative(&self) -> &str {
        &self.relative
    }

    pub fn absolute(&self) -> &Path {
        &self.absolute
    }

    pub fn is_root(&self) -> bool {
        self.relative.is_empty()
    }
}

impl StorageService {
    pub fn normalize_relative(path: &str) -> AppResult<String> {
        normalize_relative(path)
    }

    pub async fn resolve_existing(&self, path: &str) -> AppResult<ResolvedPath> {
        let resolved = self.resolve_for_write(path).await?;
        let canonical = fs::canonicalize(&resolved.absolute)
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => AppError::NotFound,
                _ => AppError::with_source("failed to resolve storage path", error),
            })?;
        if !canonical.starts_with(self.root()) {
            return Err(AppError::Forbidden);
        }
        Ok(resolved)
    }

    pub async fn resolve_for_write(&self, path: &str) -> AppResult<ResolvedPath> {
        let relative = normalize_relative(path)?;
        let absolute = relative
            .split('/')
            .filter(|component| !component.is_empty())
            .fold(self.root().to_path_buf(), |current, component| {
                current.join(component)
            });

        let mut existing_ancestor = absolute.clone();
        loop {
            match fs::symlink_metadata(&existing_ancestor).await {
                Ok(metadata) => {
                    if is_link_or_reparse_point(&metadata) {
                        return Err(AppError::Forbidden);
                    }
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if !existing_ancestor.pop() {
                        return Err(AppError::Forbidden);
                    }
                }
                Err(error) => {
                    return Err(AppError::with_source(
                        "failed to validate storage path",
                        error,
                    ));
                }
            }
        }
        let canonical_ancestor = fs::canonicalize(&existing_ancestor)
            .await
            .map_err(|error| AppError::with_source("failed to validate storage path", error))?;
        if !canonical_ancestor.starts_with(self.root()) {
            return Err(AppError::Forbidden);
        }

        Ok(ResolvedPath { relative, absolute })
    }
}

fn normalize_relative(path: &str) -> AppResult<String> {
    if path.contains('\\') || path.contains('\0') {
        return Err(AppError::BadRequest("Invalid storage path".into()));
    }

    let mut components = Vec::new();
    for component in path.trim_matches('/').split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(AppError::BadRequest("Path traversal is not allowed".into())),
            value if value.contains(':') || value.eq_ignore_ascii_case(SYSTEM_DIR) => {
                return Err(AppError::BadRequest("Invalid storage path".into()));
            }
            value => components.push(value),
        }
    }
    Ok(components.join("/"))
}

pub(super) fn reject_root_or_descendant(
    source: &ResolvedPath,
    destination: &ResolvedPath,
) -> AppResult<()> {
    if source.is_root() {
        return Err(AppError::BadRequest(
            "The storage root cannot be moved or copied".into(),
        ));
    }
    if destination.absolute().starts_with(source.absolute()) {
        return Err(AppError::Conflict(
            "Destination cannot be inside the source".into(),
        ));
    }
    Ok(())
}

pub(super) async fn require_plain_directory(parent: Option<&Path>) -> AppResult<()> {
    let parent = parent.ok_or_else(|| AppError::BadRequest("Invalid destination path".into()))?;
    let metadata = fs::symlink_metadata(parent)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                AppError::BadRequest("Destination directory does not exist".into())
            }
            _ => AppError::with_source("failed to inspect destination directory", error),
        })?;
    if !metadata.is_dir() || is_link_or_reparse_point(&metadata) {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

pub(crate) fn is_link_or_reparse_point(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}
