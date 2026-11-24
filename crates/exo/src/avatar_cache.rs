//! Shared `egui::Context` texture cache, image decoding and in-flight deduplication for avatars:
//! signed-in account photos (`profile_pictures`), contact photos (`people`) and validated BIMI
//! brand logos (`brands`).

use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::{Arc, Mutex, PoisonError},
};

use exo_ui::AvatarPhoto;

enum Entry<X> {
    Shown(AvatarPhoto, X),
    Missing,
}

struct Inner<K, X> {
    entries: HashMap<K, Entry<X>>,
    in_flight: HashSet<K>,
}

impl<K, X> Default for Inner<K, X> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            in_flight: HashSet::new(),
        }
    }
}

/// Outcome of looking up an avatar while claiming an in-flight slot on a cache miss.
pub(crate) enum Lookup {
    /// Decoded texture is ready to paint.
    Shown(AvatarPhoto),
    /// First caller to miss; caller should start the background load and finish with
    /// [`AvatarCache::finish_decode`].
    Claimed,
    /// Already in flight or previously marked missing.
    Unavailable,
}

/// Thread-safe avatar texture cache stored in `egui::Context` temporary data under `id`.
pub(crate) struct AvatarCache<K, X = ()> {
    ctx: egui::Context,
    inner: Arc<Mutex<Inner<K, X>>>,
}

impl<K, X> Clone for AvatarCache<K, X> {
    fn clone(&self) -> Self {
        Self {
            ctx: self.ctx.clone(),
            inner: self.inner.clone(),
        }
    }
}

impl<K, X> AvatarCache<K, X>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    X: Clone + Send + Sync + 'static,
{
    pub(crate) fn of(ctx: &egui::Context, id: &'static str) -> Self {
        let inner = ctx.data_mut(|data| {
            data.get_temp_mut_or_default::<Arc<Mutex<Inner<K, X>>>>(egui::Id::new(id))
                .clone()
        });
        Self {
            ctx: ctx.clone(),
            inner,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner<K, X>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn photo<Q>(&self, key: &Q) -> Option<AvatarPhoto>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        match self.lock().entries.get(key) {
            Some(Entry::Shown(photo, _)) => Some(photo.clone()),
            Some(Entry::Missing) | None => None,
        }
    }

    pub(crate) fn extra<Q>(&self, key: &Q) -> Option<X>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        match self.lock().entries.get(key) {
            Some(Entry::Shown(_, extra)) => Some(extra.clone()),
            Some(Entry::Missing) | None => None,
        }
    }

    pub(crate) fn is_shown<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        matches!(self.lock().entries.get(key), Some(Entry::Shown(..)))
    }

    pub(crate) fn claim(&self, key: K) -> bool {
        self.lock().in_flight.insert(key)
    }

    pub(crate) fn unclaim<Q>(&self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.lock().in_flight.remove(key);
    }

    pub(crate) fn get_or_claim(&self, key: K) -> Lookup {
        let mut inner = self.lock();
        if let Some(entry) = inner.entries.get(&key) {
            return match entry {
                Entry::Shown(photo, _) => Lookup::Shown(photo.clone()),
                Entry::Missing => Lookup::Unavailable,
            };
        }
        if inner.in_flight.insert(key) {
            Lookup::Claimed
        } else {
            Lookup::Unavailable
        }
    }

    pub(crate) fn set(&self, key: K, value: Option<(AvatarPhoto, X)>) {
        let mut inner = self.lock();
        match value {
            Some((photo, extra)) => {
                inner.entries.insert(key, Entry::Shown(photo, extra));
            }
            None => {
                inner.entries.remove(&key);
            }
        }
        drop(inner);
        self.ctx.request_repaint();
    }

    pub(crate) fn extend(&self, items: impl IntoIterator<Item = (K, AvatarPhoto, X)>) {
        let mut inner = self.lock();
        for (key, photo, extra) in items {
            inner.entries.insert(key, Entry::Shown(photo, extra));
        }
        drop(inner);
        self.ctx.request_repaint();
    }

    pub(crate) fn remove<Q>(&self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let removed = self.lock().entries.remove(key).is_some();
        if removed {
            self.ctx.request_repaint();
        }
    }

    pub(crate) fn retain(&self, mut keep: impl FnMut(&K) -> bool) {
        let mut inner = self.lock();
        let before = inner.entries.len();
        inner.entries.retain(|key, _| keep(key));
        inner.in_flight.retain(|key| keep(key));
        let changed = inner.entries.len() != before;
        drop(inner);
        if changed {
            self.ctx.request_repaint();
        }
    }

    pub(crate) fn clear_missing(&self) {
        self.lock()
            .entries
            .retain(|_, entry| !matches!(entry, Entry::Missing));
    }
}

impl<K> AvatarCache<K, ()>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
{
    pub(crate) fn finish_decode(&self, key: K, label: &str, bytes: &[u8]) {
        let entry = match decode(&self.ctx, label, bytes) {
            Some(photo) => Entry::Shown(photo, ()),
            None => Entry::Missing,
        };
        let mut inner = self.lock();
        inner.in_flight.remove(&key);
        inner.entries.insert(key, entry);
        drop(inner);
        self.ctx.request_repaint();
    }
}

/// Decodes `bytes` into an [`AvatarPhoto`] texture pair, preserving alpha when present.
pub(crate) fn try_decode(
    ctx: &egui::Context,
    label: &str,
    bytes: &[u8],
) -> Result<AvatarPhoto, image::ImageError> {
    let image = image::load_from_memory(bytes)?.into_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Ok(AvatarPhoto::new(
        ctx,
        label,
        egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
    ))
}

/// [`try_decode`] discarding decode errors.
pub(crate) fn decode(ctx: &egui::Context, label: &str, bytes: &[u8]) -> Option<AvatarPhoto> {
    try_decode(ctx, label, bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([20, 120, 220, 255]))
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("png");
        out.into_inner()
    }

    #[test]
    fn deduplicates_in_flight_loads_and_retries_missing_entries_after_clear() {
        let ctx = egui::Context::default();
        let cache = AvatarCache::<String>::of(&ctx, "exo::test::avatars");
        assert!(matches!(cache.get_or_claim("a".into()), Lookup::Claimed));
        assert!(matches!(
            cache.get_or_claim("a".into()),
            Lookup::Unavailable
        ));

        cache.finish_decode("a".into(), "test", b"not an image");
        assert!(matches!(
            cache.get_or_claim("a".into()),
            Lookup::Unavailable
        ));

        cache.clear_missing();
        assert!(matches!(cache.get_or_claim("a".into()), Lookup::Claimed));
        cache.finish_decode("a".into(), "test", &png());
        assert!(matches!(cache.get_or_claim("a".into()), Lookup::Shown(_)));
        assert!(cache.photo("a").is_some());
    }
}
