use std::{
    ffi::OsString,
    os::{fd::OwnedFd, unix::ffi::OsStringExt},
    path::Path,
    sync::Arc,
};

use rustix::{
    fs::{
        fchmod, fstatvfs, fsync, mkdirat, open, openat, openat2, renameat_with, unlinkat, AtFlags,
        Dir, Mode, OFlags, RenameFlags, ResolveFlags,
    },
    io::Errno,
};
use tokio::fs::File;

use crate::error::{AppError, AppResult};

const RESOLVE_WITHIN_ROOT: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_MAGICLINKS);

#[derive(Clone)]
pub(crate) struct LinuxRoot {
    descriptor: Arc<OwnedFd>,
}

pub(crate) struct LinuxDirectoryEntry {
    pub(crate) name: OsString,
    pub(crate) metadata: std::fs::Metadata,
}

impl LinuxRoot {
    pub(crate) fn open(root: &Path) -> AppResult<Self> {
        let descriptor = open(
            root,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| linux_error("failed to open local storage root handle", error))?;
        Ok(Self {
            descriptor: Arc::new(descriptor),
        })
    }

    pub(super) async fn validate_existing(&self, relative: &str) -> AppResult<()> {
        self.metadata(relative).await.map(|_| ())
    }

    pub(super) async fn validate_for_write(&self, relative: &str) -> AppResult<()> {
        let parent =
            relative.rsplit_once('/').map_or(
                ".",
                |(parent, _)| if parent.is_empty() { "." } else { parent },
            );
        self.open_path(
            parent,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        )
        .await
        .map_err(|error| match error {
            AppError::NotFound => {
                AppError::BadRequest("Destination directory does not exist".into())
            }
            error => error,
        })?;

        if relative.is_empty() {
            return Ok(());
        }
        match self
            .open_path(relative, OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC)
            .await
        {
            Ok(_) | Err(AppError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn metadata(&self, relative: &str) -> AppResult<std::fs::Metadata> {
        let descriptor = self
            .open_path(relative, OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC)
            .await?;
        let metadata = std::fs::File::from(descriptor)
            .metadata()
            .map_err(|error| AppError::with_source("failed to read file metadata", error))?;
        if metadata.file_type().is_symlink() {
            return Err(AppError::Forbidden);
        }
        Ok(metadata)
    }

    pub(crate) async fn open_file_for_read(&self, relative: &str) -> AppResult<File> {
        let descriptor = self
            .open_path(
                relative,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            )
            .await?;
        let file = std::fs::File::from(descriptor);
        let metadata = file
            .metadata()
            .map_err(|error| AppError::with_source("failed to inspect opened file", error))?;
        if metadata.file_type().is_symlink() {
            return Err(AppError::Forbidden);
        }
        if !metadata.is_file() {
            return Err(AppError::NotFound);
        }
        Ok(File::from_std(file))
    }

    pub(crate) async fn create_directory(&self, relative: &str) -> AppResult<()> {
        self.create_directory_with_mode(relative, 0o777).await
    }

    async fn create_directory_with_mode(&self, relative: &str, mode: u32) -> AppResult<()> {
        let descriptor = self.descriptor.clone();
        let relative = relative.to_owned();
        tokio::task::spawn_blocking(move || {
            let (parent, name) = open_parent(&descriptor, &relative)?;
            mkdirat(&parent, name, Mode::from(mode)).map_err(|error| {
                map_mutation_error("failed to create directory below local storage root", error)
            })
        })
        .await
        .map_err(|error| AppError::with_source("local storage directory task failed", error))?
    }

    pub(crate) async fn ensure_private_directory(&self, relative: &str) -> AppResult<bool> {
        let created = match self.metadata(relative).await {
            Ok(metadata) if metadata.is_dir() => false,
            Ok(_) => {
                return Err(AppError::Conflict(
                    "Transaction path must be a private local directory".into(),
                ))
            }
            Err(AppError::NotFound) => {
                self.create_directory_with_mode(relative, 0o700).await?;
                true
            }
            Err(AppError::Forbidden) => {
                return Err(AppError::Conflict(
                    "Transaction path must be a private local directory".into(),
                ))
            }
            Err(error) => return Err(error),
        };
        self.set_permissions(relative, 0o700, true).await?;
        self.sync_directory(relative).await?;
        if created {
            self.sync_parent(relative).await?;
        }
        Ok(created)
    }

    pub(crate) async fn set_private_file_permissions(&self, relative: &str) -> AppResult<()> {
        self.set_permissions(relative, 0o600, false).await
    }

    async fn set_permissions(&self, relative: &str, mode: u32, directory: bool) -> AppResult<()> {
        let descriptor = self.descriptor.clone();
        let relative = relative.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            if directory {
                flags |= OFlags::DIRECTORY;
            }
            let target = openat2(
                descriptor.as_ref(),
                relative,
                flags,
                Mode::empty(),
                RESOLVE_WITHIN_ROOT,
            )
            .map_err(map_resolution_error)?;
            fchmod(&target, Mode::from(mode)).map_err(|error| {
                map_mutation_error("failed to restrict local storage permissions", error)
            })
        })
        .await
        .map_err(|error| AppError::with_source("local storage permission task failed", error))?
    }

    async fn sync_directory(&self, relative: &str) -> AppResult<()> {
        let descriptor = self.descriptor.clone();
        let relative = relative.to_owned();
        tokio::task::spawn_blocking(move || {
            let directory = openat2(
                descriptor.as_ref(),
                relative,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
                RESOLVE_WITHIN_ROOT,
            )
            .map_err(map_resolution_error)?;
            fsync(&directory).map_err(|error| {
                map_mutation_error("failed to flush local storage directory", error)
            })
        })
        .await
        .map_err(|error| AppError::with_source("local storage directory sync task failed", error))?
    }

    pub(crate) async fn rename_noreplace(&self, source: &str, destination: &str) -> AppResult<()> {
        let descriptor = self.descriptor.clone();
        let source = source.to_owned();
        let destination = destination.to_owned();
        tokio::task::spawn_blocking(move || {
            let (source_parent, source_name) = open_parent(&descriptor, &source)?;
            let (destination_parent, destination_name) = open_parent(&descriptor, &destination)?;
            renameat_with(
                &source_parent,
                source_name,
                &destination_parent,
                destination_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(|error| map_mutation_error("failed to rename below local storage root", error))
        })
        .await
        .map_err(|error| AppError::with_source("local storage rename task failed", error))?
    }

    pub(crate) async fn create_file_new(
        &self,
        relative: &str,
        mode: u32,
    ) -> AppResult<std::fs::File> {
        let descriptor = self.descriptor.clone();
        let relative = relative.to_owned();
        tokio::task::spawn_blocking(move || {
            let (parent, name) = open_parent(&descriptor, &relative)?;
            let file = openat(
                &parent,
                name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from(mode),
            )
            .map_err(|error| {
                map_mutation_error("failed to create file below local storage root", error)
            })?;
            Ok(std::fs::File::from(file))
        })
        .await
        .map_err(|error| AppError::with_source("local storage file task failed", error))?
    }

    pub(crate) async fn remove_file(&self, relative: &str) -> AppResult<()> {
        let descriptor = self.descriptor.clone();
        let relative = relative.to_owned();
        tokio::task::spawn_blocking(move || {
            let (parent, name) = open_parent(&descriptor, &relative)?;
            unlinkat(&parent, name, AtFlags::empty()).map_err(|error| {
                map_mutation_error("failed to remove file below local storage root", error)
            })
        })
        .await
        .map_err(|error| AppError::with_source("local storage removal task failed", error))?
    }

    pub(crate) async fn remove_any_bounded(
        &self,
        relative: &str,
        max_removed_nodes: usize,
    ) -> AppResult<(usize, bool)> {
        if max_removed_nodes == 0 {
            return Ok((0, false));
        }
        let descriptor = self.descriptor.clone();
        let relative = relative.to_owned();
        tokio::task::spawn_blocking(move || {
            remove_any_bounded(&descriptor, &relative, max_removed_nodes)
        })
        .await
        .map_err(|error| AppError::with_source("local storage cleanup task failed", error))?
    }

    pub(crate) async fn sync_parent(&self, relative: &str) -> AppResult<()> {
        let descriptor = self.descriptor.clone();
        let relative = relative.to_owned();
        tokio::task::spawn_blocking(move || {
            let (parent, _) = open_parent(&descriptor, &relative)?;
            fsync(&parent).map_err(|error| {
                map_mutation_error("failed to flush local storage parent directory", error)
            })
        })
        .await
        .map_err(|error| AppError::with_source("local storage sync task failed", error))?
    }

    pub(crate) async fn read_directory(
        &self,
        relative: &str,
    ) -> AppResult<Vec<LinuxDirectoryEntry>> {
        self.read_directory_with_policy(relative, false).await
    }

    pub(crate) async fn read_directory_with_policy(
        &self,
        relative: &str,
        reject_links: bool,
    ) -> AppResult<Vec<LinuxDirectoryEntry>> {
        let descriptor = self.descriptor.clone();
        let relative = relative.to_owned();
        tokio::task::spawn_blocking(move || read_directory(&descriptor, &relative, reject_links))
            .await
            .map_err(|error| AppError::with_source("local storage listing task failed", error))?
    }

    pub(crate) async fn copy_path(
        &self,
        source: &str,
        destination: &str,
        directory: bool,
    ) -> AppResult<()> {
        if !directory {
            return self.copy_file(source, destination).await;
        }
        let mut pending = vec![(source.to_owned(), destination.to_owned())];
        while let Some((current_source, current_destination)) = pending.pop() {
            self.create_directory(&current_destination).await?;
            for entry in self
                .read_directory_with_policy(&current_source, true)
                .await?
            {
                let name = entry.name.to_str().ok_or_else(|| {
                    AppError::Conflict("Local storage entry is not valid UTF-8".into())
                })?;
                let source_child = join_relative(&current_source, name);
                let destination_child = join_relative(&current_destination, name);
                if entry.metadata.is_dir() {
                    pending.push((source_child, destination_child));
                } else if entry.metadata.is_file() {
                    self.copy_file(&source_child, &destination_child).await?;
                } else {
                    return Err(AppError::Forbidden);
                }
            }
        }
        Ok(())
    }

    async fn copy_file(&self, source: &str, destination: &str) -> AppResult<()> {
        let mut source = self.open_file_for_read(source).await?;
        let destination = self.create_file_new(destination, 0o600).await?;
        let mut destination = File::from_std(destination);
        tokio::io::copy(&mut source, &mut destination)
            .await
            .map_err(|error| AppError::with_source("failed to copy local storage file", error))?;
        destination
            .sync_all()
            .await
            .map_err(|error| AppError::with_source("failed to flush copied file", error))
    }

    pub(crate) async fn path_size(
        &self,
        relative: &str,
        skip_reserved_root_entry: bool,
    ) -> AppResult<u64> {
        let descriptor = self.descriptor.clone();
        let relative = relative.to_owned();
        tokio::task::spawn_blocking(move || {
            calculate_path_size(&descriptor, &relative, skip_reserved_root_entry)
        })
        .await
        .map_err(|error| AppError::with_source("local storage size task failed", error))?
    }

    pub(crate) async fn available_space(&self) -> AppResult<u64> {
        let descriptor = self.descriptor.clone();
        tokio::task::spawn_blocking(move || {
            let status = fstatvfs(descriptor.as_ref())
                .map_err(|error| linux_error("failed to inspect local storage capacity", error))?;
            Ok(status.f_bavail.saturating_mul(status.f_frsize))
        })
        .await
        .map_err(|error| AppError::with_source("local storage capacity task failed", error))?
    }

    async fn open_path(&self, relative: &str, flags: OFlags) -> AppResult<OwnedFd> {
        let descriptor = self.descriptor.clone();
        let relative = if relative.is_empty() {
            ".".to_owned()
        } else {
            relative.to_owned()
        };
        tokio::task::spawn_blocking(move || {
            openat2(
                descriptor.as_ref(),
                relative,
                flags,
                Mode::empty(),
                RESOLVE_WITHIN_ROOT,
            )
            .map_err(map_resolution_error)
        })
        .await
        .map_err(|error| AppError::with_source("local storage path task failed", error))?
    }
}

fn open_parent(descriptor: &OwnedFd, relative: &str) -> AppResult<(OwnedFd, String)> {
    let (parent, name) = match relative.rsplit_once('/') {
        Some((parent, name)) if !name.is_empty() => {
            (if parent.is_empty() { "." } else { parent }, name)
        }
        None if !relative.is_empty() => (".", relative),
        _ => return Err(AppError::BadRequest("Invalid local storage path".into())),
    };
    let parent = openat2(
        descriptor,
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        RESOLVE_WITHIN_ROOT,
    )
    .map_err(map_resolution_error)?;
    Ok((parent, name.to_owned()))
}

fn read_directory(
    descriptor: &OwnedFd,
    relative: &str,
    reject_links: bool,
) -> AppResult<Vec<LinuxDirectoryEntry>> {
    let directory = openat2(
        descriptor,
        if relative.is_empty() { "." } else { relative },
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        RESOLVE_WITHIN_ROOT,
    )
    .map_err(map_resolution_error)?;
    let mut directory = Dir::new(directory)
        .map_err(|error| linux_error("failed to read local storage directory", error))?;
    let mut result = Vec::new();
    while let Some(entry) = directory.next() {
        let entry = entry
            .map_err(|error| linux_error("failed to read local storage directory entry", error))?;
        let name = entry.file_name().to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        let entry_descriptor = match openat2(
            directory
                .fd()
                .map_err(|error| linux_error("failed to access local directory handle", error))?,
            entry.file_name(),
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            RESOLVE_WITHIN_ROOT,
        ) {
            Ok(descriptor) => descriptor,
            Err(Errno::LOOP) if !reject_links => continue,
            Err(error) => return Err(map_resolution_error(error)),
        };
        let metadata = std::fs::File::from(entry_descriptor)
            .metadata()
            .map_err(|error| {
                AppError::with_source("failed to inspect local storage directory entry", error)
            })?;
        if metadata.file_type().is_symlink() {
            if reject_links {
                return Err(AppError::Forbidden);
            }
            continue;
        }
        result.push(LinuxDirectoryEntry {
            name: OsString::from_vec(name.to_vec()),
            metadata,
        });
    }
    Ok(result)
}

fn join_relative(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn calculate_path_size(
    descriptor: &OwnedFd,
    relative: &str,
    skip_reserved_root_entry: bool,
) -> AppResult<u64> {
    let initial = openat2(
        descriptor,
        if relative.is_empty() { "." } else { relative },
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        RESOLVE_WITHIN_ROOT,
    )
    .map_err(map_resolution_error)?;
    let metadata = std::fs::File::from(initial)
        .metadata()
        .map_err(|error| AppError::with_source("failed to inspect local storage usage", error))?;
    if metadata.file_type().is_symlink() {
        return Err(AppError::Forbidden);
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Err(AppError::Forbidden);
    }

    let root_directory = openat2(
        descriptor,
        if relative.is_empty() { "." } else { relative },
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        RESOLVE_WITHIN_ROOT,
    )
    .map_err(map_resolution_error)?;
    let root_directory = Dir::new(root_directory)
        .map_err(|error| linux_error("failed to read local storage usage", error))?;
    let mut stack = vec![(root_directory, true)];
    let mut total = 0_u64;
    while let Some((mut directory, is_root)) = stack.pop() {
        while let Some(entry) = directory.next() {
            let entry = entry
                .map_err(|error| linux_error("failed to read local storage usage entry", error))?;
            let name = entry.file_name().to_bytes();
            if matches!(name, b"." | b"..")
                || is_root
                    && skip_reserved_root_entry
                    && name.eq_ignore_ascii_case(crate::storage_transaction::SYSTEM_DIR.as_bytes())
            {
                continue;
            }
            let directory_fd = directory.fd().map_err(|error| {
                linux_error("failed to access local storage usage directory", error)
            })?;
            let entry_descriptor = match openat2(
                directory_fd,
                entry.file_name(),
                OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
                RESOLVE_WITHIN_ROOT,
            ) {
                Ok(descriptor) => descriptor,
                Err(Errno::LOOP) => continue,
                Err(error) => return Err(map_resolution_error(error)),
            };
            let metadata = std::fs::File::from(entry_descriptor)
                .metadata()
                .map_err(|error| {
                    AppError::with_source("failed to inspect local storage usage entry", error)
                })?;
            if metadata.file_type().is_symlink() {
                continue;
            } else if metadata.is_file() {
                total = total
                    .checked_add(metadata.len())
                    .ok_or_else(|| AppError::internal("local storage usage exceeds u64"))?;
            } else if metadata.is_dir() {
                let child = openat2(
                    directory_fd,
                    entry.file_name(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                    RESOLVE_WITHIN_ROOT,
                )
                .map_err(map_resolution_error)?;
                stack.push((
                    Dir::new(child).map_err(|error| {
                        linux_error("failed to read local storage usage directory", error)
                    })?,
                    false,
                ));
            }
        }
    }
    Ok(total)
}

fn remove_any_bounded(
    descriptor: &OwnedFd,
    relative: &str,
    max_removed_nodes: usize,
) -> AppResult<(usize, bool)> {
    let (parent, name) = open_parent(descriptor, relative)?;
    let target = match openat2(
        &parent,
        &name,
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        RESOLVE_WITHIN_ROOT,
    ) {
        Ok(target) => target,
        Err(Errno::NOENT) => return Ok((0, true)),
        Err(error) => return Err(map_resolution_error(error)),
    };
    let metadata = std::fs::File::from(target)
        .metadata()
        .map_err(|error| AppError::with_source("failed to inspect local cleanup path", error))?;
    if metadata.file_type().is_symlink() {
        return Err(AppError::Conflict(
            "Unexpected internal link; cleanup stopped".into(),
        ));
    }
    if metadata.is_file() {
        return match unlinkat(&parent, &name, AtFlags::empty()) {
            Ok(()) => Ok((1, true)),
            Err(Errno::NOENT) => Ok((0, true)),
            Err(error) => Err(map_mutation_error(
                "failed to remove internal staging file",
                error,
            )),
        };
    }
    if !metadata.is_dir() {
        return Err(AppError::Conflict(
            "Unexpected internal path type; cleanup stopped".into(),
        ));
    }

    let directory = openat2(
        &parent,
        &name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        RESOLVE_WITHIN_ROOT,
    )
    .map_err(map_resolution_error)?;
    let mut remaining = max_removed_nodes;
    let mut removed = 0;
    if !remove_directory_contents(directory, &mut remaining, &mut removed)? || remaining == 0 {
        return Ok((removed, false));
    }
    match unlinkat(&parent, &name, AtFlags::REMOVEDIR) {
        Ok(()) => Ok((removed + 1, true)),
        Err(Errno::NOENT) => Ok((removed, true)),
        Err(Errno::NOTEMPTY) => Ok((removed, false)),
        Err(error) => Err(map_mutation_error(
            "failed to remove internal staging directory",
            error,
        )),
    }
}

fn remove_directory_contents(
    descriptor: OwnedFd,
    remaining: &mut usize,
    removed: &mut usize,
) -> AppResult<bool> {
    let mut directory = Dir::new(descriptor)
        .map_err(|error| linux_error("failed to read internal staging directory", error))?;
    while let Some(entry) = directory.next() {
        if *remaining == 0 {
            return Ok(false);
        }
        let entry = entry.map_err(|error| {
            linux_error("failed to read internal staging directory entry", error)
        })?;
        let name = entry.file_name().to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        let directory_fd = directory.fd().map_err(|error| {
            linux_error("failed to access internal staging directory handle", error)
        })?;
        let target = match openat2(
            directory_fd,
            entry.file_name(),
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            RESOLVE_WITHIN_ROOT,
        ) {
            Ok(target) => target,
            Err(Errno::NOENT) => continue,
            Err(error) => return Err(map_resolution_error(error)),
        };
        let metadata = std::fs::File::from(target).metadata().map_err(|error| {
            AppError::with_source("failed to inspect internal staging entry", error)
        })?;
        if metadata.file_type().is_symlink() {
            return Err(AppError::Conflict(
                "Unexpected internal link; cleanup stopped".into(),
            ));
        }
        if metadata.is_dir() {
            let child = match openat2(
                directory_fd,
                entry.file_name(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
                RESOLVE_WITHIN_ROOT,
            ) {
                Ok(child) => child,
                Err(Errno::NOENT) => continue,
                Err(error) => return Err(map_resolution_error(error)),
            };
            if !remove_directory_contents(child, remaining, removed)? || *remaining == 0 {
                return Ok(false);
            }
            match unlinkat(directory_fd, entry.file_name(), AtFlags::REMOVEDIR) {
                Ok(()) => {
                    *remaining -= 1;
                    *removed += 1;
                }
                Err(Errno::NOENT) => {}
                Err(Errno::NOTEMPTY) => return Ok(false),
                Err(error) => {
                    return Err(map_mutation_error(
                        "failed to remove internal staging directory",
                        error,
                    ))
                }
            }
        } else if metadata.is_file() {
            match unlinkat(directory_fd, entry.file_name(), AtFlags::empty()) {
                Ok(()) => {
                    *remaining -= 1;
                    *removed += 1;
                }
                Err(Errno::NOENT) => {}
                Err(error) => {
                    return Err(map_mutation_error(
                        "failed to remove internal staging file",
                        error,
                    ))
                }
            }
        } else {
            return Err(AppError::Conflict(
                "Unexpected internal path type; cleanup stopped".into(),
            ));
        }
    }
    Ok(true)
}

fn map_mutation_error(context: &'static str, error: Errno) -> AppError {
    match error {
        Errno::EXIST => AppError::Conflict("Destination already exists".into()),
        Errno::NOENT => AppError::NotFound,
        Errno::LOOP | Errno::XDEV | Errno::NOTDIR | Errno::ACCESS | Errno::PERM => {
            AppError::Forbidden
        }
        Errno::NOSYS => AppError::ServiceUnavailable(
            "Local storage requires Linux descriptor-relative operations".into(),
        ),
        _ => linux_error(context, error),
    }
}

fn map_resolution_error(error: Errno) -> AppError {
    match error {
        Errno::NOENT => AppError::NotFound,
        Errno::LOOP | Errno::XDEV | Errno::NOTDIR | Errno::ACCESS | Errno::PERM => {
            AppError::Forbidden
        }
        Errno::NOSYS => AppError::ServiceUnavailable(
            "Local storage requires Linux openat2 path protection".into(),
        ),
        _ => linux_error("failed to resolve path below local storage root", error),
    }
}

fn linux_error(context: &'static str, error: Errno) -> AppError {
    AppError::with_source(context, std::io::Error::from(error))
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::fs::symlink;

    use tempfile::tempdir;
    use tokio::io::AsyncReadExt;

    use super::*;

    #[tokio::test]
    async fn root_handle_rejects_symlinks_and_survives_path_replacement() {
        let parent = tempdir().expect("temporary parent");
        let root = parent.path().join("storage");
        let moved_root = parent.path().join("storage-moved");
        let outside = parent.path().join("outside");
        std::fs::create_dir(&root).expect("create storage root");
        std::fs::create_dir(&outside).expect("create outside directory");
        std::fs::write(outside.join("secret"), b"outside").expect("write outside file");

        let handle = LinuxRoot::open(&root).expect("open storage root handle");
        symlink(&outside, root.join("escape")).expect("create escape symlink");
        assert!(matches!(
            handle.open_file_for_read("escape/secret").await,
            Err(AppError::Forbidden)
        ));

        std::fs::rename(&root, &moved_root).expect("move original root");
        std::fs::create_dir(&root).expect("replace path with a different directory");
        std::fs::write(root.join("replacement"), b"replacement").expect("write replacement file");
        std::fs::write(moved_root.join("original"), b"original")
            .expect("write through original directory path");

        assert!(matches!(
            handle.open_file_for_read("replacement").await,
            Err(AppError::NotFound)
        ));
        let mut file = handle
            .open_file_for_read("original")
            .await
            .expect("root handle remains bound to original directory");
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .await
            .expect("read original file");
        assert_eq!(contents, "original");
    }

    #[tokio::test]
    async fn mutations_copy_listing_and_cleanup_remain_bound_to_root_handle() {
        let parent = tempdir().expect("temporary parent");
        let root = parent.path().join("storage");
        let moved_root = parent.path().join("storage-moved");
        let outside = parent.path().join("outside");
        std::fs::create_dir(&root).expect("create storage root");
        std::fs::create_dir(&outside).expect("create outside directory");
        std::fs::write(outside.join("secret"), b"outside").expect("write outside file");

        let handle = LinuxRoot::open(&root).expect("open storage root handle");
        std::fs::rename(&root, &moved_root).expect("move original root");
        std::fs::create_dir(&root).expect("replace root path");

        handle
            .create_directory("source")
            .await
            .expect("create below pinned root");
        let mut file = handle
            .create_file_new("source/note.txt", 0o600)
            .await
            .expect("create file below pinned root");
        file.write_all(b"original").expect("write rooted file");
        file.sync_all().expect("flush rooted file");
        drop(file);
        assert!(moved_root.join("source/note.txt").is_file());
        assert!(!root.join("source").exists());

        handle
            .rename_noreplace("source/note.txt", "source/renamed.txt")
            .await
            .expect("rename below pinned root");
        let mut occupied = handle
            .create_file_new("source/occupied.txt", 0o600)
            .await
            .expect("create occupied destination");
        occupied.write_all(b"occupied").expect("write destination");
        drop(occupied);
        assert!(matches!(
            handle
                .rename_noreplace("source/renamed.txt", "source/occupied.txt")
                .await,
            Err(AppError::Conflict(_))
        ));

        handle
            .copy_path("source", "copy", true)
            .await
            .expect("copy directory below pinned root");
        assert_eq!(handle.path_size("copy", false).await.unwrap(), 16);
        let names = handle
            .read_directory("copy")
            .await
            .expect("list copied directory")
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&OsString::from("renamed.txt")));
        assert!(names.contains(&OsString::from("occupied.txt")));

        symlink(&outside, moved_root.join("source/escape")).expect("create internal symlink");
        assert!(matches!(
            handle.copy_path("source", "unsafe-copy", true).await,
            Err(AppError::Forbidden)
        ));
        assert!(!handle
            .read_directory("source")
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.name == OsString::from("escape")));

        let first = handle
            .remove_any_bounded("copy", 1)
            .await
            .expect("remove first bounded batch");
        assert_eq!(first, (1, false));
        let mut complete = false;
        while !complete {
            complete = handle
                .remove_any_bounded("copy", 1)
                .await
                .expect("continue bounded cleanup")
                .1;
        }
        assert!(!moved_root.join("copy").exists());
        assert!(!root.join("copy").exists());
    }
}
