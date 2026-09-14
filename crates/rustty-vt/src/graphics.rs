//! Kitty graphics storage. Pixel buffers are shared with render snapshots.
use crate::{Effect, GridPoint, Screen, Terminal};
use base64::Engine;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Read,
    sync::Arc,
};

pub mod unicode;

const MAX_DATA: usize = 400 * 1024 * 1024;
const MAX_DIMENSION: u32 = 10000;
const PARENT_CHAIN_LIMIT: usize = 8;

#[derive(Clone, Debug)]
pub struct Image {
    pub id: u32,
    pub number: u32,
    pub width: u32,
    pub height: u32,
    /// Unassociated RGBA8, independent of the renderer's target color space.
    pub pixels: Arc<[u8]>,
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

#[derive(Clone, Debug)]
pub struct AnimationFrame {
    pub pixels: Arc<[u8]>,
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
    pub fn displayed_pixels(&self) -> Arc<[u8]> {
        if self.current_frame == 0 {
            self.pixels.clone()
        } else {
            self.frames[self.current_frame - 1].pixels.clone()
        }
    }
    fn frame(&self, number: usize) -> Option<&Arc<[u8]>> {
        if number == 1 {
            Some(&self.pixels)
        } else {
            self.frames.get(number.checked_sub(2)?).map(|f| &f.pixels)
        }
    }
    fn set_frame(&mut self, number: usize, pixels: Arc<[u8]>) {
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

#[derive(Clone, Debug)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
            .min(screen.history.len() + screen.rows.len() - 1);
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

#[derive(Clone, Debug)]
struct Loading {
    command: Command,
    data: Vec<u8>,
    image_id: u32,
    image_generation: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Graphics {
    pub images: HashMap<u32, Image>,
    pub placements: Vec<Placement>,
    pub generation: u64,
    pub limit: usize,
    loading: Option<Loading>,
    next_image: u32,
    next_placement: u32,
}

impl Default for Graphics {
    fn default() -> Self {
        Self {
            images: HashMap::new(),
            placements: Vec::new(),
            generation: 0,
            limit: 320_000_000,
            loading: None,
            next_image: 2147483647,
            next_placement: 0,
        }
    }
}

impl Graphics {
    /// Resolve a Unicode placeholder's optional application-supplied placement ID.
    pub fn placeholder_target(&self, image_id: u32, placement_id: u32) -> Option<&Placement> {
        self.placements
            .iter()
            .filter(|p| {
                p.image_id == image_id
                    && if placement_id == 0 {
                        p.virtual_placement
                    } else {
                        p.placement_id == PlacementId::External(placement_id)
                    }
            })
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
        let parent = self
            .placements
            .iter()
            .filter(|p| {
                p.image_id == image_id
                    && (placement_id == 0 || p.placement_id == PlacementId::External(placement_id))
            })
            .min_by_key(|p| match p.placement_id {
                PlacementId::External(id) => (false, id),
                PlacementId::Internal(id) => (true, id),
            })
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
                .iter()
                .find(|p| (p.image_id, p.placement_id) == key)
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
        let mut placements = self.placements.clone();
        if !placements.is_empty() {
            let start = screen.history.len().saturating_sub(screen.viewport_offset) as i64;
            let anchors: HashSet<_> = placements.iter().map(|p| p.row).collect();
            let offsets: HashMap<_, _> = screen
                .all_rows()
                .enumerate()
                .filter(|(_, row)| anchors.contains(&row.id))
                .map(|(index, row)| (row.id, index as i64 - start))
                .collect();
            for placement in &mut placements {
                placement.viewport_row =
                    Some(offsets.get(&placement.row).copied().unwrap_or(i64::MIN));
            }
        }
        Self {
            images: self.images.clone(),
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
        loop {
            let keys: HashSet<_> = self
                .placements
                .iter()
                .map(|p| (p.image_id, p.placement_id))
                .collect();
            let before = self.placements.len();
            self.placements.retain(|p| {
                let keep = p.parent.is_none_or(|parent| keys.contains(&parent));
                if !keep {
                    removed.insert(p.image_id);
                }
                keep
            });
            if self.placements.len() == before {
                return removed;
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
    fn resolve_id(&self, id: u32, number: u32) -> Option<u32> {
        if id != 0 {
            self.images.contains_key(&id).then_some(id)
        } else {
            self.images
                .values()
                .filter(|i| i.number == number && number != 0)
                .max_by_key(|i| i.generation)
                .map(|i| i.id)
        }
    }
    fn allocate_id(&mut self, implicit: bool) -> u32 {
        // Numbered uploads choose the lowest free client ID. Anonymous uploads
        // use a separate counter so they rarely collide with client choices.
        let mut id = if implicit { self.next_image } else { 1 };
        while id == 0 || self.images.contains_key(&id) {
            id = id.wrapping_add(1);
        }
        if implicit {
            self.next_image = id.wrapping_add(1);
        }
        id
    }
    fn reserve(&mut self, bytes: usize, exclude: u32) -> Result<(), &'static str> {
        if bytes > self.limit {
            return Err("ENOMEM: out of memory");
        }
        while self.bytes_used().saturating_add(bytes) > self.limit {
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
            self.placements.retain(|p| p.image_id != id);
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

#[derive(Clone, Debug, Default)]
struct Command {
    values: BTreeMap<u8, i64>,
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
        if self.quiet() >= 2 || self.quiet() == 1 && message == "OK" || id == 0 && self.n(b'I') == 0
        {
            return;
        }
        let mut values = Vec::new();
        if id != 0 {
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
        effects.push(Effect::Write(
            format!("\x1b_G{};{message}\x1b\\", values.join(",")).into_bytes(),
        ));
    }
}

impl Terminal {
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
        if self.graphics().limit == 0 {
            return;
        }
        let Some((mut command, payload)) = Command::parse(bytes) else {
            return;
        };
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
        if action == b'd' {
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
            let Some(resolved) = self.graphics().resolve_id(id, command.n(b'I')) else {
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
        if matches!(action, b't' | b'T') && id != 0 && self.graphics().loading.is_none() {
            self.graphics_delete(&Command {
                values: [(b'd', i64::from(b'I')), (b'i', i64::from(id))].into(),
            });
        }
        let engine = base64::engine::general_purpose::GeneralPurpose::new(
            &base64::alphabet::STANDARD,
            base64::engine::general_purpose::GeneralPurposeConfig::new()
                .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
        );
        let Ok(mut data) = engine.decode(payload) else {
            command.reply(id, 0, "EINVAL: invalid data", effects);
            return;
        };
        let medium = command
            .values
            .get(&b't')
            .copied()
            .unwrap_or(i64::from(b'd')) as u8;
        if medium != b'd' {
            command.reply(id, 0, "EINVAL: unsupported medium", effects);
            return;
        }
        let mut image_generation = None;
        if action != b'q'
            && let Some(mut loading) = self.screen_mut().graphics.loading.take()
        {
            if loading.data.len().saturating_add(data.len()) > MAX_DATA {
                command.reply(id, 0, "ENOMEM: out of memory", effects);
                return;
            }
            loading.data.append(&mut data);
            data = loading.data;
            let more = command.n(b'm');
            let quiet = command.quiet();
            command = loading.command;
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
                let Some(resolved) = self.graphics().resolve_id(id, command.n(b'I')) else {
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
                id = self.screen_mut().graphics.allocate_id(command.n(b'I') == 0);
            }
        }
        if action != b'q' && command.n(b'm') != 0 {
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
            command.reply(id, 0, "OK", effects);
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
        let old_size = storage.images.get(&id).map_or(0, Image::bytes);
        if let Err(error) = storage.reserve(pixels.len().saturating_sub(old_size), id) {
            command.reply(command.n(b'i'), 0, error, effects);
            return;
        }
        storage.generation = storage.generation.wrapping_add(1);
        storage.placements.retain(|p| p.image_id != id);
        storage.reap_orphans();
        storage.images.insert(
            id,
            Image {
                id,
                number: command.n(b'I'),
                width,
                height,
                pixels: pixels.into(),
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
        let result = if command.action() == b'T' {
            self.graphics_place(id, &command)
        } else {
            Ok(())
        };
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
        let cell = [
            self.width_px / u32::from(self.cols),
            self.height_px / u32::from(self.rows),
        ];
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
        let row = self.screen().rows[cursor.row].id;
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
        let [columns, rows] = placement.grid_size(&self.graphics().images[&id], cell);
        let graphics = &mut self.screen_mut().graphics;
        let placement_id = if cmd.n(b'p') != 0 {
            PlacementId::External(cmd.n(b'p'))
        } else {
            let n = graphics.next_placement;
            graphics.next_placement = n.wrapping_add(1);
            PlacementId::Internal(n)
        };
        if cmd.n(b'p') != 0 {
            graphics
                .placements
                .retain(|p| p.image_id != id || p.placement_id != placement_id);
        }
        placement.placement_id = placement_id;
        graphics.placements.push(placement);
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
        if new {
            image.frames.push(AnimationFrame {
                pixels: canvas.into(),
                gap_ms: if gap == 0 { 40 } else { gap.max(0) as u32 },
            });
        } else {
            image.set_frame(number, canvas.into());
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
        image.set_frame(cmd.n(b'c') as usize, canvas.into());
        graphics.generation = graphics.generation.wrapping_add(1);
        if cmd.n(b'c') as usize - 1 == image.current_frame {
            image.generation = graphics.generation;
        }
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }

    fn graphics_delete(&mut self, cmd: &Command) {
        let what = cmd.values.get(&b'd').copied().unwrap_or(i64::from(b'a')) as u8;
        let image_id = self
            .graphics()
            .resolve_id(cmd.n(b'i'), cmd.n(b'I'))
            .unwrap_or(0);
        let row_ids: Vec<_> = self.screen().rows.iter().map(|r| r.id).collect();
        let cursor = self.screen().cursor.clone();
        let cell = [
            self.width_px / u32::from(self.cols),
            self.height_px / u32::from(self.rows),
        ];
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
                graphics.placements.retain(|p| p.image_id != image_id);
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
            graphics.placements.retain(|p| {
                let [columns, rows] = images
                    .get(&p.image_id)
                    .map_or([0; 2], |image| p.grid_size(image, cell));
                let y = row_ids.iter().position(|&id| id == p.row);
                let at = |x: usize, row: usize| {
                    y.is_some_and(|y| {
                        x >= p.col
                            && x < p.col.saturating_add(columns as usize)
                            && row >= y
                            && row < y.saturating_add(rows as usize)
                    })
                };
                let remove = match what.to_ascii_lowercase() {
                    b'a' => visible.contains(&(p.image_id, p.placement_id)),
                    b'i' | b'n' => {
                        p.image_id == image_id
                            && (cmd.n(b'p') == 0
                                || p.placement_id == PlacementId::External(cmd.n(b'p')))
                    }
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
                            && (cmd.n(b'x') as usize) <= p.col.saturating_add(columns as usize)
                    }
                    b'y' => y.is_some_and(|y| {
                        cmd.n(b'y') as usize > y
                            && (cmd.n(b'y') as usize) <= y.saturating_add(rows as usize)
                    }),
                    b'z' => p.z == cmd.signed(b'z'),
                    _ => false,
                };
                if remove {
                    removed.insert(p.image_id);
                }
                !remove
            });
            removed.extend(graphics.reap_orphans());
            if what.is_ascii_uppercase() {
                if matches!(what, b'I' | b'N') && cmd.n(b'p') == 0 {
                    removed.insert(image_id);
                }
                for id in removed {
                    if !graphics.placements.iter().any(|p| p.image_id == id) {
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
    if cmd.n(b'o') == u32::from(b'z') {
        let mut decoded = Vec::new();
        flate2::read::ZlibDecoder::new(data.as_slice())
            .take(MAX_DATA as u64 + 1)
            .read_to_end(&mut decoded)
            .map_err(|_| "EINVAL: decompression failed")?;
        if decoded.len() > MAX_DATA {
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
        decoder.set_limits(png::Limits { bytes: MAX_DATA });
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().map_err(|_| "EINVAL: invalid data")?;
        width = reader.info().width;
        height = reader.info().height;
        // The native PNG callback bounds the expanded RGBA output before the
        // terminal checks dimensions. Keep that bound for grayscale input too.
        let pixel_count = u64::from(width) * u64::from(height);
        if pixel_count > MAX_DATA as u64 / 4 {
            return Err("EINVAL: invalid data");
        }
        let len = reader
            .output_buffer_size()
            .filter(|&n| n <= MAX_DATA)
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
        let ids: HashSet<_> = self.rows.iter().map(|r| r.id).collect();
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

    pub(crate) fn clear_visible_images(&mut self, cell: [u32; 2]) {
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
        self.graphics.images.retain(|id, _| retained.contains(id));
        if before != (self.graphics.placements.len(), self.graphics.images.len()) {
            self.graphics.generation = self.graphics.generation.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            t.graphics().images[&1].pixels.as_ref(),
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
        assert_eq!(t.graphics().images[&1].pixels.as_ref(), [12, 34, 56, 78]);
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        zlib.write_all(&[90, 80, 70]).unwrap();
        let payload = base64::engine::general_purpose::STANDARD.encode(zlib.finish().unwrap());
        t.feed(format!("\x1b_Gi=2,s=1,v=1,f=24,o=z;{payload}\x1b\\").as_bytes());
        assert_eq!(t.graphics().images[&2].pixels.as_ref(), [90, 80, 70, 255]);
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
            t.graphics().images[&1].frames[1].pixels.as_ref(),
            [0, 0, 255, 255]
        );
    }
}
