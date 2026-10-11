//! The gateway's media store and the channel-agnostic fetch seam (§11.5).
//!
//! Bytes live under one account-level root, outside any session workspace, and
//! are addressed by the stable `media://inbound/<id>` handle stored on the
//! event. This is a *second* resolver next to `Workspace`; the two are kept
//! separate on purpose.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};

use crate::database::events::StoredEvent;
use crate::event::{Event, EventOrigin, Media, MediaHandle, Part};
use crate::identity::fnv1a;

/// Per-file cap; the public Telegram Bot API cannot serve more than 20 MiB.
pub const MEDIA_MAX_BYTES: usize = 20 * 1024 * 1024;
/// Downloaded media older than this is pruned on startup.
pub const MEDIA_TTL: Duration = Duration::from_secs(60 * 60 * 24 * 7);
/// How long a single media download may take before it is abandoned.
pub const MEDIA_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

pub type BoxedFetchFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Fetch raw bytes for a platform media handle. Implemented per channel.
pub trait MediaFetcher: Send + Sync {
    fn fetch<'a>(&'a self, handle: &MediaHandle) -> BoxedFetchFuture<'a, Result<Vec<u8>>>;
}

pub struct MediaStore {
    root: PathBuf,
}

impl MediaStore {
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating media store {}", root.display()))?;
        let root = root
            .canonicalize()
            .with_context(|| format!("resolving media store {}", root.display()))?;

        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The stable id for a platform handle: FNV-1a hex (like identity refs) plus
    /// an extension derived from the mime, when we know one.
    pub fn id_for(handle: &MediaHandle, mime: Option<&str>) -> String {
        let hash = format!("{:016x}", fnv1a(handle.0.as_bytes()));

        match mime.and_then(extension_for) {
            Some(extension) => format!("{hash}.{extension}"),
            None => hash,
        }
    }

    /// The `media://` handle for a platform handle.
    pub fn uri(handle: &MediaHandle, mime: Option<&str>) -> String {
        format!("media://inbound/{}", Self::id_for(handle, mime))
    }

    /// The single path component behind a `media://inbound/<id>` URI.
    fn uri_id(uri: &str) -> Result<&str> {
        uri.strip_prefix("media://inbound/")
            .ok_or_else(|| anyhow!("invalid-path: expected `media://inbound/<id>`, got `{uri}`"))
    }

    /// Resolve a `media://inbound/<id>` URI to a path inside the store.
    pub fn resolve(&self, uri: &str) -> Result<PathBuf> {
        self.path(Self::uri_id(uri)?)
    }

    /// Resolve a bare id to a path, rejecting anything that is not one component.
    pub fn path(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\']) {
            bail!("path-not-allowed: `{id}` is not a single path component");
        }

        Ok(self.root.join(id))
    }

    pub fn contains(&self, id: &str) -> bool {
        self.path(id).map(|path| path.is_file()).unwrap_or(false)
    }

    /// Store `bytes` under `id` atomically.
    pub fn save(&self, id: &str, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MEDIA_MAX_BYTES {
            bail!(
                "media too large: {} bytes (cap {MEDIA_MAX_BYTES})",
                bytes.len()
            );
        }

        let path = self.path(id)?;
        let temporary = path.with_extension("part");

        std::fs::write(&temporary, bytes)
            .with_context(|| format!("writing {}", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("committing {}", path.display()))?;

        Ok(())
    }

    /// Remove files older than `ttl`, returning how many were deleted.
    pub fn prune(&self, ttl: Duration) -> usize {
        let Some(cutoff) = SystemTime::now().checked_sub(ttl) else {
            return 0;
        };

        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return 0;
        };

        let mut removed = 0;
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };

            if metadata.is_file()
                && metadata.modified().is_ok_and(|mtime| mtime < cutoff)
                && std::fs::remove_file(entry.path()).is_ok()
            {
                removed += 1;
            }
        }

        removed
    }
}

/// A file extension for a mime type, when we know one.
fn extension_for(mime: &str) -> Option<&'static str> {
    let extension = match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "video/webm" => "webm",
        "video/mp4" => "mp4",
        "audio/ogg" => "ogg",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "application/pdf" => "pdf",
        "application/x-tgsticker" => "tgs",
        "application/octet-stream" => "bin",
        "text/plain" => "txt",
        _ => return None,
    };

    Some(extension)
}

/// Download and store any media the batch references but the store is missing.
/// Best-effort: failures only warn, so a missing file degrades to an
/// unavailable reference rather than failing the turn.
pub async fn ensure_loaded(
    events: &[StoredEvent],
    fetcher: &dyn MediaFetcher,
    store: &MediaStore,
    timeout: Duration,
) {
    for stored in events {
        let EventOrigin::Platform(platform) = &stored.event.origin else {
            continue;
        };

        for media in platform.parts.iter().filter_map(part_media) {
            let Some(uri) = &media.uri else {
                continue;
            };
            let Ok(id) = MediaStore::uri_id(uri) else {
                continue;
            };
            if store.contains(id) {
                continue;
            }

            match tokio::time::timeout(timeout, fetcher.fetch(&media.handle)).await {
                Ok(Ok(bytes)) => match store.save(id, &bytes) {
                    Ok(()) => {
                        tracing::debug!(target: "media", "stored {uri} ({} bytes)", bytes.len());
                    }
                    Err(error) => {
                        tracing::warn!(target: "media", "storing {uri} failed: {error:#}");
                    }
                },
                Ok(Err(error)) => {
                    tracing::warn!(target: "media", "fetching {uri} failed: {error:#}");
                }
                Err(_) => {
                    tracing::warn!(target: "media", "fetching {uri} timed out");
                }
            }
        }
    }
}

/// Fill each media part's `uri` with its `media://` handle before the event is
/// stored. Pure and in-memory: the bytes are fetched later, in the session task.
pub fn tag_event(event: &mut Event) {
    let EventOrigin::Platform(platform) = &mut event.origin else {
        return;
    };

    for part in &mut platform.parts {
        let media = match part {
            Part::Photo { media, .. }
            | Part::Voice { media, .. }
            | Part::Document { media, .. }
            | Part::Sticker { media } => media,
            Part::Text { .. } => continue,
        };

        if media.uri.is_none() {
            media.uri = Some(MediaStore::uri(media.key(), media.mime.as_deref()));
        }
    }
}

fn part_media(part: &Part) -> Option<&Media> {
    match part {
        Part::Photo { media, .. }
        | Part::Voice { media, .. }
        | Part::Document { media, .. }
        | Part::Sticker { media } => Some(media),
        Part::Text { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{Channel, DeliveryTarget};
    use crate::event::{ActorRef, Event, EventId, EventOrigin};

    fn store() -> MediaStore {
        MediaStore::new(crate::workspace::temp_base("media")).unwrap()
    }

    fn handle(file_id: &str) -> MediaHandle {
        MediaHandle(file_id.to_string())
    }

    fn media_part(file_id: &str, uri: Option<String>) -> Part {
        Part::Photo {
            media: Media {
                handle: handle(file_id),
                unique_id: MediaHandle::default(),
                uri,
                mime: Some("image/jpeg".to_string()),
            },
            caption: None,
        }
    }

    #[test]
    fn the_store_key_prefers_the_unique_id() {
        let media = Media::new(
            handle("download-handle"),
            handle("stable-id"),
            Some("image/jpeg".to_string()),
        );

        assert_eq!(media.key(), &handle("stable-id"));

        let fallback = Media::new(handle("only"), MediaHandle::default(), None);
        assert_eq!(fallback.key(), &handle("only"));
    }

    fn stored_media(part: Part) -> StoredEvent {
        let mut event = Event::platform_text(
            DeliveryTarget::direct(Channel::Telegram, "default", "1"),
            ActorRef::new("member_a"),
            "1".to_string(),
            "",
        );
        let EventOrigin::Platform(platform) = &mut event.origin else {
            panic!("helper must build a platform event");
        };
        platform.parts = vec![part];

        StoredEvent {
            id: EventId(1),
            event,
        }
    }

    struct FakeFetcher(Option<Vec<u8>>);

    impl MediaFetcher for FakeFetcher {
        fn fetch<'a>(&'a self, _handle: &MediaHandle) -> BoxedFetchFuture<'a, Result<Vec<u8>>> {
            let result = match &self.0 {
                Some(bytes) => Ok(bytes.clone()),
                None => Err(anyhow!("boom")),
            };
            Box::pin(async move { result })
        }
    }

    #[test]
    fn a_handle_maps_to_a_stable_id_and_uri() {
        let handle = handle("file");

        assert_eq!(
            MediaStore::id_for(&handle, Some("image/webp")),
            MediaStore::id_for(&handle, Some("image/webp"))
        );
        assert_eq!(
            MediaStore::uri(&handle, Some("image/webp")),
            format!(
                "media://inbound/{}",
                MediaStore::id_for(&handle, Some("image/webp"))
            )
        );
    }

    #[test]
    fn a_known_mime_selects_the_extension() {
        let handle = handle("file");

        assert!(MediaStore::id_for(&handle, Some("image/webp")).ends_with(".webp"));
        assert!(MediaStore::id_for(&handle, Some("application/x-tgsticker")).ends_with(".tgs"));
        assert_eq!(
            MediaStore::id_for(&handle, None),
            MediaStore::id_for(&handle, Some("application/x-unknown"))
        );
    }

    #[test]
    fn resolve_rejects_anything_but_an_inbound_id() {
        let store = store();

        assert!(store.resolve("file:///etc/passwd").is_err());
        assert!(store.resolve("media://outbound/x").is_err());
        assert!(store.resolve("media://inbound/a/b").is_err());
        assert!(store.resolve("media://inbound/..").is_err());
        assert_eq!(
            store.resolve("media://inbound/abc").unwrap(),
            store.root().join("abc")
        );
    }

    #[test]
    fn save_commits_atomically_and_contains_reports_it() {
        let store = store();

        assert!(!store.contains("abc"));
        store.save("abc", b"bytes").unwrap();
        assert!(store.contains("abc"));
        assert_eq!(std::fs::read(store.root().join("abc")).unwrap(), b"bytes");
    }

    #[test]
    fn save_rejects_a_file_over_the_cap() {
        let store = store();
        let too_big = vec![0u8; MEDIA_MAX_BYTES + 1];

        let error = store.save("big", &too_big).unwrap_err();

        assert!(error.to_string().starts_with("media too large:"), "{error}");
    }

    #[test]
    fn prune_removes_old_files_and_keeps_fresh_ones() {
        let store = store();
        store.save("old", b"x").unwrap();
        store.save("new", b"y").unwrap();

        let old = std::fs::File::options()
            .write(true)
            .open(store.root().join("old"))
            .unwrap();
        old.set_modified(SystemTime::now() - Duration::from_secs(60 * 60 * 24 * 30))
            .unwrap();

        assert_eq!(store.prune(MEDIA_TTL), 1);
        assert!(!store.contains("old"));
        assert!(store.contains("new"));
    }

    #[tokio::test]
    async fn ensure_loaded_stores_missing_media() {
        let store = store();
        let uri = MediaStore::uri(&handle("file"), Some("image/jpeg"));
        let events = vec![stored_media(media_part("file", Some(uri.clone())))];

        let fetcher = FakeFetcher(Some(b"bytes".to_vec()));
        ensure_loaded(&events, &fetcher, &store, MEDIA_FETCH_TIMEOUT).await;

        let id = MediaStore::id_for(&handle("file"), Some("image/jpeg"));
        assert_eq!(std::fs::read(store.root().join(&id)).unwrap(), b"bytes");
    }

    #[tokio::test]
    async fn ensure_loaded_degrades_on_a_fetch_error() {
        let store = store();
        let uri = MediaStore::uri(&handle("file"), Some("image/jpeg"));
        let events = vec![stored_media(media_part("file", Some(uri)))];

        ensure_loaded(&events, &FakeFetcher(None), &store, MEDIA_FETCH_TIMEOUT).await;

        assert!(!store.contains(&MediaStore::id_for(&handle("file"), Some("image/jpeg"))));
    }
}
