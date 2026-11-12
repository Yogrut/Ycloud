use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime},
};

use axum::{
    body::Body,
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::TryStreamExt;
use tokio::{
    fs::{self, File},
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{watch, Notify},
};
use tokio_util::io::{ReaderStream, StreamReader};

use crate::{
    directory_listing::is_gallery_image_name,
    error::{AppError, AppResult},
    state::AppState,
    storage::FileResponseMode,
    storage_backend::StorageBackend,
};

const MAX_JOBS: usize = 512;
const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PIXELS: u64 = 36_000_000;
const MAX_OUTPUT_BYTES: u64 = 512 * 1024;
const CACHE_BYTES: u64 = 128 * 1024 * 1024;
const CACHE_FILES: usize = 4096;
const TRIM_INTERVAL: usize = 16;
const JOB_TIMEOUT: Duration = Duration::from_secs(45);
const RECORD_HEADER_BYTES: usize = 33;
const STARTING: u8 = 0;
const READY: u8 = 1;
const DISABLED: u8 = 2;

#[derive(Clone)]
pub(crate) struct Gallery {
    inner: Arc<GalleryState>,
}

struct GalleryState {
    root: PathBuf,
    availability: AtomicU8,
    notify: Notify,
    queue: Mutex<Queue>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Job {
    storage: String,
    path: String,
}

#[derive(Default)]
struct Queue {
    waiting: VecDeque<Job>,
    pending: HashSet<Job>,
    active: Option<Job>,
    changed: bool,
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum Outcome {
    Ready = 1,
    NotImage = 2,
    Unavailable = 3,
    TooLarge = 4,
    SourceUnavailable = 5,
}

impl Outcome {
    fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Ready,
            2 => Self::NotImage,
            3 => Self::Unavailable,
            4 => Self::TooLarge,
            5 => Self::SourceUnavailable,
            _ => return None,
        })
    }

    fn status(self) -> StatusCode {
        match self {
            Self::Ready => StatusCode::OK,
            Self::NotImage => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::Unavailable => StatusCode::UNPROCESSABLE_ENTITY,
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::SourceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

impl Gallery {
    pub(crate) fn new(config: &Path) -> Self {
        Self {
            inner: Arc::new(GalleryState {
                root: config
                    .with_extension("gallery-cache")
                    .join(crate::storage_transaction::SYSTEM_DIR),
                availability: AtomicU8::new(STARTING),
                notify: Notify::new(),
                queue: Mutex::new(Queue::default()),
            }),
        }
    }

    pub(crate) fn enqueue_committed(&self, storage: &str, path: &str) {
        self.enqueue(storage, path, true);
    }

    fn enqueue(&self, storage: &str, path: &str, committed: bool) -> bool {
        let availability = self.inner.availability.load(Ordering::Acquire);
        if availability == DISABLED
            || (availability == STARTING && !committed)
            || !is_gallery_image_name(path)
        {
            return false;
        }
        let Ok(path) = crate::storage::StorageService::normalize_relative(path) else {
            return false;
        };
        let job = Job {
            storage: storage.to_owned(),
            path,
        };
        let mut queue = self.inner.queue.lock().expect("gallery queue poisoned");
        if queue.pending.contains(&job) {
            if committed && queue.active.as_ref() == Some(&job) {
                queue.changed = true;
            }
            return true;
        }
        if queue.pending.len() >= MAX_JOBS {
            return false;
        }
        queue.pending.insert(job.clone());
        queue.waiting.push_back(job);
        self.inner.notify.notify_one();
        true
    }

    pub(crate) async fn response(
        &self,
        backend: &StorageBackend,
        storage: &str,
        path: &str,
        method: &Method,
    ) -> AppResult<Response> {
        if !is_gallery_image_name(path) {
            return Ok(StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response());
        }
        let path = crate::storage::StorageService::normalize_relative(path)?;
        let stamp = backend.gallery_stamp(&path).await?;
        if let Some((outcome, file)) = self.open(storage, &path, &stamp).await? {
            if !matches!(outcome, Outcome::Ready) {
                return Ok(outcome.status().into_response());
            }
            let size = file.metadata().await.map_err(cache_io)?.len() - RECORD_HEADER_BYTES as u64;
            let body = if *method == Method::HEAD {
                Body::empty()
            } else {
                Body::from_stream(ReaderStream::with_capacity(file, 32 * 1024))
            };
            return Ok((
                [
                    (header::CONTENT_TYPE, "image/jpeg".to_owned()),
                    (header::CONTENT_LENGTH, size.to_string()),
                ],
                body,
            )
                .into_response());
        }
        if !self.enqueue(storage, &path, false) {
            return Err(AppError::ServiceUnavailable(
                "Thumbnail generation is unavailable or busy".into(),
            ));
        }
        Ok((StatusCode::ACCEPTED, [(header::RETRY_AFTER, "2")]).into_response())
    }

    fn cached(&self, storage: &str, path: &str) -> PathBuf {
        use std::fmt::Write as _;
        let key = digest(&(storage, path));
        let name = key
            .as_ref()
            .iter()
            .fold(String::with_capacity(64), |mut text, byte| {
                write!(text, "{byte:02x}").expect("writing into a String");
                text
            });
        self.inner.root.join(format!("{name}.cache"))
    }

    async fn open(
        &self,
        storage: &str,
        path: &str,
        stamp: &(u64, String),
    ) -> AppResult<Option<(Outcome, File)>> {
        let mut file = match File::open(self.cached(storage, path)).await {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(cache_io(error)),
        };
        let mut header = [0_u8; RECORD_HEADER_BYTES];
        if let Err(error) = file.read_exact(&mut header).await {
            return if error.kind() == std::io::ErrorKind::UnexpectedEof {
                Ok(None)
            } else {
                Err(cache_io(error))
            };
        }
        if &header[..32] != digest(&(1, stamp)).as_ref() {
            return Ok(None);
        }
        let outcome = Outcome::from_byte(header[32]);
        if matches!(outcome, Some(Outcome::SourceUnavailable))
            && file
                .metadata()
                .await
                .map_err(cache_io)?
                .modified()
                .and_then(|modified| modified.elapsed().map_err(std::io::Error::other))
                .is_ok_and(|age| age >= Duration::from_secs(30))
        {
            return Ok(None);
        }
        Ok(outcome.map(|outcome| (outcome, file)))
    }

    pub(crate) async fn forget(&self, storage: &str, path: &str) {
        if is_gallery_image_name(path) {
            if let Ok(path) = crate::storage::StorageService::normalize_relative(path) {
                let _ = fs::remove_file(self.cached(storage, &path)).await;
            }
        }
    }

    pub(crate) fn spawn(
        &self,
        state: AppState,
        mut stop: watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        let gallery = self.clone();
        tokio::spawn(async move {
            let startup = async {
                let mut directory = fs::DirBuilder::new();
                directory.recursive(true);
                #[cfg(unix)]
                directory.mode(0o700);
                directory.create(&gallery.inner.root).await?;
                let cache_root = fs::canonicalize(&gallery.inner.root).await?;
                for mount in state.config.local_mounts.all() {
                    if fs::canonicalize(&mount.path)
                        .await
                        .is_ok_and(|mount| mount.starts_with(&cache_root))
                    {
                        return Err(std::io::Error::other(
                            "gallery cache cannot contain a user storage mount",
                        ));
                    }
                }
                gallery.trim().await?;
                // Tools come from the deployment, never from request data.
                for program in ["vipsheader", "vipsthumbnail"] {
                    let status = command(program).arg("--version").status().await?;
                    if !status.success() {
                        return Err(std::io::Error::other("libvips tools are unavailable"));
                    }
                }
                Ok::<_, std::io::Error>(())
            };
            match tokio::time::timeout(Duration::from_secs(5), startup).await {
                Ok(Ok(())) => gallery.inner.availability.store(READY, Ordering::Release),
                error => {
                    gallery
                        .inner
                        .availability
                        .store(DISABLED, Ordering::Release);
                    *gallery.inner.queue.lock().expect("gallery queue poisoned") = Queue::default();
                    tracing::warn!(?error, "gallery generator is disabled");
                    return;
                }
            }
            let mut writes = 0;
            loop {
                if *stop.borrow() {
                    break;
                }
                let job = {
                    let mut queue = gallery.inner.queue.lock().expect("gallery queue poisoned");
                    let job = queue.waiting.pop_front();
                    queue.active = job.clone();
                    job
                };
                let Some(job) = job else {
                    tokio::select! {
                        _ = stop.changed() => break,
                        _ = gallery.inner.notify.notified() => continue,
                    }
                };
                let result = tokio::select! {
                    _ = stop.changed() => break,
                    result = gallery.generate(&state, &job) => result,
                };
                {
                    let mut queue = gallery.inner.queue.lock().expect("gallery queue poisoned");
                    queue.active = None;
                    if std::mem::take(&mut queue.changed) {
                        queue.waiting.push_back(job);
                    } else {
                        queue.pending.remove(&job);
                    }
                }
                match result {
                    Ok(true) => {
                        writes += 1;
                        if writes == TRIM_INTERVAL {
                            writes = 0;
                            if let Err(error) = gallery.trim().await {
                                tracing::warn!(%error, "gallery cache trimming failed");
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(error) => tracing::debug!(%error, "gallery source could not be processed"),
                }
            }
            gallery
                .inner
                .availability
                .store(DISABLED, Ordering::Release);
            let _ = fs::remove_file(gallery.inner.root.join("source.part")).await;
            let _ = fs::remove_file(gallery.inner.root.join("result.part.jpg")).await;
            let _ = fs::remove_file(gallery.inner.root.join("cache.part")).await;
        })
    }

    async fn generate(&self, state: &AppState, job: &Job) -> AppResult<bool> {
        let backend = state.storage_backend(&job.storage).await?;
        let stamp = backend.gallery_stamp(&job.path).await?;
        if self.open(&job.storage, &job.path, &stamp).await?.is_some() {
            return Ok(false);
        }
        let input = self.inner.root.join("source.part");
        let output = self.inner.root.join("result.part.jpg");
        let _ = fs::remove_file(&output).await;
        let outcome = if stamp.0 > MAX_SOURCE_BYTES {
            Outcome::TooLarge
        } else {
            let outcome =
                match tokio::time::timeout(JOB_TIMEOUT, stage(&backend, &job.path, &input)).await {
                    Ok(Ok(false)) => Outcome::NotImage,
                    Ok(Ok(true)) => tokio::time::timeout(JOB_TIMEOUT, render(&self.inner.root))
                        .await
                        .unwrap_or(Outcome::Unavailable),
                    Ok(Err(AppError::PayloadTooLarge)) => Outcome::TooLarge,
                    _ => Outcome::SourceUnavailable,
                };
            let _ = fs::remove_file(&input).await;
            outcome
        };
        if backend.gallery_stamp(&job.path).await? != stamp {
            let _ = fs::remove_file(&output).await;
            return Ok(false);
        }
        let staged = self.inner.root.join("cache.part");
        let mut record = File::create(&staged).await.map_err(cache_io)?;
        record
            .write_all(digest(&(1, &stamp)).as_ref())
            .await
            .map_err(cache_io)?;
        record.write_all(&[outcome as u8]).await.map_err(cache_io)?;
        if matches!(outcome, Outcome::Ready) {
            let mut image = File::open(&output).await.map_err(cache_io)?;
            tokio::io::copy(&mut image, &mut record)
                .await
                .map_err(cache_io)?;
        }
        record.flush().await.map_err(cache_io)?;
        drop(record);
        let _ = fs::remove_file(output).await;
        fs::rename(staged, self.cached(&job.storage, &job.path))
            .await
            .map_err(cache_io)?;
        Ok(true)
    }

    async fn trim(&self) -> std::io::Result<()> {
        let mut directory = fs::read_dir(&self.inner.root).await?;
        let mut records = Vec::new();
        let mut bytes = 0_u64;
        while let Some(entry) = directory.next_entry().await? {
            let name = entry.file_name();
            let Some((key, extension)) = name.to_str().and_then(|name| name.split_once('.')) else {
                continue;
            };
            if key.len() != 64
                || !key.bytes().all(|byte| byte.is_ascii_hexdigit())
                || extension != "cache"
                || !entry.file_type().await?.is_file()
            {
                continue;
            }
            let metadata = entry.metadata().await?;
            bytes = bytes.saturating_add(metadata.len());
            records.push((
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                entry.path(),
                metadata.len(),
            ));
        }
        records.sort_unstable_by_key(|record| record.0);
        let mut remaining = records.len();
        for (_, path, size) in records {
            if bytes
                <= CACHE_BYTES
                    - (MAX_OUTPUT_BYTES + RECORD_HEADER_BYTES as u64) * TRIM_INTERVAL as u64
                && remaining <= CACHE_FILES - TRIM_INTERVAL
            {
                break;
            }
            fs::remove_file(path).await?;
            bytes = bytes.saturating_sub(size);
            remaining -= 1;
        }
        Ok(())
    }
}

fn digest(material: &impl serde::Serialize) -> ring::digest::Digest {
    ring::digest::digest(
        &ring::digest::SHA256,
        &serde_json::to_vec(material).expect("gallery cache key serializes"),
    )
}

fn cache_io(error: std::io::Error) -> AppError {
    AppError::with_source("gallery cache I/O failed", error)
}

async fn stage(backend: &StorageBackend, path: &str, input: &Path) -> AppResult<bool> {
    let response = backend
        .stream_file(
            path,
            &HeaderMap::new(),
            FileResponseMode::Attachment,
            &Method::GET,
        )
        .await?;
    let mut reader = StreamReader::new(
        response
            .into_body()
            .into_data_stream()
            .map_err(std::io::Error::other),
    );
    let mut prefix = Vec::with_capacity(64);
    (&mut reader)
        .take(64)
        .read_to_end(&mut prefix)
        .await
        .map_err(cache_io)?;
    if !image_signature(&prefix) {
        return Ok(false);
    }
    let mut file = File::create(input).await.map_err(cache_io)?;
    file.write_all(&prefix).await.map_err(cache_io)?;
    let remaining = MAX_SOURCE_BYTES - prefix.len() as u64;
    let size = tokio::io::copy(&mut reader.take(remaining + 1), &mut file)
        .await
        .map_err(cache_io)?;
    if size > remaining {
        return Err(AppError::PayloadTooLarge);
    }
    file.flush().await.map_err(cache_io)?;
    Ok(true)
}

fn image_signature(prefix: &[u8]) -> bool {
    prefix.starts_with(b"\xff\xd8\xff")
        || prefix.starts_with(b"\x89PNG\r\n\x1a\n")
        || prefix.starts_with(b"GIF87a")
        || prefix.starts_with(b"GIF89a")
        || (prefix.starts_with(b"RIFF") && prefix.get(8..12) == Some(b"WEBP"))
        || (prefix.get(4..8) == Some(b"ftyp") && {
            let box_size = prefix[..4].try_into().map(u32::from_be_bytes).unwrap_or(0) as usize;
            matches!(prefix.get(8..12), Some(b"avif" | b"avis"))
                || prefix
                    .get(16..box_size.min(prefix.len()))
                    .is_some_and(|brands| {
                        brands
                            .chunks_exact(4)
                            .any(|brand| matches!(brand, b"avif" | b"avis"))
                    })
        })
}

fn command(program: &str) -> Command {
    // Limit decoder heap/anonymous data without constraining the HTTP process.
    // This is not a cap on the total RSS of the whole container.
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new("prlimit");
        command.args(["--data=117440512", "--", program]);
        command
    };
    #[cfg(not(target_os = "linux"))]
    let mut command = Command::new(program);
    command
        .env("VIPS_CONCURRENCY", "1")
        .env("VIPS_BLOCK_UNTRUSTED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

async fn render(root: &Path) -> Outcome {
    let mut header = command("vipsheader");
    let Ok(info) = header
        .current_dir(root)
        .arg("source.part")
        .stdout(Stdio::piped())
        .output()
        .await
    else {
        return Outcome::Unavailable;
    };
    if !info.status.success() {
        return Outcome::Unavailable;
    }
    let dimensions = String::from_utf8_lossy(&info.stdout);
    let pixels = dimensions.split_whitespace().find_map(|word| {
        let (width, height) = word.split_once('x')?;
        width.parse::<u64>().ok()?.checked_mul(height.parse().ok()?)
    });
    if pixels.is_none_or(|pixels| pixels == 0 || pixels > MAX_PIXELS) {
        return Outcome::TooLarge;
    }
    let result = command("vipsthumbnail")
        .current_dir(root)
        .args([
            "--size",
            "640x640>",
            "--path",
            "result.part.jpg[Q=78,strip]",
            "source.part",
        ])
        .status()
        .await;
    if !result.is_ok_and(|status| status.success()) {
        return Outcome::Unavailable;
    }
    if !fs::metadata(root.join("result.part.jpg"))
        .await
        .is_ok_and(|meta| (1..=MAX_OUTPUT_BYTES).contains(&meta.len()))
    {
        return Outcome::Unavailable;
    }
    Outcome::Ready
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_raster_signatures_without_trusting_the_extension() {
        assert!(image_signature(b"\xff\xd8\xff\xe0"));
        assert!(image_signature(b"\x89PNG\r\n\x1a\n"));
        assert!(image_signature(b"GIF89a"));
        assert!(image_signature(b"RIFFxxxxWEBP"));
        assert!(image_signature(b"\0\0\0\x18ftypmif1\0\0\0\0miafavif"));
        assert!(!image_signature(b"PK\x03\x04"));
        assert!(!image_signature(b"\0\0\0\x14ftypmp42\0\0\0\0isom"));
        assert!(!image_signature(b""));
    }

    #[tokio::test]
    async fn disk_cache_never_returns_a_different_original_version() {
        let dir = crate::test_support::TestDirectory::new("gallery-disk-version");
        let gallery = Gallery::new(&dir.path().join("config.json"));
        fs::create_dir_all(&gallery.inner.root).await.unwrap();
        let before = (100_u64, "version 1".to_owned());
        let after = (100_u64, "version 2".to_owned());
        let key = gallery.cached("local", "photo.jpg");
        let mut bytes = digest(&(1, &before)).as_ref().to_vec();
        bytes.push(Outcome::Ready as u8);
        bytes.extend_from_slice(b"thumbnail");
        fs::write(&key, bytes).await.unwrap();
        assert!(gallery
            .open("local", "photo.jpg", &after)
            .await
            .unwrap()
            .is_none());
        let (_, mut file) = gallery
            .open("local", "photo.jpg", &before)
            .await
            .unwrap()
            .unwrap();
        let mut image = Vec::new();
        file.read_to_end(&mut image).await.unwrap();
        assert_eq!(image, b"thumbnail");
        drop(file);
        gallery.forget("local", "photo.jpg").await;
        assert!(!fs::try_exists(key).await.unwrap());
    }
}
