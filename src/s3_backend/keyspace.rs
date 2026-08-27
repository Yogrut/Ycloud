use crate::{
    error::{AppError, AppResult},
    storage::StorageService,
};

pub(super) fn parent_relative(relative: &str) -> &str {
    relative.rsplit_once('/').map_or("", |(parent, _)| parent)
}

pub(super) fn internal_key(prefix: &str, category: &str, id: &str) -> String {
    format!("{prefix}.ycloud-system/{category}/{id}")
}

pub(super) fn valid_transaction_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn copy_source(bucket: &str, key: &str) -> String {
    format!("{bucket}/{}", percent_encode_s3_key(key))
}

fn percent_encode_s3_key(key: &str) -> String {
    let mut encoded = String::with_capacity(key.len());
    for byte in key.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            encoded.push(char::from(*byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

pub(super) fn object_key(prefix: &str, relative: &str) -> AppResult<String> {
    let relative = StorageService::normalize_relative(relative)?;
    if relative.is_empty() {
        return Err(AppError::BadRequest("对象路径不能是存储根目录".into()));
    }
    Ok(format!("{prefix}{relative}"))
}

pub(super) fn list_prefix(prefix: &str, relative: &str) -> AppResult<String> {
    let relative = StorageService::normalize_relative(relative)?;
    if relative.is_empty() {
        return Ok(prefix.to_owned());
    }
    Ok(format!("{prefix}{relative}/"))
}
