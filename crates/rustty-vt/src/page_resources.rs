//! Page resource allocation offsets without native backing memory.
use crate::page_layout::{BitmapLayout, SetLayout};
use crate::screen::Style;

/// Native PAGE set bookkeeping, shared by snapshot admission and live styles.
/// Dead IDs retain their buckets until admission reclaims them or the page is
/// cloned. In particular, releasing a reference does not rehash the table.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct SetAdmission<T> {
    table: Vec<u16>,
    entries: Vec<SetEntry<T>>,
    capacity: usize,
    max_psl: u8,
    psl_stats: [u16; 32],
    living: usize,
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

    #[cfg(test)]
    pub fn allocated_buckets(&self) -> usize {
        self.table.capacity()
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

    pub fn admit_hashed(&mut self, value: T, hash: u64) -> bool {
        self.acquire_hashed(value, hash).is_some()
    }

    fn acquire_hashed(&mut self, value: T, hash: u64) -> Option<u16> {
        self.add_hashed(value, hash).ok()
    }

    pub fn lookup_hashed(&self, value: &T, hash: u64) -> Option<u16> {
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
            if entry.psl == psl && entry.references > 0 && entry.value.as_ref() == Some(value) {
                return Some(id);
            }
        }
        None
    }

    pub fn add_hashed(&mut self, value: T, hash: u64) -> Result<u16, SetFull> {
        while self
            .entries
            .last()
            .is_some_and(|entry| entry.references == 0)
        {
            self.delete_item(self.entries.len() as u16);
            self.entries.pop();
        }
        if let Some(id) = self.lookup_hashed(&value, hash) {
            self.retain(id);
            return Ok(id);
        }
        if self.psl_stats[31] != 0 {
            return Err(SetFull::OutOfMemory);
        }
        if self.entries.len() + 1 >= self.capacity {
            // Match the native floating-point conversion of cap * 0.9.
            return Err(if self.living < (self.capacity as f64 * 0.9) as usize {
                SetFull::NeedsRehash
            } else {
                SetFull::OutOfMemory
            });
        }
        let next = (self.entries.len() + 1) as u16;
        Ok(self.insert(value, hash, next))
    }

    fn add_with_id_hashed(&mut self, value: T, hash: u64, id: u16) -> Result<u16, SetFull> {
        assert!(id != 0);
        if usize::from(id) <= self.entries.len() {
            let entry = &self.entries[usize::from(id) - 1];
            if entry.references == 0 {
                if let Some(existing) = self.lookup_hashed(&value, hash) {
                    self.retain(existing);
                    return Ok(existing);
                }
                if self.psl_stats[31] != 0 {
                    return Err(SetFull::OutOfMemory);
                }
                self.delete_item(id);
                return Ok(self.insert(value, hash, id));
            } else if entry.value.as_ref() == Some(&value) {
                self.retain(id);
                return Ok(id);
            }
        }
        self.add_hashed(value, hash)
    }

    fn insert(&mut self, value: T, hash: u64, new_id: u16) -> u16 {
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
        if id == 0 {
            return;
        }
        let entry = &mut self.entries[usize::from(id) - 1];
        assert!(entry.references > 0);
        entry.references = entry
            .references
            .checked_add(1)
            .expect("native style reference overflow");
    }

    pub fn release(&mut self, id: u16) {
        if id == 0 {
            return;
        }
        let entry = &mut self.entries[usize::from(id) - 1];
        assert!(entry.references > 0);
        entry.references -= 1;
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

/// PAGE LINK decoding reserves new strings before reclaiming a discarded
/// final set entry, exactly as native decodePage/addContext do.
pub(crate) struct HyperlinkAdmission {
    set: SetAdmission<HyperlinkEntry>,
    strings: BitmapAllocator<32>,
}

struct HyperlinkEntry {
    id: crate::screen::HyperlinkId,
    uri: Vec<u8>,
    id_allocation: Option<(usize, usize)>,
    uri_allocation: (usize, usize),
}

impl PartialEq for HyperlinkEntry {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.uri == other.uri
    }
}

impl Eq for HyperlinkEntry {}

impl HyperlinkAdmission {
    pub fn new(set: SetLayout, strings: BitmapLayout) -> Self {
        Self {
            set: SetAdmission::new(set),
            strings: BitmapAllocator::new(strings),
        }
    }

    /// `retain` is false for zero or duplicate wire IDs. Those entries are
    /// still decoded/admitted before their temporary reference is released.
    pub fn admit(&mut self, id: &crate::screen::HyperlinkId, uri: &[u8], retain: bool) -> bool {
        use crate::screen::HyperlinkId;
        if uri.is_empty() || matches!(id, HyperlinkId::Explicit(value) if value.is_empty()) {
            return false;
        }
        let id_allocation = if let HyperlinkId::Explicit(value) = id {
            let Some(offset) = self.strings.alloc(value.len()) else {
                return false;
            };
            Some((offset, value.len()))
        } else {
            None
        };
        let Some(offset) = self.strings.alloc(uri.len()) else {
            if let Some((offset, len)) = id_allocation {
                self.strings.free(offset, len);
            }
            return false;
        };
        let uri_allocation = (offset, uri.len());

        while let Some(entry) = self.set.pop_unused() {
            self.free_strings(entry.id_allocation, entry.uri_allocation);
        }
        let prior_entries = self.set.entries.len();
        let acquired = self.set.acquire_hashed(
            HyperlinkEntry {
                id: id.clone(),
                uri: uri.to_vec(),
                id_allocation,
                uri_allocation,
            },
            hyperlink_hash(id, uri),
        );
        // A duplicate value or failed admission frees the incoming strings.
        if acquired.is_none_or(|id| usize::from(id) <= prior_entries) {
            self.free_strings(id_allocation, uri_allocation);
        }
        if let Some(id) = acquired {
            if !retain {
                self.set.release(id);
            }
            true
        } else {
            false
        }
    }

    fn free_strings(&mut self, id: Option<(usize, usize)>, uri: (usize, usize)) {
        if let Some((offset, len)) = id {
            self.strings.free(offset, len);
        }
        self.strings.free(uri.0, uri.1);
    }
}

/// Bookkeeping for `terminal/bitmap_allocator.zig`.
///
/// Offsets are relative to this allocator's region, including its bitmap header.
/// The page ledger adds the region's page offset. Callers pass byte lengths;
/// grapheme codepoints occupy four native bytes each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BitmapAllocator<const CHUNK: usize> {
    bitmaps: Vec<u64>,
    chunks_start: usize,
    search_start: usize,
}

impl<const CHUNK: usize> BitmapAllocator<CHUNK> {
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

    #[cfg(test)]
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
pub(crate) fn hyperlink_hash(id: &crate::screen::HyperlinkId, uri: &[u8]) -> u64 {
    let mut bytes = Vec::new();
    match id {
        crate::screen::HyperlinkId::Explicit(id) => {
            bytes.push(0);
            bytes.extend_from_slice(id);
            bytes.extend_from_slice(&id.len().to_le_bytes());
        }
        crate::screen::HyperlinkId::Implicit(id) => {
            bytes.push(1);
            bytes.extend_from_slice(&id.to_le_bytes());
        }
    }
    bytes.extend_from_slice(uri);
    bytes.extend_from_slice(&uri.len().to_le_bytes());
    wyhash(&bytes)
}

// Zig std.hash.Wyhash's one-shot path with seed zero. Reuse it for the native
// admission hash; Rust's randomized HashMap hash would change collision limits.
fn wyhash(bytes: &[u8]) -> u64 {
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
    fn read8(bytes: &[u8]) -> u64 {
        u64::from_le_bytes(bytes[..8].try_into().unwrap())
    }
    fn read4(bytes: &[u8]) -> u64 {
        u32::from_le_bytes(bytes[..4].try_into().unwrap()).into()
    }

    let mut state = [mix(SECRET[0], SECRET[1]); 3];
    let (mut a, mut b) = if bytes.len() <= 16 {
        if bytes.len() >= 4 {
            let end = bytes.len() - 4;
            let quarter = (bytes.len() >> 3) << 2;
            (
                (read4(bytes) << 32) | read4(&bytes[quarter..]),
                (read4(&bytes[end..]) << 32) | read4(&bytes[end - quarter..]),
            )
        } else if !bytes.is_empty() {
            (
                (u64::from(bytes[0]) << 16)
                    | (u64::from(bytes[bytes.len() >> 1]) << 8)
                    | u64::from(bytes[bytes.len() - 1]),
                0,
            )
        } else {
            (0, 0)
        }
    } else {
        let mut offset = 0;
        if bytes.len() >= 48 {
            while offset + 48 < bytes.len() {
                for i in 0..3 {
                    let chunk = &bytes[offset + 16 * i..];
                    state[i] = mix(read8(chunk) ^ SECRET[i + 1], read8(&chunk[8..]) ^ state[i]);
                }
                offset += 48;
            }
            state[0] ^= state[1] ^ state[2];
        }
        while offset + 16 < bytes.len() {
            state[0] = mix(
                read8(&bytes[offset..]) ^ SECRET[1],
                read8(&bytes[offset + 8..]) ^ state[0],
            );
            offset += 16;
        }
        (
            read8(&bytes[bytes.len() - 16..]),
            read8(&bytes[bytes.len() - 8..]),
        )
    };
    a ^= SECRET[1];
    b ^= state[0];
    let product = u128::from(a) * u128::from(b);
    mix(
        product as u64 ^ SECRET[0] ^ bytes.len() as u64,
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
        HyperlinkAdmission::new(layout.hyperlink_set_layout, layout.string_alloc_layout)
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
    fn hyperlink_hash_matches_native_page_entry_vectors() {
        use crate::screen::HyperlinkId;
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
                hyperlink_hash(&HyperlinkId::Implicit(0x01020304), &uri),
                implicit
            );
            assert_eq!(hyperlink_hash(&HyperlinkId::Explicit(id), &uri), explicit);
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
