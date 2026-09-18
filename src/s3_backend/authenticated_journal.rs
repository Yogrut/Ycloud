use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ring::hmac;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::error::{AppError, AppResult};

const ENVELOPE_SCHEMA_VERSION: u32 = 1;
const AUTH_DOMAIN: &[u8] = b"ycloud:s3-authenticated-journal:v1\0";
pub(super) const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
pub(super) const MAX_ENVELOPE_BYTES: usize = 96 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema_version: u32,
    purpose: String,
    payload: String,
    auth_tag: String,
}

pub(super) fn encode<T: Serialize>(
    auth_key: &[u8; 32],
    purpose: &str,
    value: &T,
) -> AppResult<Vec<u8>> {
    validate_purpose(purpose)?;
    let payload = serde_json::to_vec(value)
        .map_err(|error| AppError::with_source("failed to encode S3 journal payload", error))?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(AppError::ServiceUnavailable(
            "对象存储事务记录超过安全上限".into(),
        ));
    }
    let envelope = Envelope {
        schema_version: ENVELOPE_SCHEMA_VERSION,
        purpose: purpose.to_owned(),
        payload: URL_SAFE_NO_PAD.encode(&payload),
        auth_tag: sign_payload(auth_key, purpose, &payload),
    };
    let encoded = serde_json::to_vec(&envelope)
        .map_err(|error| AppError::with_source("failed to encode S3 journal envelope", error))?;
    if encoded.len() > MAX_ENVELOPE_BYTES {
        return Err(AppError::ServiceUnavailable(
            "对象存储事务认证信封超过安全上限".into(),
        ));
    }
    Ok(encoded)
}

pub(super) fn decode<T: DeserializeOwned>(
    auth_key: &[u8; 32],
    purpose: &str,
    data: &[u8],
) -> AppResult<T> {
    validate_purpose(purpose)?;
    if data.len() > MAX_ENVELOPE_BYTES {
        return Err(AppError::ServiceUnavailable(
            "对象存储事务认证信封超过安全上限".into(),
        ));
    }
    let envelope: Envelope = serde_json::from_slice(data).map_err(|_| {
        AppError::ServiceUnavailable(
            "对象存储恢复记录不是受支持的认证信封；已保留并拒绝自动执行".into(),
        )
    })?;
    if envelope.schema_version != ENVELOPE_SCHEMA_VERSION || envelope.purpose != purpose {
        return Err(AppError::ServiceUnavailable(
            "对象存储恢复记录认证用途或版本不匹配；已保留并拒绝自动执行".into(),
        ));
    }
    let payload = URL_SAFE_NO_PAD.decode(&envelope.payload).map_err(|_| {
        AppError::ServiceUnavailable("对象存储恢复记录认证负载无效；已保留记录".into())
    })?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(AppError::ServiceUnavailable(
            "对象存储事务记录超过安全上限".into(),
        ));
    }
    verify_payload(auth_key, purpose, &payload, &envelope.auth_tag)?;
    serde_json::from_slice(&payload)
        .map_err(|error| AppError::with_source("invalid authenticated S3 journal payload", error))
}

pub(super) fn sign_payload(auth_key: &[u8; 32], purpose: &str, payload: &[u8]) -> String {
    let authenticated = authenticated_bytes(purpose, payload);
    let key = hmac::Key::new(hmac::HMAC_SHA256, auth_key);
    URL_SAFE_NO_PAD.encode(hmac::sign(&key, &authenticated).as_ref())
}

pub(super) fn verify_payload(
    auth_key: &[u8; 32],
    purpose: &str,
    payload: &[u8],
    encoded_tag: &str,
) -> AppResult<()> {
    let tag = URL_SAFE_NO_PAD.decode(encoded_tag).map_err(|_| {
        AppError::ServiceUnavailable("对象存储恢复记录认证标签无效；已保留记录".into())
    })?;
    let authenticated = authenticated_bytes(purpose, payload);
    let key = hmac::Key::new(hmac::HMAC_SHA256, auth_key);
    hmac::verify(&key, &authenticated, &tag).map_err(|_| {
        AppError::ServiceUnavailable("对象存储恢复记录认证失败；已保留并拒绝自动执行".into())
    })
}

fn authenticated_bytes(purpose: &str, payload: &[u8]) -> Vec<u8> {
    let mut authenticated =
        Vec::with_capacity(AUTH_DOMAIN.len() + 12 + purpose.len() + payload.len());
    authenticated.extend_from_slice(AUTH_DOMAIN);
    authenticated.extend_from_slice(&ENVELOPE_SCHEMA_VERSION.to_be_bytes());
    authenticated.extend_from_slice(&(purpose.len() as u32).to_be_bytes());
    authenticated.extend_from_slice(purpose.as_bytes());
    authenticated.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    authenticated.extend_from_slice(payload);
    authenticated
}

fn validate_purpose(purpose: &str) -> AppResult<()> {
    if purpose.is_empty()
        || purpose.len() > 128
        || purpose.bytes().any(|byte| !byte.is_ascii_graphic())
    {
        return Err(AppError::internal(
            "invalid S3 journal authentication purpose",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn envelope_round_trip_rejects_tampering_wrong_key_purpose_and_legacy_json() {
        let key = [0x35; 32];
        let value = json!({"id": "record", "stage": "prepared"});
        let encoded = encode(&key, "upload-transaction:v1", &value).unwrap();
        assert_eq!(
            decode::<serde_json::Value>(&key, "upload-transaction:v1", &encoded).unwrap(),
            value
        );
        assert!(
            decode::<serde_json::Value>(&[0x36; 32], "upload-transaction:v1", &encoded).is_err()
        );
        assert!(decode::<serde_json::Value>(&key, "multipart-session:v1", &encoded).is_err());
        assert!(
            decode::<serde_json::Value>(&key, "upload-transaction:v1", br#"{"id":"legacy"}"#)
                .is_err()
        );

        let mut envelope: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        envelope["payload"] = json!(URL_SAFE_NO_PAD.encode(br#"{"id":"changed"}"#));
        let tampered = serde_json::to_vec(&envelope).unwrap();
        assert!(decode::<serde_json::Value>(&key, "upload-transaction:v1", &tampered).is_err());
    }
}
