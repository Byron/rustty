//! Kitty graphics storage. Pixel buffers are shared with render snapshots.
use crate::{Effect, GridPoint, Screen, Terminal};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Read,
    sync::Arc,
};

mod images;
pub use images::Images;
mod pixels;
mod placements;
pub mod quota;
mod sixel;
pub use placements::Placements;
#[cfg(all(unix, not(target_os = "android")))]
mod shm;
pub mod unicode;

const MAX_DATA: usize = 400 * 1024 * 1024;
const MAX_DIMENSION: u32 = 10000;
const PARENT_CHAIN_LIMIT: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Image {
    pub id: u32,
    /// tmux sources without an application mapping live only through placements.
    #[serde(default)]
    pub tmux_unmapped: bool,
    /// SIXEL sources are placed directly, without Kitty's extra quota object.
    #[serde(default)]
    pub tmux_sixel_cell: Option<[u32; 2]>,
    #[serde(default)]
    pub tmux_order: Option<u64>,
    pub number: u32,
    pub width: u32,
    pub height: u32,
    /// Unassociated RGBA8, independent of the renderer's target color space.
    #[serde(with = "pixels::serde_rgba")]
    pub pixels: Arc<Vec<u8>>,
    pub generation: u64,
    identity: u64,
    pub frames: Vec<AnimationFrame>,
    pub current_frame: usize,
    pub root_gap_ms: u32,
    pub animation_state: u8,
    pub max_loops: u32,
    pub completed_loops: u32,
    pub frame_shown_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnimationFrame {
    #[serde(with = "pixels::serde_rgba")]
    pub pixels: Arc<Vec<u8>>,
    pub gap_ms: u32,
}

impl Image {
    pub fn display_pixels(&self) -> &[u8] {
        if self.current_frame == 0 {
            &self.pixels
        } else {
            &self.frames[self.current_frame - 1].pixels
        }
    }
    pub fn displayed_pixels(&self) -> Arc<Vec<u8>> {
        if self.current_frame == 0 {
            self.pixels.clone()
        } else {
            self.frames[self.current_frame - 1].pixels.clone()
        }
    }
    fn frame(&self, number: usize) -> Option<&Arc<Vec<u8>>> {
        if number == 1 {
            Some(&self.pixels)
        } else {
            self.frames.get(number.checked_sub(2)?).map(|f| &f.pixels)
        }
    }
    fn set_frame(&mut self, number: usize, pixels: Arc<Vec<u8>>) {
        if number == 1 {
            self.pixels = pixels;
        } else {
            self.frames[number - 2].pixels = pixels;
        }
    }
    fn gap(&self, index: usize) -> u32 {
        if index == 0 {
            self.root_gap_ms
        } else {
            self.frames[index - 1].gap_ms
        }
    }
    fn bytes(&self) -> usize {
        self.pixels.len() + self.frames.iter().map(|f| f.pixels.len()).sum::<usize>()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Placement {
    pub image_id: u32,
    pub placement_id: PlacementId,
    pub row: u64,
    pub col: usize,
    /// Requested c/r values; zero requests intrinsic or aspect-ratio sizing.
    pub columns: u32,
    pub rows: u32,
    /// Projected anchor in viewport snapshots, including roots above the viewport.
    pub viewport_row: Option<i64>,
    pub z: i32,
    /// Requested pixel-space source rectangle; zero size means full image.
    pub source: [u32; 4],
    pub offset: [u32; 2],
    pub virtual_placement: bool,
    pub parent: Option<(u32, PlacementId)>,
    pub parent_offset: [i32; 2],
}

/// Automatic and application-supplied placement IDs occupy separate namespaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PlacementId {
    Internal(u32),
    External(u32),
}

impl Placement {
    /// Resolve a live parent chain, preserving native per-link i32 saturation.
    /// Replacing an ancestor can make an existing descendant exceed the limit.
    pub fn resolve_chain<'a>(
        &'a self,
        mut lookup: impl FnMut((u32, PlacementId)) -> Option<&'a Placement>,
    ) -> Option<(&'a Placement, [i32; 2])> {
        let mut current = self;
        let mut offset = [0i32; 2];
        for depth in 0..=PARENT_CHAIN_LIMIT {
            let Some(parent) = current.parent else {
                return Some((current, offset));
            };
            if depth == PARENT_CHAIN_LIMIT {
                return None;
            }
            offset[0] = offset[0].saturating_add(current.parent_offset[0]);
            offset[1] = offset[1].saturating_add(current.parent_offset[1]);
            current = lookup(parent)?;
        }
        None
    }

    pub fn source_rect(&self, image: &Image) -> [u32; 4] {
        let [x, y, width, height] = self.source;
        let x = x.min(image.width);
        let y = y.min(image.height);
        [
            x,
            y,
            (if width == 0 { image.width } else { width }).min(image.width - x),
            (if height == 0 { image.height } else { height }).min(image.height - y),
        ]
    }

    pub fn cell_offset(&self, cell: [u32; 2]) -> [u32; 2] {
        [
            self.offset[0].min(cell[0].saturating_sub(1)),
            self.offset[1].min(cell[1].saturating_sub(1)),
        ]
    }

    pub fn pixel_size(&self, image: &Image, cell: [u32; 2]) -> [u32; 2] {
        let [_, _, width, height] = self.source_rect(image);
        if self.columns == 0 && self.rows == 0 {
            return [width, height];
        }
        let offset = self.cell_offset(cell);
        let target_width = cell[0]
            .saturating_mul(self.columns)
            .saturating_sub(offset[0]);
        let target_height = cell[1].saturating_mul(self.rows).saturating_sub(offset[1]);
        let scale = |value: u32, numerator: u32, denominator: u32| {
            if denominator == 0 {
                return 0;
            }
            ((u64::from(value) * u64::from(numerator) + u64::from(denominator) / 2)
                / u64::from(denominator))
            .min(u64::from(u32::MAX)) as u32
        };
        match (self.columns, self.rows) {
            (_, 0) => [target_width, scale(target_width, height, width)],
            (0, _) => [scale(target_height, width, height), target_height],
            _ => [target_width, target_height],
        }
    }

    pub fn grid_size(&self, image: &Image, cell: [u32; 2]) -> [u32; 2] {
        if self.columns != 0 && self.rows != 0 {
            return [self.columns, self.rows];
        }
        let size = self.pixel_size(image, cell);
        let offset = self.cell_offset(cell);
        let axis = |index: usize| {
            if cell[index] == 0 {
                0
            } else {
                size[index]
                    .saturating_add(offset[index])
                    .div_ceil(cell[index])
            }
        };
        [axis(0), axis(1)]
    }

    /// Inclusive grid bounds, clipped at the screen's last physical row/column.
    pub fn grid_rect(
        &self,
        screen: &Screen,
        image: &Image,
        cell: [u32; 2],
    ) -> Option<(GridPoint, GridPoint)> {
        if self.virtual_placement || self.parent.is_some() {
            return None;
        }
        let [columns, rows] = self.grid_size(image, cell);
        if columns == 0 || rows == 0 {
            return None;
        }
        let y = screen.all_rows().position(|row| row.id == self.row)?;
        let end_y = y
            .saturating_add(rows as usize - 1)
            .min(screen.history_len() + screen.height() - 1);
        let end_x = self
            .col
            .saturating_add(columns as usize - 1)
            .min(screen.columns - 1);
        Some((
            GridPoint {
                row: self.row,
                col: self.col,
            },
            GridPoint {
                row: screen.all_rows().nth(end_y)?.id,
                col: end_x,
            },
        ))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Loading {
    command: Command,
    data: Vec<u8>,
    image_id: u32,
    image_generation: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Graphics {
    quota: Option<quota::Quota>,
    quota_owner: u64,
    quota_images: BTreeMap<u32, (u64, u64)>,
    quota_placements: BTreeMap<(u32, PlacementId), u64>,
    pub images: Images,
    pub placements: Placements,
    pub generation: u64,
    pub limit: usize,
    loading: Option<Loading>,
    next_image: u32,
    next_placement: u32,
}

impl Default for Graphics {
    fn default() -> Self {
        Self {
            quota: None,
            quota_owner: 0,
            quota_images: BTreeMap::new(),
            quota_placements: BTreeMap::new(),
            images: Images::default(),
            placements: Placements::default(),
            generation: 0,
            limit: 320_000_000,
            loading: None,
            next_image: 2147483647,
            next_placement: 0,
        }
    }
}

/// Display state and pending upload continuation for a passive terminal mirror.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ViewSnapshot {
    pub images: Vec<Image>,
    pub placements: Vec<Placement>,
    pub generation: u64,
    limit: usize,
    loading: Option<Loading>,
    next_image: u32,
    next_placement: u32,
}

impl Graphics {
    fn attach_quota(&mut self, quota: quota::Quota, shared: Option<&Graphics>) {
        if self.quota.as_ref().is_some_and(|old| old.same(&quota)) {
            return;
        }
        self.release_quota();
        self.quota_owner = shared.map_or_else(|| quota.owner(), |graphics| graphics.quota_owner);
        self.quota = Some(quota.clone());
        if let Some(shared) = shared {
            self.images = shared.images.clone();
            self.quota_images = shared.quota_images.clone();
        } else {
            let mut images = self
                .images
                .values()
                .map(|image| (image.generation, image.id, image.identity, image.bytes()))
                .collect::<Vec<_>>();
            images.sort_unstable();
            for (_, id, identity, bytes) in images {
                if let Ok(token) = quota.reserve(self.quota_owner, None, bytes) {
                    self.quota_images.insert(id, (token, identity));
                } else {
                    self.images.remove(&id);
                    self.placements.remove_image(id);
                }
            }
        }
        let placements = self
            .placements
            .iter()
            .map(|p| (p.image_id, p.placement_id))
            .collect::<Vec<_>>();
        for key in placements {
            if self
                .images
                .get(&key.0)
                .is_some_and(|image| image.tmux_sixel_cell.is_some())
            {
                continue;
            }
            if let Some(&(parent, _)) = self.quota_images.get(&key.0)
                && let Ok(token) = quota.reserve(self.quota_owner, Some(parent), 0)
            {
                self.quota_placements.insert(key, token);
            } else {
                self.placements.remove(key);
            }
        }
        self.prune_quota();
    }
    fn copy_sources(&mut self, source: &Self) {
        let before = (self.images.len(), self.placements.len());
        self.placements.retain(|placement| {
            self.images
                .get(&placement.image_id)
                .map(|image| image.identity)
                == source
                    .images
                    .get(&placement.image_id)
                    .map(|image| image.identity)
        });
        self.images = source.images.clone();
        self.quota_images = source.quota_images.clone();
        self.loading = source.loading.clone();
        self.next_image = source.next_image;
        self.sync_quota();
        if before != (self.images.len(), self.placements.len()) {
            self.generation = self.generation.wrapping_add(1);
        }
    }
    pub(crate) fn without_placements(&self) -> Self {
        let mut sources = self.clone();
        sources.placements = Placements::default();
        sources.quota_placements.clear();
        sources
    }
    pub(crate) fn release_placement_quota(&mut self) {
        if let Some(quota) = &self.quota {
            for token in self.quota_placements.values() {
                quota.release(*token);
            }
        }
        self.quota_placements.clear();
    }
    pub(crate) fn release_quota(&mut self) {
        if let Some(quota) = &self.quota {
            quota.release_owner(self.quota_owner);
        }
        self.quota_images.clear();
        self.quota_placements.clear();
        self.quota = None;
    }
    fn sync_quota(&mut self) {
        let Some(quota) = &self.quota else {
            return;
        };
        let before = (self.quota_images.len(), self.quota_placements.len());
        self.quota_images.retain(|id, &mut (token, identity)| {
            if self
                .images
                .get(id)
                .is_some_and(|image| image.identity == identity)
            {
                true
            } else {
                quota.release(token);
                false
            }
        });
        self.quota_placements.retain(|key, token| {
            if self.placements.get(*key).is_some() {
                true
            } else {
                quota.release(*token);
                false
            }
        });
        self.prune_quota();
        if before != (self.quota_images.len(), self.quota_placements.len()) {
            self.generation = self.generation.wrapping_add(1);
        }
    }
    fn prune_quota(&mut self) {
        let Some(quota) = &self.quota else {
            return;
        };
        let before = (self.images.len(), self.placements.len());
        self.quota_images.retain(|id, (token, _)| {
            if quota.contains(*token) {
                true
            } else {
                self.images.remove(id);
                self.placements.remove_image(*id);
                false
            }
        });
        self.quota_placements.retain(|key, token| {
            if quota.contains(*token) {
                true
            } else {
                self.placements.remove(*key);
                false
            }
        });
        self.reap_orphans();
        if before != (self.images.len(), self.placements.len()) {
            self.generation = self.generation.wrapping_add(1);
        }
    }
    pub fn view_snapshot(&self) -> ViewSnapshot {
        let mut images: Vec<_> = self.images.values().cloned().collect();
        images.sort_by_key(|image| image.id);
        let mut placements = self.placements.to_vec();
        placements.sort_by_key(|placement| (placement.image_id, placement.placement_id));
        ViewSnapshot {
            images,
            placements,
            generation: self.generation,
            limit: self.limit,
            loading: self.loading.clone(),
            next_image: self.next_image,
            next_placement: self.next_placement,
        }
    }

    /// Validate all decoded dimensions and references before replacing live state.
    pub fn from_view_snapshot(snapshot: ViewSnapshot) -> Result<Self, &'static str> {
        Self::from_view_snapshot_with_limit(snapshot, MAX_DATA)
    }
    pub fn from_view_snapshot_with_limit(
        snapshot: ViewSnapshot,
        max_bytes: usize,
    ) -> Result<Self, &'static str> {
        let mut result = Self::default();
        let mut used = 0usize;
        for image in snapshot.images {
            let length = usize::try_from(image.width)
                .ok()
                .and_then(|w| w.checked_mul(image.height as usize))
                .and_then(|n| n.checked_mul(4))
                .ok_or("image dimensions overflow")?;
            if image.width == 0
                || image.height == 0
                || image.width > MAX_DIMENSION
                || image.height > MAX_DIMENSION
                || image.pixels.len() != length
                || image.tmux_sixel_cell.is_some_and(|cell| {
                    cell.into_iter()
                        .any(|size| size == 0 || size > MAX_DIMENSION)
                })
                || image
                    .frames
                    .iter()
                    .any(|frame| frame.pixels.len() != length)
                || image.current_frame > image.frames.len()
            {
                return Err("invalid graphics image");
            }
            used = used
                .checked_add(
                    length
                        .checked_mul(image.frames.len() + 1)
                        .ok_or("image allocation overflow")?,
                )
                .ok_or("image allocation overflow")?;
            if used > max_bytes || result.images.contains_key(&image.id) {
                return Err("excessive or duplicate graphics image");
            }
            result.images.insert(image.id, image);
        }
        for placement in snapshot.placements {
            if !result.images.contains_key(&placement.image_id)
                || result
                    .placements
                    .get((placement.image_id, placement.placement_id))
                    .is_some()
                || placement.col > u16::MAX as usize
                || placement.rows > MAX_DIMENSION
                || placement.columns > MAX_DIMENSION
            {
                return Err("invalid graphics placement");
            }
            result.placements.insert(placement);
        }
        for placement in result.placements.iter() {
            let mut parent = placement.parent;
            let mut visited = HashSet::from([(placement.image_id, placement.placement_id)]);
            while let Some(key) = parent {
                if !visited.insert(key) || visited.len() > PARENT_CHAIN_LIMIT + 1 {
                    return Err("invalid graphics parent chain");
                }
                parent = result
                    .placements
                    .get(key)
                    .ok_or("missing graphics parent")?
                    .parent;
            }
        }
        if snapshot.loading.as_ref().is_some_and(|loading| {
            loading.data.len() > MAX_DATA || loading.command.values.len() > 52
        }) {
            return Err("excessive pending graphics upload");
        }
        result.generation = snapshot.generation;
        result.limit = snapshot.limit.min(max_bytes);
        result.loading = snapshot.loading;
        result.next_image = snapshot.next_image;
        result.next_placement = snapshot.next_placement;
        Ok(result)
    }
}

impl Graphics {
    /// Resolve a Unicode placeholder's optional application-supplied placement ID.
    pub fn placeholder_target(&self, image_id: u32, placement_id: u32) -> Option<&Placement> {
        if placement_id != 0 {
            return self
                .placements
                .get((image_id, PlacementId::External(placement_id)));
        }
        self.placements
            .iter()
            .filter(|p| p.image_id == image_id && p.virtual_placement)
            .min_by_key(|p| match p.placement_id {
                PlacementId::External(id) => (false, id),
                PlacementId::Internal(id) => (true, id),
            })
    }

    fn resolve_parent(
        &self,
        child: Option<(u32, PlacementId)>,
        image_id: u32,
        placement_id: u32,
    ) -> Result<(u32, PlacementId), &'static str> {
        if !self.images.contains_key(&image_id) {
            return Err("ENOPARENT: parent image not found");
        }
        let parent = if placement_id != 0 {
            self.placements
                .get((image_id, PlacementId::External(placement_id)))
        } else {
            self.placements
                .iter()
                .filter(|p| p.image_id == image_id)
                .min_by_key(|p| match p.placement_id {
                    PlacementId::External(id) => (false, id),
                    PlacementId::Internal(id) => (true, id),
                })
        }
        .ok_or("ENOPARENT: parent placement not found")?;
        let parent_key = (parent.image_id, parent.placement_id);
        if child == Some(parent_key) {
            return Err("EINVAL: placement cannot be its own parent");
        }
        let mut key = parent_key;
        for depth in 1..=PARENT_CHAIN_LIMIT {
            if child == Some(key) {
                return Err("ECYCLE: parent chain creates a cycle");
            }
            let placement = self
                .placements
                .get(key)
                .ok_or("ENOENT: parent chain ancestor not found")?;
            let Some(next) = placement.parent else {
                return Ok(parent_key);
            };
            if depth == PARENT_CHAIN_LIMIT {
                return Err("ETOODEEP: parent chain too deep");
            }
            key = next;
        }
        unreachable!("the bounded parent walk always returns")
    }

    pub(crate) fn snapshot(&self, screen: &Screen) -> Self {
        let mut placements = if self.placements.is_empty() {
            Placements::default()
        } else {
            self.placements.clone()
        };
        if !placements.is_empty() {
            let start = screen.history_len().saturating_sub(screen.viewport_offset) as i64;
            let anchors: HashSet<_> = placements.iter().map(|p| p.row).collect();
            let offsets: HashMap<_, _> = screen
                .all_rows()
                .enumerate()
                .filter(|(_, row)| anchors.contains(&row.id))
                .map(|(index, row)| (row.id, index as i64 - start))
                .collect();
            placements.project_rows(&offsets);
        }
        Self {
            images: self.images.clone(),
            quota: self.quota.clone(),
            quota_owner: self.quota_owner,
            quota_images: self.quota_images.clone(),
            quota_placements: self.quota_placements.clone(),
            placements,
            generation: self.generation,
            limit: self.limit,
            loading: None,
            next_image: self.next_image,
            next_placement: self.next_placement,
        }
    }
    pub fn bytes_used(&self) -> usize {
        self.images.values().map(Image::bytes).sum()
    }
    pub(crate) fn discard_row(&mut self, row: u64) {
        self.placements.retain(|p| p.row != row);
    }

    fn reap_orphans(&mut self) -> HashSet<u32> {
        let mut removed = HashSet::new();
        if !self.placements.has_relatives() {
            return removed;
        }
        loop {
            let orphans: Vec<_> = self
                .placements
                .iter()
                .filter(|p| {
                    p.parent
                        .is_some_and(|parent| self.placements.get(parent).is_none())
                })
                .map(|p| (p.image_id, p.placement_id))
                .collect();
            if orphans.is_empty() {
                return removed;
            }
            for key in orphans {
                self.placements.remove(key);
                removed.insert(key.0);
            }
        }
    }
    pub(crate) fn reflow(&mut self, points: &HashMap<(u64, usize), GridPoint>) {
        self.placements.retain_mut(|p| {
            if p.virtual_placement || p.parent.is_some() {
                return true;
            }
            let Some(point) = points.get(&(p.row, p.col)) else {
                return false;
            };
            p.row = point.row;
            p.col = point.col;
            true
        });
    }
    fn resolve_id(&mut self, id: u32, number: u32) -> Option<u32> {
        if id != 0 {
            self.images
                .get(&id)
                .filter(|image| !image.tmux_unmapped)
                .map(|_| id)
        } else if number != 0 {
            self.images.resolve_number(number)
        } else {
            None
        }
    }
    fn allocate_tmux_id(&mut self) -> u32 {
        let mut id = if self.next_image == 0 {
            u32::MAX
        } else {
            self.next_image
        };
        while id == 0 || self.images.contains_key(&id) {
            id = id.wrapping_sub(1);
        }
        self.next_image = id.wrapping_sub(1);
        id
    }
    fn rekey_image(&mut self, old: u32, new: u32) {
        let Some(mut image) = self.images.remove(&old) else {
            return;
        };
        image.id = new;
        self.images.insert(new, image);
        if let Some(token) = self.quota_images.remove(&old) {
            self.quota_images.insert(new, token);
        }
        self.placements.retain_mut(|placement| {
            if placement.image_id == old {
                placement.image_id = new;
            }
            if let Some(parent) = &mut placement.parent
                && parent.0 == old
            {
                parent.0 = new;
            }
            true
        });
        let tokens = std::mem::take(&mut self.quota_placements);
        self.quota_placements = tokens
            .into_iter()
            .map(|((id, placement), token)| ((if id == old { new } else { id }, placement), token))
            .collect();
        self.generation = self.generation.wrapping_add(1);
    }
    fn allocate_id(&mut self, implicit: bool) -> u32 {
        // Numbered uploads choose the lowest free client ID. Anonymous uploads
        // use a separate counter so they rarely collide with client choices.
        if !implicit {
            return self.images.allocate_id();
        }
        let mut id = self.next_image;
        while id == 0 || self.images.contains_key(&id) {
            id = id.wrapping_add(1);
        }
        self.next_image = id.wrapping_add(1);
        id
    }
    fn reserve(&mut self, bytes: usize, exclude: u32) -> Result<(), &'static str> {
        if bytes > self.limit {
            return Err("ENOMEM: out of memory");
        }
        while self.images.cached_bytes_used().saturating_add(bytes) > self.limit {
            let Some(id) = self
                .images
                .values()
                .filter(|i| i.id != exclude)
                .min_by_key(|i| i.generation)
                .map(|i| i.id)
            else {
                return Err("ENOMEM: out of memory");
            };
            self.images.remove(&id);
            self.placements.remove_image(id);
        }
        Ok(())
    }
    /// Advance animations against the host's monotonic clock; return next deadline.
    pub fn tick(&mut self, now_ms: u64) -> Option<u64> {
        let mut next = None;
        for image in self.images.values_mut() {
            if image.animation_state < 2
                || image.frames.is_empty()
                || image.max_loops != 0 && image.completed_loops >= image.max_loops
            {
                continue;
            }
            if image.root_gap_ms == 0 && image.frames.iter().all(|f| f.gap_ms == 0) {
                continue;
            }
            // Unplaced images must not keep the renderer awake.
            if !self.placements.iter().any(|p| p.image_id == image.id) {
                continue;
            }
            let shown = image.frame_shown_at_ms.unwrap_or(now_ms).min(now_ms);
            image.frame_shown_at_ms = Some(shown);
            let mut deadline = shown.saturating_add(u64::from(image.gap(image.current_frame)));
            if now_ms >= deadline {
                // Advance once, skipping gapless frames without ever displaying
                // them. Parking at the loop boundary retains the displayed frame.
                let mut index = image.current_frame;
                loop {
                    let following = (index + 1) % (image.frames.len() + 1);
                    if following == 0 {
                        if image.animation_state == 2 {
                            break;
                        }
                        image.completed_loops = image.completed_loops.saturating_add(1);
                        if image.max_loops != 0 && image.completed_loops >= image.max_loops {
                            break;
                        }
                    }
                    index = following;
                    if image.gap(index) != 0 {
                        image.current_frame = index;
                        image.frame_shown_at_ms = Some(now_ms);
                        self.generation = self.generation.wrapping_add(1);
                        image.generation = self.generation;
                        deadline = now_ms.saturating_add(u64::from(image.gap(index)));
                        break;
                    }
                }
            }
            if deadline > now_ms {
                next = Some(next.map_or(deadline, |n: u64| n.min(deadline)));
            }
        }
        next
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Command {
    values: BTreeMap<u8, i64>,
    #[serde(default)]
    tmux: bool,
    #[serde(default)]
    image_limit: Option<usize>,
}

impl Command {
    fn parse(bytes: &[u8]) -> Option<(Self, &[u8])> {
        let bytes = bytes.strip_prefix(b"G")?;
        let split = bytes.iter().position(|&b| b == b';').unwrap_or(bytes.len());
        let mut command = Self::default();
        for pair in bytes[..split].split(|&b| b == b',') {
            if pair.is_empty() {
                continue;
            }
            let equal = pair.iter().position(|&b| b == b'=')?;
            let (key, value) = (&pair[..equal], &pair[equal + 1..]);
            if key.len() != 1 || !key[0].is_ascii_alphabetic() {
                continue;
            }
            let value = std::str::from_utf8(value).ok()?;
            let number = if value.len() == 1 && !value.as_bytes()[0].is_ascii_digit() {
                i64::from(value.as_bytes()[0])
            } else if matches!(key[0], b'z' | b'H' | b'V') {
                i64::from(value.parse::<i32>().ok()?)
            } else {
                i64::from(value.parse::<u32>().ok()?)
            };
            command.values.insert(key[0], number);
        }
        Some((command, bytes.get(split + 1..).unwrap_or(b"")))
    }
    fn limit(&self) -> usize {
        if self.tmux {
            self.image_limit.unwrap_or(64 * 1024 * 1024).min(MAX_DATA)
        } else {
            MAX_DATA
        }
    }
    fn n(&self, key: u8) -> u32 {
        self.values.get(&key).copied().unwrap_or(0) as u32
    }
    fn signed(&self, key: u8) -> i32 {
        self.n(key) as i32
    }
    fn action(&self) -> u8 {
        self.values.get(&b'a').copied().unwrap_or(i64::from(b't')) as u8
    }
    fn quiet(&self) -> u32 {
        self.n(b'q')
    }
    fn reply(&self, id: u32, frame: usize, message: &str, effects: &mut Vec<Effect>) {
        if self.quiet() >= 2
            || self.quiet() == 1 && message == "OK"
            || (id == 0 && self.n(b'I') == 0 && !(self.tmux && self.action() == b'q'))
        {
            return;
        }
        let mut values = Vec::new();
        if id != 0 || (self.tmux && self.action() == b'q') {
            values.push(format!("i={id}"));
        }
        if self.n(b'I') != 0 {
            values.push(format!("I={}", self.n(b'I')));
        }
        if self.n(b'p') != 0 {
            values.push(format!("p={}", self.n(b'p')));
        }
        if frame != 0 {
            values.push(format!("r={frame}"));
        }
        let message = if self.tmux {
            message.split(':').next().unwrap_or(message)
        } else {
            message
        };
        effects.push(Effect::Write(
            format!("\x1b_G{};{message}\x1b\\", values.join(",")).into_bytes(),
        ));
    }
}

impl Terminal {
    pub fn set_tmux_image_quota(&mut self, quota: quota::Quota) {
        self.primary.graphics.attach_quota(quota.clone(), None);
        if let Some(alt) = &mut self.alternate {
            alt.graphics
                .attach_quota(quota.clone(), Some(&self.primary.graphics));
        }
        self.tmux_image_quota = Some(quota);
        self.prune_tmux_images();
    }
    /// Rebind an engine clone to a fork containing the same owner and token IDs.
    pub fn rebind_tmux_image_quota(&mut self, quota: quota::Quota) {
        if self.primary.graphics.quota.is_some() {
            self.primary.graphics.quota = Some(quota.clone());
        }
        if let Some(alt) = &mut self.alternate
            && alt.graphics.quota.is_some()
        {
            alt.graphics.quota = Some(quota.clone());
        }
        self.tmux_image_quota = Some(quota);
    }
    pub fn tmux_image_owners(&self) -> impl Iterator<Item = u64> + '_ {
        std::iter::once(&self.primary)
            .chain(self.alternate.iter())
            .filter_map(|screen| {
                screen
                    .graphics
                    .quota
                    .as_ref()
                    .map(|_| screen.graphics.quota_owner)
            })
    }
    pub fn uses_tmux_image_quota(&self, quota: &quota::Quota) -> bool {
        self.tmux_image_quota
            .as_ref()
            .is_some_and(|current| current.same(quota))
    }
    pub fn prune_tmux_images(&mut self) {
        let before = (
            self.primary.graphics.generation,
            self.alternate.as_ref().map(|alt| alt.graphics.generation),
        );
        if let Some(quota) = self.tmux_image_quota.clone() {
            self.primary.graphics.attach_quota(quota.clone(), None);
            if let Some(alt) = &mut self.alternate {
                alt.graphics
                    .attach_quota(quota, Some(&self.primary.graphics));
            }
        }
        if let Some(quota) = &self.tmux_image_quota {
            for (&id, image) in &*self.primary.graphics.images {
                if image.tmux_unmapped
                    && image.tmux_sixel_cell.is_none()
                    && let Some(&(token, _)) = self.primary.graphics.quota_images.get(&id)
                {
                    quota.unmap(token);
                }
            }
        }
        self.primary.graphics.sync_quota();
        if let Some(alt) = &mut self.alternate {
            alt.graphics.sync_quota();
        }
        if self.tmux_graphics {
            let placed = self
                .primary
                .graphics
                .placements
                .iter()
                .chain(
                    self.alternate
                        .iter()
                        .flat_map(|alt| alt.graphics.placements.iter()),
                )
                .map(|placement| placement.image_id)
                .collect::<HashSet<_>>();
            for graphics in std::iter::once(&mut self.primary.graphics)
                .chain(self.alternate.iter_mut().map(|alt| &mut alt.graphics))
            {
                graphics
                    .images
                    .retain(|id, image| !image.tmux_unmapped || placed.contains(id));
                graphics.sync_quota();
            }
        }
        if before
            != (
                self.primary.graphics.generation,
                self.alternate.as_ref().map(|alt| alt.graphics.generation),
            )
        {
            self.generation = self.generation.wrapping_add(1);
        }
    }
    pub(crate) fn synchronize_tmux_sources(&mut self) {
        if !self.tmux_graphics {
            return;
        }
        if let Some(alt) = &mut self.alternate {
            if self.alternate_active {
                self.primary.graphics.copy_sources(&alt.graphics);
            } else {
                alt.graphics.copy_sources(&self.primary.graphics);
            }
        }
    }
    pub fn graphics(&self) -> &Graphics {
        &self.screen().graphics
    }
    pub fn set_graphics_limit(&mut self, bytes: usize) {
        self.primary.graphics.limit = bytes;
        let _ = self.primary.graphics.reserve(0, 0);
        if let Some(alt) = &mut self.alternate {
            alt.graphics.limit = bytes;
            let _ = alt.graphics.reserve(0, 0);
        }
    }
    pub fn tick_graphics(&mut self, now_ms: u64) -> Option<u64> {
        let before = self.screen().graphics.generation;
        let next = self.screen_mut().graphics.tick(now_ms);
        if self.screen().graphics.generation != before {
            self.generation = self.generation.wrapping_add(1);
        }
        next
    }
    pub(crate) fn graphics_command(&mut self, bytes: &[u8], effects: &mut Vec<Effect>) {
        let generation = self.screen().graphics.generation;
        self.prune_tmux_images();
        self.graphics_command_inner(bytes, effects);
        self.synchronize_tmux_sources();
        self.prune_tmux_images();
        if self.screen().graphics.generation != generation {
            self.generation = self.generation.wrapping_add(1);
        }
    }
    fn graphics_command_inner(&mut self, bytes: &[u8], effects: &mut Vec<Effect>) {
        if self.graphics().limit == 0 {
            return;
        }
        let Some((mut command, payload)) = Command::parse(bytes) else {
            return;
        };
        command.tmux = self.tmux_graphics;
        command.image_limit = Some(self.tmux_image_limit);
        let mut id = command.n(b'i');
        if id != 0 && command.n(b'I') != 0 {
            command.reply(
                id,
                0,
                "EINVAL: image ID and number are mutually exclusive",
                effects,
            );
            return;
        }
        let action = command.action();
        if !matches!(
            action,
            b't' | b'T' | b'q' | b'p' | b'd' | b'f' | b'a' | b'c'
        ) {
            return;
        }
        if self.tmux_graphics
            && matches!(action, b't' | b'T')
            && self
                .graphics()
                .images
                .get(&id)
                .is_some_and(|image| image.tmux_unmapped)
        {
            let replacement = self.screen_mut().graphics.allocate_tmux_id();
            self.primary.graphics.rekey_image(id, replacement);
            if let Some(alt) = &mut self.alternate {
                alt.graphics.rekey_image(id, replacement);
            }
        }
        if action == b'd' {
            if self.tmux_graphics
                && (command.n(b'I') != 0
                    || command.signed(b'z') != 0
                    || !matches!(
                        command
                            .values
                            .get(&b'd')
                            .copied()
                            .unwrap_or(i64::from(b'a')) as u8,
                        b'a' | b'A' | b'i' | b'I'
                    ))
            {
                command.reply(id, 0, "ENOTSUP", effects);
                return;
            }
            self.graphics_delete(&command);
            return;
        }
        if action == b'p' {
            let error = if id == 0 && command.n(b'I') == 0 {
                Some("EINVAL: image ID or number required")
            } else if command.n(b'U') != 0 && command.n(b'P') != 0 {
                Some("EINVAL: virtual placement cannot refer to a parent")
            } else {
                None
            };
            if let Some(error) = error {
                command.reply(id, 0, error, effects);
                return;
            }
        }
        if matches!(action, b'p' | b'a' | b'c') {
            let Some(resolved) = self.screen_mut().graphics.resolve_id(id, command.n(b'I')) else {
                command.reply(id, 0, "ENOENT: image not found", effects);
                return;
            };
            id = resolved;
            let result = match action {
                b'p' => self.graphics_place(id, &command),
                b'a' => self.graphics_animation(id, &command),
                _ => self.graphics_compose(id, &command),
            };
            if let Err(error) = result {
                command.reply(id, 0, error, effects);
            } else if matches!(action, b'p' | b'c') {
                command.reply(id, 0, "OK", effects);
            }
            return;
        }
        // A new transmission replaces an explicit image immediately, including
        // its placements and orphaned descendants, even if decoding fails.
        if !self.tmux_graphics
            && matches!(action, b't' | b'T')
            && id != 0
            && self.graphics().loading.is_none()
        {
            self.graphics_delete(&Command {
                values: [(b'd', i64::from(b'I')), (b'i', i64::from(id))].into(),
                ..Command::default()
            });
        }
        let engine = base64::engine::general_purpose::GeneralPurpose::new(
            &base64::alphabet::STANDARD,
            base64::engine::general_purpose::GeneralPurposeConfig::new()
                .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
        );
        if self.tmux_graphics && payload.len() > command.limit() {
            let quiet = command.values.get(&b'q').copied();
            if let Some(loading) = self.screen_mut().graphics.loading.take() {
                command = loading.command;
                id = loading.image_id;
                if let Some(quiet) = quiet {
                    command.values.insert(b'q', quiet);
                }
            }
            command.reply(id, 0, "EINVAL", effects);
            return;
        }
        let filtered;
        let payload = if self.tmux_graphics {
            filtered = payload
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect::<Vec<_>>();
            filtered.as_slice()
        } else {
            payload
        };
        let Ok(mut data) = engine.decode(payload) else {
            let quiet = command.values.get(&b'q').copied();
            if self.tmux_graphics
                && let Some(loading) = self.screen_mut().graphics.loading.take()
            {
                command = loading.command;
                id = loading.image_id;
                if let Some(quiet) = quiet {
                    command.values.insert(b'q', quiet);
                }
            }
            command.reply(id, 0, "EINVAL: invalid data", effects);
            return;
        };
        let medium = command
            .values
            .get(&b't')
            .copied()
            .unwrap_or(i64::from(b'd')) as u8;
        if medium != b'd' && medium != b's' {
            command.reply(id, 0, "EINVAL: unsupported medium", effects);
            return;
        }
        if medium == b's' {
            // Local transports carry the complete image even with m=1.
            command.values.remove(&b'm');
        }
        let mut image_generation = None;
        if (action != b'q' || self.tmux_graphics)
            && let Some(mut loading) = self.screen_mut().graphics.loading.take()
        {
            if loading.data.len().saturating_add(data.len()) > command.limit() {
                let quiet = command.values.get(&b'q').copied();
                command = loading.command;
                id = loading.image_id;
                if let Some(quiet) = quiet {
                    command.values.insert(b'q', quiet);
                }
                command.reply(
                    id,
                    0,
                    if self.tmux_graphics {
                        "EINVAL"
                    } else {
                        "ENOMEM: out of memory"
                    },
                    effects,
                );
                return;
            }
            loading.data.append(&mut data);
            data = loading.data;
            let more = command.n(b'm');
            let quiet = command.quiet();
            command = loading.command;
            command.image_limit = Some(self.tmux_image_limit);
            command.values.insert(b'm', i64::from(more));
            if quiet > 0 {
                command.values.insert(b'q', i64::from(quiet));
            }
            id = loading.image_id;
            image_generation = loading.image_generation;
        } else {
            if matches!(action, b't' | b'T')
                && !matches!(
                    command.values.get(&b'f').copied().unwrap_or(32),
                    24 | 32 | 100
                )
            {
                command.reply(id, 0, "EINVAL: unsupported format", effects);
                return;
            }
            if action == b'f' {
                let Some(resolved) = self.screen_mut().graphics.resolve_id(id, command.n(b'I'))
                else {
                    command.reply(
                        id,
                        command.n(b'r') as usize,
                        "ENOENT: image not found",
                        effects,
                    );
                    return;
                };
                id = resolved;
                image_generation = Some(self.graphics().images[&id].identity);
                command.values.insert(b'i', i64::from(id));
            } else if action != b'q' && id == 0 {
                id = if self.tmux_graphics && command.n(b'I') == 0 {
                    self.screen_mut().graphics.allocate_tmux_id()
                } else {
                    self.screen_mut().graphics.allocate_id(command.n(b'I') == 0)
                };
            }
            if medium == b's' {
                #[cfg(all(unix, not(target_os = "android")))]
                let loaded = shm::read(&command, &data);
                #[cfg(not(all(unix, not(target_os = "android"))))]
                let loaded: Result<Vec<u8>, &str> = Err("EINVAL: unsupported medium");
                match loaded {
                    Ok(bytes) => data = bytes,
                    Err(error) => {
                        command.reply(command.n(b'i'), 0, error, effects);
                        return;
                    }
                }
            }
        }
        if (action != b'q' || self.tmux_graphics) && command.n(b'm') != 0 {
            self.screen_mut().graphics.loading = Some(Loading {
                command,
                data,
                image_id: id,
                image_generation,
            });
            return;
        }
        let (width, height, pixels) = match decode_image(&command, data) {
            Ok(decoded) => decoded,
            Err(error) => {
                command.reply(command.n(b'i'), 0, error, effects);
                return;
            }
        };
        if command.action() == b'q' {
            let storage = &mut self.screen_mut().graphics;
            let result = if let Some(quota) = &storage.quota {
                quota
                    .reserve(storage.quota_owner, None, pixels.len())
                    .map(|token| quota.release(token))
            } else {
                Ok(())
            };
            storage.prune_quota();
            command.reply(id, 0, result.err().unwrap_or("OK"), effects);
            return;
        }
        if command.action() == b'f' {
            let result =
                self.graphics_frame(id, &command, width, height, &pixels, image_generation);
            match result {
                Ok(frame) => command.reply(id, frame, "OK", effects),
                Err(error) => command.reply(id, command.n(b'r') as usize, error, effects),
            }
            return;
        }
        let implicit = command.n(b'i') == 0 && command.n(b'I') == 0;
        let storage = &mut self.screen_mut().graphics;
        let token = if let Some(quota) = &storage.quota {
            let result = quota.reserve(storage.quota_owner, None, pixels.len());
            storage.prune_quota();
            match result {
                Ok(token) => Some(token),
                Err(error) => {
                    command.reply(command.n(b'i'), 0, error, effects);
                    return;
                }
            }
        } else {
            let old_size = storage.images.get(&id).map_or(0, Image::bytes);
            if let Err(error) = storage.reserve(pixels.len().saturating_sub(old_size), id) {
                command.reply(command.n(b'i'), 0, error, effects);
                return;
            }
            None
        };
        if implicit && command.tmux && command.action() == b't' {
            if let Some(token) = token {
                storage.quota.as_ref().unwrap().release(token);
            }
            return;
        }
        // C retains the previous mapping until both new objects have been admitted.
        let previous = token.map(|_| storage.clone());
        storage.generation = storage.generation.wrapping_add(1);
        storage.placements.remove_image(id);
        storage.reap_orphans();
        storage.images.insert(
            id,
            Image {
                id,
                tmux_unmapped: implicit && command.tmux,
                tmux_sixel_cell: None,
                tmux_order: command.tmux.then_some(token.unwrap_or(storage.generation)),
                number: command.n(b'I'),
                width,
                height,
                pixels: pixels::intern(width, height, pixels),
                generation: storage.generation,
                identity: storage.generation,
                frames: Vec::new(),
                current_frame: 0,
                root_gap_ms: 0,
                animation_state: 1,
                max_loops: 0,
                completed_loops: 0,
                frame_shown_at_ms: None,
            },
        );
        if let Some(token) = token {
            storage.quota_images.insert(id, (token, storage.generation));
        }
        let result = if command.action() == b'T' {
            self.graphics_place(id, &command)
        } else {
            Ok(())
        };
        if let Some(mut previous) = previous {
            let storage = &mut self.screen_mut().graphics;
            let quota = storage.quota.as_ref().unwrap().clone();
            if result.is_err() {
                quota.release(token.unwrap());
                previous.prune_quota();
                previous.generation = storage.generation.wrapping_add(1);
                *storage = previous;
            } else {
                if let Some(&(old, _)) = previous.quota_images.get(&id) {
                    quota.release(old);
                }
                for (key, old) in previous.quota_placements {
                    if key.0 == id {
                        quota.release(old);
                    }
                }
            }
        }
        if implicit
            && command.tmux
            && result.is_ok()
            && let Some(token) = token
        {
            self.screen().graphics.quota.as_ref().unwrap().unmap(token);
        }
        if !implicit {
            command.reply(id, 0, result.err().unwrap_or("OK"), effects);
        }
        self.generation = self.generation.wrapping_add(1);
    }

    fn graphics_place(&mut self, id: u32, cmd: &Command) -> Result<(), &'static str> {
        if cmd.n(b'U') != 0 && cmd.n(b'P') != 0 {
            return Err("EINVAL: virtual placement cannot refer to a parent");
        }
        if !self.graphics().images.contains_key(&id) {
            return Err("ENOENT: image not found");
        }
        let mut cell = [
            self.width_px / u32::from(self.cols),
            self.height_px / u32::from(self.rows),
        ];
        if self.tmux_graphics {
            if cell[0] == 0 {
                cell[0] = 8;
            }
            if cell[1] == 0 {
                cell[1] = 16;
            }
        }
        let mut offset = [cmd.n(b'X'), cmd.n(b'Y')];
        for axis in 0..2 {
            if cell[axis] > 0 {
                offset[axis] = offset[axis].min(cell[axis] - 1);
            }
        }
        let parent = if cmd.n(b'P') != 0 {
            self.screen_mut().graphics.reap_orphans();
            Some(self.graphics().resolve_parent(
                (cmd.n(b'p') != 0).then_some((id, PlacementId::External(cmd.n(b'p')))),
                cmd.n(b'P'),
                cmd.n(b'Q'),
            )?)
        } else {
            None
        };
        let cursor = self.screen().cursor.clone();
        let row = self.screen().row(cursor.row).id;
        let mut placement = Placement {
            image_id: id,
            placement_id: PlacementId::External(cmd.n(b'p')),
            row,
            col: cursor.col,
            columns: cmd.n(b'c'),
            rows: cmd.n(b'r'),
            viewport_row: None,
            z: cmd.signed(b'z'),
            source: [cmd.n(b'x'), cmd.n(b'y'), cmd.n(b'w'), cmd.n(b'h')],
            offset,
            virtual_placement: cmd.n(b'U') != 0,
            parent,
            parent_offset: [cmd.signed(b'H'), cmd.signed(b'V')],
        };
        if self.tmux_graphics
            && placement.source_rect(&self.graphics().images[&id])[2..].contains(&0)
        {
            return Err("EINVAL");
        }
        let [columns, rows] = placement.grid_size(&self.graphics().images[&id], cell);
        if self.tmux_graphics
            && (columns > u16::MAX.into()
                || rows > u16::MAX.into()
                || u64::from(columns) * u64::from(rows) > cmd.limit() as u64)
        {
            return Err("EINVAL");
        }
        let graphics = &mut self.screen_mut().graphics;
        let placement_id = if cmd.n(b'p') != 0 {
            PlacementId::External(cmd.n(b'p'))
        } else {
            let n = graphics.next_placement;
            graphics.next_placement = n.wrapping_add(1);
            PlacementId::Internal(n)
        };
        placement.placement_id = placement_id;
        if let Some(quota) = graphics.quota.clone() {
            let parent = graphics
                .quota_images
                .get(&id)
                .map(|&(token, _)| token)
                .ok_or("ENOENT")?;
            let result = quota.reserve(graphics.quota_owner, Some(parent), 0);
            graphics.prune_quota();
            let token = result?;
            if let Some(old) = graphics.quota_placements.insert((id, placement_id), token) {
                quota.release(old);
            }
            quota.touch(token);
        }
        graphics.placements.insert(placement);
        graphics.generation = graphics.generation.wrapping_add(1);
        if cmd.n(b'C') != 1 && cmd.n(b'U') == 0 && parent.is_none() {
            let target = cursor.col.saturating_add(columns as usize);
            let wraps = target >= self.cols as usize;
            let requested = rows.saturating_sub(1) as usize + usize::from(wraps);
            let before = if (self.margins.top..=self.margins.bottom).contains(&cursor.row)
                && (self.margins.left..=self.margins.right).contains(&cursor.col)
            {
                self.margins.bottom - cursor.row
            } else {
                0
            };
            for _ in 0..requested.min(before + self.rows as usize) {
                self.index();
            }
            self.screen_mut().cursor.col = if wraps { 0 } else { target };
            self.screen_mut().cursor.pending_wrap = false;
        }
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }

    fn graphics_frame(
        &mut self,
        id: u32,
        cmd: &Command,
        width: u32,
        height: u32,
        pixels: &[u8],
        generation: Option<u64>,
    ) -> Result<usize, &'static str> {
        let image = self
            .graphics()
            .images
            .get(&id)
            .ok_or("ENOENT: image not found")?;
        if generation.is_some_and(|g| g != image.identity) {
            return Err("ENOENT: image not found");
        }
        if width > image.width || height > image.height {
            return Err("EINVAL: frame dimensions exceed image");
        }
        let count = image.frames.len() + 1;
        let number = if cmd.n(b'r') == 0 || cmd.n(b'r') as usize > count + 1 {
            count + 1
        } else {
            cmd.n(b'r') as usize
        };
        let bytes = image.pixels.len();
        let new = number == count + 1;
        let mut canvas = if new {
            if cmd.n(b'c') != 0 {
                image
                    .frame(cmd.n(b'c') as usize)
                    .ok_or("EINVAL: base frame not found")?
                    .to_vec()
            } else {
                let background = cmd.n(b'Y').to_be_bytes();
                background.repeat(bytes / 4)
            }
        } else {
            image.frame(number).unwrap().to_vec()
        };
        let image_width = image.width;
        let image_height = image.height;
        if new {
            self.screen_mut()
                .graphics
                .reserve(bytes, id)
                .map_err(|_| "ENOSPC: animation frame storage full")?;
        }
        compose(
            &mut canvas,
            image_width,
            image_height,
            pixels,
            width,
            height,
            cmd.n(b'x'),
            cmd.n(b'y'),
            cmd.n(b'X') == 1,
        );
        let image = self.screen_mut().graphics.images.get_mut(&id).unwrap();
        let gap = cmd.signed(b'z');
        let pixels = pixels::intern(image_width, image_height, canvas);
        if new {
            image.frames.push(AnimationFrame {
                pixels,
                gap_ms: if gap == 0 { 40 } else { gap.max(0) as u32 },
            });
        } else {
            image.set_frame(number, pixels);
            if gap != 0 {
                if number == 1 {
                    image.root_gap_ms = gap.max(0) as u32;
                } else {
                    image.frames[number - 2].gap_ms = gap.max(0) as u32;
                }
            }
            if number - 1 == image.current_frame {
                image.frame_shown_at_ms = None;
                image.generation = image.generation.wrapping_add(1);
            }
        }
        self.screen_mut().graphics.generation = self.graphics().generation.wrapping_add(1);
        self.generation = self.generation.wrapping_add(1);
        Ok(number)
    }

    fn graphics_animation(&mut self, id: u32, cmd: &Command) -> Result<(), &'static str> {
        let graphics = &mut self.screen_mut().graphics;
        let image = graphics
            .images
            .get_mut(&id)
            .ok_or("ENOENT: image not found")?;
        let mut changed = false;
        let frame = cmd.n(b'r') as usize;
        if frame > 0 && frame <= image.frames.len() + 1 && cmd.signed(b'z') != 0 {
            let gap = cmd.signed(b'z').max(0) as u32;
            if frame == 1 {
                image.root_gap_ms = gap;
            } else {
                image.frames[frame - 2].gap_ms = gap;
            }
            changed = true;
        }
        let frame_changed = cmd.n(b'c') > 0
            && cmd.n(b'c') as usize <= image.frames.len() + 1
            && cmd.n(b'c') as usize - 1 != image.current_frame;
        if frame_changed {
            image.current_frame = cmd.n(b'c') as usize - 1;
            image.frame_shown_at_ms = None;
            changed = true;
        }
        if (1..=3).contains(&cmd.n(b's')) {
            if image.animation_state == 1 && cmd.n(b's') != 1 {
                image.frame_shown_at_ms = None;
            }
            image.animation_state = cmd.n(b's') as u8;
            image.completed_loops = 0;
            changed = true;
        }
        if cmd.n(b'v') > 0 {
            image.max_loops = cmd.n(b'v') - 1;
            changed = true;
        }
        if changed {
            graphics.generation = graphics.generation.wrapping_add(1);
            if frame_changed {
                image.generation = graphics.generation;
            }
            self.generation = self.generation.wrapping_add(1);
        }
        Ok(())
    }

    fn graphics_compose(&mut self, id: u32, cmd: &Command) -> Result<(), &'static str> {
        let graphics = &mut self.screen_mut().graphics;
        let image = graphics
            .images
            .get_mut(&id)
            .ok_or("ENOENT: image not found")?;
        let source = image
            .frame(cmd.n(b'r') as usize)
            .ok_or("ENOENT: source frame not found")?
            .clone();
        let mut canvas = image
            .frame(cmd.n(b'c') as usize)
            .ok_or("ENOENT: destination frame not found")?
            .to_vec();
        let width = if cmd.n(b'w') == 0 {
            image.width
        } else {
            cmd.n(b'w')
        };
        let height = if cmd.n(b'h') == 0 {
            image.height
        } else {
            cmd.n(b'h')
        };
        let sx = cmd.n(b'X');
        let sy = cmd.n(b'Y');
        let dx = cmd.n(b'x');
        let dy = cmd.n(b'y');
        if dx.saturating_add(width) > image.width || dy.saturating_add(height) > image.height {
            return Err("EINVAL: destination rectangle out of bounds");
        }
        if sx.saturating_add(width) > image.width || sy.saturating_add(height) > image.height {
            return Err("EINVAL: source rectangle out of bounds");
        }
        if cmd.n(b'r') == cmd.n(b'c')
            && sx.max(dx) < sx.min(dx).saturating_add(width)
            && sy.max(dy) < sy.min(dy).saturating_add(height)
        {
            return Err("EINVAL: source and destination rectangles overlap");
        }
        for y in 0..height {
            let source_start = ((sy + y) as usize * image.width as usize + sx as usize) * 4;
            let dest_start = ((dy + y) as usize * image.width as usize + dx as usize) * 4;
            for x in 0..width as usize {
                blend(
                    &mut canvas[dest_start + x * 4..dest_start + x * 4 + 4],
                    &source[source_start + x * 4..source_start + x * 4 + 4],
                    cmd.n(b'C') != 0,
                );
            }
        }
        image.set_frame(
            cmd.n(b'c') as usize,
            pixels::intern(image.width, image.height, canvas),
        );
        graphics.generation = graphics.generation.wrapping_add(1);
        if cmd.n(b'c') as usize - 1 == image.current_frame {
            image.generation = graphics.generation;
        }
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }

    fn graphics_delete(&mut self, cmd: &Command) {
        let what = cmd.values.get(&b'd').copied().unwrap_or(i64::from(b'a')) as u8;
        let kind = what.to_ascii_lowercase();
        let tmux = self.tmux_graphics;
        let saved_images = if tmux {
            let inactive = if self.alternate_active {
                Some(&self.primary)
            } else {
                self.alternate.as_ref()
            };
            inactive
                .into_iter()
                .flat_map(|screen| {
                    screen
                        .graphics
                        .placements
                        .iter()
                        .map(|placement| placement.image_id)
                })
                .collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };
        let image_id = self
            .screen_mut()
            .graphics
            .resolve_id(cmd.n(b'i'), cmd.n(b'I'))
            .unwrap_or(0);
        let row_ids: Vec<_> = if matches!(kind, b'c' | b'p' | b'q' | b'y') {
            self.screen().rows().map(|r| r.id).collect()
        } else {
            Vec::new()
        };
        let cursor = self.screen().cursor.clone();
        let mut cell = [
            self.width_px / u32::from(self.cols),
            self.height_px / u32::from(self.rows),
        ];
        if self.tmux_graphics {
            if cell[0] == 0 {
                cell[0] = 8;
            }
            if cell[1] == 0 {
                cell[1] = 16;
            }
        }
        let visible = if what.eq_ignore_ascii_case(&b'a') {
            self.screen().visible_placements(cell)
        } else {
            HashSet::new()
        };
        let graphics = &mut self.screen_mut().graphics;
        graphics.loading = None;
        if what.eq_ignore_ascii_case(&b'f') {
            let Some(image) = graphics.images.get_mut(&image_id) else {
                return;
            };
            if image.frames.is_empty() {
                if !what.is_ascii_uppercase() {
                    return;
                }
                graphics.images.remove(&image_id);
                graphics.placements.remove_image(image_id);
                graphics.reap_orphans();
            } else {
                let frame = (cmd.n(b'r') as usize).clamp(1, image.frames.len() + 1);
                if frame == 1 {
                    let first = image.frames.remove(0);
                    image.pixels = first.pixels;
                    image.root_gap_ms = first.gap_ms;
                } else {
                    image.frames.remove(frame - 2);
                }
                let removed = frame - 1;
                if removed == image.current_frame {
                    image.current_frame = image.current_frame.min(image.frames.len());
                    image.frame_shown_at_ms = None;
                    image.generation = graphics.generation.wrapping_add(1);
                } else if removed < image.current_frame {
                    image.current_frame -= 1;
                }
            }
        } else {
            let mut removed = HashSet::new();
            let images = &graphics.images;
            let placement_id = cmd.n(b'p');
            if matches!(kind, b'i' | b'n') && placement_id != 0 {
                if graphics
                    .placements
                    .remove((image_id, PlacementId::External(placement_id)))
                    .is_some()
                    && what.is_ascii_uppercase()
                {
                    removed.insert(image_id);
                }
            } else if matches!(kind, b'i' | b'n') {
                graphics.placements.remove_image(image_id);
            } else {
                graphics.placements.retain(|p| {
                    if tmux
                        && images
                            .get(&p.image_id)
                            .is_some_and(|image| image.tmux_sixel_cell.is_some())
                    {
                        return true;
                    }
                    // ID-based deletions don't need image geometry or screen row searches.
                    let size = || {
                        images
                            .get(&p.image_id)
                            .map_or([0; 2], |image| p.grid_size(image, cell))
                    };
                    let y = || row_ids.iter().position(|&id| id == p.row);
                    let at = |x: usize, row: usize| {
                        y().is_some_and(|y| {
                            let [columns, rows] = size();
                            x >= p.col
                                && x < p.col.saturating_add(columns as usize)
                                && row >= y
                                && row < y.saturating_add(rows as usize)
                        })
                    };
                    let remove = match kind {
                        b'a' => visible.contains(&(p.image_id, p.placement_id)),
                        b'c' => at(cursor.col, cursor.row),
                        b'p' => at(
                            cmd.n(b'x').saturating_sub(1) as usize,
                            cmd.n(b'y').saturating_sub(1) as usize,
                        ),
                        b'q' => {
                            p.z == cmd.signed(b'z')
                                && at(
                                    cmd.n(b'x').saturating_sub(1) as usize,
                                    cmd.n(b'y').saturating_sub(1) as usize,
                                )
                        }
                        b'r' => (cmd.n(b'x')..=cmd.n(b'y')).contains(&p.image_id),
                        b'x' => {
                            cmd.n(b'x') as usize > p.col
                                && (cmd.n(b'x') as usize)
                                    <= p.col.saturating_add(size()[0] as usize)
                        }
                        b'y' => y().is_some_and(|y| {
                            cmd.n(b'y') as usize > y
                                && (cmd.n(b'y') as usize) <= y.saturating_add(size()[1] as usize)
                        }),
                        b'z' => p.z == cmd.signed(b'z'),
                        _ => false,
                    };
                    if remove && what.is_ascii_uppercase() {
                        removed.insert(p.image_id);
                    }
                    !remove
                });
            }
            removed.extend(graphics.reap_orphans());
            if what.is_ascii_uppercase() {
                if matches!(what, b'I' | b'N')
                    && cmd.n(b'p') == 0
                    && !saved_images.contains(&image_id)
                {
                    graphics.images.remove(&image_id);
                    removed.remove(&image_id);
                }
                for id in removed {
                    if !graphics.placements.has_image(id) && !saved_images.contains(&id) {
                        graphics.images.remove(&id);
                    }
                }
            }
        }
        graphics.generation = graphics.generation.wrapping_add(1);
        self.generation = self.generation.wrapping_add(1);
    }
}

fn decode_image(cmd: &Command, mut data: Vec<u8>) -> Result<(u32, u32, Vec<u8>), &'static str> {
    let limit = cmd.limit();
    if data.len() > limit {
        return Err("EINVAL: invalid data");
    }
    if cmd.n(b'o') == u32::from(b'z') {
        let mut decoded = Vec::new();
        flate2::read::ZlibDecoder::new(data.as_slice())
            .take(limit as u64 + 1)
            .read_to_end(&mut decoded)
            .map_err(|_| "EINVAL: decompression failed")?;
        if decoded.len() > limit {
            return Err("EINVAL: decompression failed");
        }
        data = decoded;
    } else if cmd.n(b'o') != 0 {
        return Err("EINVAL: invalid data");
    }
    let format = match cmd.n(b'f') {
        0 => 32,
        f => f,
    };
    let mut width = cmd.n(b's');
    let mut height = cmd.n(b'v');
    if format == 100 {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(data.as_slice()));
        decoder.set_limits(png::Limits { bytes: limit });
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().map_err(|_| "EINVAL: invalid data")?;
        width = reader.info().width;
        height = reader.info().height;
        // The native PNG callback bounds the expanded RGBA output before the
        // terminal checks dimensions. Keep that bound for grayscale input too.
        let pixel_count = u64::from(width) * u64::from(height);
        if pixel_count > limit as u64 / 4 {
            return Err("EINVAL: invalid data");
        }
        let len = reader
            .output_buffer_size()
            .filter(|&n| n <= limit)
            .ok_or("EINVAL: invalid data")?;
        let mut bytes = vec![0; len];
        let (color, depth) = reader.output_color_type();
        let stride = reader
            .output_line_size(width)
            .ok_or("EINVAL: invalid data")?;
        let mut decoded = 0;
        while decoded < len {
            let row = reader
                .next_interlaced_row()
                .map_err(|_| "EINVAL: invalid data")?
                .ok_or("EINVAL: invalid data")?;
            if row.data().len() > len - decoded {
                return Err("EINVAL: invalid data");
            }
            match row.interlace() {
                png::InterlaceInfo::Null(_) => {
                    bytes[decoded..decoded + row.data().len()].copy_from_slice(row.data());
                }
                png::InterlaceInfo::Adam7(info) => png::expand_interlaced_row(
                    &mut bytes,
                    stride,
                    row.data(),
                    info,
                    color.samples() as u8 * depth as u8,
                ),
            }
            decoded += row.data().len();
        }
        // Flush the compressed stream and validate IDAT checksums. Wuffs stops
        // after the final IDAT; png also reads the following chunk header. EOF
        // is harmless only after every pixel and the full IDAT were consumed.
        match reader.next_interlaced_row() {
            Ok(None) => {}
            Err(png::DecodingError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof
                    && png_ends_after_idat(&data) => {}
            _ => return Err("EINVAL: invalid data"),
        }
        check_dimensions(width, height)?;
        data = match color {
            png::ColorType::Rgba => bytes,
            png::ColorType::Rgb => bytes
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            png::ColorType::Grayscale => bytes.iter().flat_map(|&p| [p, p, p, 255]).collect(),
            png::ColorType::GrayscaleAlpha => bytes
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            _ => return Err("EINVAL: unsupported pixel depth"),
        };
    } else {
        check_dimensions(width, height)?;
        if u64::from(width) * u64::from(height) > limit as u64 / 4 {
            return Err("EINVAL: invalid data");
        }
        let bpp = match format {
            24 => 3,
            32 => 4,
            _ => return Err("EINVAL: unsupported format"),
        };
        let expected = width as usize * height as usize * bpp;
        if cmd.action() == b'f' {
            if data.len() < expected {
                return Err("ENODATA: insufficient data");
            }
            // Kitty accepts excess raw animation data, unlike ordinary images.
            data.truncate(expected);
        } else if data.len() != expected {
            return Err("EINVAL: invalid data");
        }
        if bpp == 3 {
            data = data
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect();
        }
    }
    Ok((width, height, data))
}

fn png_ends_after_idat(data: &[u8]) -> bool {
    let mut chunks = data.get(8..).unwrap_or_default();
    while chunks.len() >= 12 {
        let len = u32::from_be_bytes(chunks[..4].try_into().unwrap()) as usize;
        let Some(rest) = len.checked_add(12).and_then(|end| chunks.get(end..)) else {
            return false;
        };
        if rest.len() < 8 {
            return &chunks[4..8] == b"IDAT";
        }
        chunks = rest;
    }
    false
}

fn check_dimensions(width: u32, height: u32) -> Result<(), &'static str> {
    if width == 0 || height == 0 {
        return Err("EINVAL: dimensions required");
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err("EINVAL: dimensions too large");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn compose(
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    src: &[u8],
    sw: u32,
    sh: u32,
    x: u32,
    y: u32,
    overwrite: bool,
) {
    for sy in 0..sh.min(dh.saturating_sub(y)) {
        for sx in 0..sw.min(dw.saturating_sub(x)) {
            let di = ((y + sy) as usize * dw as usize + (x + sx) as usize) * 4;
            let si = (sy as usize * sw as usize + sx as usize) * 4;
            blend(&mut dst[di..di + 4], &src[si..si + 4], overwrite);
        }
    }
}

fn blend(dst: &mut [u8], src: &[u8], overwrite: bool) {
    if overwrite || src[3] == 255 {
        dst.copy_from_slice(src);
        return;
    }
    let sa = u32::from(src[3]);
    let da = u32::from(dst[3]);
    let alpha = sa * 255 + da * (255 - sa);
    if alpha == 0 {
        dst.fill(0);
        return;
    }
    for i in 0..3 {
        dst[i] = ((u32::from(src[i]) * sa * 255 + u32::from(dst[i]) * da * (255 - sa) + alpha / 2)
            / alpha) as u8;
    }
    dst[3] = ((alpha + 127) / 255) as u8;
}

impl Screen {
    fn visible_placements(&self, cell: [u32; 2]) -> HashSet<(u32, PlacementId)> {
        let ids: HashSet<_> = self.rows().map(|r| r.id).collect();
        self.graphics
            .placements
            .iter()
            .filter(|p| {
                !p.virtual_placement
                    && p.parent.is_none()
                    && (ids.contains(&p.row)
                        || self
                            .graphics
                            .images
                            .get(&p.image_id)
                            .and_then(|image| p.grid_rect(self, image, cell))
                            .is_some_and(|(_, end)| ids.contains(&end.row)))
            })
            .map(|p| (p.image_id, p.placement_id))
            .collect()
    }

    pub(crate) fn clear_visible_images(&mut self, cell: [u32; 2], retain_sources: bool) {
        let before = (self.graphics.placements.len(), self.graphics.images.len());
        let visible = self.visible_placements(cell);
        self.graphics
            .placements
            .retain(|p| !visible.contains(&(p.image_id, p.placement_id)));
        self.graphics.reap_orphans();
        let retained: HashSet<_> = self
            .graphics
            .placements
            .iter()
            .map(|p| p.image_id)
            .collect();
        if !retain_sources {
            self.graphics.images.retain(|id, _| retained.contains(id));
        }
        if before != (self.graphics.placements.len(), self.graphics.images.len()) {
            self.graphics.generation = self.graphics.generation.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduplicated_images_keep_independent_quotas_and_snapshot_lifetimes() {
        let upload = |terminal: &mut Terminal, id| {
            terminal.feed(format!("\x1b_Ga=t,i={id},f=32,s=1,v=1,q=2;ERIT/w==\x1b\\").as_bytes());
        };
        let mut first = Terminal::new(8, 4, 100);
        let mut second = Terminal::new(8, 4, 100);
        first.set_graphics_limit(8);
        second.set_graphics_limit(4);
        upload(&mut first, 1);
        upload(&mut first, 2);
        upload(&mut second, 9);
        assert!(Arc::ptr_eq(
            &first.graphics().images[&1].pixels,
            &second.graphics().images[&9].pixels
        ));
        assert_eq!(first.graphics().bytes_used(), 8);
        assert_eq!(second.graphics().bytes_used(), 4);
        upload(&mut first, 3);
        assert!(!first.graphics().images.contains_key(&1));
        assert_eq!(first.graphics().bytes_used(), 8);
        assert!(second.graphics().images.contains_key(&9));

        let snapshot = second.screen().snapshot_viewport();
        let pixels = Arc::downgrade(&snapshot.graphics.images[&9].pixels);
        drop(first);
        drop(second);
        assert_eq!(pixels.strong_count(), 1);
        assert_eq!(
            snapshot.graphics.images[&9].pixels.as_slice(),
            [17, 18, 19, 255]
        );
        drop(snapshot);
        assert!(pixels.upgrade().is_none());
    }

    #[test]
    fn frame_reservations_preserve_the_target_and_reuse_freed_capacity() {
        let mut terminal = Terminal::new(8, 4, 0);
        terminal.set_graphics_limit(8);
        for id in [1, 2] {
            terminal.feed(format!("\x1b_Gi={id},f=32,s=1,v=1;AQID/w==\x1b\\").as_bytes());
        }
        terminal.feed(b"\x1b_Ga=f,i=1,f=32,s=1,v=1;BAUG/w==\x1b\\");
        assert_eq!(terminal.graphics().bytes_used(), 8);
        assert_eq!(terminal.graphics().images.len(), 1);
        assert_eq!(terminal.graphics().images[&1].frames.len(), 1);
        assert_eq!(
            terminal.feed(b"\x1b_Ga=f,i=1,r=3,f=32,s=1,v=1;BAUG/w==\x1b\\"),
            [Effect::Write(
                b"\x1b_Gi=1,r=3;ENOSPC: animation frame storage full\x1b\\".to_vec()
            )]
        );
        assert_eq!(terminal.graphics().bytes_used(), 8);
        assert_eq!(terminal.graphics().images[&1].frames.len(), 1);
        terminal.feed(b"\x1b_Ga=d,d=f,i=1,r=2\x1b\\");
        terminal.feed(b"\x1b_Gi=3,f=32,s=1,v=1;AQID/w==\x1b\\");
        assert_eq!(terminal.graphics().bytes_used(), 8);
        assert!(terminal.graphics().images.contains_key(&1));
        assert!(terminal.graphics().images.contains_key(&3));
        assert_eq!(
            terminal.feed(b"\x1b_Gi=4,f=32,s=3,v=1;AAAAAAAAAAAAAAAA\x1b\\"),
            [Effect::Write(
                b"\x1b_Gi=4;ENOMEM: out of memory\x1b\\".to_vec()
            )]
        );
        assert_eq!(terminal.graphics().images.len(), 2);
    }

    #[test]
    fn relative_parent_validation_preserves_placements_on_failure() {
        let mut terminal = Terminal::new(8, 4, 100);
        terminal.feed(b"\x1b_Ga=T,i=1,p=1,C=1,f=32,s=1,v=1;AQID/w==\x1b\\");
        for id in 2..=9 {
            let result =
                terminal.feed(format!("\x1b_Ga=p,i=1,p={id},P=1,Q={}\x1b\\", id - 1).as_bytes());
            assert_eq!(
                result,
                [Effect::Write(
                    format!("\x1b_Gi=1,p={id};OK\x1b\\").into_bytes()
                )]
            );
        }
        for (options, error) in [
            ("p=10,P=7,Q=1", "ENOPARENT: parent image not found"),
            ("p=10,P=1,Q=77", "ENOPARENT: parent placement not found"),
            ("p=10,P=1,Q=9", "ETOODEEP: parent chain too deep"),
            ("p=1,P=1,Q=1", "EINVAL: placement cannot be its own parent"),
            ("p=1,P=1,Q=2", "ECYCLE: parent chain creates a cycle"),
        ] {
            let result = terminal.feed(format!("\x1b_Ga=p,i=1,{options}\x1b\\").as_bytes());
            let placement = options.split(',').next().unwrap();
            assert_eq!(
                result,
                [Effect::Write(
                    format!("\x1b_Gi=1,{placement};{error}\x1b\\").into_bytes()
                )]
            );
            assert_eq!(terminal.graphics().placements.len(), 9);
            assert_eq!(terminal.graphics().placements[0].parent, None);
        }
    }

    #[test]
    fn relative_chain_saturates_each_link_and_rejects_missing_ancestors() {
        let mut terminal = Terminal::new(8, 4, 100);
        terminal.feed(b"\x1b_Ga=T,i=1,p=1,C=1,f=32,s=1,v=1;AQID/w==\x1b\\");
        for (id, offset) in [(2, -1), (3, 1), (4, i32::MAX)] {
            terminal.feed(
                format!(
                    "\x1b_Ga=p,i=1,p={id},P=1,Q={},H={offset},V={offset}\x1b\\",
                    id - 1
                )
                .as_bytes(),
            );
        }
        let graphics = terminal.graphics();
        let child = &graphics.placements[3];
        let lookup = |key| {
            graphics
                .placements
                .iter()
                .find(|p| (p.image_id, p.placement_id) == key)
        };
        let (root, offset) = child.resolve_chain(lookup).unwrap();
        assert_eq!(root.placement_id, PlacementId::External(1));
        assert_eq!(offset, [i32::MAX - 1; 2]);
        assert!(child.resolve_chain(|_| None).is_none());
    }

    #[test]
    fn anonymous_and_numbered_images_use_independent_id_allocation() {
        let mut terminal = Terminal::new(8, 4, 10);
        assert!(
            terminal
                .feed(b"\x1b_Gf=32,s=1,v=1;AQID/w==\x1b\\")
                .is_empty()
        );
        assert!(terminal.graphics().images.contains_key(&2147483647));
        terminal.feed(b"\x1b_Gi=1,f=32,s=1,v=1;AQID/w==\x1b\\");
        terminal.feed(b"\x1b_GI=7,f=32,s=1,v=1;AQID/w==\x1b\\");
        assert_eq!(terminal.graphics().images[&2].number, 7);
        terminal.feed(b"\x1b_Ga=d,d=I,i=1\x1b\\");
        terminal.feed(b"\x1b_GI=8,f=32,s=1,v=1;AQID/w==\x1b\\");
        assert_eq!(terminal.graphics().images[&1].number, 8);
        assert_eq!(terminal.screen_mut().graphics.allocate_id(true), 2147483648);
        terminal.screen_mut().graphics.next_image = u32::MAX;
        assert_eq!(terminal.screen_mut().graphics.allocate_id(true), u32::MAX);
        assert_eq!(terminal.screen_mut().graphics.allocate_id(true), 3);
    }

    #[test]
    fn numbered_lookup_follows_animation_changes_and_eviction() {
        let mut terminal = Terminal::new(8, 4, 0);
        for _ in 0..2 {
            terminal.feed(b"\x1b_GI=7,f=32,s=1,v=1;AQID/w==\x1b\\");
        }
        assert_eq!(terminal.screen_mut().graphics.resolve_id(0, 7), Some(2));
        terminal.feed(b"\x1b_Ga=f,i=1,f=32,s=1,v=1;BAUG/w==\x1b\\");
        terminal.feed(b"\x1b_Ga=a,i=1,c=2\x1b\\");
        assert_eq!(terminal.screen_mut().graphics.resolve_id(0, 7), Some(1));
        terminal.feed(b"\x1b_Ga=d,d=F,i=1,r=2\x1b\\");
        assert_eq!(terminal.screen_mut().graphics.resolve_id(0, 7), Some(1));
        terminal.set_graphics_limit(4);
        assert_eq!(terminal.screen_mut().graphics.resolve_id(0, 7), Some(1));
        terminal.feed(b"\x1b_Ga=d,d=N,I=7\x1b\\");
        assert_eq!(terminal.screen_mut().graphics.resolve_id(0, 7), None);
        assert_eq!(terminal.screen_mut().graphics.resolve_id(0, 0), None);
    }

    #[test]
    fn transmission_ids_are_chosen_before_chunking_and_pixel_validation() {
        let mut terminal = Terminal::new(8, 4, 10);
        terminal.feed(b"\x1b_Gi=1,f=32,s=1,v=1;AQID/w==\x1b\\");
        terminal.feed(b"\x1b_GI=9,f=32,s=1,v=1,m=1;AQI=\x1b\\");
        terminal.feed(b"\x1b_Ga=q,i=1,f=32,s=1,v=1;AQID/w==\x1b\\");
        assert_eq!(
            terminal.feed(b"\x1b_Gm=0;A/8=\x1b\\"),
            [Effect::Write(b"\x1b_Gi=2,I=9;OK\x1b\\".to_vec())]
        );
        assert_eq!(terminal.graphics().images[&2].number, 9);
        terminal.feed(b"\x1b_GI=10,f=32,s=1,v=1;AQ==\x1b\\");
        assert_eq!(
            terminal.feed(b"\x1b_GI=10,f=32,s=1,v=1;AQID/w==\x1b\\"),
            [Effect::Write(b"\x1b_Gi=3,I=10;OK\x1b\\".to_vec())]
        );
        assert!(terminal.feed(b"\x1b_Gf=32,s=1,v=1;AQ==\x1b\\").is_empty());
        terminal.feed(b"\x1b_Gf=99,s=1,v=1;AQID/w==\x1b\\");
        terminal.feed(b"\x1b_Gf=32,s=1,v=1;AQID/w==\x1b\\");
        assert!(!terminal.graphics().images.contains_key(&2147483647));
        assert!(terminal.graphics().images.contains_key(&2147483648));
        terminal.set_graphics_limit(3);
        assert!(
            terminal
                .feed(b"\x1b_Gf=32,s=1,v=1;AQID/w==\x1b\\")
                .is_empty()
        );
    }

    #[test]
    fn transmit_query_chunks_place_delete_and_quiet() {
        let mut t = Terminal::new(80, 24, 100);
        t.set_pixel_size(800, 480);
        assert_eq!(
            t.feed(b"\x1b_Ga=q,i=4,s=1,v=1,f=24;AAE=\x1b\\"),
            [Effect::Write(
                b"\x1b_Gi=4;EINVAL: invalid data\x1b\\".to_vec()
            )]
        );
        assert!(t.graphics().images.is_empty());
        assert!(
            t.feed(b"\x1b_Ga=T,i=1,s=2,v=1,f=24,m=1;AQID\x1b\\")
                .is_empty()
        );
        assert_eq!(
            t.feed(b"\x1b_Gm=0;BAUG\x1b\\"),
            [Effect::Write(b"\x1b_Gi=1;OK\x1b\\".to_vec())]
        );
        assert_eq!(
            t.graphics().images[&1].pixels.as_slice(),
            [1, 2, 3, 255, 4, 5, 6, 255]
        );
        assert_eq!(t.graphics().placements.len(), 1);
        assert_eq!(t.graphics().placements[0].columns, 0);
        assert_eq!(
            t.graphics().placements[0].grid_size(&t.graphics().images[&1], [10, 20]),
            [1, 1]
        );
        t.feed(b"\x1b_Ga=d,d=I,i=1\x1b\\");
        assert!(t.graphics().images.is_empty());
        assert!(
            t.feed(b"\x1b_Ga=q,i=5,s=1,v=1,f=24,q=1;AQID\x1b\\")
                .is_empty()
        );
    }
    #[test]
    fn placement_size_uses_shared_integer_geometry() {
        let mut t = Terminal::new(10, 8, 100);
        t.set_pixel_size(80, 128);
        let data = base64::engine::general_purpose::STANDARD.encode([0; 4 * 4 * 2]);
        t.feed(format!("\x1b_Ga=t,i=1,f=32,s=4,v=2;{data}\x1b\\").as_bytes());
        t.feed(b"\x1b_Ga=p,i=1,p=1,C=1,c=2,X=1,Y=3\x1b\\");
        let graphics = t.graphics();
        let p = &graphics.placements[0];
        assert_eq!(p.pixel_size(&graphics.images[&1], [8, 16]), [15, 8]);
        assert_eq!(p.grid_size(&graphics.images[&1], [8, 16]), [2, 1]);
        t.feed(b"\x1b_Ga=p,i=1,p=1,C=1,r=2,X=1,Y=3\x1b\\");
        let graphics = t.graphics();
        let p = &graphics.placements[0];
        assert_eq!(p.pixel_size(&graphics.images[&1], [8, 16]), [58, 29]);
        assert_eq!(p.grid_size(&graphics.images[&1], [8, 16]), [8, 2]);
        assert_eq!(p.grid_size(&graphics.images[&1], [0, 0]), [0, 0]);
    }

    #[test]
    fn anonymous_placement_ids_do_not_collide_with_external_ids() {
        let mut t = Terminal::new(10, 3, 100);
        t.feed(b"\x1b_Ga=T,i=1,f=32,s=1,v=1,C=1;AQID/w==\x1b\\");
        t.feed(b"\x1b_Ga=p,i=1,C=1\x1b\\\x1b_Ga=p,i=1,p=1,C=1\x1b\\");
        let ids: Vec<_> = t
            .graphics()
            .placements
            .iter()
            .map(|p| p.placement_id)
            .collect();
        assert_eq!(
            ids,
            [
                PlacementId::Internal(0),
                PlacementId::Internal(1),
                PlacementId::External(1)
            ]
        );
        t.feed(b"\x1b_Ga=d,d=i,i=1,p=1\x1b\\");
        assert_eq!(t.graphics().placements.len(), 2);
        t.feed(b"\x1b_Ga=t,i=1,f=32,s=1,v=1;AQID/w==\x1b\\");
        assert!(t.graphics().placements.is_empty());
    }

    #[test]
    fn keyed_deletion_preserves_other_tiles_and_reaps_relative_descendants() {
        for (kind, target) in [('i', "i=2"), ('n', "I=17"), ('I', "i=2"), ('N', "I=17")] {
            let mut t = Terminal::new(10, 3, 100);
            for _ in 0..2 {
                t.feed(b"\x1b_Ga=t,I=17,f=32,s=1,v=1,q=2;AQID/w==\x1b\\");
            }
            t.feed(b"\x1b_Ga=t,i=3,f=32,s=1,v=1,q=2;AQID/w==\x1b\\");
            for options in [
                "i=1,p=1",
                "i=2,p=1",
                "i=2,p=2",
                "i=2",
                "i=3,p=1,P=2,Q=1",
                "i=3,p=2,P=3,Q=1",
            ] {
                t.feed(format!("\x1b_Ga=p,{options},c=1,r=1,C=1,q=2\x1b\\").as_bytes());
            }
            assert_eq!(t.graphics().placements.len(), 6);
            t.feed(format!("\x1b_Ga=d,d={kind},{target},p=1\x1b\\").as_bytes());
            let mut keys: Vec<_> = t
                .graphics()
                .placements
                .iter()
                .map(|p| (p.image_id, p.placement_id))
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                [
                    (1, PlacementId::External(1)),
                    (2, PlacementId::Internal(0)),
                    (2, PlacementId::External(2)),
                ],
                "{kind}"
            );
            assert!(t.graphics().images.contains_key(&2));
            assert_eq!(t.graphics().images.contains_key(&3), kind.is_lowercase());

            // The remaining tiles have no relatives; delete just the external ID.
            t.feed(format!("\x1b_Ga=d,d={kind},{target},p=2\x1b\\").as_bytes());
            assert_eq!(t.graphics().placements.len(), 2);
            assert!(t.graphics().images.contains_key(&2));
            t.feed(format!("\x1b_Ga=d,d={kind},{target}\x1b\\").as_bytes());
            assert_eq!(t.graphics().placements.len(), 1);
            assert_eq!(t.graphics().placements[0].image_id, 1);
            assert_eq!(t.graphics().images.contains_key(&2), kind.is_lowercase());
        }
    }

    #[test]
    fn upload_discards_publicly_retained_placements_of_a_reused_image_id() {
        let mut terminal = Terminal::new(10, 3, 0);
        terminal.feed(b"\x1b_Ga=T,I=17,f=32,s=1,v=1,C=1;AQID/w==\x1b\\");
        let graphics = &mut terminal.screen_mut().graphics;
        graphics.images.remove(&1);
        let mut placement = graphics.placements[0].clone();
        placement.placement_id = PlacementId::External(42);
        graphics.placements.insert(placement);
        assert_eq!(graphics.placements.len(), 2);
        terminal.feed(b"\x1b_GI=18,f=32,s=1,v=1;AQID/w==\x1b\\");
        assert_eq!(terminal.graphics().images[&1].number, 18);
        assert!(terminal.graphics().placements.is_empty());
    }

    #[test]
    fn whole_image_deletion_preserves_independently_placed_descendant_images() {
        for (kind, target) in [('I', "i=1"), ('N', "I=17")] {
            let mut terminal = Terminal::new(10, 3, 0);
            terminal.feed(b"\x1b_GI=17,f=32,s=1,v=1;AQID/w==\x1b\\");
            for id in [2, 3] {
                terminal.feed(format!("\x1b_Gi={id},f=32,s=1,v=1;AQID/w==\x1b\\").as_bytes());
            }
            for options in [
                "i=1,p=1",
                "i=1,p=2",
                "i=2,p=1,P=1,Q=1",
                "i=2,p=2",
                "i=3,p=1,P=2,Q=1",
            ] {
                terminal.feed(format!("\x1b_Ga=p,{options},C=1,q=2\x1b\\").as_bytes());
            }
            assert_eq!(terminal.graphics().placements.len(), 5);
            terminal.feed(format!("\x1b_Ga=d,d={kind},{target}\x1b\\").as_bytes());
            let graphics = terminal.graphics();
            assert_eq!(graphics.images.len(), 1);
            assert!(graphics.images.contains_key(&2));
            assert_eq!(graphics.placements.len(), 1);
            let placement = &graphics.placements[0];
            assert_eq!(
                (placement.image_id, placement.placement_id),
                (2, PlacementId::External(2))
            );
        }
    }

    #[test]
    fn animation_frames_are_bounded_and_advance_on_host_clock() {
        let mut t = Terminal::new(10, 3, 10);
        t.feed(b"\x1b_Gi=1,s=1,v=1,f=32;/wAA/w==\x1b\\");
        assert_eq!(
            t.feed(b"\x1b_Ga=f,i=1,s=1,v=1,f=32,z=50;AP8A/w==\x1b\\"),
            [Effect::Write(b"\x1b_Gi=1,r=2;OK\x1b\\".to_vec())]
        );
        t.feed(b"\x1b_Ga=a,i=1,r=1,z=50,s=3\x1b\\");
        let generation = t.generation;
        assert_eq!(t.tick_graphics(100), None);
        assert_eq!(t.generation, generation);
        assert_eq!(t.graphics().images[&1].frame_shown_at_ms, None);
        t.feed(b"\x1b_Ga=p,i=1,C=1\x1b\\");
        assert_eq!(t.tick_graphics(100), Some(150));
        let generation = t.graphics().images[&1].generation;
        t.feed(b"\x1b_Ga=a,i=1,c=1,s=3\x1b\\");
        assert_eq!(t.graphics().images[&1].generation, generation);
        assert_eq!(t.tick_graphics(125), Some(150));
        assert_eq!(t.tick_graphics(5), Some(55));
        assert_eq!(t.graphics().images[&1].frame_shown_at_ms, Some(5));
        t.tick_graphics(150);
        assert_eq!(t.graphics().images[&1].display_pixels(), [0, 255, 0, 255]);
        t.tick_graphics(200);
        assert_eq!(t.graphics().images[&1].display_pixels(), [255, 0, 0, 255]);
        t.feed(b"\x1b_Ga=d,d=i,i=1\x1b\\");
        let generation = t.generation;
        assert_eq!(t.tick_graphics(1000), None);
        assert_eq!(t.generation, generation);
        t.set_graphics_limit(4);
        assert_eq!(t.graphics().bytes_used(), 0);
    }
    #[test]
    fn invalid_dimensions_and_mutually_exclusive_ids_do_not_store() {
        let mut t = Terminal::new(10, 3, 10);
        let e = t.feed(b"\x1b_Gi=1,I=2,s=1,v=1;AAAAAA==\x1b\\");
        assert!(
            matches!(&e[0],Effect::Write(v) if String::from_utf8_lossy(v).contains("mutually exclusive"))
        );
        let e = t.feed(b"\x1b_Gi=1,s=4294967295,v=1;AAAAAA==\x1b\\");
        assert!(
            matches!(&e[0],Effect::Write(v) if String::from_utf8_lossy(v).contains("dimensions too large"))
        );
        assert!(t.graphics().images.is_empty());
    }

    #[test]
    fn png_and_zlib_uploads_decode_to_rgba() {
        use std::io::Write;
        let mut encoded = Vec::new();
        {
            let mut png = png::Encoder::new(&mut encoded, 1, 1);
            png.set_color(png::ColorType::Rgba);
            png.set_depth(png::BitDepth::Eight);
            png.write_header()
                .unwrap()
                .write_image_data(&[12, 34, 56, 78])
                .unwrap();
        }
        let mut t = Terminal::new(10, 3, 10);
        let payload = base64::engine::general_purpose::STANDARD.encode(encoded);
        t.feed(format!("\x1b_Gi=1,f=100;{payload}\x1b\\").as_bytes());
        assert_eq!(t.graphics().images[&1].pixels.as_slice(), [12, 34, 56, 78]);
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        zlib.write_all(&[90, 80, 70]).unwrap();
        let payload = base64::engine::general_purpose::STANDARD.encode(zlib.finish().unwrap());
        t.feed(format!("\x1b_Gi=2,s=1,v=1,f=24,o=z;{payload}\x1b\\").as_bytes());
        assert_eq!(t.graphics().images[&2].pixels.as_slice(), [90, 80, 70, 255]);
    }

    #[test]
    fn png_accepts_missing_trailer_but_requires_complete_pixels_and_checksum() {
        let mut encoded = Vec::new();
        {
            let mut png = png::Encoder::new(&mut encoded, 1, 1);
            png.set_color(png::ColorType::Rgba);
            png.set_depth(png::BitDepth::Eight);
            png.write_header()
                .unwrap()
                .write_image_data(&[12, 34, 56, 78])
                .unwrap();
        }
        let (command, _) = Command::parse(b"Gf=100;").unwrap();
        let idat_end = encoded.len() - 12;
        for length in 0..=encoded.len() {
            let decoded = decode_image(&command, encoded[..length].to_vec());
            if length >= idat_end {
                assert_eq!(decoded.unwrap(), (1, 1, vec![12, 34, 56, 78]));
            } else {
                assert!(decoded.is_err(), "accepted truncated PNG at {length}");
            }
        }
        encoded[idat_end - 1] ^= 1;
        assert!(decode_image(&command, encoded[..idat_end].to_vec()).is_err());
        assert!(decode_image(&command, encoded).is_err());
    }

    #[test]
    fn frame_upload_survives_playback_changes() {
        let mut t = Terminal::new(10, 3, 10);
        t.feed(b"\x1b_Gi=1,s=1,v=1,f=32;/wAA/w==\x1b\\");
        t.feed(b"\x1b_Ga=f,i=1,s=1,v=1,f=32;AP8A/w==\x1b\\");
        t.feed(b"\x1b_Ga=f,i=1,s=1,v=1,f=32,m=1;AAD/\x1b\\");
        t.feed(b"\x1b_Ga=a,i=1,c=2\x1b\\");
        t.feed(b"\x1b_Gm=0;/w==\x1b\\");
        assert_eq!(t.graphics().images[&1].frames.len(), 2);
        assert_eq!(
            t.graphics().images[&1].frames[1].pixels.as_slice(),
            [0, 0, 255, 255]
        );
    }
}
