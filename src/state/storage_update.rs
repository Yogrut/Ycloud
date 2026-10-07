//! Shared lifecycle decisions for local and S3 settings edits. Backend-specific
//! validation, initialization and retired-connection cleanup keep their owners.

use super::{prepare_storage_backend, AppState};
use crate::{
    config::{ConfigFile, StorageBackendConfig, StorageInstanceConfig},
    error::{AppError, AppResult},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdmissionChange {
    Unchanged,
    Reenable,
    InterruptPolicy,
    ReplaceConnection,
}

#[derive(Debug, PartialEq, Eq)]
struct StorageUpdatePlan {
    admission: AdmissionChange,
    invalidate_uploads: bool,
}

impl StorageUpdatePlan {
    fn new(previous: &StorageInstanceConfig, next: &StorageInstanceConfig, cached: bool) -> Self {
        let reuse = cached && same_connection(&previous.backend, &next.backend);
        let mode_changed = matches!((&previous.backend, &next.backend),
            (StorageBackendConfig::S3(old), StorageBackendConfig::S3(new))
                if old.relay_upload != new.relay_upload);
        let invalidate_uploads =
            !reuse || !next.enabled || mode_changed || guest_access_restricted(previous, next);
        let admission = if !reuse {
            AdmissionChange::ReplaceConnection
        } else if next.enabled && !previous.enabled {
            AdmissionChange::Reenable
        } else if invalidate_uploads {
            AdmissionChange::InterruptPolicy
        } else {
            AdmissionChange::Unchanged
        };
        Self {
            admission,
            invalidate_uploads,
        }
    }

    fn reuse_backend(&self) -> bool {
        self.admission != AdmissionChange::ReplaceConnection
    }
}

fn same_connection(previous: &StorageBackendConfig, next: &StorageBackendConfig) -> bool {
    match (previous, next) {
        (StorageBackendConfig::Local(old), StorageBackendConfig::Local(new)) => {
            old.mount_id == new.mount_id
        }
        (StorageBackendConfig::S3(old), StorageBackendConfig::S3(new)) => {
            // Only these two fields are runtime policy, not connection identity.
            // Equality still includes any future connection fields by default.
            let mut connection = new.clone();
            connection.capacity_limit_bytes = old.capacity_limit_bytes;
            connection.relay_upload = old.relay_upload;
            &connection == old
        }
        _ => false,
    }
}

pub(super) fn guest_access_restricted(
    previous: &StorageInstanceConfig,
    next: &StorageInstanceConfig,
) -> bool {
    previous.allow_guest_access
        && (!next.allow_guest_access
            || (previous.allow_guest_download.unwrap_or(true)
                && !next.allow_guest_download.unwrap_or(true)))
}

impl AppState {
    /// Called only while the settings entry point holds storage_updates. Keep
    /// its edit guard alive through preparation, persistence and publication.
    pub(super) async fn apply_storage_edit(
        &self,
        next: &ConfigFile,
        previous: &StorageInstanceConfig,
        storage_id: &str,
    ) -> AppResult<()> {
        next.validate()?;
        let candidate = next
            .storage_instances
            .iter()
            .find(|instance| instance.id == storage_id)
            .ok_or(AppError::NotFound)?;
        let cached = self.backends.cached(storage_id).await;
        let plan = StorageUpdatePlan::new(previous, candidate, cached.is_some());
        let _edit = match &cached {
            Some(backend) => match plan.admission {
                AdmissionChange::Unchanged => None,
                AdmissionChange::Reenable => Some(backend.reenable_guard().await?),
                AdmissionChange::InterruptPolicy => backend.interrupt_for_policy(),
                AdmissionChange::ReplaceConnection => Some(backend.interrupt_for_edit().await?),
            },
            None => None,
        };
        // A replacement may share a remote namespace or capacity-ledger ID.
        // Do not start its recovery while the old generation still owns writes.
        let prepared = if plan.reuse_backend() {
            cached.clone()
        } else if candidate.enabled {
            Some(
                prepare_storage_backend(
                    &self.config,
                    &self.local_io_gate,
                    next.max_upload_bytes,
                    storage_id,
                    &candidate.backend,
                )
                .await?,
            )
        } else {
            None
        };
        // Persistence retains old S3 cleanup responsibility before config is
        // published. Failed preparation/save must not retire the cached owner.
        self.persist_storage_selection(next).await?;
        if plan.invalidate_uploads {
            self.upload_batches
                .invalidate_storage(storage_id, cached.clone())
                .await;
        }
        if !plan.reuse_backend() {
            if let Some(backend) = &cached {
                backend.retire();
            }
        }
        if let Some(prepared) = prepared {
            prepared.set_capacity_limit(candidate.backend.capacity_limit_bytes());
            self.publish_backend(storage_id.to_string(), prepared).await;
            self.backends
                .set_enabled(storage_id, candidate.enabled)
                .await;
        } else {
            self.backends.remove(storage_id).await;
        }
        self.archive_tickets.clear().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
