//! Owner-wide tmux image admission. Screens share limits, usage order and ancestry.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug)]
pub struct Quota(Arc<Mutex<State>>);
#[derive(Clone, Debug)]
struct State {
    next: u64,
    clock: u64,
    count: usize,
    bytes: usize,
    entries: BTreeMap<u64, Entry>,
}
#[derive(Clone, Debug)]
struct Entry {
    owner: u64,
    parent: Option<u64>,
    bytes: usize,
    used: u64,
    mapped: bool,
}
impl Default for Quota {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            next: 0,
            clock: 0,
            count: 1000,
            bytes: 1024 * 1024 * 1024,
            entries: BTreeMap::new(),
        })))
    }
}
impl Quota {
    /// Isolate an engine checkpoint while retaining its screen and image IDs.
    pub fn fork(&self) -> Self {
        Self(Arc::new(Mutex::new(self.0.lock().unwrap().clone())))
    }
    pub(super) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(super) fn release_owner(&self, owner: u64) {
        self.0
            .lock()
            .unwrap()
            .entries
            .retain(|_, entry| entry.owner != owner);
    }
    pub fn set_limits(&self, count: usize, bytes: usize) {
        let mut state = self.0.lock().unwrap();
        state.count = count;
        state.bytes = bytes;
    }
    pub(super) fn owner(&self) -> u64 {
        let mut s = self.0.lock().unwrap();
        s.next += 1;
        s.next
    }
    pub(super) fn contains(&self, id: u64) -> bool {
        self.0.lock().unwrap().entries.contains_key(&id)
    }
    pub fn retain_owners(&self, owners: &BTreeSet<u64>) {
        self.0
            .lock()
            .unwrap()
            .entries
            .retain(|_, entry| owners.contains(&entry.owner));
    }
    pub(super) fn reserve(
        &self,
        owner: u64,
        parent: Option<u64>,
        bytes: usize,
    ) -> Result<u64, &'static str> {
        let mut s = self.0.lock().unwrap();
        if bytes > s.bytes {
            return Err("EINVAL");
        }
        let mut keep = BTreeSet::new();
        let mut cursor = parent;
        while let Some(id) = cursor {
            if !keep.insert(id) {
                return Err("EINVAL");
            }
            cursor = s.entries.get(&id).ok_or("ENOENT")?.parent;
        }
        loop {
            let size = s
                .entries
                .values()
                .fold(0usize, |sum, entry| sum.saturating_add(entry.bytes));
            let size_full = size.saturating_add(bytes) > s.bytes;
            if s.entries.len() < s.count && !size_full {
                break;
            }
            let oldest = s
                .entries
                .iter()
                .filter(|(id, entry)| !keep.contains(id) && (!size_full || entry.parent.is_none()))
                .min_by_key(|(_, entry)| entry.used)
                .map(|(id, _)| *id)
                .ok_or("EINVAL")?;
            remove(&mut s, oldest);
        }
        s.next += 1;
        let id = s.next;
        s.clock += 1;
        let used = s.clock;
        s.entries.insert(
            id,
            Entry {
                owner,
                parent,
                bytes,
                used,
                mapped: parent.is_none(),
            },
        );
        touch(&mut s, id);
        Ok(id)
    }
    pub(super) fn release(&self, id: u64) {
        remove(&mut self.0.lock().unwrap(), id);
    }
    pub(super) fn unmap(&self, id: u64) {
        let mut s = self.0.lock().unwrap();
        if let Some(entry) = s.entries.get_mut(&id) {
            entry.mapped = false;
        }
        if !s.entries.values().any(|entry| entry.parent == Some(id)) {
            remove(&mut s, id);
        }
    }
    pub(super) fn touch(&self, id: u64) {
        touch(&mut self.0.lock().unwrap(), id);
    }
}
fn touch(s: &mut State, mut id: u64) {
    loop {
        s.clock += 1;
        let Some(entry) = s.entries.get_mut(&id) else {
            break;
        };
        entry.used = s.clock;
        if let Some(parent) = entry.parent {
            id = parent;
        } else {
            break;
        }
    }
}
fn remove(s: &mut State, id: u64) {
    let parent = s.entries.get(&id).and_then(|entry| entry.parent);
    let children = s
        .entries
        .iter()
        .filter(|(_, entry)| entry.parent == Some(id))
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    for child in children {
        remove(s, child);
    }
    s.entries.remove(&id);
    if let Some(parent) = parent
        && s.entries.get(&parent).is_some_and(|entry| !entry.mapped)
        && !s.entries.values().any(|entry| entry.parent == Some(parent))
    {
        remove(s, parent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn global_usage_order_ancestor_protection_and_size_pressure() {
        let q = Quota::default();
        q.set_limits(3, 8);
        let a = q.reserve(1, None, 4).unwrap();
        let b = q.reserve(2, None, 4).unwrap();
        q.touch(a);
        let p = q.reserve(1, Some(a), 0).unwrap();
        let c = q.reserve(3, None, 4).unwrap();
        assert!(!q.contains(b));
        assert!(q.contains(a) && q.contains(p) && q.contains(c));
        q.set_limits(1, 8);
        assert!(q.reserve(1, Some(a), 0).is_err());
        assert!(q.contains(a));
        assert!(!q.contains(p));
        assert!(!q.contains(c));
        assert!(q.reserve(1, None, 9).is_err());
        q.release(a);
        assert!(q.reserve(2, None, 8).is_ok());
    }
}
