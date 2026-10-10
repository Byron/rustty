use super::Image;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
};

/// Image storage with cached lookups and memory accounting.
#[derive(Debug, Default)]
pub struct Images {
    entries: HashMap<u32, Image>,
    // shortcut: duplicate numbers scan images; index them if profiles justify it.
    numbers: Option<HashMap<u32, Option<u32>>>,
    lowest_free: u32,
    cached_bytes: Option<usize>,
}

impl Images {
    pub(super) fn cached_bytes_used(&mut self) -> usize {
        *self
            .cached_bytes
            .get_or_insert_with(|| self.entries.values().map(Image::bytes).sum())
    }

    pub(super) fn allocate_id(&mut self) -> u32 {
        while self.lowest_free == 0 || self.entries.contains_key(&self.lowest_free) {
            self.lowest_free = self.lowest_free.wrapping_add(1);
        }
        // Failed or unfinished uploads must not consume the chosen ID.
        self.lowest_free
    }

    pub fn insert(&mut self, key: u32, image: Image) -> Option<Image> {
        let number = image.number;
        let added_bytes = self.cached_bytes.and_then(|_| checked_bytes(&image));
        let previous = self.entries.insert(key, image);
        self.cached_bytes = self.cached_bytes.and_then(|used| {
            let previous_bytes = previous.as_ref().map_or(Some(0), checked_bytes)?;
            used.checked_sub(previous_bytes)?.checked_add(added_bytes?)
        });
        if let Some(previous) = &previous {
            self.forget_number(previous.number, key);
        }
        if number != 0
            && let Some(numbers) = &mut self.numbers
        {
            numbers
                .entry(number)
                .and_modify(|id| *id = None)
                .or_insert(Some(key));
        }
        previous
    }

    pub fn remove(&mut self, key: &u32) -> Option<Image> {
        let image = self.entries.remove(key)?;
        self.cached_bytes = self
            .cached_bytes
            .and_then(|used| used.checked_sub(checked_bytes(&image)?));
        self.lowest_free = self.lowest_free.min(*key);
        self.forget_number(image.number, *key);
        Some(image)
    }

    fn forget_number(&mut self, number: u32, key: u32) {
        let Some(numbers) = &mut self.numbers else {
            return;
        };
        let remove = match numbers.get(&number) {
            Some(Some(id)) => *id == key,
            Some(None) => !self.entries.values().any(|image| image.number == number),
            None => false,
        };
        if remove {
            numbers.remove(&number);
        }
    }

    pub(super) fn resolve_number(&mut self, number: u32) -> Option<u32> {
        let numbers = self.numbers.get_or_insert_with(|| {
            let mut numbers = HashMap::new();
            for (&key, image) in &self.entries {
                if image.number != 0 {
                    numbers
                        .entry(image.number)
                        .and_modify(|id| *id = None)
                        .or_insert(Some(key));
                }
            }
            numbers
        });
        match numbers.get(&number)? {
            Some(key) => self.entries.get(key).map(|image| image.id),
            None => self
                .entries
                .values()
                .filter(|image| image.number == number)
                .max_by_key(|image| image.generation)
                .map(|image| image.id),
        }
    }
}

impl Clone for Images {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            // Render snapshots do not need the lookup cache.
            numbers: None,
            lowest_free: self.lowest_free,
            cached_bytes: self.cached_bytes,
        }
    }
}

impl Deref for Images {
    type Target = HashMap<u32, Image>;

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl DerefMut for Images {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.numbers = None;
        self.lowest_free = 0;
        self.cached_bytes = None;
        &mut self.entries
    }
}

// Updating a cache must not add overflow panics to public map mutations.
fn checked_bytes(image: &Image) -> Option<usize> {
    image
        .frames
        .iter()
        .try_fold(image.pixels.len(), |used, frame| {
            used.checked_add(frame.pixels.len())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_accounting_tracks_shared_buffers_replacement_and_public_mutation() {
        let mut terminal = crate::Terminal::new(8, 4, 0);
        terminal.feed(b"\x1b_Gi=1,f=32,s=1,v=1;AQID/w==\x1b\\");
        let images = &mut terminal.screen_mut().graphics.images;
        let seed = images[&1].clone();
        assert_eq!(images.cached_bytes_used(), 4);
        let mut second = seed.clone();
        second.id = 2;
        images.insert(2, second.clone());
        assert_eq!(images.cached_bytes_used(), 8);
        second.pixels = vec![0; 8].into();
        second.frames.push(crate::graphics::AnimationFrame {
            pixels: seed.pixels.clone(),
            gap_ms: 1,
        });
        images.insert(2, second);
        assert_eq!(images.cached_bytes_used(), 16);

        images.get_mut(&2).unwrap().pixels = vec![0; 12].into();
        assert_eq!(images.cached_bytes_used(), 20);
        images
            .get_mut(&2)
            .unwrap()
            .frames
            .push(crate::graphics::AnimationFrame {
                pixels: seed.pixels.clone(),
                gap_ms: 1,
            });
        assert_eq!(images.cached_bytes_used(), 24);
        let mut copy = images.clone();
        images.remove(&1);
        assert_eq!(images.cached_bytes_used(), 20);
        assert_eq!(copy.cached_bytes_used(), 24);
        images.insert(2, seed);
        assert_eq!(images.cached_bytes_used(), 4);
        images.remove(&2);
        images.remove(&2);
        assert_eq!(images.cached_bytes_used(), 0);
        copy.retain(|&id, _| id == 1);
        assert_eq!(copy.cached_bytes_used(), 4);
        copy.clear();
        assert_eq!(copy.cached_bytes_used(), 0);
    }

    #[test]
    fn lowest_free_id_follows_insertion_removal_cloning_and_public_mutation() {
        let mut terminal = crate::Terminal::new(8, 4, 0);
        terminal.feed(b"\x1b_GI=7,f=32,s=1,v=1;AQID/w==\x1b\\");
        let images = &mut terminal.screen_mut().graphics.images;
        let seed = images[&1].clone();
        assert_eq!(images.allocate_id(), 2);
        assert_eq!(images.allocate_id(), 2);
        for id in (2..=32).chain([u32::MAX]) {
            let mut image = seed.clone();
            image.id = id;
            images.insert(id, image);
        }
        assert_eq!(images.allocate_id(), 33);
        images.remove(&1);
        assert_eq!(images.allocate_id(), 1);
        images.insert(1, seed.clone());
        assert_eq!(images.allocate_id(), 33);

        let mut copy = images.clone();
        images.remove(&2);
        assert_eq!(images.allocate_id(), 2);
        assert_eq!(copy.allocate_id(), 33);
        copy.retain(|&id, _| id != 3);
        assert_eq!(copy.allocate_id(), 3);
        *copy = HashMap::from([(0, seed.clone()), (1, seed)]);
        assert_eq!(copy.allocate_id(), 2);
        copy.remove(&0);
        assert_eq!(copy.allocate_id(), 2);
        copy.clear();
        assert_eq!(copy.allocate_id(), 1);
        assert_eq!(images.allocate_id(), 2);
    }

    #[test]
    fn numbered_lookup_tracks_replacements_removals_and_public_mutation() {
        let mut terminal = crate::Terminal::new(8, 4, 0);
        terminal.feed(b"\x1b_GI=7,f=32,s=1,v=1;AQID/w==\x1b\\");
        let images = &mut terminal.screen_mut().graphics.images;
        assert_eq!(images.resolve_number(7), Some(1));
        let mut second = images[&1].clone();
        second.id = 20;
        second.number = 8;
        images.insert(2, second.clone());
        assert_eq!(images.resolve_number(8), Some(20));
        second.id = 2;
        images.insert(2, second.clone());
        assert_eq!(images.resolve_number(8), Some(2));
        second.number = 9;
        images.insert(2, second);
        assert_eq!(images.resolve_number(8), None);
        assert_eq!(images.resolve_number(9), Some(2));
        assert!(images.remove(&2).is_some());
        assert!(images.remove(&2).is_none());
        assert_eq!(images.resolve_number(9), None);

        images.get_mut(&1).unwrap().number = 11;
        assert!(images.numbers.is_none());
        assert_eq!(images.resolve_number(7), None);
        assert_eq!(images.resolve_number(11), Some(1));
        let mut duplicate = images[&1].clone();
        duplicate.id = 2;
        duplicate.generation += 1;
        images.insert(2, duplicate);
        assert_eq!(images.resolve_number(11), Some(2));
        images.get_mut(&1).unwrap().generation += 2;
        assert_eq!(images.resolve_number(11), Some(1));
        images.get_mut(&2).unwrap().generation = images[&1].generation;
        let tied = images
            .values()
            .max_by_key(|image| image.generation)
            .unwrap()
            .id;
        assert_eq!(images.resolve_number(11), Some(tied));
        images.remove(&2);
        assert_eq!(images.resolve_number(11), Some(1));
        images.remove(&1);
        assert_eq!(images.resolve_number(11), None);
        assert!(images.numbers.as_ref().unwrap().is_empty());
    }

    #[test]
    fn clones_and_arbitrary_map_mutation_keep_lookups_independent() {
        let mut terminal = crate::Terminal::new(8, 4, 0);
        terminal.feed(b"\x1b_GI=7,f=32,s=1,v=1;AQID/w==\x1b\\");
        let images = &mut terminal.screen_mut().graphics.images;
        assert_eq!(images.resolve_number(7), Some(1));
        let mut copy = images.clone();
        assert!(copy.numbers.is_none());
        let mut image = copy[&1].clone();
        image.number = 8;
        *copy = HashMap::from([(1, image)]);
        assert_eq!(copy.resolve_number(7), None);
        assert_eq!(copy.resolve_number(8), Some(1));
        assert_eq!(images.resolve_number(7), Some(1));
        copy.retain(|_, _| false);
        assert_eq!(copy.resolve_number(8), None);
        copy = images.clone();
        assert_eq!(copy.resolve_number(7), Some(1));
        copy.clear();
        assert_eq!(copy.resolve_number(7), None);
        assert_eq!(images.resolve_number(7), Some(1));
    }
}
