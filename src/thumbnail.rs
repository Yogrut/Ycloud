use std::{
    collections::VecDeque,
    hash::{Hash, Hasher},
    io::Cursor,
    sync::Mutex,
};

use axum::{
    body::{to_bytes, Body},
    http::{header, HeaderMap, Method, Response, StatusCode},
};
use base64::Engine;
use bytes::Bytes;
use image::{
    codecs::jpeg::JpegEncoder, DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits,
};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
    storage::FileResponseMode,
    storage_backend::StorageBackend,
};

const SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_PIXELS: u64 = 24_000_000;
const MAX_DECODE_BYTES: u64 = 128 * 1024 * 1024;
const THUMBNAIL_EDGE: u32 = 640;
const CACHE_BYTES: usize = 8 * 1024 * 1024;
const CACHE_ENTRIES: usize = 128;

#[derive(Clone)]
struct CachedThumbnail {
    path: String,
    version: String,
    bytes: Bytes,
    etag: String,
}

// Only compressed thumbnails live here. Ownership follows the immutable
// storage instance; no source buffer or decoded image survives generation.
pub(crate) struct ThumbnailCache {
    entries: Mutex<VecDeque<CachedThumbnail>>,
    generation: [tokio::sync::Mutex<()>; 32],
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            generation: std::array::from_fn(|_| tokio::sync::Mutex::new(())),
        }
    }
}

impl ThumbnailCache {
    fn get(&self, path: &str, version: &str) -> Option<CachedThumbnail> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = entries
            .iter()
            .position(|entry| entry.path == path && entry.version == version)?;
        let entry = entries.remove(index)?;
        entries.push_back(entry.clone());
        Some(entry)
    }

    fn insert(&self, entry: CachedThumbnail) {
        if entry.bytes.len() > CACHE_BYTES {
            return;
        }
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|old| old.path != entry.path);
        entries.push_back(entry);
        while entries.len() > CACHE_ENTRIES
            || entries.iter().map(|entry| entry.bytes.len()).sum::<usize>() > CACHE_BYTES
        {
            entries.pop_front();
        }
    }

    async fn generation_lock(&self, path: &str) -> tokio::sync::MutexGuard<'_, ()> {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut hash);
        self.generation[hash.finish() as usize % self.generation.len()]
            .lock()
            .await
    }
}

fn invalid_image() -> AppError {
    AppError::BadRequest("This file cannot be decoded as a thumbnail".into())
}

fn encode_thumbnail(source: &[u8]) -> AppResult<Vec<u8>> {
    let mut reader = ImageReader::new(Cursor::new(source))
        .with_guessed_format()
        .map_err(|_| invalid_image())?;
    // Decode actual bytes. A JPEG named .png is still a valid image.
    if !matches!(
        reader.format(),
        Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::Gif | ImageFormat::WebP)
    ) {
        return Err(invalid_image());
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(10_000);
    limits.max_image_height = Some(10_000);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| invalid_image())?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > MAX_PIXELS || decoder.total_bytes() > MAX_DECODE_BYTES
    {
        return Err(AppError::PayloadTooLarge);
    }
    let orientation = decoder.orientation().map_err(|_| invalid_image())?;
    let image = DynamicImage::from_decoder(decoder).map_err(|_| invalid_image())?;
    let mut thumbnail = if width > THUMBNAIL_EDGE || height > THUMBNAIL_EDGE {
        image.thumbnail(THUMBNAIL_EDGE, THUMBNAIL_EDGE)
    } else {
        image
    };
    thumbnail.apply_orientation(orientation);
    let rgba = thumbnail.into_rgba8();
    let rgb = image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [red, green, blue, alpha] = rgba.get_pixel(x, y).0;
        let alpha = u32::from(alpha);
        image::Rgb(
            [red, green, blue]
                .map(|channel| ((u32::from(channel) * alpha + 245 * (255 - alpha)) / 255) as u8),
        )
    });
    let mut encoded = Vec::new();
    JpegEncoder::new_with_quality(&mut encoded, 72)
        .encode_image(&rgb)
        .map_err(|error| AppError::with_source("failed to encode thumbnail", error))?;
    Ok(encoded)
}

pub(crate) async fn response(
    state: &AppState,
    backend: &StorageBackend,
    path: &str,
    headers: &HeaderMap,
    method: &Method,
) -> AppResult<Response<Body>> {
    let cache = backend.thumbnail_cache();
    let _generation = cache.generation_lock(path).await;
    let version = backend.thumbnail_version(path).await?;
    if let Some(cached) = version
        .as_deref()
        .and_then(|version| cache.get(path, version))
    {
        return image_response(cached.bytes, &cached.etag, headers, method);
    }
    // Bound concurrent decoding, not HTTP responses or cache hits.
    // Retain the CPU permit even if the request waiter disconnects.
    let permit = state
        .thumbnail_gate
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| AppError::ServiceUnavailable("Thumbnail worker is unavailable".into()))?;
    let response = backend
        .stream_file(
            path,
            &HeaderMap::new(),
            FileResponseMode::Preview,
            &Method::GET,
        )
        .await?;
    if response.status() != StatusCode::OK {
        return Err(AppError::from(response.status()));
    }
    if response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|length| length.to_str().ok()?.parse::<u64>().ok())
        .is_some_and(|length| length > SOURCE_BYTES as u64)
    {
        return Err(AppError::PayloadTooLarge);
    }
    let source = to_bytes(response.into_body(), SOURCE_BYTES)
        .await
        .map_err(|error| {
            if std::error::Error::source(&error)
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>())
            {
                AppError::PayloadTooLarge
            } else {
                AppError::with_source("failed to read thumbnail source", error)
            }
        })?;
    let encoded = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        encode_thumbnail(&source)
    })
    .await
    .map_err(|error| AppError::with_source("thumbnail worker failed", error))??;
    let bytes = Bytes::from(encoded);
    // This strong ETag identifies the actual generated response, not a guessed
    // original-file identity made from timestamps.
    let etag = format!(
        "\"thumbnail-{}\"",
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref())
    );
    if let Some(version) = version {
        if backend.thumbnail_version(path).await?.as_deref() == Some(version.as_str()) {
            cache.insert(CachedThumbnail {
                path: path.to_owned(),
                version,
                bytes: bytes.clone(),
                etag: etag.clone(),
            });
        }
    }
    image_response(bytes, &etag, headers, method)
}

fn image_response(
    bytes: Bytes,
    etag: &str,
    headers: &HeaderMap,
    method: &Method,
) -> AppResult<Response<Body>> {
    let validators = crate::storage::FileValidators::object(Some(etag), None);
    if let Some(mut response) = validators.precondition_response(headers, method)? {
        if response.status() == StatusCode::NOT_MODIFIED {
            set_cache_headers(&mut response);
        }
        return Ok(response);
    }
    let mut response = validators
        .apply(Response::builder())
        .header(header::CONTENT_TYPE, "image/jpeg")
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(if method == Method::HEAD {
            Body::empty()
        } else {
            Body::from(bytes)
        })
        .map_err(|error| AppError::with_source("failed to build thumbnail response", error))?;
    set_cache_headers(&mut response);
    Ok(response)
}

fn set_cache_headers(response: &mut Response<Body>) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-cache"),
    );
    response
        .headers_mut()
        .insert(header::VARY, axum::http::HeaderValue::from_static("Cookie"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_a_real_smaller_image_without_cropping_the_source() {
        let original = image::RgbImage::from_pixel(1600, 1000, image::Rgb([20, 50, 100]));
        let mut source = Cursor::new(Vec::new());
        original.write_to(&mut source, ImageFormat::Png).unwrap();
        let thumbnail = encode_thumbnail(source.get_ref()).unwrap();
        assert_ne!(thumbnail, *source.get_ref());
        assert_eq!(image::guess_format(&thumbnail).unwrap(), ImageFormat::Jpeg);
        let decoded = image::load_from_memory(&thumbnail).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (640, 400));
    }

    #[test]
    fn unsupported_bytes_do_not_return_original_bytes() {
        assert!(encode_thumbnail(b"not an image").is_err());
    }

    #[test]
    fn cache_is_compressed_only_bounded_and_discards_replaced_versions() {
        let cache = ThumbnailCache::default();
        let make = |path: &str, version: &str, size| CachedThumbnail {
            path: path.into(),
            version: version.into(),
            bytes: Bytes::from(vec![0; size]),
            etag: "\"test\"".into(),
        };
        cache.insert(make("one", "v1", CACHE_BYTES / 2));
        cache.insert(make("two", "v1", CACHE_BYTES / 2));
        assert!(cache.get("one", "v1").is_some());
        cache.insert(make("three", "v1", CACHE_BYTES / 2));
        assert!(cache.get("two", "v1").is_none());
        cache.insert(make("one", "v2", 10));
        assert!(cache.get("one", "v1").is_none());
        assert!(cache.get("one", "v2").is_some());
        for index in 0..CACHE_ENTRIES + 2 {
            cache.insert(make(&index.to_string(), "v1", 1));
        }
        assert!(cache.entries.lock().unwrap().len() <= CACHE_ENTRIES);
        let other_instance = ThumbnailCache::default();
        assert!(other_instance.get("one", "v2").is_none());
    }
}
