//! Change notifications for browser directory views.
//!
//! Every mutation funnels through `StorageBackend`, so reporting there covers
//! the HTTP API, WebDAV, uploads and batch transfers alike. A signal names only
//! the storage and the directory: subscribers re-read the page they already
//! show, and no file name ever travels over the wire.

use std::sync::OnceLock;

use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub(crate) struct DirChange {
    pub storage: String,
    pub dir: String,
}

static CHANNEL: OnceLock<broadcast::Sender<DirChange>> = OnceLock::new();

fn channel() -> &'static broadcast::Sender<DirChange> {
    CHANNEL.get_or_init(|| broadcast::channel(256).0)
}

pub(crate) fn subscribe() -> broadcast::Receiver<DirChange> {
    channel().subscribe()
}

/// Reports that `path`'s parent directory changed. Backends without an identity
/// (unit tests) and empty subscriber lists stay free.
pub(crate) fn publish(storage_id: &str, path: &str) {
    if storage_id.is_empty() {
        return;
    }
    let dir = path
        .trim_matches('/')
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    let _ = channel().send(DirChange {
        storage: storage_id.to_owned(),
        dir: dir.to_owned(),
    });
}
