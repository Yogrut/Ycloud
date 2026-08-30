use serde_json::Value;
use uuid::Uuid;

use super::{
    CONFIG_SCHEMA_VERSION, DEFAULT_MAX_UPLOAD_BATCH_BYTES, DEFAULT_MAX_UPLOAD_BATCH_ENTRIES,
    DEFAULT_STORAGE_ID,
};

pub(super) fn migrate_config(raw: &mut Value) -> anyhow::Result<bool> {
    let mut migrated = false;
    let schema_version = raw
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if schema_version > CONFIG_SCHEMA_VERSION as u64 {
        anyhow::bail!(
            "Configuration schema version {schema_version} is newer than this Ycloud build"
        );
    }
    if schema_version < 5 {
        if raw.get("storage_backend").is_none() {
            raw["storage_backend"] = serde_json::json!({
                "type": "local",
                "settings": {}
            });
        }
        migrated = true;
    }
    if schema_version < 6 {
        migrate_storage_instances(raw);
        migrated = true;
    }
    if schema_version < 7 {
        raw["user_accounts"] = serde_json::json!([]);
        migrated = true;
    }
    if schema_version < 9 {
        migrate_storage_visibility(raw);
        migrated = true;
    }
    if schema_version < 10 {
        raw["max_upload_batch_bytes"] = serde_json::json!(DEFAULT_MAX_UPLOAD_BATCH_BYTES);
        raw["max_upload_batch_entries"] = serde_json::json!(DEFAULT_MAX_UPLOAD_BATCH_ENTRIES);
        migrated = true;
    }
    if schema_version < 11 {
        raw["admin_totp_secret"] = serde_json::Value::Null;
        raw["admin_recovery_code_hashes"] = serde_json::json!([]);
        migrated = true;
    }
    if schema_version < CONFIG_SCHEMA_VERSION as u64 {
        raw["schema_version"] = serde_json::json!(CONFIG_SCHEMA_VERSION);
        migrated = true;
    }
    Ok(migrated)
}

fn migrate_storage_instances(raw: &mut Value) {
    let pending = raw
        .get("pending_storage_backend")
        .cloned()
        .filter(|value| !value.is_null());
    let active = raw
        .get("storage_backend")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({ "type": "local", "settings": {} }));
    let active_is_local = active
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "local");
    let pending_is_local = pending.as_ref().is_some_and(|backend| {
        backend
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "local")
    });
    let mut instances = vec![serde_json::json!({
        "id": DEFAULT_STORAGE_ID,
        "name": if active_is_local { "Local storage" } else { "S3 storage" },
        "backend": active
    })];
    if !active_is_local {
        let local_backend = if pending_is_local {
            pending.clone().expect("pending local checked above")
        } else {
            serde_json::json!({ "type": "local", "settings": {} })
        };
        instances.push(serde_json::json!({
            "id": "local",
            "name": "Local storage",
            "backend": local_backend
        }));
    }
    raw["storage_instances"] = Value::Array(instances);
    raw["default_storage_id"] = serde_json::json!(DEFAULT_STORAGE_ID);
    if let Some(pending) = pending.filter(|_| !pending_is_local) {
        raw["pending_storage_instance"] = serde_json::json!({
            "id": format!("pending-{}", Uuid::new_v4().simple()),
            "name": "Pending storage",
            "backend": pending
        });
    }
    if let Some(object) = raw.as_object_mut() {
        object.remove("storage_backend");
        object.remove("pending_storage_backend");
    }
}

fn migrate_storage_visibility(raw: &mut Value) {
    let former_default = raw
        .get("default_storage_id")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_STORAGE_ID)
        .to_string();
    if let Some(instances) = raw
        .get_mut("storage_instances")
        .and_then(Value::as_array_mut)
    {
        for instance in instances {
            let is_former_default = instance
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id == former_default);
            instance["enabled"] = serde_json::json!(true);
            instance["allow_guest_access"] = serde_json::json!(is_former_default);
        }
    }
}
