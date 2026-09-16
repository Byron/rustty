//! Page resource allocation offsets without native backing memory.
#[cfg(feature = "allocation-probe")]
use crate::allocation_probe::{self as probe, Kind, Scope};
use crate::page_layout::{BitmapLayout, SetLayout};
use crate::screen::{Cursor, HyperlinkData, HyperlinkId, Style};
use std::num::NonZeroU32;
use std::sync::Arc;

/// Native PAGE set bookkeeping, shared by snapshot admission and live styles.
/// Dead IDs retain their buckets until admission reclaims them or the page is
/// cloned. In particular, releasing a reference does not rehash the table.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct SetAdmission<T> {
    table: Vec<u16>,
    entries: Vec<SetEntry<T>>,
    capacity: usize,
    max_psl: u8,
    psl_stats: [u16; 32],
    living: usize,
}

impl<T> Default for SetAdmission<T> {
    fn default() -> Self {
        Self {
            table: Vec::new(),
            entries: Vec::new(),
            capacity: 0,
            max_psl: 0,
            psl_stats: [0; 32],
            living: 0,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct SetEntry<T> {
    value: Option<T>,
    references: u16,
    psl: u8,
    bucket: Option<usize>,
}

impl<T> Default for SetEntry<T> {
    fn default() -> Self {
        Self {
            value: None,
            references: 0,
            psl: 0,
            bucket: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetFull {
    NeedsRehash,
    OutOfMemory,
}

pub(crate) type StyleAdmission = SetAdmission<Style>;

impl StyleAdmission {
    pub fn admit(&mut self, value: Style) -> bool {
        self.acquire(value).is_ok()
    }

    pub fn acquire(&mut self, value: Style) -> Result<u16, SetFull> {
        if value == Style::default() {
            Ok(0)
        } else {
            self.add_hashed(value, value.native_hash())
        }
    }

    pub fn acquire_with_id(&mut self, value: Style, id: u16) -> Result<u16, SetFull> {
        if value == Style::default() {
            Ok(0)
        } else {
            self.add_with_id_hashed(value, value.native_hash(), id)
        }
    }
}

impl<T: Eq> SetAdmission<T> {
    pub fn storage_bytes(&self) -> usize {
        self.table.capacity() * size_of::<u16>()
            + self.entries.capacity() * size_of::<SetEntry<T>>()
    }

    pub fn reset(&mut self) {
        self.table.fill(0);
        self.entries.clear();
        self.max_psl = 0;
        self.psl_stats.fill(0);
        self.living = 0;
    }

    pub fn new(layout: SetLayout) -> Self {
        Self {
            table: vec![0; layout.table_cap],
            entries: Vec::new(),
            capacity: layout.cap,
            max_psl: 0,
            psl_stats: [0; 32],
            living: 0,
        }
    }

    pub fn count(&self) -> usize {
        self.living
    }

    #[cfg(test)]
    pub fn reference_count(&self, id: u16) -> u16 {
        self.entries[usize::from(id) - 1].references
    }

    pub fn get(&self, id: u16) -> &T {
        let entry = &self.entries[usize::from(id) - 1];
        assert!(entry.references > 0);
        entry.value.as_ref().unwrap()
    }

    pub fn iter(&self) -> impl Iterator<Item = (u16, &T)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.references > 0)
            .map(|(index, entry)| ((index + 1) as u16, entry.value.as_ref().unwrap()))
    }

    #[cfg(test)]
    pub fn admit_hashed(&mut self, value: T, hash: u64) -> bool {
        self.acquire_hashed(value, hash).is_some()
    }

    #[cfg(test)]
    fn acquire_hashed(&mut self, value: T, hash: u64) -> Option<u16> {
        self.add_hashed(value, hash).ok()
    }

    fn lookup_by(&self, hash: u64, matches: impl Fn(&T) -> bool) -> Option<u16> {
        if self.table.is_empty() {
            return None;
        }
        let mask = self.table.len() - 1;
        for psl in 0..=self.max_psl {
            let id = self.table[(hash as usize).wrapping_add(usize::from(psl)) & mask];
            if id == 0 {
                break;
            }
            let entry = &self.entries[usize::from(id) - 1];
            if entry.psl < psl {
                break;
            }
            if entry.psl == psl
                && entry.references > 0
                && entry.value.as_ref().is_some_and(&matches)
            {
                return Some(id);
            }
        }
        None
    }

    pub fn add_hashed(&mut self, value: T, hash: u64) -> Result<u16, SetFull> {
        self.add_hashed_with(value, hash, &mut |_| {})
    }

    fn add_hashed_with(
        &mut self,
        value: T,
        hash: u64,
        deleted: &mut impl FnMut(T),
    ) -> Result<u16, SetFull> {
        self.add_with(value, hash, None, deleted)
    }

    fn add_with_id_hashed(&mut self, value: T, hash: u64, id: u16) -> Result<u16, SetFull> {
        assert!(id != 0);
        self.add_with(value, hash, Some(id), &mut |_| {})
    }

    fn add_with(
        &mut self,
        value: T,
        hash: u64,
        preferred: Option<u16>,
        deleted: &mut impl FnMut(T),
    ) -> Result<u16, SetFull> {
        let (id, existing) =
            self.prepare_insert(hash, preferred, |entry| entry == &value, deleted)?;
        if existing {
            deleted(value);
            self.retain(id);
            Ok(id)
        } else {
            Ok(self.insert(value, hash, id, deleted))
        }
    }

    /// Select an ID using native cleanup/lookup order, before owning a new payload.
    fn prepare_insert(
        &mut self,
        hash: u64,
        preferred: Option<u16>,
        matches: impl Fn(&T) -> bool,
        deleted: &mut impl FnMut(T),
    ) -> Result<(u16, bool), SetFull> {
        if let Some(id) = preferred.filter(|&id| id != 0 && usize::from(id) <= self.entries.len()) {
            let entry = &self.entries[usize::from(id) - 1];
            if entry.references == 0 {
                if let Some(existing) = self.lookup_by(hash, &matches) {
                    return Ok((existing, true));
                }
                if self.psl_stats[31] != 0 {
                    return Err(SetFull::OutOfMemory);
                }
                if let Some(value) = self.delete_item(id) {
                    deleted(value);
                }
                return Ok((id, false));
            } else if entry.value.as_ref().is_some_and(&matches) {
                return Ok((id, true));
            }
        }
        while self
            .entries
            .last()
            .is_some_and(|entry| entry.references == 0)
        {
            if let Some(value) = self.delete_item(self.entries.len() as u16) {
                deleted(value);
            }
            self.entries.pop();
        }
        if let Some(id) = self.lookup_by(hash, matches) {
            return Ok((id, true));
        }
        if self.psl_stats[31] != 0 {
            return Err(SetFull::OutOfMemory);
        }
        if self.entries.len() + 1 >= self.capacity {
            return Err(if self.living < (self.capacity as f64 * 0.9) as usize {
                SetFull::NeedsRehash
            } else {
                SetFull::OutOfMemory
            });
        }
        Ok(((self.entries.len() + 1) as u16, false))
    }

    pub fn reserve_entries(&mut self, count: usize) {
        self.entries.reserve(count);
    }

    fn insert(&mut self, value: T, hash: u64, new_id: u16, deleted: &mut impl FnMut(T)) -> u16 {
        let appended = usize::from(new_id) > self.entries.len();
        if appended {
            self.entries.push(SetEntry::default());
        }
        self.entries[usize::from(new_id) - 1] = SetEntry {
            value: Some(value),
            references: 0,
            psl: 0,
            bucket: None,
        };
        let mut held_id = new_id;
        let mut chosen_id = new_id;
        let mask = self.table.len() - 1;
        for distance in 0..self.table.len() - 1 {
            let bucket = (hash as usize).wrapping_add(distance) & mask;
            let id = self.table[bucket];
            let held_psl = self.entries[usize::from(held_id) - 1].psl;
            if id == 0 || self.entries[usize::from(id) - 1].references == 0 {
                if id != 0 {
                    let dead = &mut self.entries[usize::from(id) - 1];
                    if let Some(value) = dead.value.take() {
                        deleted(value);
                    }
                    self.psl_stats[usize::from(dead.psl)] -= 1;
                    *dead = SetEntry::default();
                    if id < new_id {
                        chosen_id = id;
                    }
                }
                self.table[bucket] = held_id;
                self.entries[usize::from(held_id) - 1].bucket = Some(bucket);
                self.psl_stats[usize::from(held_psl)] += 1;
                self.max_psl = self.max_psl.max(held_psl);
                break;
            }
            let held_refs = self.entries[usize::from(held_id) - 1].references;
            let resident = &self.entries[usize::from(id) - 1];
            if resident.psl < held_psl
                || (resident.psl == held_psl && resident.references < held_refs)
            {
                self.psl_stats[usize::from(resident.psl)] -= 1;
                self.table[bucket] = held_id;
                self.entries[usize::from(held_id) - 1].bucket = Some(bucket);
                self.psl_stats[usize::from(held_psl)] += 1;
                self.max_psl = self.max_psl.max(held_psl);
                held_id = id;
            }
            self.entries[usize::from(held_id) - 1].psl += 1;
        }
        if chosen_id != new_id {
            let entry = std::mem::take(&mut self.entries[usize::from(new_id) - 1]);
            self.table[entry.bucket.unwrap()] = chosen_id;
            self.entries[usize::from(chosen_id) - 1] = entry;
            if appended {
                self.entries.pop();
            }
        }
        self.entries[usize::from(chosen_id) - 1].references = 1;
        self.living += 1;
        chosen_id
    }

    pub fn retain(&mut self, id: u16) {
        self.retain_many(id, 1);
    }

    pub fn retain_many(&mut self, id: u16, count: u16) {
        if id == 0 {
            return;
        }
        let entry = &mut self.entries[usize::from(id) - 1];
        assert!(entry.references > 0);
        entry.references = entry
            .references
            .checked_add(count)
            .expect("native page resource reference overflow");
    }

    pub fn release(&mut self, id: u16) {
        self.release_many(id, 1);
    }

    pub fn release_many(&mut self, id: u16, count: u16) {
        if id == 0 {
            return;
        }
        let entry = &mut self.entries[usize::from(id) - 1];
        assert!(entry.references > 0);
        entry.references = entry
            .references
            .checked_sub(count)
            .expect("resource reference underflow");
        if entry.references == 0 {
            self.living -= 1;
        }
    }

    fn delete_item(&mut self, id: u16) -> Option<T> {
        let entry = std::mem::take(&mut self.entries[usize::from(id) - 1]);
        let Some(mut hole) = entry.bucket else {
            return entry.value;
        };
        self.psl_stats[usize::from(entry.psl)] -= 1;
        let mask = self.table.len() - 1;
        let mut next = (hole + 1) & mask;
        while self.table[next] != 0 {
            let moved = &mut self.entries[usize::from(self.table[next]) - 1];
            if moved.psl == 0 {
                break;
            }
            self.psl_stats[usize::from(moved.psl)] -= 1;
            moved.psl -= 1;
            moved.bucket = Some(hole);
            self.psl_stats[usize::from(moved.psl)] += 1;
            self.table[hole] = self.table[next];
            hole = next;
            next = (next + 1) & mask;
        }
        self.table[hole] = 0;
        while self.max_psl > 0 && self.psl_stats[usize::from(self.max_psl)] == 0 {
            self.max_psl -= 1;
        }
        entry.value
    }

    #[cfg(test)]
    fn pop_unused(&mut self) -> Option<T> {
        while self
            .entries
            .last()
            .is_some_and(|entry| entry.references == 0)
        {
            let value = self.delete_item(self.entries.len() as u16);
            self.entries.pop();
            if value.is_some() {
                return value;
            }
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Hyperlink {
    pub id: HyperlinkId,
    pub uri: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HyperlinkIdRef<'a> {
    Implicit(u32),
    Explicit(&'a [u8]),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct HyperlinkKey<'a> {
    id: HyperlinkIdRef<'a>,
    pub uri: &'a [u8],
}

impl<'a> HyperlinkKey<'a> {
    pub fn new(uri: &'a [u8], explicit: Option<&'a [u8]>, implicit: u32) -> Self {
        Self {
            id: explicit.map_or(HyperlinkIdRef::Implicit(implicit), HyperlinkIdRef::Explicit),
            uri,
        }
    }

    pub fn from_data(link: &'a HyperlinkData) -> Self {
        Self::from_id(
            link.uri_bytes(),
            link.id.as_ref().unwrap_or(&HyperlinkId::Implicit(0)),
        )
    }

    fn from_id(uri: &'a [u8], id: &'a HyperlinkId) -> Self {
        match id {
            HyperlinkId::Explicit(id) => Self::new(uri, Some(id), 0),
            HyperlinkId::Implicit(id) => Self::new(uri, None, *id),
        }
    }

    fn owned(self) -> Hyperlink {
        Hyperlink {
            id: match self.id {
                HyperlinkIdRef::Implicit(id) => HyperlinkId::Implicit(id),
                HyperlinkIdRef::Explicit(id) => HyperlinkId::Explicit(id.to_vec()),
            },
            uri: self.uri.to_vec(),
        }
    }
}

impl Hyperlink {
    pub fn key(&self) -> HyperlinkKey<'_> {
        HyperlinkKey::from_id(&self.uri, &self.id)
    }

    pub fn from_data(link: &HyperlinkData) -> Self {
        #[cfg(feature = "allocation-probe")]
        let _scope = Scope::enter(Kind::TemporaryPayload);
        Self {
            id: link.id.clone().unwrap_or(HyperlinkId::Implicit(0)),
            uri: link.uri_bytes().to_vec(),
        }
    }

    pub fn from_cursor(cursor: &Cursor) -> Option<Self> {
        #[cfg(feature = "allocation-probe")]
        let _scope = Scope::enter(Kind::TemporaryPayload);
        cursor.hyperlink.as_deref().map(Self::from_data)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HyperlinkFull {
    Strings,
    Set(SetFull),
    Map,
}

/// Native hyperlink references, cell-map capacity and retained string runs.
/// Dead set entries keep their strings until insertion actually reaps them.
#[derive(Clone, Debug, Default)]
pub(crate) struct HyperlinkAdmission {
    set: SetAdmission<HyperlinkEntry>,
    strings: BitmapAllocator<32>,
    cells: usize,
    capacity: usize,
    payload_bytes: usize,
}

#[derive(Clone, Debug)]
struct HyperlinkEntry {
    link: Arc<Hyperlink>,
    data: Arc<HyperlinkData>,
    id_allocation: Option<(usize, usize)>,
    uri_allocation: (usize, usize),
}

impl PartialEq for HyperlinkEntry {
    fn eq(&self, other: &Self) -> bool {
        self.link == other.link
    }
}

impl Eq for HyperlinkEntry {}

impl HyperlinkEntry {
    fn payload_bytes(&self) -> usize {
        size_of::<Hyperlink>()
            + 2 * size_of::<usize>()
            + self.link.uri.capacity()
            + match &self.link.id {
                HyperlinkId::Explicit(id) => id.capacity(),
                _ => 0,
            }
            + self.data.storage_bytes()
    }
}

impl HyperlinkAdmission {
    pub fn new(set: SetLayout, strings: BitmapLayout, capacity: usize) -> Self {
        Self {
            set: SetAdmission::new(set),
            strings: BitmapAllocator::new(strings),
            cells: 0,
            capacity,
            payload_bytes: 0,
        }
    }

    pub fn storage_bytes(&self) -> usize {
        self.set.storage_bytes() + self.strings.storage_bytes() + self.payload_bytes
    }

    pub fn reset(&mut self) {
        self.set.reset();
        self.strings.reset();
        self.cells = 0;
        self.payload_bytes = 0;
    }

    pub fn data(&self, id: u16) -> &Arc<HyperlinkData> {
        &self.set.get(id).data
    }

    pub fn get(&self, id: u16) -> &Hyperlink {
        &self.set.get(id).link
    }

    pub fn iter(&self) -> impl Iterator<Item = (u16, &Hyperlink)> {
        self.set.iter().map(|(id, entry)| (id, entry.link.as_ref()))
    }

    pub fn release(&mut self, id: u16) {
        self.set.release(id);
    }

    pub fn retain_cell(&mut self, id: u16) -> Result<(), HyperlinkFull> {
        if id != 0 {
            if self.cells == self.capacity {
                return Err(HyperlinkFull::Map);
            }
            self.set.retain(id);
            self.cells += 1;
        }
        Ok(())
    }

    pub fn release_cell(&mut self, id: u16) {
        if id != 0 {
            self.set.release(id);
            self.cells -= 1;
        }
    }

    /// Cursor insertion reserves URI then ID, even for an existing value.
    pub fn insert(&mut self, link: HyperlinkKey<'_>) -> Result<u16, HyperlinkFull> {
        #[cfg(feature = "allocation-probe")]
        probe::event(|c| c.hyperlink_admissions += 1);
        self.allocate(link, false, None, None)
    }

    /// PAGE LINK records reserve ID before URI. Zero/duplicate wire IDs still
    /// allocate a temporary set reference before the decoder releases it.
    pub fn decode(&mut self, link: &Hyperlink, retain: bool) -> Result<u16, HyperlinkFull> {
        #[cfg(feature = "allocation-probe")]
        probe::event(|c| c.hyperlink_admissions += 1);
        let id = self.allocate(link.key(), true, None, None)?;
        if !retain {
            self.release(id);
        }
        Ok(id)
    }

    /// Page copies check the cell map, then reuse a live value before trying
    /// any string allocation. A new value prefers its source page's ID.
    pub fn copy_cell(
        &mut self,
        link: HyperlinkKey<'_>,
        preferred: u16,
    ) -> Result<u16, HyperlinkFull> {
        self.copy_value(link, preferred, None)
    }

    pub fn copy_from(&mut self, source: &Self, id: u16) -> Result<u16, HyperlinkFull> {
        let entry = source.set.get(id);
        self.copy_value(entry.link.key(), id, Some(entry))
    }

    fn copy_value(
        &mut self,
        link: HyperlinkKey<'_>,
        preferred: u16,
        shared: Option<&HyperlinkEntry>,
    ) -> Result<u16, HyperlinkFull> {
        #[cfg(feature = "allocation-probe")]
        probe::event(|c| c.hyperlink_admissions += 1);
        if self.cells == self.capacity {
            return Err(HyperlinkFull::Map);
        }
        let hash = hyperlink_hash(link);
        let id = if let Some(id) = self.set.lookup_by(hash, |entry| entry.link.key() == link) {
            self.set.retain(id);
            id
        } else {
            self.allocate(link, false, Some(preferred), shared)?
        };
        self.cells += 1;
        Ok(id)
    }

    pub fn reserve_entries(&mut self, source: &Self) {
        self.set.reserve_entries(source.set.count());
    }

    /// Native reflow duplicates the strings before checking the set, unlike
    /// ordinary page copies. The temporary copy can itself require growth.
    pub fn reflow_cell(
        &mut self,
        link: HyperlinkKey<'_>,
        preferred: u16,
    ) -> Result<u16, HyperlinkFull> {
        #[cfg(feature = "allocation-probe")]
        probe::event(|c| c.hyperlink_admissions += 1);
        if self.cells == self.capacity {
            return Err(HyperlinkFull::Map);
        }
        let id = self.allocate(link, false, Some(preferred), None)?;
        self.cells += 1;
        Ok(id)
    }

    /// Native cursor-map growth probes an extra URI copy before rebuilding.
    pub fn reserve_uri(&mut self, length: usize) -> bool {
        self.strings.alloc(length).is_some()
    }

    fn allocate(
        &mut self,
        link: HyperlinkKey<'_>,
        id_first: bool,
        preferred: Option<u16>,
        shared: Option<&HyperlinkEntry>,
    ) -> Result<u16, HyperlinkFull> {
        #[cfg(feature = "allocation-probe")]
        probe::event(|c| c.string_reservations += 1);
        if link.uri.is_empty() || matches!(link.id, HyperlinkIdRef::Explicit(id) if id.is_empty()) {
            return Err(HyperlinkFull::Strings);
        }
        let mut id_allocation = None;
        let mut uri_allocation = None;
        for is_id in [id_first, !id_first] {
            let value = if is_id {
                let HyperlinkIdRef::Explicit(id) = link.id else {
                    continue;
                };
                id
            } else {
                link.uri
            };
            let Some(offset) = self.strings.alloc(value.len()) else {
                for (offset, length) in id_allocation.into_iter().chain(uri_allocation) {
                    self.strings.free(offset, length);
                }
                return Err(HyperlinkFull::Strings);
            };
            if is_id {
                id_allocation = Some((offset, value.len()));
            } else {
                uri_allocation = Some((offset, value.len()));
            }
        }
        let uri_allocation = uri_allocation.unwrap();
        let hash = hyperlink_hash(link);
        let strings = &mut self.strings;
        let payload_bytes = &mut self.payload_bytes;
        let mut deleted = |entry: HyperlinkEntry| {
            *payload_bytes -= entry.payload_bytes();
            Self::free_strings(strings, entry.id_allocation, entry.uri_allocation);
        };
        let prepared = self.set.prepare_insert(
            hash,
            preferred,
            |entry| entry.link.key() == link,
            &mut deleted,
        );
        let (id, existing) = match prepared {
            Ok(value) => value,
            Err(error) => {
                Self::free_strings(&mut self.strings, id_allocation, uri_allocation);
                return Err(HyperlinkFull::Set(error));
            }
        };
        if existing {
            Self::free_strings(&mut self.strings, id_allocation, uri_allocation);
            self.set.retain(id);
            return Ok(id);
        }
        let (owned, data) = if let Some(shared) = shared {
            (shared.link.clone(), shared.data.clone())
        } else {
            #[cfg(feature = "allocation-probe")]
            let _scope = Scope::enter(Kind::OwnedPayload);
            let owned = Arc::new(link.owned());
            let data = Arc::new(HyperlinkData::new(link.uri, Some(owned.id.clone())));
            (owned, data)
        };
        let entry = HyperlinkEntry {
            link: owned,
            data,
            id_allocation,
            uri_allocation,
        };
        self.payload_bytes += entry.payload_bytes();
        let payload_bytes = &mut self.payload_bytes;
        let strings = &mut self.strings;
        Ok(self
            .set
            .insert(entry, hash, id, &mut |entry: HyperlinkEntry| {
                *payload_bytes -= entry.payload_bytes();
                Self::free_strings(strings, entry.id_allocation, entry.uri_allocation);
            }))
    }

    fn free_strings(
        strings: &mut BitmapAllocator<32>,
        id: Option<(usize, usize)>,
        uri: (usize, usize),
    ) {
        if let Some((offset, len)) = id {
            strings.free(offset, len);
        }
        strings.free(uri.0, uri.1);
    }

    #[cfg(test)]
    fn admit(&mut self, id: &HyperlinkId, uri: &[u8], retain: bool) -> bool {
        self.decode(
            &Hyperlink {
                id: id.clone(),
                uri: uri.to_vec(),
            },
            retain,
        )
        .is_ok()
    }

    #[cfg(test)]
    pub fn assert_references(&self, cells: impl Iterator<Item = u16>, cursor: Option<u16>) {
        let mut counts = std::collections::HashMap::<u16, usize>::new();
        let mut cell_count = 0;
        for id in cells.filter(|&id| id != 0) {
            *counts.entry(id).or_default() += 1;
            cell_count += 1;
        }
        assert_eq!(self.cells, cell_count);
        assert!(self.cells <= self.capacity);
        if let Some(id) = cursor {
            *counts.entry(id).or_default() += 1;
        }
        for (id, _) in self.set.iter() {
            assert_eq!(
                usize::from(self.set.reference_count(id)),
                counts.remove(&id).unwrap_or(0)
            );
        }
        assert!(counts.is_empty(), "cells referenced a dead hyperlink");

        let mut runs: Vec<_> = self
            .set
            .entries
            .iter()
            .filter_map(|entry| entry.value.as_ref())
            .flat_map(|entry| {
                entry
                    .id_allocation
                    .into_iter()
                    .chain(std::iter::once(entry.uri_allocation))
            })
            .collect();
        runs.sort_unstable();
        let mut strings = self.strings.clone();
        let mut end = strings.chunks_start;
        for (offset, length) in runs {
            assert!(offset >= end, "overlapping hyperlink strings");
            end = offset + BitmapAllocator::<32>::bytes_required(length).unwrap();
            strings.free(offset, length);
        }
        assert_eq!(strings.used_bytes(), 0, "unowned hyperlink strings");
    }
}

/// A cell's unique suffix allocation. Moves keep it; copies allocate a new run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GraphemeAllocation {
    offset: NonZeroU32,
    pub len: u8,
}

/// Native grapheme admission and shared text for the cells that own each run.
#[derive(Clone, Debug, Default)]
pub(crate) struct GraphemeAdmission {
    allocator: BitmapAllocator<16>,
    count: usize,
    capacity: usize,
    // Only allocation starts own text; continuation chunks stay empty. Grow
    // lazily so scalar-only pages and admission probes allocate no text slots.
    texts: Vec<Option<Arc<str>>>,
    payload_bytes: usize,
}

impl GraphemeAdmission {
    pub fn new(layout: BitmapLayout, capacity: usize) -> Self {
        Self {
            allocator: BitmapAllocator::new(layout),
            count: 0,
            capacity,
            texts: Vec::new(),
            payload_bytes: 0,
        }
    }

    pub fn used_bytes(&self) -> usize {
        self.allocator.used_bytes()
    }

    pub fn storage_bytes(&self) -> usize {
        self.allocator.storage_bytes()
            + self.texts.capacity() * size_of::<Option<Arc<str>>>()
            + self.payload_bytes
    }

    pub fn reset(&mut self) {
        self.allocator.reset();
        self.texts.clear();
        self.count = 0;
        self.payload_bytes = 0;
    }

    pub fn text(&self, allocation: GraphemeAllocation) -> &str {
        self.texts[self.text_index(allocation)]
            .as_deref()
            .expect("grapheme allocation has text")
    }

    pub fn text_arc(&self, allocation: GraphemeAllocation) -> Arc<str> {
        self.texts[self.text_index(allocation)]
            .as_ref()
            .expect("grapheme allocation has text")
            .clone()
    }

    fn text_index(&self, allocation: GraphemeAllocation) -> usize {
        (allocation.offset.get() as usize - self.allocator.chunks_start) / 16
    }

    fn text_slot(&mut self, allocation: GraphemeAllocation) -> &mut Option<Arc<str>> {
        let index = self.text_index(allocation);
        if self.texts.len() <= index {
            self.texts.resize_with(index + 1, || None);
        }
        &mut self.texts[index]
    }

    pub fn set_text(&mut self, allocation: GraphemeAllocation, text: Arc<str>) {
        debug_assert_eq!(text.chars().count(), usize::from(allocation.len) + 1);
        self.payload_bytes += text.len() + 2 * size_of::<usize>();
        if let Some(previous) = self.text_slot(allocation).replace(text) {
            self.payload_bytes -= previous.len() + 2 * size_of::<usize>();
        }
    }

    pub fn acquire(&mut self, len: u8) -> Result<GraphemeAllocation, SetFull> {
        #[cfg(feature = "allocation-probe")]
        probe::event(|c| c.grapheme_admissions += 1);
        assert!((1..=64).contains(&len));
        let offset = self
            .allocator
            .alloc(usize::from(len) * 4)
            .ok_or(SetFull::OutOfMemory)?;
        if self.count == self.capacity {
            self.allocator.free(offset, usize::from(len) * 4);
            return Err(SetFull::OutOfMemory);
        }
        self.count += 1;
        Ok(GraphemeAllocation {
            offset: NonZeroU32::new(offset.try_into().unwrap()).unwrap(),
            len,
        })
    }

    pub fn append(
        &mut self,
        previous: Option<GraphemeAllocation>,
    ) -> Result<GraphemeAllocation, SetFull> {
        #[cfg(feature = "allocation-probe")]
        probe::event(|c| c.grapheme_appends += 1);
        let Some(mut previous) = previous else {
            return self.acquire(1);
        };
        assert!(previous.len < 64);
        if previous.len % 4 == 0 {
            // Native allocates the replacement before releasing the old run.
            // The existing map entry is reused even when the map is full.
            let offset = self
                .allocator
                .alloc(usize::from(previous.len + 1) * 4)
                .ok_or(SetFull::OutOfMemory)?;
            self.allocator.free(
                previous.offset.get() as usize,
                usize::from(previous.len) * 4,
            );
            let old_index = self.text_index(previous);
            previous.offset = NonZeroU32::new(offset.try_into().unwrap()).unwrap();
            if let Some(text) = self.texts.get_mut(old_index).and_then(Option::take) {
                assert!(self.text_slot(previous).replace(text).is_none());
            }
        }
        previous.len += 1;
        Ok(previous)
    }

    pub fn release(&mut self, allocation: GraphemeAllocation) {
        self.allocator.free(
            allocation.offset.get() as usize,
            usize::from(allocation.len) * 4,
        );
        self.count -= 1;
        let index = self.text_index(allocation);
        if let Some(text) = self.texts.get_mut(index).and_then(Option::take) {
            self.payload_bytes -= text.len() + 2 * size_of::<usize>();
        }
    }

    #[cfg(test)]
    pub fn assert_allocations(&self, allocations: impl Iterator<Item = GraphemeAllocation>) {
        let mut allocations: Vec<_> = allocations.collect();
        allocations.sort_unstable_by_key(|allocation| allocation.offset);
        assert_eq!(allocations.len(), self.count);
        let mut remaining = self.clone();
        let mut end = self.allocator.chunks_start;
        for allocation in allocations {
            assert!(
                allocation.offset.get() as usize >= end,
                "overlapping grapheme allocations"
            );
            end = allocation.offset.get() as usize
                + BitmapAllocator::<16>::bytes_required(usize::from(allocation.len) * 4).unwrap();
            remaining.release(allocation);
        }
        assert_eq!(remaining.used_bytes(), 0, "unowned grapheme allocations");
        assert!(
            remaining.texts.iter().all(Option::is_none),
            "unowned grapheme text"
        );
    }
}

/// Bookkeeping for `terminal/bitmap_allocator.zig`.
///
/// Offsets are relative to this allocator's region, including its bitmap header.
/// The page ledger adds the region's page offset. Callers pass byte lengths;
/// grapheme codepoints occupy four native bytes each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BitmapAllocator<const CHUNK: usize> {
    bitmaps: Vec<u64>,
    chunks_start: usize,
    search_start: usize,
}

impl<const CHUNK: usize> BitmapAllocator<CHUNK> {
    fn storage_bytes(&self) -> usize {
        self.bitmaps.capacity() * size_of::<u64>()
    }

    fn reset(&mut self) {
        self.bitmaps.fill(0);
        self.search_start = 0;
    }

    pub fn new(layout: BitmapLayout) -> Self {
        assert!(CHUNK.is_power_of_two());
        let capacity = layout
            .bitmap_count
            .checked_mul(64)
            .and_then(|chunks| chunks.checked_mul(CHUNK))
            .expect("bitmap capacity overflow");
        assert_eq!(
            layout.total_size,
            layout.chunks_start.checked_add(capacity).unwrap(),
            "bitmap layout must reserve every addressable chunk"
        );
        Self {
            bitmaps: vec![0; layout.bitmap_count],
            chunks_start: layout.chunks_start,
            search_start: 0,
        }
    }

    pub fn bytes_required(byte_len: usize) -> Option<usize> {
        assert!(CHUNK.is_power_of_two());
        byte_len.checked_add(CHUNK - 1).map(|n| n & !(CHUNK - 1))
    }

    /// Reserve a native-sized run, returning None for zero length or exhaustion.
    pub fn alloc(&mut self, byte_len: usize) -> Option<usize> {
        let chunks = Self::bytes_required(byte_len)? / CHUNK;
        if chunks == 0 {
            return None;
        }
        let start = self.search_start.min(self.bitmaps.len());
        let chunk = start * 64 + find_free_chunks(&self.bitmaps[start..], chunks)?;
        self.set_bits(chunk, chunks, true);
        while self.search_start < self.bitmaps.len() && self.bitmaps[self.search_start] == u64::MAX
        {
            self.search_start += 1;
        }
        Some(self.chunks_start + chunk * CHUNK)
    }

    /// Release a live allocation using its original offset and byte length.
    pub fn free(&mut self, offset: usize, byte_len: usize) {
        let start = offset
            .checked_sub(self.chunks_start)
            .expect("resource offset precedes the chunks region");
        assert_eq!(start % CHUNK, 0, "resource offset must start a chunk");
        let len = Self::bytes_required(byte_len).expect("resource length overflow");
        assert!(len > 0, "resource allocation must not be empty");
        assert!(
            start
                .checked_add(len)
                .is_some_and(|end| end <= self.capacity_bytes()),
            "resource allocation extends beyond the chunks region"
        );
        let chunk = start / CHUNK;
        self.set_bits(chunk, len / CHUNK, false);
        self.search_start = self.search_start.min(chunk / 64);
    }

    pub fn capacity_bytes(&self) -> usize {
        self.bitmaps.len() * 64 * CHUNK
    }

    fn used_bytes(&self) -> usize {
        self.bitmaps
            .iter()
            .map(|bits| bits.count_ones() as usize)
            .sum::<usize>()
            * CHUNK
    }

    fn set_bits(&mut self, mut chunk: usize, mut count: usize, used: bool) {
        while count > 0 {
            let bit = chunk % 64;
            let bits = count.min(64 - bit);
            let mask = (u64::MAX >> (64 - bits)) << bit;
            if used {
                self.bitmaps[chunk / 64] |= mask;
            } else {
                self.bitmaps[chunk / 64] &= !mask;
            }
            chunk += bits;
            count -= bits;
        }
    }
}

fn find_free_chunks(bitmaps: &[u64], count: usize) -> Option<usize> {
    if count <= 64 {
        // Native small allocations stay within one bitmap word, even when
        // neighboring partial words contain a sufficiently large free span.
        for (index, &bitmap) in bitmaps.iter().enumerate() {
            let free = !bitmap;
            let mut starts = free;
            for shift in 1..count {
                starts &= free >> shift;
            }
            if starts != 0 {
                return Some(index * 64 + starts.trailing_zeros() as usize);
            }
        }
        return None;
    }

    let mut index = 0;
    'search: while index < bitmaps.len() {
        let prefix = bitmaps[index].leading_zeros() as usize;
        if prefix == 0 {
            index += 1;
            continue;
        }
        let first = index * 64 + 64 - prefix;
        let mut remaining = count - prefix;
        index += 1;
        while remaining > 64 {
            if *bitmaps.get(index)? != 0 {
                continue 'search;
            }
            remaining -= 64;
            index += 1;
        }
        // Native assumes the final word exists; exhaustion must remain safe
        // here even when the only available prefix is in the last word.
        if bitmaps.get(index)?.trailing_zeros() as usize >= remaining {
            return Some(first);
        }
    }
    None
}

/// The native page hyperlink hash includes ID kind, raw strings and their
/// machine-sized lengths. Keep that representation for resource admission.
fn hyperlink_hash(link: HyperlinkKey<'_>) -> u64 {
    let uri = link.uri;
    // Hash the native byte sequence without constructing a temporary key buffer.
    let id_len;
    let implicit;
    let (kind, id, length): (&[u8], &[u8], &[u8]) = match link.id {
        HyperlinkIdRef::Explicit(id) => {
            id_len = id.len().to_le_bytes();
            (&[0], id, &id_len)
        }
        HyperlinkIdRef::Implicit(id) => {
            implicit = id.to_le_bytes();
            (&[1], &implicit, &[])
        }
    };
    let uri_len = uri.len().to_le_bytes();
    let parts = [kind, id, length, uri, &uri_len];
    let len = parts.iter().map(|part| part.len()).sum();
    wyhash_with(len, |mut offset, count| {
        let mut bytes = [0; 8];
        let mut copied = 0;
        for part in parts {
            if offset >= part.len() {
                offset -= part.len();
                continue;
            }
            let take = (count - copied).min(part.len() - offset);
            bytes[copied..copied + take].copy_from_slice(&part[offset..offset + take]);
            copied += take;
            if copied == count {
                break;
            }
            offset = 0;
        }
        debug_assert_eq!(copied, count);
        u64::from_le_bytes(bytes)
    })
}

// Zig std.hash.Wyhash's one-shot path with seed zero. Reuse it for the native
// admission hash; Rust's randomized HashMap hash would change collision limits.
#[cfg(test)]
fn wyhash(bytes: &[u8]) -> u64 {
    wyhash_with(bytes.len(), |offset, count| {
        let mut word = [0; 8];
        word[..count].copy_from_slice(&bytes[offset..offset + count]);
        u64::from_le_bytes(word)
    })
}

fn wyhash_with(len: usize, read: impl Fn(usize, usize) -> u64) -> u64 {
    const SECRET: [u64; 4] = [
        0xa0761d6478bd642f,
        0xe7037ed1a0b428db,
        0x8ebc6af09c88c6e3,
        0x589965cc75374cc3,
    ];
    fn mix(a: u64, b: u64) -> u64 {
        let product = u128::from(a) * u128::from(b);
        product as u64 ^ (product >> 64) as u64
    }
    let mut state = [mix(SECRET[0], SECRET[1]); 3];
    let (mut a, mut b) = if len <= 16 {
        if len >= 4 {
            let end = len - 4;
            let quarter = (len >> 3) << 2;
            (
                (read(0, 4) << 32) | read(quarter, 4),
                (read(end, 4) << 32) | read(end - quarter, 4),
            )
        } else if len != 0 {
            (
                (read(0, 1) << 16) | (read(len >> 1, 1) << 8) | read(len - 1, 1),
                0,
            )
        } else {
            (0, 0)
        }
    } else {
        let mut offset = 0;
        if len >= 48 {
            while offset + 48 < len {
                for i in 0..3 {
                    let chunk = offset + 16 * i;
                    state[i] = mix(
                        read(chunk, 8) ^ SECRET[i + 1],
                        read(chunk + 8, 8) ^ state[i],
                    );
                }
                offset += 48;
            }
            state[0] ^= state[1] ^ state[2];
        }
        while offset + 16 < len {
            state[0] = mix(read(offset, 8) ^ SECRET[1], read(offset + 8, 8) ^ state[0]);
            offset += 16;
        }
        (read(len - 16, 8), read(len - 8, 8))
    };
    a ^= SECRET[1];
    b ^= state[0];
    let product = u128::from(a) * u128::from(b);
    mix(
        product as u64 ^ SECRET[0] ^ len as u64,
        (product >> 64) as u64 ^ SECRET[1],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page_layout::PageCapacity;
    use crate::screen::Color;

    fn hyperlink_admission() -> HyperlinkAdmission {
        let layout = PageCapacity::STANDARD.metadata().unwrap();
        HyperlinkAdmission::new(
            layout.hyperlink_set_layout,
            layout.string_alloc_layout,
            layout.hyperlink_map_layout.capacity as usize * 80 / 100,
        )
    }

    #[test]
    fn discarded_hyperlink_reserves_incoming_strings_before_reclaim() {
        use crate::screen::HyperlinkId;
        let mut links = hyperlink_admission();
        assert!(links.admit(&HyperlinkId::Implicit(1), &vec![b'x'; 2048], false));
        assert!(!links.admit(&HyperlinkId::Implicit(2), b"x", true));
        assert_eq!(links.strings.used_bytes(), 2048);
        assert_eq!(links.set.entries[0].references, 0);

        let mut links = hyperlink_admission();
        assert!(links.admit(&HyperlinkId::Implicit(1), &vec![b'x'; 1984], false));
        assert!(links.admit(&HyperlinkId::Explicit(b"id".to_vec()), b"x", true));
        assert_eq!(links.strings.used_bytes(), 64);
        assert_eq!(links.set.entries.len(), 1);
        assert_eq!(links.set.entries[0].references, 1);
    }

    #[test]
    fn hyperlink_deduplication_and_failed_uri_free_temporary_strings() {
        use crate::screen::HyperlinkId;
        let mut links = hyperlink_admission();
        let uri = vec![b'x'; 2016];
        assert!(links.admit(&HyperlinkId::Implicit(1), &uri, true));
        assert!(!links.admit(&HyperlinkId::Implicit(1), &uri, true));
        assert_eq!(links.set.entries[0].references, 1);

        let mut links = hyperlink_admission();
        let uri = vec![b'x'; 992];
        assert!(links.admit(&HyperlinkId::Implicit(1), &uri, true));
        assert!(links.admit(&HyperlinkId::Implicit(1), &uri, true));
        assert_eq!(links.strings.used_bytes(), 992);
        assert_eq!(links.set.entries[0].references, 2);

        let mut links = hyperlink_admission();
        assert!(links.admit(&HyperlinkId::Implicit(1), &vec![b'x'; 1984], true));
        assert!(!links.admit(&HyperlinkId::Explicit(b"id".to_vec()), &[b'x'; 65], true));
        assert!(links.admit(&HyperlinkId::Implicit(2), &[b'x'; 64], true));
        assert_eq!(links.strings.used_bytes(), 2048);
    }

    #[test]
    fn discarded_set_tail_restores_probe_chain_and_collision_capacity() {
        let layout = PageCapacity {
            styles: 64,
            ..PageCapacity::STANDARD
        }
        .metadata()
        .unwrap()
        .styles_layout;
        for bucket in [0, 63] {
            let mut set = SetAdmission::new(layout);
            assert_eq!(set.acquire_hashed(1, bucket), Some(1));
            assert_eq!(set.acquire_hashed(2, (bucket + 1) & 63), Some(2));
            assert_eq!(set.acquire_hashed(3, bucket), Some(3));
            set.release(3);
            assert_eq!(set.pop_unused(), Some(3));
            assert_eq!(set.table[bucket as usize], 1);
            assert_eq!(set.table[((bucket + 1) & 63) as usize], 2);
            assert_eq!(set.max_psl, 0);
        }
        let mut set = SetAdmission::new(layout);
        for value in 0..32 {
            assert!(set.admit_hashed(value, 0));
        }
        assert!(!set.admit_hashed(100, 40));
        set.release(32);
        assert_eq!(set.pop_unused(), Some(31));
        assert_eq!(set.max_psl, 30);
        assert!(set.admit_hashed(100, 40));
    }

    #[test]
    fn borrowed_hyperlink_hash_matches_contiguous_keys_at_every_boundary() {
        for uri_len in (0..=256).chain([511, 512, 1984, 4096]) {
            let uri: Vec<_> = (0..uri_len).map(|i| (i * 37) as u8).collect();
            for id_len in 0..=64 {
                let id: Vec<_> = (0..id_len).map(|i| (i * 13) as u8).collect();
                let mut bytes = vec![0];
                bytes.extend_from_slice(&id);
                bytes.extend_from_slice(&id.len().to_le_bytes());
                bytes.extend_from_slice(&uri);
                bytes.extend_from_slice(&uri.len().to_le_bytes());
                assert_eq!(
                    hyperlink_hash(HyperlinkKey::new(&uri, Some(&id), 0)),
                    wyhash(&bytes)
                );
            }
        }
    }

    #[test]
    fn rebuilding_shares_link_payloads_but_reflow_still_reserves_strings() {
        let mut source = hyperlink_admission();
        let uri = vec![b'x'; 2016];
        let key = HyperlinkKey::new(&uri, None, 1);
        let id = source.insert(key).unwrap();
        let mut copied = hyperlink_admission();
        copied.capacity = 3;
        assert_eq!(copied.copy_from(&source, id), Ok(id));
        assert!(Arc::ptr_eq(source.data(id), copied.data(id)));
        assert!(Arc::ptr_eq(
            &source.set.get(id).link,
            &copied.set.get(id).link
        ));
        assert_eq!(copied.copy_cell(key, id), Ok(id));
        assert_eq!(copied.reflow_cell(key, id), Err(HyperlinkFull::Strings));
        assert_eq!(copied.strings.used_bytes(), 2016);
        assert_eq!(copied.cells, 2);
        copied.assert_references([id, id].into_iter(), None);
    }

    #[test]
    fn hyperlink_hash_matches_native_page_entry_vectors() {
        // Native PageEntry.hash on macOS ARM64, including the 16/48-byte
        // Wyhash boundaries and raw binary URI/ID strings.
        let vectors = [
            (0, 0x8ca46852722ececa, 0xea584159da1c779f),
            (1, 0x22caeae0ed236f09, 0x5e1845c6a0962c92),
            (2, 0x7b726a8475e68706, 0x333e7aebe4e18ade),
            (3, 0xbf820fa1d9be285f, 0xcad4ab387e858e75),
            (4, 0x918fdce1766e7fa3, 0x3d5351f7c5600d99),
            (8, 0xfbfbf09bfb803b5d, 0x255b5f48b5ac1c27),
            (15, 0xac96333ec480985c, 0x72ae9a546971c06b),
            (16, 0x88b1cf6377bc8c37, 0x8a56db12c261c914),
            (17, 0xc26652a324feb74e, 0x8f3e0095b4c8ba57),
            (31, 0x4f2e3a9aa46c2192, 0x8a52b63418672ed4),
            (32, 0x922fd9fd53ea99bf, 0xac7fc83006291f62),
            (33, 0x1c09df58ddfd7522, 0x8b32444d8b27738d),
            (35, 0x89e8ea3835745eb9, 0x9d174d4c52f89ae7),
            (36, 0xa32dc6b2460fbdeb, 0xff0fdd189fac8629),
            (47, 0xe25fd0283c6705a3, 0xf32f43078a3b0d65),
            (48, 0xd100975c254ee54f, 0xb21c1077e0ce6139),
            (49, 0xaa2e5a1d228dffa9, 0x4c349863cf66410e),
            (50, 0x4c10afa4a2588c50, 0x42322df84a406b20),
            (63, 0xae7e6922a158f652, 0x58ce416647b15d0b),
            (64, 0x357f83fb8d57310, 0xf11a6c1ded38001f),
            (65, 0xc2a4908e64c4d5c4, 0xbbda3a747839f1e3),
            (95, 0x4a689a7a88531d7c, 0xdbe9a8b4bd4ce96a),
            (96, 0x80c4a91ae8a6421, 0x125d79f1646824e2),
            (97, 0x8edf41c219259112, 0x4a960346c0d2c022),
            (128, 0x13452b884c680dcc, 0xb4d349e0e7ef42d6),
            (255, 0x5b525ebb343090f4, 0xe9966ba272d2f74b),
            (256, 0x92c0b34170e416ad, 0x1129d45aae96d8d),
            (1024, 0x1012e709faff6abe, 0xe8e4d8e30ef45f45),
        ];
        for (len, implicit, explicit) in vectors {
            let uri: Vec<_> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let id: Vec<_> = (0..(len * 3 + 5) % 97)
                .map(|i| (i * 13 + 7) as u8)
                .collect();
            assert_eq!(
                hyperlink_hash(HyperlinkKey::new(&uri, None, 0x01020304)),
                implicit
            );
            assert_eq!(
                hyperlink_hash(HyperlinkKey::new(&uri, Some(&id), 0)),
                explicit
            );
        }
    }

    fn style_set(capacity: u16) -> StyleAdmission {
        StyleAdmission::new(
            PageCapacity {
                styles: capacity,
                ..PageCapacity::STANDARD
            }
            .metadata()
            .unwrap()
            .styles_layout,
        )
    }

    fn numbered_style(number: u32) -> Style {
        Style {
            foreground: Color::Rgb(number as u8, (number >> 8) as u8, (number >> 16) as u8),
            ..Style::default()
        }
    }

    #[test]
    fn live_set_reuses_dead_ids_and_trims_before_duplicate_lookup() {
        let layout = PageCapacity {
            styles: 16,
            ..PageCapacity::STANDARD
        }
        .metadata()
        .unwrap()
        .styles_layout;
        let mut set = SetAdmission::new(layout);
        assert_eq!(set.add_hashed(11, 0), Ok(1));
        assert_eq!(set.add_hashed(22, 1), Ok(2));
        assert_eq!(set.add_hashed(33, 2), Ok(3));
        set.release(2);
        assert_eq!(set.add_with_id_hashed(11, 0, 2), Ok(1));
        assert_eq!(set.count(), 2);
        assert_eq!(set.add_with_id_hashed(44, 1, 2), Ok(2));
        set.release(3);
        assert_eq!(set.add_hashed(11, 0), Ok(1));
        assert_eq!(set.entries.len(), 2);
        set.release(2);
        assert_eq!(set.add_hashed(55, 1), Ok(2));
        assert_eq!(set.count(), 2);
    }

    #[test]
    fn live_set_rehash_threshold_counts_living_entries() {
        for (released, expected) in [(10, SetFull::OutOfMemory), (11, SetFull::NeedsRehash)] {
            let layout = PageCapacity::STANDARD.metadata().unwrap().styles_layout;
            let mut set = SetAdmission::new(layout);
            for value in 0..103 {
                assert!(set.add_hashed(value, value).is_ok());
            }
            for id in 1..=released {
                set.release(id);
            }
            assert_eq!(set.add_hashed(1000, 127), Err(expected));
            // A live duplicate still succeeds without requiring free IDs.
            assert_eq!(set.add_hashed(102, 102), Ok(103));
        }
    }

    fn styles_in_bucket(mask: u64, bucket: u64, count: usize) -> Vec<Style> {
        (0..0x1000000)
            .map(numbered_style)
            .filter(|style| style.native_hash() & mask == bucket)
            .take(count)
            .collect()
    }

    #[test]
    fn style_admission_reserves_zero_and_reuses_live_values_when_full() {
        for capacity in [0, 1, 2, 3, 4, 8, 16, 64, 128] {
            let mut styles = style_set(capacity);
            assert!(styles.admit(Style::default()));
            assert!(styles.entries.is_empty());
            let live_capacity = styles.capacity.saturating_sub(1);
            // Use distinct home buckets to make capacity the only limit.
            for bucket in 0..live_capacity {
                let value = styles_in_bucket((styles.table.len() - 1) as u64, bucket as u64, 1)[0];
                assert!(styles.admit(value));
                assert!(styles.admit(value));
                assert_eq!(styles.entries[bucket].references, 2);
            }
            let rejected = if styles.table.is_empty() {
                numbered_style(0)
            } else {
                styles_in_bucket((styles.table.len() - 1) as u64, live_capacity as u64, 1)[0]
            };
            assert!(!styles.admit(rejected));
            assert_eq!(styles.entries.len(), live_capacity);
            if let Some(entry) = styles.entries.first() {
                let value = entry.value.unwrap();
                assert!(styles.admit(value));
                assert_eq!(styles.entries[0].references, 3);
            }
        }
    }

    #[test]
    fn style_admission_collision_limit_rejects_even_unrelated_new_values() {
        for bucket in [0, 250, 255] {
            let mut styles = style_set(256);
            let values = styles_in_bucket(255, bucket, 33);
            for value in &values[..32] {
                assert!(styles.admit(*value));
            }
            assert_eq!(styles.max_psl, 31);
            assert!(styles.entries.len() + 1 < styles.capacity);
            assert!(!styles.admit(values[32]));
            assert!(!styles.admit(styles_in_bucket(255, (bucket + 80) & 255, 1)[0]));
            // Lookup precedes both the PSL and capacity checks.
            for value in values[..32].iter().rev() {
                assert!(styles.admit(*value));
            }
            assert!(styles.entries.iter().all(|entry| entry.references == 2));
        }
    }

    #[test]
    fn style_admission_refcounts_break_displacement_ties() {
        let mut styles = style_set(16);
        let home_zero = styles_in_bucket(15, 0, 2);
        let home_one = styles_in_bucket(15, 1, 2);
        assert!(styles.admit(home_zero[0]));
        for _ in 0..5 {
            assert!(styles.admit(home_one[0]));
        }
        assert!(styles.admit(home_one[1]));
        assert!(styles.admit(home_zero[1]));
        // The displaced, frequently referenced style wins the equal-PSL tie.
        assert_eq!(&styles.table[..4], &[1, 4, 2, 3]);
        assert_eq!(styles.entries[1].psl, 1);
        assert_eq!(styles.entries[2].psl, 2);
        assert_eq!(styles.max_psl, 2);
    }

    fn allocator<const CHUNK: usize>(words: usize) -> BitmapAllocator<CHUNK> {
        BitmapAllocator::new(BitmapLayout {
            total_size: words * (8 + 64 * CHUNK),
            bitmap_count: words,
            bitmap_start: 0,
            chunks_start: words * 8,
        })
    }

    #[test]
    fn native_chunk_rounding_and_small_capacity() {
        for (bytes, rounded) in [(0, 0), (1, 16), (4, 16), (16, 16), (17, 32), (24, 32)] {
            assert_eq!(BitmapAllocator::<16>::bytes_required(bytes), Some(rounded));
        }
        for (bytes, rounded) in [(1, 32), (32, 32), (33, 64)] {
            assert_eq!(BitmapAllocator::<32>::bytes_required(bytes), Some(rounded));
        }
        assert_eq!(BitmapAllocator::<16>::bytes_required(usize::MAX), None);
        let capacity = PageCapacity {
            grapheme_bytes: 48,
            ..PageCapacity::STANDARD
        };
        let layout = capacity.metadata().unwrap().grapheme_alloc_layout;
        let mut arena = BitmapAllocator::<16>::new(layout);
        assert_eq!(arena.capacity_bytes(), 1024);
        for chunk in 0..64 {
            assert_eq!(arena.alloc(1), Some(layout.chunks_start + chunk * 16));
        }
        assert_eq!(arena.alloc(1), None);
        assert_eq!(arena.used_bytes(), arena.capacity_bytes());
        let mut empty = allocator::<32>(0);
        assert_eq!(empty.alloc(1), None);
        assert_eq!((empty.capacity_bytes(), empty.used_bytes()), (0, 0));
    }

    #[test]
    fn native_first_fit_and_search_hint_preserve_partial_words() {
        let mut arena = allocator::<1>(3);
        let first = arena.alloc(1).unwrap();
        for chunk in 1..64 {
            assert_eq!(arena.alloc(1), Some(first + chunk));
        }
        assert_eq!(arena.search_start, 1);
        assert_eq!(arena.alloc(1), Some(first + 64));
        arena.free(first, 1);
        assert_eq!(arena.search_start, 0);
        assert_eq!(arena.alloc(1), Some(first));

        let mut arena = allocator::<1>(2);
        let first = arena.alloc(60).unwrap();
        assert_eq!(arena.alloc(8), Some(first + 64));
        assert_eq!(arena.search_start, 0);
        assert_eq!(arena.alloc(4), Some(first + 60));
        assert_eq!(arena.search_start, 1);
    }

    #[test]
    fn native_multi_chunk_graphemes_free_the_rounded_allocation() {
        let mut arena = allocator::<16>(1);
        let first = arena.alloc(6 * 4).unwrap();
        assert_eq!(arena.alloc(4), Some(first + 32));
        assert_eq!(arena.used_bytes(), 48);
        arena.free(first, 6 * 4);
        assert_eq!(arena.used_bytes(), 16);
        assert_eq!(arena.alloc(4), Some(first));
    }

    #[test]
    fn grapheme_text_survives_failed_growth_and_follows_its_allocation() {
        let mut graphemes = GraphemeAdmission {
            allocator: allocator::<16>(1),
            capacity: 64,
            ..Default::default()
        };
        let first = graphemes.acquire(4).unwrap();
        let text: Arc<str> = "a\u{301}\u{302}\u{303}\u{304}".into();
        graphemes.set_text(first, text.clone());
        let mut occupied: Vec<_> = (1..64).map(|_| graphemes.acquire(1).unwrap()).collect();

        assert_eq!(graphemes.append(Some(first)), Err(SetFull::OutOfMemory));
        assert_eq!(graphemes.used_bytes(), 1024);
        assert_eq!(graphemes.text(first), &*text);
        assert!(Arc::ptr_eq(&graphemes.text_arc(first), &text));

        graphemes.release(occupied.pop().unwrap());
        graphemes.release(occupied.pop().unwrap());
        let grown = graphemes.append(Some(first)).unwrap();
        assert_ne!(grown.offset, first.offset);
        assert_eq!(grown.len, 5);
        assert!(graphemes.texts[graphemes.text_index(first)].is_none());
        assert!(Arc::ptr_eq(&graphemes.text_arc(grown), &text));

        let extended: Arc<str> = format!("{text}\u{305}").into();
        graphemes.set_text(grown, extended.clone());
        let snapshot = graphemes.clone();
        graphemes.release(grown);
        assert!(graphemes.texts.iter().all(Option::is_none));
        assert!(Arc::ptr_eq(&snapshot.text_arc(grown), &extended));
        for allocation in occupied {
            graphemes.release(allocation);
        }
        assert_eq!(graphemes.used_bytes(), 0);
        let reused = graphemes.acquire(4).unwrap();
        assert_eq!(reused.offset, first.offset);
        assert!(graphemes.texts.iter().all(Option::is_none));
        graphemes.release(reused);
    }

    #[test]
    fn grapheme_clones_keep_offsets_when_repacking_would_exhaust_capacity() {
        let mut source = GraphemeAdmission {
            allocator: allocator::<16>(2),
            capacity: 128,
            ..Default::default()
        };
        let allocations: Vec<_> = [15, 15, 15, 15, 4, 15, 15, 15, 15, 4]
            .into_iter()
            .map(|chunks| {
                let allocation = source.acquire(chunks * 4).unwrap();
                source.set_text(
                    allocation,
                    format!("a{}", "\u{301}".repeat(usize::from(allocation.len))).into(),
                );
                allocation
            })
            .collect();
        let reordered: Vec<_> = [4, 9, 0, 1, 2, 3, 5, 6, 7, 8]
            .map(|index| allocations[index])
            .into();
        let mut repacked = GraphemeAdmission {
            allocator: allocator::<16>(2),
            capacity: 128,
            ..Default::default()
        };
        assert!(
            reordered
                .iter()
                .any(|allocation| repacked.acquire(allocation.len).is_err())
        );

        let snapshot = source.clone();
        snapshot.assert_allocations(reordered.iter().copied());
        for allocation in &reordered {
            assert!(Arc::ptr_eq(
                &source.text_arc(*allocation),
                &snapshot.text_arc(*allocation)
            ));
        }
        let mut subset = source.clone();
        for &allocation in &reordered[1..] {
            subset.release(allocation);
        }
        subset.assert_allocations(std::iter::once(reordered[0]));
        assert_eq!(subset.texts.iter().flatten().count(), 1);
        subset.release(reordered[0]);
        assert_eq!(subset.used_bytes(), 0);
        assert!(subset.texts.iter().all(Option::is_none));
        source.assert_allocations(allocations.into_iter());
    }

    #[test]
    fn sparse_grapheme_slots_follow_chunks_not_map_capacity() {
        let layout = PageCapacity {
            grapheme_bytes: 48,
            ..PageCapacity::STANDARD
        }
        .metadata()
        .unwrap();
        let mut graphemes = GraphemeAdmission::new(
            layout.grapheme_alloc_layout,
            layout.grapheme_map_layout.capacity as usize,
        );
        assert_eq!(graphemes.capacity, 4);
        assert_eq!(graphemes.allocator.chunks_start, 8);
        let allocations = std::array::from_fn::<_, 4, _>(|_| graphemes.acquire(64).unwrap());
        assert!(
            graphemes.texts.is_empty(),
            "admission probes need no text slots"
        );
        assert_eq!(
            allocations.map(|a| graphemes.text_index(a)),
            [0, 16, 32, 48]
        );
        let text: Arc<str> = format!("a{}", "\u{301}".repeat(64)).into();
        for allocation in allocations {
            graphemes.set_text(allocation, text.clone());
            assert_eq!(graphemes.text(allocation), &*text);
        }
        assert_eq!(graphemes.texts.len(), 49);
        assert_eq!(graphemes.texts.iter().flatten().count(), 4);
        let snapshot = graphemes.clone();
        graphemes.release(allocations[3]);
        let reused = graphemes.acquire(64).unwrap();
        assert_eq!(reused, allocations[3]);
        assert!(graphemes.texts[48].is_none());
        assert!(Arc::ptr_eq(&snapshot.text_arc(reused), &text));
        graphemes.release(reused);
        for allocation in &allocations[..3] {
            graphemes.release(*allocation);
        }
        graphemes.assert_allocations(std::iter::empty());
    }

    #[test]
    fn native_large_spans_cross_words_and_preserve_neighbors_when_freed() {
        let mut arena = allocator::<1>(3);
        let first = arena.alloc(96).unwrap();
        let second = arena.alloc(96).unwrap();
        assert_eq!(second, first + 96);
        assert_eq!(arena.bitmaps, [u64::MAX; 3]);
        arena.free(first, 96);
        assert_eq!(arena.bitmaps, [0, u64::MAX << 32, u64::MAX]);
        assert_eq!(arena.used_bytes(), 96);
        assert_eq!(arena.alloc(96), Some(first));
        arena.free(second, 96);
        assert_eq!(arena.bitmaps, [u64::MAX, u32::MAX as u64, 0]);
        arena.free(first, 96);
        assert_eq!(arena.bitmaps, [0; 3]);

        let first = arena.alloc(56).unwrap();
        assert_eq!(arena.alloc(65), Some(first + 56));
        assert_eq!(arena.bitmaps, [u64::MAX, (1 << 57) - 1, 0]);
    }

    #[test]
    fn native_large_spans_restart_at_the_next_partial_word() {
        let mut arena = allocator::<1>(4);
        arena.bitmaps = vec![u32::MAX as u64, 1 << 10, 0, 0];
        let before = arena.clone();
        // The first free suffix is interrupted by bit 10 of the second word.
        let offset = arena.alloc(100).unwrap();
        assert_eq!(offset, arena.chunks_start + 75);
        assert_eq!(
            arena.bitmaps,
            [u32::MAX as u64, u64::MAX << 10, (1 << 47) - 1, 0]
        );
        arena.free(offset, 100);
        assert_eq!(arena, before);
    }

    #[test]
    fn native_small_spans_do_not_join_holes_across_word_boundaries() {
        let mut arena = allocator::<1>(2);
        let first = arena.alloc(60).unwrap();
        let second = arena.alloc(64).unwrap();
        arena.free(second, 4);
        assert_eq!(arena.capacity_bytes() - arena.used_bytes(), 8);
        let before = arena.clone();
        assert_eq!(arena.alloc(8), None);
        assert_eq!(arena, before);
        assert_eq!(arena.alloc(4), Some(first + 60));
        assert_eq!(arena.alloc(4), Some(second));
    }

    #[test]
    fn exhausted_large_spans_leave_the_bitmap_unchanged() {
        let mut arena = allocator::<1>(1);
        let before = arena.clone();
        for bytes in [0, 65, 128, usize::MAX] {
            assert_eq!(arena.alloc(bytes), None);
            assert_eq!(arena, before);
        }
        let mut arena = allocator::<32>(2);
        arena.alloc(63 * 32).unwrap();
        let before = arena.clone();
        assert_eq!(arena.alloc(66 * 32), None);
        assert_eq!(arena, before);
        assert!(arena.alloc(65 * 32).is_some());
    }
}
