use std::path::{Path, PathBuf};

use anyhow::Context;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand_core::{OsRng, RngCore};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

const KEY_BYTES: usize = 32;
const MANAGED_KEY_FILE: &str = "master.key";
const MANAGED_KEY_DIRECTORY: &str = ".ycloud-system/secrets";
const SECRET_PREFIX: &str = "enc:v2:";

pub(super) struct SecretStore {
    key: [u8; KEY_BYTES],
}

impl SecretStore {
    pub(super) async fn for_read(config_path: &Path) -> anyhow::Result<Self> {
        if let Some(key) = external_key().await? {
            return Ok(Self { key });
        }
        let path = managed_key_path(config_path);
        let key = read_managed_key(&path).await.with_context(|| {
            format!(
                "Ycloud 主密钥缺失或不可用；已有加密配置需要原主密钥：{}",
                path.display()
            )
        })?;
        Ok(Self { key })
    }

    pub(super) async fn for_write(config_path: &Path) -> anyhow::Result<Self> {
        if let Some(key) = external_key().await? {
            return Ok(Self { key });
        }
        let path = managed_key_path(config_path);
        match read_managed_key(&path).await {
            Ok(key) => Ok(Self { key }),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                let key = create_managed_key(&path).await?;
                tracing::info!(
                    path = %path.display(),
                    "Ycloud created a protected local configuration master key"
                );
                Ok(Self { key })
            }
            Err(error) => Err(error).with_context(|| {
                format!(
                    "Failed to load managed Ycloud master key {}",
                    path.display()
                )
            }),
        }
    }

    pub(super) fn seal(&self, context: &str, plaintext: &str) -> anyhow::Result<String> {
        let key = self.aead_key()?;
        let mut nonce_bytes = [0_u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let mut sealed = plaintext.as_bytes().to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::from(context.as_bytes()),
            &mut sealed,
        )
        .map_err(|_| anyhow::anyhow!("Failed to encrypt sensitive configuration"))?;
        Ok(format!(
            "{SECRET_PREFIX}{}:{}",
            URL_SAFE_NO_PAD.encode(nonce_bytes),
            URL_SAFE_NO_PAD.encode(sealed)
        ))
    }

    pub(super) fn open(&self, context: &str, encoded: &str) -> anyhow::Result<String> {
        self.open_with_prefix(SECRET_PREFIX, context, encoded)
    }

    pub(super) fn open_legacy(&self, field: &str, encoded: &str) -> anyhow::Result<String> {
        self.open_with_prefix("enc:v1:", field, encoded)
    }

    fn open_with_prefix(
        &self,
        prefix: &str,
        context: &str,
        encoded: &str,
    ) -> anyhow::Result<String> {
        let payload = encoded
            .strip_prefix(prefix)
            .ok_or_else(|| anyhow::anyhow!("Unsupported sensitive configuration encoding"))?;
        let (nonce, ciphertext) = payload
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("Invalid encrypted configuration"))?;
        let nonce: [u8; 12] = URL_SAFE_NO_PAD
            .decode(nonce)
            .context("Invalid encrypted configuration nonce")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid encrypted configuration nonce length"))?;
        let mut ciphertext = URL_SAFE_NO_PAD
            .decode(ciphertext)
            .context("Invalid encrypted configuration payload")?;
        let plaintext = self
            .aead_key()?
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(context.as_bytes()),
                &mut ciphertext,
            )
            .map_err(|_| anyhow::anyhow!("Ycloud 主密钥不匹配，或加密配置已被修改"))?;
        String::from_utf8(plaintext.to_vec())
            .context("Decrypted sensitive configuration is not valid UTF-8")
    }

    pub(super) fn is_current_encoding(value: &str) -> bool {
        value.starts_with(SECRET_PREFIX)
    }

    fn aead_key(&self) -> anyhow::Result<LessSafeKey> {
        let unbound = UnboundKey::new(&AES_256_GCM, &self.key)
            .map_err(|_| anyhow::anyhow!("Failed to initialize configuration encryption"))?;
        Ok(LessSafeKey::new(unbound))
    }

    #[cfg(test)]
    pub(super) fn for_test() -> Self {
        Self {
            key: [0x59; KEY_BYTES],
        }
    }
}

pub(super) fn managed_key_path(config_path: &Path) -> PathBuf {
    let parent = config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    parent.join(MANAGED_KEY_DIRECTORY).join(MANAGED_KEY_FILE)
}

async fn external_key() -> anyhow::Result<Option<[u8; KEY_BYTES]>> {
    if let Some(path) = std::env::var_os("YCLOUD_CONFIG_KEY_FILE") {
        let path = PathBuf::from(path);
        let metadata = tokio::fs::metadata(&path)
            .await
            .with_context(|| format!("Failed to read YCLOUD_CONFIG_KEY_FILE {}", path.display()))?;
        if !metadata.is_file() || metadata.len() > 4_096 {
            anyhow::bail!(
                "YCLOUD_CONFIG_KEY_FILE must be a regular file no larger than 4096 bytes"
            );
        }
        let value = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("Failed to read YCLOUD_CONFIG_KEY_FILE {}", path.display()))?;
        return decode_key(value.trim(), "YCLOUD_CONFIG_KEY_FILE").map(Some);
    }
    match std::env::var("YCLOUD_CONFIG_KEY") {
        Ok(value) => decode_key(value.trim(), "YCLOUD_CONFIG_KEY").map(Some),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(error).context("Failed to read YCLOUD_CONFIG_KEY"),
    }
}

fn decode_key(value: &str, source: &str) -> anyhow::Result<[u8; KEY_BYTES]> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .with_context(|| format!("{source} must be URL-safe base64 without padding"))?;
    decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("{source} must decode to exactly 32 bytes"))
}

async fn create_managed_key(path: &Path) -> anyhow::Result<[u8; KEY_BYTES]> {
    let parent = path.parent().expect("managed key always has a parent");
    tokio::fs::create_dir_all(parent)
        .await
        .context("Failed to create Ycloud secret directory")?;
    secure_directory_permissions(parent).await?;

    let mut key = [0_u8; KEY_BYTES];
    OsRng.fill_bytes(&mut key);
    let encoded = encode_managed_key(&key)?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = match options.open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return read_managed_key(path).await;
        }
        Err(error) => return Err(error).context("Failed to create managed Ycloud master key"),
    };
    let write_result = async {
        file.write_all(encoded.as_bytes()).await?;
        file.sync_all().await
    }
    .await;
    drop(file);
    if let Err(error) = write_result {
        let _ = tokio::fs::remove_file(path).await;
        return Err(error).context("Failed to persist managed Ycloud master key");
    }
    secure_file_permissions(path).await?;
    Ok(key)
}

async fn read_managed_key(path: &Path) -> anyhow::Result<[u8; KEY_BYTES]> {
    let metadata = tokio::fs::metadata(path).await?;
    if !metadata.is_file() || metadata.len() > 4_096 {
        anyhow::bail!("Managed Ycloud master key must be a regular file no larger than 4096 bytes");
    }
    let encoded = tokio::fs::read_to_string(path).await?;
    secure_file_permissions(path).await?;
    decode_managed_key(encoded.trim())
}

#[cfg(windows)]
fn encode_managed_key(key: &[u8; KEY_BYTES]) -> anyhow::Result<String> {
    Ok(format!(
        "dpapi:v1:{}",
        URL_SAFE_NO_PAD.encode(dpapi_protect(key)?)
    ))
}

#[cfg(not(windows))]
fn encode_managed_key(key: &[u8; KEY_BYTES]) -> anyhow::Result<String> {
    Ok(format!("raw:v1:{}", URL_SAFE_NO_PAD.encode(key)))
}

#[cfg(windows)]
fn decode_managed_key(value: &str) -> anyhow::Result<[u8; KEY_BYTES]> {
    let payload = value
        .strip_prefix("dpapi:v1:")
        .ok_or_else(|| anyhow::anyhow!("Unsupported managed Ycloud master key format"))?;
    let protected = URL_SAFE_NO_PAD
        .decode(payload)
        .context("Invalid managed Ycloud master key")?;
    dpapi_unprotect(&protected)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Managed Ycloud master key has invalid length"))
}

#[cfg(not(windows))]
fn decode_managed_key(value: &str) -> anyhow::Result<[u8; KEY_BYTES]> {
    let payload = value
        .strip_prefix("raw:v1:")
        .ok_or_else(|| anyhow::anyhow!("Unsupported managed Ycloud master key format"))?;
    decode_key(payload, "managed Ycloud master key")
}

#[cfg(windows)]
fn dpapi_protect(value: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB},
    };

    let input_len = u32::try_from(value.len()).context("DPAPI input is too large")?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_len,
        pbData: value.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let success = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error()).context("Windows DPAPI protection failed");
    }
    let protected =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(protected)
}

#[cfg(windows)]
fn dpapi_unprotect(value: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };

    let input_len = u32::try_from(value.len()).context("DPAPI input is too large")?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_len,
        pbData: value.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let success = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error()).context("Windows DPAPI decryption failed");
    }
    let plaintext =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(plaintext)
}

#[cfg(unix)]
async fn secure_directory_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .context("Failed to restrict Ycloud secret directory permissions")
}

#[cfg(not(unix))]
async fn secure_directory_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
async fn secure_file_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .context("Failed to restrict Ycloud master key permissions")
}

#[cfg(not(unix))]
async fn secure_file_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_values_are_bound_to_their_context() {
        let store = SecretStore::for_test();
        let sealed = store.seal("storage-a:secret", "credential").unwrap();
        assert_eq!(
            store.open("storage-a:secret", &sealed).unwrap(),
            "credential"
        );
        assert!(store.open("storage-b:secret", &sealed).is_err());
    }

    #[test]
    fn external_key_format_is_strict() {
        let encoded = URL_SAFE_NO_PAD.encode([7_u8; KEY_BYTES]);
        assert_eq!(decode_key(&encoded, "test").unwrap(), [7_u8; KEY_BYTES]);
        assert!(decode_key("too-short", "test").is_err());
    }
}
