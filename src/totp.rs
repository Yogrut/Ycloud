use qrcode::{render::svg, QrCode};
use rand_core::{OsRng, RngCore};
use ring::hmac;
use std::{collections::HashSet, sync::Arc};
use tokio::sync::Mutex;

use crate::error::{AppError, AppResult};

const STEP_SECONDS: i64 = 30;
const DIGITS: u32 = 6;
const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const RECOVERY_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Clone, Default)]
pub struct TotpReplayStore {
    consumed: Arc<Mutex<HashSet<u64>>>,
}

impl TotpReplayStore {
    /// Atomically marks a valid TOTP counter as consumed for this process.
    pub async fn consume(&self, counter: u64) -> bool {
        let current = current_counter();
        let mut consumed = self.consumed.lock().await;
        consumed.retain(|value| value.saturating_add(2) >= current);
        consumed.insert(counter)
    }

    pub async fn clear(&self) {
        self.consumed.lock().await.clear();
    }
}

pub fn generate_secret() -> String {
    let mut bytes = [0_u8; 20];
    OsRng.fill_bytes(&mut bytes);
    encode_base32(&bytes)
}

pub fn generate_recovery_codes(count: usize) -> Vec<String> {
    (0..count)
        .map(|_| {
            let mut random = [0_u8; 10];
            OsRng.fill_bytes(&mut random);
            let value = random
                .iter()
                .map(|byte| RECOVERY_ALPHABET[*byte as usize % RECOVERY_ALPHABET.len()] as char)
                .collect::<String>();
            format!("{}-{}", &value[..5], &value[5..])
        })
        .collect()
}

pub fn normalize_recovery_code(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect()
}

pub fn validate_secret(secret: &str) -> AppResult<()> {
    let decoded = decode_base32(secret)?;
    if !(20..=64).contains(&decoded.len()) {
        return Err(AppError::BadRequest("TOTP 密钥长度无效".into()));
    }
    Ok(())
}

pub fn verify_now(secret: &str, code: &str) -> bool {
    verify_now_counter(secret, code).is_some()
}

pub fn verify_now_counter(secret: &str, code: &str) -> Option<u64> {
    verify_at_counter(secret, code.trim(), chrono::Utc::now().timestamp())
}

pub fn provisioning_uri(secret: &str, username: &str) -> String {
    let label = percent_encode(&format!("Ycloud:{username}"));
    format!(
        "otpauth://totp/{label}?secret={secret}&issuer=Ycloud&algorithm=SHA1&digits=6&period=30"
    )
}

pub fn provisioning_qr_svg(uri: &str) -> AppResult<String> {
    let code =
        QrCode::new(uri.as_bytes()).map_err(|_| AppError::internal("无法生成两步验证二维码"))?;
    Ok(code
        .render::<svg::Color>()
        .min_dimensions(220, 220)
        .dark_color(svg::Color("#172033"))
        .light_color(svg::Color("#ffffff"))
        .build())
}

#[cfg(test)]
fn verify_at(secret: &str, code: &str, unix_seconds: i64) -> bool {
    verify_at_counter(secret, code, unix_seconds).is_some()
}

fn verify_at_counter(secret: &str, code: &str, unix_seconds: i64) -> Option<u64> {
    if code.len() != DIGITS as usize || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let Ok(secret) = decode_base32(secret) else {
        return None;
    };
    let counter = unix_seconds.div_euclid(STEP_SECONDS);
    (-1_i64..=1).find_map(|offset| {
        let counter = counter
            .checked_add(offset)
            .and_then(|value| u64::try_from(value).ok())?;
        let expected = totp_code(&secret, counter);
        (expected == code).then_some(counter)
    })
}

fn current_counter() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp().div_euclid(STEP_SECONDS)).unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn current_code_for_test(secret: &str) -> String {
    let secret = decode_base32(secret).expect("test secret must be valid base32");
    totp_code(&secret, current_counter())
}

fn totp_code(secret: &[u8], counter: u64) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let digest = hmac::sign(&key, &counter.to_be_bytes());
    let bytes = digest.as_ref();
    let offset = (bytes[bytes.len() - 1] & 0x0f) as usize;
    let binary = (u32::from(bytes[offset] & 0x7f) << 24)
        | (u32::from(bytes[offset + 1]) << 16)
        | (u32::from(bytes[offset + 2]) << 8)
        | u32::from(bytes[offset + 3]);
    format!("{:06}", binary % 10_u32.pow(DIGITS))
}

fn encode_base32(bytes: &[u8]) -> String {
    let mut output = String::new();
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            output.push(BASE32[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        output.push(BASE32[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    output
}

fn decode_base32(value: &str) -> AppResult<Vec<u8>> {
    if value.is_empty() || value.contains('=') {
        return Err(AppError::BadRequest("TOTP 密钥格式无效".into()));
    }
    let mut output = Vec::with_capacity(value.len() * 5 / 8);
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for byte in value.bytes() {
        let upper = byte.to_ascii_uppercase();
        let index = BASE32
            .iter()
            .position(|candidate| *candidate == upper)
            .ok_or_else(|| AppError::BadRequest("TOTP 密钥格式无效".into()))?;
        buffer = (buffer << 5) | index as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
        }
    }
    Ok(output)
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_rfc_6238_sha1_vector() {
        let secret = encode_base32(b"12345678901234567890");
        assert_eq!(totp_code(&decode_base32(&secret).unwrap(), 1), "287082");
        assert!(verify_at(&secret, "287082", 59));
    }

    #[test]
    fn generated_secret_round_trips() {
        let secret = generate_secret();
        assert_eq!(decode_base32(&secret).unwrap().len(), 20);
        validate_secret(&secret).unwrap();
    }

    #[test]
    fn recovery_codes_are_normalized() {
        assert_eq!(normalize_recovery_code("abcde-23456"), "ABCDE23456");
    }

    #[test]
    fn provisioning_uri_renders_as_svg_qr_code() {
        let uri = provisioning_uri("JBSWY3DPEHPK3PXP", "admin");
        let svg = provisioning_qr_svg(&uri).unwrap();
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("<svg"));
    }

    #[tokio::test]
    async fn replay_store_consumes_each_counter_once() {
        let store = TotpReplayStore::default();
        let counter = current_counter();
        assert!(store.consume(counter).await);
        assert!(!store.consume(counter).await);
        store.clear().await;
        assert!(store.consume(counter).await);
    }
}
