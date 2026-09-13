//! Kitty graphics storage. Pixel buffers are shared with render snapshots.
use crate::{Effect, GridPoint, Screen, Terminal};
use base64::Engine;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Read,
    sync::Arc,
};

const MAX_DATA: usize = 400 * 1024 * 1024;
const MAX_DIMENSION: u32 = 10000;

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
    pub placement_id: u32,
    pub row: u64,
    pub col: usize,
    pub columns: u32,
    pub rows: u32,
    /// Requested c/r values, before inferring cells for cursor movement.
    pub requested_size: [u32; 2],
    /// Projected anchor in viewport snapshots, including roots above the viewport.
    pub viewport_row: Option<i64>,
    pub z: i32,
    /// Pixel-space source rectangle, clipped to the image.
    pub source: [u32; 4],
    pub offset: [u32; 2],
    pub virtual_placement: bool,
    pub parent: Option<(u32, u32)>,
    pub parent_offset: [i32; 2],
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
            next_placement: 1,
        }
    }
}

impl Graphics {
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
            if image.animation_state < 2 || image.frames.is_empty() {
                continue;
            }
            if image.root_gap_ms == 0 && image.frames.iter().all(|f| f.gap_ms == 0) {
                continue;
            }
            let shown = *image.frame_shown_at_ms.get_or_insert(now_ms);
            let mut deadline = shown.saturating_add(u64::from(image.gap(image.current_frame)));
            // At most one cycle per tick; old deadlines never cause unbounded catch-up.
            for _ in 0..=image.frames.len() {
                if now_ms < deadline {
                    break;
                }
                if image.current_frame == image.frames.len() {
                    if image.animation_state == 2 {
                        image.frame_shown_at_ms = None;
                        break;
                    }
                    image.completed_loops = image.completed_loops.saturating_add(1);
                    if image.max_loops != 0 && image.completed_loops >= image.max_loops {
                        image.animation_state = 1;
                        break;
                    }
                    image.current_frame = 0;
                } else {
                    image.current_frame += 1;
                }
                image.generation = image.generation.wrapping_add(1);
                self.generation = self.generation.wrapping_add(1);
                image.frame_shown_at_ms = Some(now_ms);
                deadline = now_ms.saturating_add(u64::from(image.gap(image.current_frame)));
            }
            if image.animation_state >= 2
                && !(image.animation_state == 2 && image.current_frame == image.frames.len())
            {
                let deadline = deadline.max(now_ms.saturating_add(1));
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
            } else if action == b'p' {
                command.reply(id, 0, "OK", effects);
            }
            return;
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
        let image = self
            .graphics()
            .images
            .get(&id)
            .ok_or("ENOENT: image not found")?;
        let x = cmd.n(b'x').min(image.width);
        let y = cmd.n(b'y').min(image.height);
        let width = if cmd.n(b'w') == 0 {
            image.width
        } else {
            cmd.n(b'w')
        }
        .min(image.width - x);
        let height = if cmd.n(b'h') == 0 {
            image.height
        } else {
            cmd.n(b'h')
        }
        .min(image.height - y);
        let cell_w = (self.width_px / u32::from(self.cols)).max(1);
        let cell_h = (self.height_px / u32::from(self.rows)).max(1);
        let offset = [cmd.n(b'X').min(cell_w - 1), cmd.n(b'Y').min(cell_h - 1)];
        let mut columns = cmd.n(b'c');
        let mut rows = cmd.n(b'r');
        if columns == 0 && rows == 0 {
            columns = (width + offset[0]).div_ceil(cell_w);
            rows = (height + offset[1]).div_ceil(cell_h);
        } else if columns == 0 {
            columns = (u64::from(width) * u64::from(rows) * u64::from(cell_h)
                / u64::from(height.max(1)))
            .div_ceil(u64::from(cell_w))
            .min(u64::from(u32::MAX)) as u32;
        } else if rows == 0 {
            rows = (u64::from(height) * u64::from(columns) * u64::from(cell_w)
                / u64::from(width.max(1)))
            .div_ceil(u64::from(cell_h))
            .min(u64::from(u32::MAX)) as u32;
        }
        let parent = if cmd.n(b'P') != 0 {
            let parent = self
                .graphics()
                .placements
                .iter()
                .find(|p| {
                    p.image_id == cmd.n(b'P') && (cmd.n(b'Q') == 0 || p.placement_id == cmd.n(b'Q'))
                })
                .ok_or("ENOPARENT: parent placement not found")?;
            if id == parent.image_id && cmd.n(b'p') == parent.placement_id {
                return Err("EINVAL: placement cannot be its own parent");
            }
            let mut current = Some((parent.image_id, parent.placement_id));
            let mut seen = HashSet::new();
            while let Some(key) = current {
                if key == (id, cmd.n(b'p')) || !seen.insert(key) {
                    return Err("ECYCLE: parent chain creates a cycle");
                }
                if seen.len() > 64 {
                    return Err("ETOODEEP: parent chain too deep");
                }
                current = self
                    .graphics()
                    .placements
                    .iter()
                    .find(|p| (p.image_id, p.placement_id) == key)
                    .and_then(|p| p.parent);
            }
            Some((parent.image_id, parent.placement_id))
        } else {
            None
        };
        let cursor = self.screen().cursor.clone();
        let row = self.screen().rows[cursor.row].id;
        let graphics = &mut self.screen_mut().graphics;
        let placement_id = if cmd.n(b'p') != 0 {
            cmd.n(b'p')
        } else {
            let n = graphics.next_placement;
            graphics.next_placement = n.wrapping_add(1).max(1);
            n
        };
        if cmd.n(b'p') != 0 {
            graphics
                .placements
                .retain(|p| p.image_id != id || p.placement_id != placement_id);
        }
        graphics.placements.push(Placement {
            image_id: id,
            placement_id,
            row,
            col: cursor.col,
            columns,
            rows,
            requested_size: [cmd.n(b'c'), cmd.n(b'r')],
            viewport_row: None,
            z: cmd.signed(b'z'),
            source: [x, y, width, height],
            offset,
            virtual_placement: cmd.n(b'U') != 0,
            parent,
            parent_offset: [cmd.signed(b'H'), cmd.signed(b'V')],
        });
        graphics.generation = graphics.generation.wrapping_add(1);
        if cmd.n(b'C') != 1 && cmd.n(b'U') == 0 && parent.is_none() {
            let target = cursor.col.saturating_add(columns as usize);
            let wraps = target >= self.cols as usize;
            let requested = rows.saturating_sub(1) as usize + usize::from(wraps);
            let before = self.margins.bottom.saturating_sub(cursor.row);
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
        let image = self
            .screen_mut()
            .graphics
            .images
            .get_mut(&id)
            .ok_or("ENOENT: image not found")?;
        let frame = cmd.n(b'r') as usize;
        if frame > 0 && frame <= image.frames.len() + 1 && cmd.signed(b'z') != 0 {
            let gap = cmd.signed(b'z').max(0) as u32;
            if frame == 1 {
                image.root_gap_ms = gap;
            } else {
                image.frames[frame - 2].gap_ms = gap;
            }
        }
        if cmd.n(b'c') > 0 && cmd.n(b'c') as usize <= image.frames.len() + 1 {
            image.current_frame = cmd.n(b'c') as usize - 1;
            image.frame_shown_at_ms = None;
        }
        if (1..=3).contains(&cmd.n(b's')) {
            image.animation_state = cmd.n(b's') as u8;
            image.completed_loops = 0;
            image.frame_shown_at_ms = None;
        }
        if cmd.n(b'v') > 0 {
            image.max_loops = cmd.n(b'v') - 1;
        }
        image.generation = image.generation.wrapping_add(1);
        self.screen_mut().graphics.generation = self.graphics().generation.wrapping_add(1);
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }

    fn graphics_compose(&mut self, id: u32, cmd: &Command) -> Result<(), &'static str> {
        let image = self
            .screen_mut()
            .graphics
            .images
            .get_mut(&id)
            .ok_or("ENOENT: image not found")?;
        let source = image
            .frame(cmd.n(b'r') as usize)
            .ok_or("EINVAL: source frame not found")?
            .clone();
        let mut canvas = image
            .frame(cmd.n(b'c') as usize)
            .ok_or("EINVAL: destination frame not found")?
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
        if sx.saturating_add(width) > image.width
            || dx.saturating_add(width) > image.width
            || sy.saturating_add(height) > image.height
            || dy.saturating_add(height) > image.height
        {
            return Err("EINVAL: rectangle out of bounds");
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
        image.generation = image.generation.wrapping_add(1);
        self.screen_mut().graphics.generation = self.graphics().generation.wrapping_add(1);
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
        let graphics = &mut self.screen_mut().graphics;
        graphics.loading = None;
        if what.eq_ignore_ascii_case(&b'f') {
            if let Some(image) = graphics.images.get_mut(&image_id) {
                let frame = cmd.n(b'r') as usize;
                if frame == 1 && !image.frames.is_empty() {
                    let first = image.frames.remove(0);
                    image.pixels = first.pixels;
                    image.root_gap_ms = first.gap_ms;
                } else if frame >= 2 && frame <= image.frames.len() + 1 {
                    image.frames.remove(frame - 2);
                } else if frame == 1 && what.is_ascii_uppercase() {
                    graphics.images.remove(&image_id);
                    graphics.placements.retain(|p| p.image_id != image_id);
                }
                if let Some(image) = graphics.images.get_mut(&image_id) {
                    image.current_frame = image.current_frame.min(image.frames.len());
                    image.frame_shown_at_ms = None;
                }
            }
        } else {
            let mut removed = HashSet::new();
            graphics.placements.retain(|p| {
                let y = row_ids.iter().position(|&id| id == p.row);
                let at = |x: usize, row: usize| {
                    y.is_some_and(|y| {
                        x >= p.col
                            && x < p.col.saturating_add(p.columns as usize)
                            && row >= y
                            && row < y.saturating_add(p.rows as usize)
                    })
                };
                let remove = match what.to_ascii_lowercase() {
                    b'a' => y.is_some() && !p.virtual_placement,
                    b'i' | b'n' => {
                        p.image_id == image_id
                            && (cmd.n(b'p') == 0 || p.placement_id == cmd.n(b'p'))
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
                            && (cmd.n(b'x') as usize) <= p.col.saturating_add(p.columns as usize)
                    }
                    b'y' => y.is_some_and(|y| {
                        cmd.n(b'y') as usize > y
                            && (cmd.n(b'y') as usize) <= y.saturating_add(p.rows as usize)
                    }),
                    b'z' => p.z == cmd.signed(b'z'),
                    _ => false,
                };
                if remove {
                    removed.insert(p.image_id);
                }
                !remove
            });
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
        if data.len() != width as usize * height as usize * bpp {
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
    pub(crate) fn clear_visible_images(&mut self) {
        let ids: HashSet<_> = self.rows.iter().map(|r| r.id).collect();
        self.graphics.placements.retain(|p| !ids.contains(&p.row));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(t.graphics().placements[0].columns, 1);
        t.feed(b"\x1b_Ga=d,d=I,i=1\x1b\\");
        assert!(t.graphics().images.is_empty());
        assert!(
            t.feed(b"\x1b_Ga=q,i=5,s=1,v=1,f=24,q=1;AQID\x1b\\")
                .is_empty()
        );
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
        assert_eq!(t.tick_graphics(100), Some(150));
        t.tick_graphics(150);
        assert_eq!(t.graphics().images[&1].display_pixels(), [0, 255, 0, 255]);
        t.tick_graphics(200);
        assert_eq!(t.graphics().images[&1].display_pixels(), [255, 0, 0, 255]);
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
