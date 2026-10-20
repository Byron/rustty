//! Share immutable decoded pixels without keeping unused image allocations alive.
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex, Weak},
};

const MAX_ENTRIES: usize = 4096;
type Key = (u32, u32, u32);

#[derive(Default)]
struct Cache {
    entries: HashMap<Key, Weak<Vec<u8>>>,
}

impl Cache {
    fn intern(&mut self, key: Key, mut pixels: Vec<u8>) -> Arc<Vec<u8>> {
        if let Some(existing) = self.entries.get(&key).and_then(Weak::upgrade)
            && *existing == pixels
        {
            return existing;
        }
        if self.entries.len() >= MAX_ENTRIES {
            self.entries.retain(|_, pixels| pixels.strong_count() != 0);
            if self.entries.len() >= MAX_ENTRIES {
                // Bound lookup metadata; existing owners remain valid after an index reset.
                self.entries.clear();
            }
        }
        pixels.shrink_to_fit();
        // A weak Arc<Vec> retains only the header after its last owner releases the buffer.
        let pixels = Arc::new(pixels);
        self.entries.insert(key, Arc::downgrade(&pixels));
        pixels
    }
}

pub(super) fn intern(width: u32, height: u32, pixels: Vec<u8>) -> Arc<Vec<u8>> {
    static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);
    let key = (width, height, crc32c::crc32c(&pixels));
    CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .intern(key, pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_pixels_share_but_checksum_collisions_and_dimensions_do_not() {
        let mut cache = Cache::default();
        let mut pixels = Vec::with_capacity(1024);
        pixels.extend_from_slice(&[1; 8]);
        let first = cache.intern((2, 1, 7), pixels);
        assert_eq!(first.capacity(), first.len());
        let duplicate = cache.intern((2, 1, 7), vec![1; 8]);
        assert!(Arc::ptr_eq(&first, &duplicate));
        let collision = cache.intern((2, 1, 7), vec![2; 8]);
        assert!(!Arc::ptr_eq(&first, &collision));
        assert_eq!(*first, [1; 8]);
        let rotated = cache.intern((1, 2, 7), vec![2; 8]);
        assert!(!Arc::ptr_eq(&rotated, &collision));
    }

    #[test]
    fn index_is_bounded_and_does_not_own_pixels() {
        let mut cache = Cache::default();
        let first = cache.intern((1, 1, 0), vec![0; 4]);
        let weak = Arc::downgrade(&first);
        drop(first);
        assert!(weak.upgrade().is_none());
        for id in 1..=MAX_ENTRIES as u32 {
            cache.intern((1, 1, id), vec![0; 4]);
        }
        assert_eq!(cache.entries.len(), 1);
        let live: Vec<_> = (0..=MAX_ENTRIES as u32)
            .map(|id| cache.intern((1, 1, id), vec![id as u8; 4]))
            .collect();
        assert!(cache.entries.len() <= MAX_ENTRIES);
        assert_eq!(*live[7], [7; 4]);
    }

    #[test]
    fn concurrent_uploads_share_one_owner() {
        let owners = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| intern(19, 7, vec![42; 19 * 7 * 4])))
                .collect();
            threads
                .into_iter()
                .map(|t| t.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(owners.iter().all(|owner| Arc::ptr_eq(owner, &owners[0])));
    }
}
pub(super) mod serde_rgba {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};
    use std::sync::Arc;

    pub fn serialize<S: Serializer>(
        pixels: &Arc<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer
            .serialize_str(&base64::engine::general_purpose::STANDARD.encode(pixels.as_slice()))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<Vec<u8>>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Bytes {
            Encoded(String),
            Legacy(Vec<u8>),
        }
        match Bytes::deserialize(deserializer)? {
            Bytes::Encoded(encoded) => base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map(Arc::new)
                .map_err(serde::de::Error::custom),
            Bytes::Legacy(bytes) => Ok(Arc::new(bytes)),
        }
    }
}
