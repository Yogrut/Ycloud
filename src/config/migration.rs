use serde_json::Value;
use uuid::Uuid;

use super::{
    CONFIG_SCHEMA_VERSION, DEFAULT_MAX_UPLOAD_BATCH_BYTES, DEFAULT_MAX_UPLOAD_BATCH_ENTRIES,
    DEFAULT_STORAGE_ID,
};

pub(super) fn migrate_config(raw: &mut Value) -> anyhow::Result<bool> {
    let schema_version = schema_version(raw)?;
    if schema_version > CONFIG_SCHEMA_VERSION as u64 {
        anyhow::bail!(
            "Configuration schema version {schema_version} is newer than this Ycloud build"
        );
    }
    // Work on a candidate: even a failed intermediate step leaves the input
    // untouched. Each migration describes the version it produces.
    let mut candidate = raw.clone();
    let removed_proxy_policy = candidate
        .get_mut("domain_binding")
        .and_then(Value::as_object_mut)
        .is_some_and(|binding| binding.remove("trusted_proxy_ips").is_some());
    for target in (schema_version + 1)..=u64::from(CONFIG_SCHEMA_VERSION) {
        migrate_step(&mut candidate, target)?;
        candidate["schema_version"] = Value::from(target);
    }
    *raw = candidate;
    Ok(removed_proxy_policy || schema_version < u64::from(CONFIG_SCHEMA_VERSION))
}

fn schema_version(raw: &Value) -> anyhow::Result<u64> {
    let object = raw
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Configuration must be an object"))?;
    if let Some(version) = object.get("schema_version") {
        return version.as_u64().ok_or_else(|| {
            anyhow::anyhow!(
                "Configuration schema_version must be an unsigned integer; refusing migration"
            )
        });
    }
    let modern_fields = [
        "storage_instances",
        "default_storage_id",
        "pending_storage_instance",
        "user_accounts",
        "admin_totp_secret",
        "admin_recovery_code_hashes",
        "domain_binding",
    ];
    if modern_fields.iter().any(|key| object.contains_key(*key))
        || !object.get("admin_username").is_some_and(Value::is_string)
        || !object
            .get("admin_password_hash")
            .is_some_and(Value::is_string)
        || !object.get("shares").is_some_and(Value::is_array)
    {
        anyhow::bail!(
            "Unversioned configuration does not match the legacy format; refusing migration"
        );
    }
    Ok(0)
}

fn migrate_step(raw: &mut Value, target: u64) -> anyhow::Result<()> {
    match target {
        15 => {
            // Keep existing private-network installations working; new S3 settings default to direct.
            if let Some(instances) = raw
                .get_mut("storage_instances")
                .and_then(Value::as_array_mut)
            {
                for instance in instances {
                    preserve_existing_relay(instance);
                }
            }
            if let Some(instance) = raw.get_mut("pending_storage_instance") {
                preserve_existing_relay(instance);
            }
        }
        5 if raw.get("storage_backend").is_none() => {
            raw["storage_backend"] = serde_json::json!({ "type": "local", "settings": {} });
        }
        6 => {
            if raw.get("storage_instances").is_some() {
                anyhow::bail!("Legacy configuration already contains storage_instances; refusing to replace them");
            }
            migrate_storage_instances(raw);
        }
        7 => set_default(raw, "user_accounts", serde_json::json!([])),
        9 => migrate_storage_visibility(raw)?,
        10 => {
            set_default(
                raw,
                "max_upload_batch_bytes",
                Value::from(DEFAULT_MAX_UPLOAD_BATCH_BYTES),
            );
            set_default(
                raw,
                "max_upload_batch_entries",
                Value::from(DEFAULT_MAX_UPLOAD_BATCH_ENTRIES),
            );
        }
        14 => {
            raw.as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("Configuration must be an object"))?
                .remove("default_storage_id");
        }
        11 => {
            set_default(raw, "admin_totp_secret", Value::Null);
            set_default(raw, "admin_recovery_code_hashes", serde_json::json!([]));
        }
        _ => {}
    }
    Ok(())
}

fn preserve_existing_relay(instance: &mut Value) {
    if instance["backend"]["type"] == "s3" {
        if let Some(settings) = instance["backend"]["settings"].as_object_mut() {
            settings.entry("relay_upload").or_insert(Value::Bool(true));
        }
    }
}

fn set_default(raw: &mut Value, key: &str, value: Value) {
    if raw.get(key).is_none() {
        raw[key] = value;
    }
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

fn migrate_storage_visibility(raw: &mut Value) -> anyhow::Result<()> {
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
            if !instance.is_object() {
                anyhow::bail!("Storage instance must be an object before migration");
            }
            let is_former_default = instance
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id == former_default);
            set_default(instance, "enabled", Value::Bool(true));
            set_default(
                instance,
                "allow_guest_access",
                Value::Bool(is_former_default),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_schema_is_an_identity_transformation() {
        let mut raw = serde_json::to_value(super::super::ConfigFile::with_test_storage()).unwrap();
        let before = raw.clone();
        assert!(!migrate_config(&mut raw).unwrap());
        assert_eq!(raw, before);
    }

    #[test]
    fn obsolete_proxy_policy_is_removed_before_full_config_validation() {
        let mut raw = serde_json::to_value(super::super::ConfigFile::with_test_storage()).unwrap();
        raw["domain_binding"] = serde_json::json!({
            "public_url": "https://files.example.com",
            "trusted_proxy_ips": ["127.0.0.1"]
        });
        assert!(migrate_config(&mut raw).unwrap());
        let candidate: super::super::ConfigFile = serde_json::from_value(raw.clone()).unwrap();
        candidate.validate().unwrap();
        assert!(!migrate_config(&mut raw).unwrap());
    }

    #[test]
    fn direct_upload_migration_preserves_existing_relay_and_explicit_choices() {
        let mut raw = serde_json::json!({
            "schema_version": 14,
            "storage_instances": [
                { "backend": { "type": "s3", "settings": {} } },
                { "backend": { "type": "s3", "settings": { "relay_upload": false } } }
            ],
            "pending_storage_instance": { "backend": { "type": "s3", "settings": {} } }
        });
        migrate_config(&mut raw).unwrap();
        assert_eq!(
            raw["storage_instances"][0]["backend"]["settings"]["relay_upload"],
            true
        );
        assert_eq!(
            raw["storage_instances"][1]["backend"]["settings"]["relay_upload"],
            false
        );
        assert_eq!(
            raw["pending_storage_instance"]["backend"]["settings"]["relay_upload"],
            true
        );
        assert!(!migrate_config(&mut raw).unwrap());
    }

    #[test]
    fn last_schema_step_preserves_existing_configuration_and_is_idempotent() {
        let mut raw = serde_json::to_value(super::super::ConfigFile::with_test_storage()).unwrap();
        raw["schema_version"] = Value::from(11);
        let mut expected = raw.clone();
        expected["schema_version"] = Value::from(CONFIG_SCHEMA_VERSION);
        assert!(migrate_config(&mut raw).unwrap());
        assert_eq!(raw, expected);
        assert!(!migrate_config(&mut raw).unwrap());
        assert_eq!(raw, expected);
    }

    #[test]
    fn removing_global_default_preserves_existing_storages() {
        let mut raw = serde_json::to_value(super::super::ConfigFile::with_test_storage()).unwrap();
        raw["schema_version"] = Value::from(13);
        raw["default_storage_id"] = Value::from("primary");
        let before = raw["storage_instances"].clone();
        migrate_config(&mut raw).unwrap();
        assert!(raw.get("default_storage_id").is_none());
        assert_eq!(raw["storage_instances"], before);
    }

    #[test]
    fn visibility_migration_retains_explicit_choices() {
        let mut raw = serde_json::to_value(super::super::ConfigFile::with_test_storage()).unwrap();
        raw["schema_version"] = Value::from(8);
        raw["storage_instances"][0]["enabled"] = Value::Bool(false);
        raw["storage_instances"][0]["allow_guest_access"] = Value::Bool(false);
        migrate_config(&mut raw).unwrap();
        assert_eq!(raw["storage_instances"][0]["enabled"], false);
        assert_eq!(raw["storage_instances"][0]["allow_guest_access"], false);
    }
}
