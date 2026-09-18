//! Only complete uploads enter owned publication; receiving the request body
//! remains cancellable. Execution and logical quota transfer together.
use super::{AtomicFileWriter, AtomicWriteResult};
use crate::{
    capacity::{CapacityReservation, CapacityTracker},
    error::{AppError, AppResult, CleanupState, CommitState},
    storage_transaction::ReplacementObserver,
};

impl AtomicFileWriter {
    pub async fn commit(self) -> AppResult<AtomicWriteResult> {
        self.commit_owned(None).await
    }

    pub(crate) async fn commit_with_capacity(
        self,
        reservation: CapacityReservation,
    ) -> AppResult<AtomicWriteResult> {
        self.commit_owned(Some(reservation)).await
    }

    async fn commit_owned(
        self,
        reservation: Option<CapacityReservation>,
    ) -> AppResult<AtomicWriteResult> {
        // The writer already owns an I/O permit and a bounded staging ticket.
        // One task takes both, plus quota, without an intervening await.
        let accounting = PublicationAccounting::new(reservation, self.bytes_written);
        tokio::spawn(self.publish(accounting))
            .await
            .map_err(|error| {
                AppError::with_source("upload publication task failed", error)
                    .with_operation(CommitState::Unknown, CleanupState::Unknown)
            })?
    }

    async fn publish(
        mut self,
        mut accounting: PublicationAccounting,
    ) -> AppResult<AtomicWriteResult> {
        if self
            .expected_bytes
            .is_some_and(|expected| expected != self.bytes_written)
        {
            return Err(AppError::BadRequest(
                "Uploaded size does not match Content-Length".into(),
            ));
        }
        let file = self
            .file
            .as_ref()
            .ok_or_else(|| AppError::internal("temporary file is already closed"))?;
        file.sync_all()
            .await
            .map_err(|error| AppError::with_source("failed to flush upload", error))?;
        self.file.take();
        let _mutation = self.mutation_gate.lock().await;
        let result = self
            .transactions
            .commit_file(
                &self.relative,
                &self.temporary,
                &self.destination,
                &mut self.ownership,
                &mut accounting,
            )
            .await
            .map(|previous_size| AtomicWriteResult {
                size: self.bytes_written,
                previous_size,
            });
        let persisted = accounting.persist().await;
        let result = result.map_err(|error| {
            error.with_operation(accounting.commit_state(), CleanupState::Pending)
        });
        if result.is_err() {
            tracing::warn!(
                "upload publication incomplete; ownership and capacity state retained for recovery"
            );
        }
        match (result, persisted) {
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(error)) => {
                Err(error.with_operation(CommitState::Committed, CleanupState::Complete))
            }
            (Err(error), _) => Err(error),
        }
    }
}

struct PublicationAccounting {
    capacity: Option<CapacityTracker>,
    reservation: Option<CapacityReservation>,
    size: u64,
    awaiting_publication: bool,
    was_published: bool,
    ledger_settled: bool,
}

impl PublicationAccounting {
    fn new(reservation: Option<CapacityReservation>, size: u64) -> Self {
        let capacity = reservation.as_ref().map(CapacityReservation::tracker);
        Self {
            capacity,
            reservation,
            size,
            awaiting_publication: false,
            was_published: false,
            ledger_settled: false,
        }
    }

    fn commit_state(&self) -> CommitState {
        if self.was_published {
            CommitState::Committed
        } else if self.awaiting_publication {
            CommitState::Unknown
        } else {
            CommitState::NotCommitted
        }
    }

    async fn persist(&mut self) -> AppResult<()> {
        if self.awaiting_publication {
            if let Some(capacity) = &self.capacity {
                capacity.mark_uncertain();
            }
            return Ok(());
        }
        if !self.was_published {
            self.ledger_settled = true;
            return Ok(());
        }
        if let Some(capacity) = &self.capacity {
            if let Err(error) = capacity.persist().await {
                capacity.mark_uncertain();
                tracing::error!("failed to persist uploaded file capacity ledger");
                return Err(error);
            }
        }
        self.ledger_settled = true;
        Ok(())
    }
}

impl ReplacementObserver for PublicationAccounting {
    fn prepare(&mut self, previous_size: u64) -> AppResult<()> {
        if let Some(reservation) = &mut self.reservation {
            reservation.rebase_replacement(previous_size, self.size)?;
        }
        Ok(())
    }

    fn recovery_owned(&mut self) {
        self.awaiting_publication = true;
    }

    fn published(&mut self, previous_size: u64) {
        if let Some(reservation) = self.reservation.take() {
            reservation.commit(previous_size, self.size);
        }
        self.awaiting_publication = false;
        self.was_published = true;
    }
}

impl Drop for PublicationAccounting {
    fn drop(&mut self) {
        if self.awaiting_publication || self.was_published && !self.ledger_settled {
            if let Some(capacity) = &self.capacity {
                capacity.mark_uncertain();
            }
        }
    }
}

#[cfg(test)]
mod tests;
