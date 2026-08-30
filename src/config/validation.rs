use std::collections::HashSet;
use std::net::IpAddr;

use argon2::PasswordHash;

use super::{
    paths_overlap, AppError, AppResult, ConfigFile, S3AddressingStyle, S3Provider,
    StorageBackendConfig, StorageInstanceConfig, CONFIG_SCHEMA_VERSION, HARD_MAX_ARCHIVE_BYTES,
    HARD_MAX_ARCHIVE_ENTRIES, HARD_MAX_STORAGE_CAPACITY_BYTES, HARD_MAX_TRANSFER_RATE_BYTES,
    HARD_MAX_UPLOAD_BATCH_BYTES, HARD_MAX_UPLOAD_BATCH_ENTRIES, HARD_MAX_UPLOAD_BYTES,
    MAX_STORAGE_INSTANCES, MAX_USER_ACCOUNTS, MIN_STORAGE_CAPACITY_BYTES, MIN_TRANSFER_BYTES,
    MIN_TRANSFER_RATE_BYTES,
};

impl ConfigFile {
    pub fn validate(&self) -> AppResult<()> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(AppError::BadRequest(
                "Unsupported configuration schema version".into(),
            ));
        }
        let username = self.admin_username.trim();
        if username.is_empty() || username.len() > 128 {
            return Err(AppError::BadRequest(
                "Administrator username must contain 1-128 characters".into(),
            ));
        }
        if PasswordHash::new(&self.admin_password_hash).is_err() {
            return Err(AppError::BadRequest(
                "Administrator password hash is invalid".into(),
            ));
        }
        if let Some(secret) = self.admin_totp_secret.as_deref() {
            crate::totp::validate_secret(secret)?;
        } else if !self.admin_recovery_code_hashes.is_empty() {
            return Err(AppError::BadRequest("未启用 TOTP 时不能保留恢复码".into()));
        }
        if self.admin_recovery_code_hashes.len() > 10
            || self
                .admin_recovery_code_hashes
                .iter()
                .any(|hash| PasswordHash::new(hash).is_err())
        {
            return Err(AppError::BadRequest("管理员恢复码配置无效".into()));
        }
        if self.user_accounts.len() > MAX_USER_ACCOUNTS {
            return Err(AppError::BadRequest("普通账号数量不能超过 100 个".into()));
        }
        if self.shares.len() > 1_000 || self.folder_locks.len() > 10_000 {
            return Err(AppError::BadRequest(
                "Configuration exceeds supported limits".into(),
            ));
        }
        validate_transfer_limits(
            self.max_upload_bytes,
            self.max_upload_batch_bytes,
            self.max_upload_batch_entries,
            self.max_archive_bytes,
            self.max_archive_entries,
        )?;
        validate_login_security_settings(
            self.admin_login_failures,
            self.web_login_failures,
            self.admin_login_block_seconds,
            self.web_login_block_seconds,
            self.security_log_retention_days,
            self.security_log_max_entries,
        )?;
        validate_transfer_rate(self.upload_rate_bytes_per_sec, "上传")?;
        validate_transfer_rate(self.download_rate_bytes_per_sec, "下载")?;
        if self.storage_instances.len() > MAX_STORAGE_INSTANCES {
            return Err(AppError::BadRequest("存储实例数量不能超过 16 个".into()));
        }
        let mut storage_ids = HashSet::new();
        let mut storage_names = HashSet::new();
        let mut local_mount_ids = HashSet::new();
        for storage in &self.storage_instances {
            validate_storage_instance(storage)?;
            if !storage_ids.insert(storage.id.as_str()) {
                return Err(AppError::Conflict("存储实例 ID 必须唯一".into()));
            }
            if !storage_names.insert(storage.name.trim().to_lowercase()) {
                return Err(AppError::Conflict("存储实例名称必须唯一".into()));
            }
            if let StorageBackendConfig::Local(settings) = &storage.backend {
                validate_storage_id(&settings.mount_id)?;
                if !local_mount_ids.insert(settings.mount_id.as_str()) {
                    return Err(AppError::Conflict(
                        "同一个部署挂载点不能配置为多个本地存储实例".into(),
                    ));
                }
            }
        }
        if self.storage_instances.is_empty() {
            return Err(AppError::BadRequest("至少需要保留一个存储实例".into()));
        }
        let default_storage = self
            .storage_instances
            .iter()
            .find(|storage| storage.id == self.default_storage_id)
            .ok_or_else(|| AppError::BadRequest("默认存储必须引用已存在的存储实例".into()))?;
        if !default_storage.enabled {
            return Err(AppError::Conflict("默认存储必须保持启用".into()));
        }
        let mut account_ids = HashSet::new();
        let mut account_names = HashSet::new();
        account_names.insert(self.admin_username.trim().to_lowercase());
        for account in &self.user_accounts {
            if account.id.is_empty()
                || account.id.len() > 128
                || !account
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                || !account_ids.insert(account.id.as_str())
            {
                return Err(AppError::Conflict("普通账号 ID 必须唯一且格式有效".into()));
            }
            let username = account.username.trim();
            if username.is_empty()
                || username.chars().count() > 128
                || username.chars().any(char::is_whitespace)
                || !account_names.insert(username.to_lowercase())
            {
                return Err(AppError::Conflict(
                    "普通账号用户名必须唯一，且不能包含空白字符".into(),
                ));
            }
            if PasswordHash::new(&account.password_hash).is_err() {
                return Err(AppError::BadRequest("普通账号密码哈希无效".into()));
            }
            let mut permission_storages = HashSet::new();
            for permission in &account.permissions {
                validate_storage_reference(&permission.storage_id, &storage_ids)?;
                if !permission_storages.insert(permission.storage_id.as_str()) {
                    return Err(AppError::Conflict(
                        "同一普通账号不能重复配置同一存储权限".into(),
                    ));
                }
                if !permission.grants_any() {
                    return Err(AppError::BadRequest(
                        "普通账号的存储权限不能全部为空".into(),
                    ));
                }
                if !permission.browse {
                    return Err(AppError::BadRequest(
                        "授予其他文件权限时必须同时授予浏览权限".into(),
                    ));
                }
            }
        }
        if let Some(pending) = &self.pending_storage_instance {
            validate_storage_instance(pending)?;
            if storage_ids.contains(pending.id.as_str()) {
                return Err(AppError::Conflict("待添加存储实例 ID 已存在".into()));
            }
            if storage_names.contains(pending.name.trim().to_lowercase().as_str()) {
                return Err(AppError::Conflict("待添加存储实例名称已存在".into()));
            }
            if let StorageBackendConfig::Local(settings) = &pending.backend {
                validate_storage_id(&settings.mount_id)?;
                if local_mount_ids.contains(settings.mount_id.as_str()) {
                    return Err(AppError::Conflict(
                        "待添加本地存储不能重复使用已有部署挂载点".into(),
                    ));
                }
            }
        }

        let mut share_ids = HashSet::new();
        let mut share_names = HashSet::new();
        for share in &self.shares {
            validate_storage_reference(&share.storage_id, &storage_ids)?;
            if share.id.is_empty()
                || share.id.len() > 128
                || !share
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                || !share_ids.insert(share.id.as_str())
            {
                return Err(AppError::Conflict("Share IDs must be unique".into()));
            }
            let name = share.name.trim();
            if name.is_empty()
                || name.len() > 128
                || name.contains('/')
                || name.contains('\\')
                || name.contains('\0')
            {
                return Err(AppError::BadRequest("Share name is invalid".into()));
            }
            if !share_names.insert(name.to_lowercase()) {
                return Err(AppError::Conflict("Share names must be unique".into()));
            }
            validate_relative_config_path(&share.path)?;
            if let Some(username) = share.username.as_deref() {
                if username.trim().is_empty() || username.len() > 128 {
                    return Err(AppError::BadRequest(
                        "WebDAV username must contain 1-128 characters".into(),
                    ));
                }
            }
            if share
                .password_hash
                .as_deref()
                .is_some_and(|hash| PasswordHash::new(hash).is_err())
            {
                return Err(AppError::BadRequest(
                    "WebDAV password hash is invalid".into(),
                ));
            }
            if share.webdav_enabled && (share.username.is_none() || share.password_hash.is_none()) {
                return Err(AppError::BadRequest(
                    "Enabled WebDAV shares require a username and password".into(),
                ));
            }
        }

        let mut lock_ids = HashSet::new();
        for lock in &self.folder_locks {
            validate_storage_reference(&lock.storage_id, &storage_ids)?;
            if lock.id.is_empty()
                || lock.id.len() > 128
                || !lock
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                || !lock_ids.insert(lock.id.as_str())
            {
                return Err(AppError::Conflict("Folder lock IDs must be unique".into()));
            }
            if lock.path.trim_matches('/').is_empty() {
                return Err(AppError::BadRequest(
                    "Folder lock path cannot be the storage root".into(),
                ));
            }
            validate_relative_config_path(&lock.path)?;
            if PasswordHash::new(&lock.password_hash).is_err() {
                return Err(AppError::BadRequest(
                    "Folder lock password hash is invalid".into(),
                ));
            }
        }
        for share in self.shares.iter().filter(|share| share.webdav_enabled) {
            if self.folder_locks.iter().any(|lock| {
                lock.storage_id == share.storage_id && paths_overlap(&share.path, &lock.path)
            }) {
                return Err(AppError::Conflict(
                    "WebDAV 挂载不能与网页文件夹锁的父目录、当前目录或子目录重叠".into(),
                ));
            }
        }
        Ok(())
    }
}

fn validate_storage_instance(storage: &StorageInstanceConfig) -> AppResult<()> {
    validate_storage_id(&storage.id)?;
    let name = storage.name.trim();
    if name.is_empty()
        || name.chars().count() > 64
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(AppError::BadRequest(
            "存储实例名称必须包含 1 到 64 个字符且不能包含路径分隔符".into(),
        ));
    }
    validate_storage_backend(&storage.backend)
}

fn validate_storage_id(storage_id: &str) -> AppResult<()> {
    if storage_id.is_empty()
        || storage_id.len() > 64
        || !storage_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(AppError::BadRequest("存储实例 ID 格式无效".into()));
    }
    Ok(())
}

fn validate_storage_reference(storage_id: &str, known_ids: &HashSet<&str>) -> AppResult<()> {
    validate_storage_id(storage_id)?;
    if !known_ids.contains(storage_id) {
        return Err(AppError::BadRequest("配置引用了不存在的存储实例".into()));
    }
    Ok(())
}

pub fn validate_login_security_settings(
    admin_failures: u32,
    web_failures: u32,
    admin_block_seconds: u64,
    web_block_seconds: u64,
    retention_days: u32,
    max_entries: usize,
) -> AppResult<()> {
    if !(3..=10).contains(&admin_failures) {
        return Err(AppError::BadRequest(
            "管理员登录错误次数必须在 3 到 10 之间".into(),
        ));
    }
    if !(3..=20).contains(&web_failures) {
        return Err(AppError::BadRequest(
            "首页登录错误次数必须在 3 到 20 之间".into(),
        ));
    }
    if !(5 * 60..=24 * 60 * 60).contains(&admin_block_seconds)
        || !(5 * 60..=24 * 60 * 60).contains(&web_block_seconds)
    {
        return Err(AppError::BadRequest(
            "登录封禁时间必须在 5 分钟到 24 小时之间".into(),
        ));
    }
    if ![1, 3, 5, 7, 15, 30].contains(&retention_days) {
        return Err(AppError::BadRequest(
            "日志保存天数只能是 1、3、5、7、15 或 30 天".into(),
        ));
    }
    if !(500..=20_000).contains(&max_entries) {
        return Err(AppError::BadRequest(
            "日志最大条目数必须在 500 到 20000 之间".into(),
        ));
    }
    Ok(())
}

pub fn validate_transfer_rate(bytes_per_second: u64, label: &str) -> AppResult<()> {
    if bytes_per_second != 0
        && !(MIN_TRANSFER_RATE_BYTES..=HARD_MAX_TRANSFER_RATE_BYTES).contains(&bytes_per_second)
    {
        return Err(AppError::BadRequest(
            format!("{label}限速必须为 0，或在 64 KiB/s 到 1 GiB/s 之间").into(),
        ));
    }
    Ok(())
}

pub fn validate_transfer_limits(
    max_upload_bytes: u64,
    max_upload_batch_bytes: u64,
    max_upload_batch_entries: usize,
    max_archive_bytes: u64,
    max_archive_entries: usize,
) -> AppResult<()> {
    if !(MIN_TRANSFER_BYTES..=HARD_MAX_UPLOAD_BYTES).contains(&max_upload_bytes) {
        return Err(AppError::BadRequest(
            "单文件上传上限超出配置格式允许的范围".into(),
        ));
    }
    if !(MIN_TRANSFER_BYTES..=HARD_MAX_UPLOAD_BATCH_BYTES).contains(&max_upload_batch_bytes)
        || max_upload_batch_bytes < max_upload_bytes
    {
        return Err(AppError::BadRequest(
            "单次批量上传总量必须不小于单文件上限".into(),
        ));
    }
    if !(1..=HARD_MAX_UPLOAD_BATCH_ENTRIES).contains(&max_upload_batch_entries) {
        return Err(AppError::BadRequest(
            "单次批量上传条目数超出配置格式允许的范围".into(),
        ));
    }
    if !(MIN_TRANSFER_BYTES..=HARD_MAX_ARCHIVE_BYTES).contains(&max_archive_bytes) {
        return Err(AppError::BadRequest(
            "打包源文件总大小上限超出配置格式允许的范围".into(),
        ));
    }
    if !(1..=HARD_MAX_ARCHIVE_ENTRIES).contains(&max_archive_entries) {
        return Err(AppError::BadRequest(
            "打包条目数量上限超出配置格式允许的范围".into(),
        ));
    }
    Ok(())
}

pub fn validate_storage_backend(backend: &StorageBackendConfig) -> AppResult<()> {
    if let Some(limit) = backend.capacity_limit_bytes() {
        if !(MIN_STORAGE_CAPACITY_BYTES..=HARD_MAX_STORAGE_CAPACITY_BYTES).contains(&limit) {
            return Err(AppError::BadRequest(
                "存储容量上限必须在 1 MiB 到 4 PiB 之间，留空表示不设置逻辑上限".into(),
            ));
        }
    }
    let StorageBackendConfig::S3(settings) = backend else {
        return Ok(());
    };

    let endpoint = settings.endpoint.trim();
    if endpoint != settings.endpoint || endpoint.is_empty() || endpoint.len() > 2_048 {
        return Err(AppError::BadRequest(
            "S3 Endpoint 必须是长度不超过 2048 字符的完整地址".into(),
        ));
    }
    let uri: axum::http::Uri = endpoint
        .parse()
        .map_err(|_| AppError::BadRequest("S3 Endpoint 地址无效".into()))?;
    let scheme = uri
        .scheme_str()
        .ok_or_else(|| AppError::BadRequest("S3 Endpoint 必须包含 http:// 或 https://".into()))?;
    if !matches!(scheme, "http" | "https")
        || uri.authority().is_none()
        || !matches!(uri.path(), "" | "/")
        || uri.query().is_some()
        || uri
            .authority()
            .is_some_and(|authority| authority.as_str().contains('@'))
    {
        return Err(AppError::BadRequest(
            "S3 Endpoint 只能是无凭据、无路径和无查询参数的 HTTP(S) 地址".into(),
        ));
    }

    let host = uri
        .authority()
        .expect("authority checked above")
        .host()
        .to_ascii_lowercase();
    let uses_https_default_port = uri
        .authority()
        .and_then(axum::http::uri::Authority::port_u16)
        .is_none_or(|port| port == 443);
    match settings.provider {
        S3Provider::AlibabaOss => {
            let public = format!("oss-{}.aliyuncs.com", settings.region);
            let internal = format!("oss-{}-internal.aliyuncs.com", settings.region);
            if scheme != "https"
                || !uses_https_default_port
                || !matches!(host.as_str(), value if value == public || value == internal)
                || settings.addressing_style != S3AddressingStyle::VirtualHosted
            {
                return Err(AppError::BadRequest(
                    "阿里云 OSS 必须使用与 Region 对应的 HTTPS 官方 Endpoint 和虚拟主机寻址".into(),
                ));
            }
        }
        S3Provider::TencentCos => {
            let expected = format!("cos.{}.myqcloud.com", settings.region);
            if scheme != "https"
                || !uses_https_default_port
                || host != expected
                || settings.addressing_style != S3AddressingStyle::VirtualHosted
            {
                return Err(AppError::BadRequest(
                    "腾讯云 COS 必须使用与 Region 对应的 HTTPS 官方 Endpoint 和虚拟主机寻址".into(),
                ));
            }
        }
        S3Provider::Minio | S3Provider::S3Compatible => {}
    }

    validate_s3_bucket(&settings.bucket)?;
    if settings.region.is_empty()
        || settings.region.len() > 64
        || !settings
            .region
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(AppError::BadRequest("S3 Region 格式无效".into()));
    }
    validate_s3_prefix(&settings.prefix)?;
    if settings.access_key_id.is_empty()
        || settings.access_key_id.len() > 256
        || settings.access_key_id.chars().any(char::is_control)
    {
        return Err(AppError::BadRequest("S3 Access Key ID 格式无效".into()));
    }
    if settings.secret_access_key.is_empty()
        || settings.secret_access_key.len() > 4_096
        || settings.secret_access_key.chars().any(char::is_control)
    {
        return Err(AppError::BadRequest("S3 Secret Access Key 格式无效".into()));
    }
    Ok(())
}

fn validate_s3_bucket(bucket: &str) -> AppResult<()> {
    let bytes = bucket.as_bytes();
    let valid = (3..=63).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(*byte, b'.' | b'-')
        })
        && !bucket.contains("..")
        && !bucket.contains(".-")
        && !bucket.contains("-.")
        && bucket.parse::<IpAddr>().is_err();
    if !valid {
        return Err(AppError::BadRequest(
            "S3 Bucket 必须符合 3-63 位 DNS 兼容命名规则".into(),
        ));
    }
    Ok(())
}

fn validate_s3_prefix(prefix: &str) -> AppResult<()> {
    if prefix.len() > 1_024
        || prefix.starts_with('/')
        || prefix.contains("//")
        || prefix.contains('\\')
        || prefix.contains('\0')
        || prefix.chars().any(char::is_control)
        || (!prefix.is_empty() && !prefix.ends_with('/'))
        || prefix
            .trim_end_matches('/')
            .split('/')
            .any(|component| matches!(component, "." | ".."))
    {
        return Err(AppError::BadRequest(
            "S3 Prefix 必须为空或使用以 / 结尾的安全相对路径".into(),
        ));
    }
    Ok(())
}

fn validate_relative_config_path(path: &str) -> AppResult<()> {
    if path.contains('\\')
        || path.contains('\0')
        || path.trim_matches('/').split('/').any(|component| {
            component == ".."
                || component.contains(':')
                || component.eq_ignore_ascii_case(crate::storage_transaction::SYSTEM_DIR)
        })
    {
        return Err(AppError::BadRequest(
            "Configured storage path is invalid".into(),
        ));
    }
    Ok(())
}

// ── Config ────────────────────────────────────────────────────────
