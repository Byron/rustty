use super::{Placement, PlacementId};
use std::{
    collections::{HashMap, HashSet},
    ops::Deref,
    slice,
};

/// Dense rendering storage with keyed updates. Iteration order is unspecified.
#[derive(Debug, Default)]
pub struct Placements {
    entries: Vec<Placement>,
    index: HashMap<(u32, PlacementId), usize>,
    by_image: Option<HashMap<u32, HashSet<PlacementId>>>,
    relatives: usize,
}

impl Placements {
    pub fn get(&self, key: (u32, PlacementId)) -> Option<&Placement> {
        self.index.get(&key).map(|&index| &self.entries[index])
    }

    pub fn insert(&mut self, placement: Placement) -> Option<Placement> {
        let key = (placement.image_id, placement.placement_id);
        self.relatives += usize::from(placement.parent.is_some());
        if let Some(&index) = self.index.get(&key) {
            let previous = std::mem::replace(&mut self.entries[index], placement);
            self.relatives -= usize::from(previous.parent.is_some());
            Some(previous)
        } else {
            self.index.insert(key, self.entries.len());
            if let Some(images) = &mut self.by_image {
                images.entry(key.0).or_default().insert(key.1);
            }
            self.entries.push(placement);
            None
        }
    }

    pub(super) fn remove(&mut self, key: (u32, PlacementId)) -> Option<Placement> {
        let index = self.index.remove(&key)?;
        let removed = self.entries.swap_remove(index);
        self.relatives -= usize::from(removed.parent.is_some());
        if let Some(images) = &mut self.by_image
            && let Some(placements) = images.get_mut(&key.0)
        {
            placements.remove(&key.1);
            if placements.is_empty() {
                images.remove(&key.0);
            }
        }
        if let Some(moved) = self.entries.get(index) {
            *self
                .index
                .get_mut(&(moved.image_id, moved.placement_id))
                .unwrap() = index;
        }
        Some(removed)
    }

    pub(super) fn remove_image(&mut self, image_id: u32) {
        if self.entries.is_empty() {
            return;
        }
        let Some(placements) = self.image_index().remove(&image_id) else {
            return;
        };
        for placement_id in placements {
            self.remove((image_id, placement_id));
        }
    }

    pub(super) fn has_image(&mut self, image_id: u32) -> bool {
        self.image_index().contains_key(&image_id)
    }

    fn image_index(&mut self) -> &mut HashMap<u32, HashSet<PlacementId>> {
        self.by_image.get_or_insert_with(|| {
            let mut images: HashMap<u32, HashSet<PlacementId>> = HashMap::new();
            for placement in &self.entries {
                images
                    .entry(placement.image_id)
                    .or_default()
                    .insert(placement.placement_id);
            }
            images
        })
    }

    pub(super) fn has_relatives(&self) -> bool {
        self.relatives != 0
    }

    pub(super) fn retain(&mut self, mut keep: impl FnMut(&Placement) -> bool) {
        // Swapped-in tail entries have already been tested.
        for index in (0..self.entries.len()).rev() {
            let placement = &self.entries[index];
            if !keep(placement) {
                self.remove((placement.image_id, placement.placement_id));
            }
        }
    }

    pub(super) fn retain_mut(&mut self, keep: impl FnMut(&mut Placement) -> bool) {
        self.entries.retain_mut(keep);
        self.reindex();
    }

    fn reindex(&mut self) {
        self.index.clear();
        self.by_image = None;
        self.relatives = 0;
        for (index, placement) in self.entries.iter().enumerate() {
            self.index
                .insert((placement.image_id, placement.placement_id), index);
            self.relatives += usize::from(placement.parent.is_some());
        }
    }

    pub(super) fn project_rows(&mut self, offsets: &HashMap<u64, i64>) {
        for placement in &mut self.entries {
            placement.viewport_row = Some(offsets.get(&placement.row).copied().unwrap_or(i64::MIN));
        }
    }
}

impl Clone for Placements {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            index: self.index.clone(),
            // Render snapshots do not need image-removal lookups.
            by_image: None,
            relatives: self.relatives,
        }
    }
}

impl Deref for Placements {
    type Target = [Placement];

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl<'a> IntoIterator for &'a Placements {
    type Item = &'a Placement;
    type IntoIter = slice::Iter<'a, Placement>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl FromIterator<Placement> for Placements {
    fn from_iter<T: IntoIterator<Item = Placement>>(iter: T) -> Self {
        let mut placements = Self::default();
        for placement in iter {
            placements.insert(placement);
        }
        placements
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(placements: &Placements) {
        assert_eq!(placements.index.len(), placements.len());
        if let Some(images) = &placements.by_image {
            assert_eq!(
                images.values().map(HashSet::len).sum::<usize>(),
                placements.len()
            );
            assert!(images.values().all(|ids| !ids.is_empty()));
            for placement in placements {
                assert!(images[&placement.image_id].contains(&placement.placement_id));
            }
        }
        assert_eq!(
            placements.relatives,
            placements.iter().filter(|p| p.parent.is_some()).count()
        );
        for placement in placements {
            assert!(std::ptr::eq(
                placements
                    .get((placement.image_id, placement.placement_id))
                    .unwrap(),
                placement,
            ));
        }
    }

    #[test]
    fn keyed_updates_survive_swaps_bulk_removal_and_snapshot_mutations() {
        let mut terminal = crate::Terminal::new(8, 4, 0);
        terminal.feed(b"\x1b_Ga=T,i=1,p=1,C=1,f=32,s=1,v=1,q=2;AQID/w==\x1b\\");
        let seed = terminal.graphics().placements[0].clone();
        let mut placements: Placements = (0..40)
            .map(|n| {
                let mut p = seed.clone();
                p.image_id = 1 + n / 20;
                p.placement_id = if n % 2 == 0 {
                    PlacementId::Internal(n / 2 % 10)
                } else {
                    PlacementId::External(n / 2 % 10)
                };
                p.row = u64::from(n);
                p.parent = (n % 3 == 0).then_some((99, PlacementId::External(1)));
                p
            })
            .collect();
        assert!(placements.has_image(1));
        assert!(!placements.has_image(99));
        check(&placements);
        placements.retain(|_| true);
        check(&placements);

        let removed = (1, PlacementId::External(3));
        assert_eq!(placements.remove(removed).unwrap().row, 7);
        assert!(placements.remove(removed).is_none());
        let moved = (2, PlacementId::External(9));
        let mut replacement = placements.get(moved).unwrap().clone();
        assert_eq!(replacement.row, 39);
        replacement.parent = None;
        assert!(
            placements
                .insert(replacement.clone())
                .unwrap()
                .parent
                .is_some()
        );
        check(&placements);
        assert_eq!(placements.remove(moved).unwrap().row, 39);
        check(&placements);
        placements.insert(replacement);

        let mut snapshot = placements.clone();
        assert!(snapshot.by_image.is_none());
        assert!(snapshot.has_image(2));
        snapshot.project_rows(&HashMap::from([(39, -2)]));
        assert_eq!(snapshot.get(moved).unwrap().viewport_row, Some(-2));
        assert_eq!(placements.get(moved).unwrap().viewport_row, None);
        placements.retain_mut(|p| {
            let keep = p.row % 2 == 0;
            p.row += 100;
            p.image_id += 10;
            keep
        });
        assert!(placements.by_image.is_none());
        assert!(!placements.has_image(1));
        assert!(placements.has_image(11));
        check(&placements);
        check(&snapshot);
        assert_eq!(placements.len(), 20);
        assert_eq!(snapshot.len(), 39);
        assert_eq!(snapshot.get(moved).unwrap().row, 39);
        placements.remove_image(11);
        placements.remove_image(11);
        assert!(!placements.has_image(11));
        assert!(placements.has_image(12));
        assert_eq!(placements.len(), 10);
        check(&placements);

        let before = placements.len();
        let mut visited = std::collections::HashSet::new();
        placements.retain(|p| {
            assert!(visited.insert((p.image_id, p.placement_id)));
            p.parent.is_none()
        });
        assert_eq!(visited.len(), before);
        assert!(!placements.has_relatives());
        let mut relative = seed.clone();
        relative.parent = Some((99, PlacementId::External(1)));
        placements.insert(relative);
        assert!(placements.has_relatives());
        placements.insert(seed.clone());
        assert!(!placements.has_relatives());
        check(&placements);
        placements.retain(|_| false);
        assert!(placements.is_empty());
        assert!(placements.insert(seed).is_none());
        check(&placements);
    }
}
