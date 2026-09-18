//! One vocabulary for internal resource names, journal versions and destinations.
//! These checks do not replace handle-relative filesystem access (B3).
use std::{fmt, path::Path};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::{AppError, AppResult},
    storage::StorageService,
};

pub(super) const MAX_JOURNAL_BYTES: u64 = 16 * 1024;
pub(super) const MAX_RECOVERY_ENTRIES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TransactionId(String);

impl TransactionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Default for TransactionId {
    fn default() -> Self {
        Self::new()
    }
}

impl TryFrom<String> for TransactionId {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let id = Uuid::parse_str(&value).map_err(|_| "invalid transaction identifier")?;
        if id.to_string() != value || id.get_version_num() != 4 {
            return Err("transaction identifier must be a canonical UUID v4");
        }
        Ok(Self(value))
    }
}

impl From<TransactionId> for String {
    fn from(id: TransactionId) -> Self {
        id.0
    }
}

impl AsRef<Path> for TransactionId {
    fn as_ref(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl fmt::Display for TransactionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplaceJournal {
    // Missing version is the previous two-field format, not an arbitrary format.
    #[serde(default)]
    pub version: u32,
    pub id: TransactionId,
    pub destination: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeletionJournal {
    pub version: u32,
    pub id: TransactionId,
    pub bytes_upper_bound: u64,
    pub created_unix: u64,
}

impl DeletionJournal {
    pub fn new(id: TransactionId, bytes_upper_bound: u64) -> AppResult<Self> {
        let created_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| AppError::with_source("system clock is before the Unix epoch", error))?
            .as_secs();
        Ok(Self {
            version: 1,
            id,
            bytes_upper_bound,
            created_unix,
        })
    }

    pub fn validate(&self) -> AppResult<()> {
        if self.version != 1 {
            return Err(AppError::Conflict(
                "Unsupported deletion debt record version; recovery stopped".into(),
            ));
        }
        Ok(())
    }
}

impl ReplaceJournal {
    pub fn new(id: TransactionId, destination: String) -> AppResult<Self> {
        let journal = Self {
            version: 1,
            id,
            destination,
        };
        journal.validate()?;
        Ok(journal)
    }

    pub fn validate(&self) -> AppResult<()> {
        if self.version > 1 {
            return Err(AppError::Conflict(
                "Unsupported transaction journal version; recovery stopped".into(),
            ));
        }
        if self.destination.is_empty()
            || self.destination.len() > 4096
            || StorageService::normalize_relative(&self.destination)? != self.destination
        {
            return Err(AppError::Conflict(
                "Invalid transaction destination; recovery stopped".into(),
            ));
        }
        Ok(())
    }
}

pub(super) fn resource_id(path: &Path) -> AppResult<TransactionId> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            AppError::Conflict("Unrecognized internal resource; recovery stopped".into())
        })?;
    TransactionId::try_from(name.to_owned())
        .map_err(|_| AppError::Conflict("Unrecognized internal resource; recovery stopped".into()))
}
