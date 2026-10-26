//! Optional GHOSTSNP Kitty records, with pixel payloads following READY.
use super::*;
use crate::snapshot::{Slice, invalid, record, record_parts, u16_bytes, u32_bytes};
use std::io::{self, Write};
use std::sync::Weak;

pub(crate) fn has_state(screen: &Screen) -> bool {
    !screen.graphics.images.is_empty()
        || !screen.graphics.placements.is_empty()
        || screen.graphics.loading.is_some()
}

pub(crate) const STORAGE: u16 = 8;
pub(crate) const PAYLOAD: u16 = 9;
pub(crate) const PAGE_PLACEMENTS: u16 = 10;

fn word(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn long(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&(value as u64).to_le_bytes());
}
fn rgba_len(width: u32, height: u32) -> Option<usize> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return None;
    }
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)
}
fn wire_len(width: u32, height: u32, format: u8) -> Option<usize> {
    let bpp = match format {
        0 => 3,
        1 => 4,
        3 => 2,
        4 => 1,
        _ => return None,
    };
    rgba_len(width, height)?.checked_div(4)?.checked_mul(bpp)
}
fn placement_key(out: &mut Vec<u8>, image: u32, id: PlacementId) {
    word(out, image);
    let (tag, id) = match id {
        PlacementId::Internal(id) => (0, id),
        PlacementId::External(id) => (1, id),
    };
    out.push(tag);
    word(out, id);
}

struct Planned<'a> {
    placement: &'a Placement,
    page: Option<usize>,
    row: u16,
    depth: usize,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Viewport,
    History(usize),
    Remaining,
}
struct Payload<'a> {
    image: &'a Image,
    frame: usize,
    stage: Stage,
}
pub(crate) struct Plan<'a> {
    key: usize,
    screen: &'a Screen,
    first_resident: usize,
    images: Vec<&'a Image>,
    placements: Vec<Planned<'a>>,
    payloads: BTreeMap<Stage, Vec<Payload<'a>>>,
}
impl<'a> Plan<'a> {
    pub fn new(screen: &'a Screen, key: usize) -> Self {
        let mut rows = HashMap::new();
        let mut absolute = 0;
        let mut first_resident = 0;
        for (page_index, page) in screen.pages.pages.iter().enumerate() {
            for row in 0..usize::from(page.rows) {
                rows.insert(page.row_ids[row], (page_index, row as u16, absolute + row));
            }
            absolute += usize::from(page.rows);
            if absolute <= screen.history_len() {
                first_resident = page_index + 1;
            }
        }
        let mut images: Vec<_> = screen.graphics.images.values().collect();
        images.sort_by_key(|image| (image.identity, image.id));
        let mut placements = Vec::new();
        for placement in &screen.graphics.placements {
            let mut root = placement;
            let mut depth = 0;
            while let Some(parent) = root.parent {
                let Some(next) = screen.graphics.placements.get(parent) else {
                    break;
                };
                root = next;
                depth += 1;
                if depth > PARENT_CHAIN_LIMIT {
                    break;
                }
            }
            if root.parent.is_some() || !screen.graphics.images.contains_key(&placement.image_id) {
                continue;
            }
            let (page, row) = if root.virtual_placement {
                (None, 0)
            } else if let Some(&(page, row, _)) = rows.get(&root.row) {
                (Some(page), row)
            } else {
                continue;
            };
            placements.push(Planned {
                placement,
                page,
                row,
                depth,
            });
        }
        placements.sort_by_key(|planned| {
            (
                planned.page,
                planned.depth,
                planned.placement.image_id,
                planned.placement.placement_id,
            )
        });
        let viewport_start = screen.history_len().saturating_sub(screen.viewport_offset);
        let mut stages = HashMap::new();
        for planned in &placements {
            let root = planned
                .placement
                .resolve_chain(|key| screen.graphics.placements.get(key))
                .unwrap()
                .0;
            let stage = if root.virtual_placement
                || rows.get(&root.row).is_some_and(|&(_, _, row)| {
                    (viewport_start..viewport_start + screen.height()).contains(&row)
                }) {
                Stage::Viewport
            } else if let Some(page) = planned.page.filter(|&page| page < first_resident) {
                Stage::History(usize::MAX - page)
            } else {
                Stage::Remaining
            };
            stages
                .entry(planned.placement.image_id)
                .and_modify(|old: &mut Stage| *old = (*old).min(stage))
                .or_insert(stage);
        }
        let mut payloads = Vec::new();
        for image in &images {
            for frame in 1..=image.frames.len() + 1 {
                if image.frame(frame).is_some() {
                    payloads.push(Payload {
                        image,
                        frame,
                        stage: stages.get(&image.id).copied().unwrap_or(Stage::Remaining),
                    });
                }
            }
        }
        payloads.sort_by_key(|p| {
            (
                p.stage,
                p.image.identity,
                p.image.id,
                p.frame != p.image.current_frame + 1,
                p.frame,
            )
        });
        let mut staged = BTreeMap::<_, Vec<_>>::new();
        for payload in payloads {
            staged.entry(payload.stage).or_default().push(payload);
        }
        Self {
            key,
            screen,
            first_resident,
            images,
            placements,
            payloads: staged,
        }
    }
    pub fn storage(&self, out: &mut impl Write) -> io::Result<()> {
        let graphics = &self.screen.graphics;
        if self.images.is_empty() && self.placements.is_empty() && graphics.loading.is_none() {
            return Ok(());
        }
        let resident: Vec<_> = self
            .placements
            .iter()
            .filter(|p| p.page.is_none_or(|page| page >= self.first_resident))
            .collect();
        let mut data = Vec::new();
        u16_bytes(&mut data, self.key)?;
        word(&mut data, graphics.next_image);
        word(&mut data, graphics.next_placement);
        u32_bytes(&mut data, self.images.len())?;
        u32_bytes(&mut data, resident.len())?;
        data.push(u8::from(graphics.loading.is_some()));
        for image in &self.images {
            for value in [image.id, image.number, image.width, image.height] {
                word(&mut data, value);
            }
            data.extend_from_slice(&[1, 0]);
            long(
                &mut data,
                rgba_len(image.width, image.height)
                    .ok_or_else(|| invalid("invalid snapshot image dimensions"))?,
            );
            let animation = !image.frames.is_empty()
                || image.animation_state > 1
                || image.root_gap_ms != 0
                || image.max_loops != 0
                || image.completed_loops != 0;
            data.push(u8::from(animation));
            if animation {
                data.push(image.animation_state.saturating_sub(1).min(2));
                for value in [
                    image.max_loops,
                    image.completed_loops,
                    image.current_frame as u32,
                    image.root_gap_ms,
                ] {
                    word(&mut data, value);
                }
                u32_bytes(&mut data, image.frames.len())?;
                for frame in &image.frames {
                    word(&mut data, frame.gap_ms);
                    long(&mut data, image.width as usize * image.height as usize * 4);
                }
            }
        }
        for planned in resident {
            self.encode_placement(&mut data, planned, true)?;
        }
        if let Some(loading) = &graphics.loading {
            encode_loading(&mut data, loading)?;
        }
        record(out, STORAGE, &data)
    }
    fn encode_placement(
        &self,
        out: &mut Vec<u8>,
        planned: &Planned<'_>,
        resident: bool,
    ) -> io::Result<()> {
        let p = planned.placement;
        placement_key(out, p.image_id, p.placement_id);
        if let Some((id, parent)) = p.parent {
            out.push(2);
            placement_key(out, id, parent);
            for offset in p.parent_offset {
                word(out, offset as u32);
            }
        } else if p.virtual_placement {
            out.push(1);
        } else {
            out.push(0);
            if resident {
                u16_bytes(out, planned.page.unwrap() - self.first_resident)?;
            }
            u16_bytes(out, planned.row.into())?;
            u16_bytes(out, p.col)?;
        }
        for value in p
            .offset
            .into_iter()
            .chain(p.source)
            .chain([p.columns, p.rows, p.z as u32])
        {
            word(out, value);
        }
        Ok(())
    }
    pub fn viewport_payloads(&mut self, out: &mut impl Write) -> io::Result<()> {
        self.payloads(out, Stage::Viewport)
    }
    pub fn remaining_payloads(&mut self, out: &mut impl Write) -> io::Result<()> {
        self.viewport_payloads(out)?;
        self.payloads(out, Stage::Remaining)
    }
    fn payloads(&mut self, out: &mut impl Write, stage: Stage) -> io::Result<()> {
        for payload in self.payloads.remove(&stage).unwrap_or_default() {
            let pixels = payload.image.frame(payload.frame).unwrap();
            let mut header = Vec::with_capacity(18);
            u16_bytes(&mut header, self.key)?;
            word(&mut header, payload.image.id);
            u32_bytes(&mut header, payload.frame)?;
            long(&mut header, pixels.len());
            record_parts(out, PAYLOAD, &[&header, pixels])?;
        }
        Ok(())
    }
    pub fn page(&mut self, out: &mut impl Write, page: usize) -> io::Result<()> {
        let start = self.placements.partition_point(|p| p.page < Some(page));
        let end = self.placements.partition_point(|p| p.page <= Some(page));
        let placements = &self.placements[start..end];
        if !placements.is_empty() {
            let mut data = Vec::new();
            u16_bytes(&mut data, self.key)?;
            u32_bytes(&mut data, placements.len())?;
            for placement in placements {
                self.encode_placement(&mut data, placement, false)?;
            }
            record(out, PAGE_PLACEMENTS, &data)?;
        }
        self.payloads(out, Stage::History(usize::MAX - page))
    }
}

pub(crate) struct Target {
    storage: u64,
    identity: u64,
    format: u8,
    // Frame deletion must not redirect a payload to the new frame at that index.
    pending: Vec<Weak<Vec<u8>>>,
}
pub(crate) type Targets = HashMap<u32, Target>;

pub(crate) fn decode_storage(
    data: &[u8],
    screen: &mut Screen,
    key: usize,
    limit: usize,
) -> io::Result<Targets> {
    let mut r = Slice::new(data);
    if usize::from(r.u16()?) != key {
        return Err(invalid("unexpected snapshot graphics screen"));
    }
    let next_image = r.u32()?;
    let next_placement = r.u32()?;
    let image_count = r.u32()?;
    let placement_count = r.u32()?;
    let loading = r.u8()? != 0;
    screen.graphics.limit = limit.min(MAX_DATA);
    let mut targets = Targets::new();
    for _ in 0..image_count {
        let id = r.u32()?;
        let number = r.u32()?;
        let width = r.u32()?;
        let height = r.u32()?;
        let format = r.u8()?;
        let _flags = r.u8()?;
        let length = r.u64()?;
        let bytes = rgba_len(width, height);
        let mut valid =
            id != 0 && wire_len(width, height, format).is_some_and(|n| n as u64 == length);
        let mut image = Image {
            id,
            tmux_unmapped: false,
            tmux_sixel_cell: None,
            tmux_order: None,
            number,
            width,
            height,
            pixels: Arc::new(Vec::new()),
            generation: 0,
            identity: 0,
            frames: Vec::new(),
            current_frame: 0,
            root_gap_ms: 0,
            animation_state: 1,
            max_loops: 0,
            completed_loops: 0,
            frame_shown_at_ms: None,
        };
        if r.u8()? != 0 {
            image.animation_state = match r.u8()? {
                1 => 2,
                2 => 3,
                _ => 1,
            };
            image.max_loops = r.u32()?;
            image.completed_loops = r.u32()?;
            image.current_frame = r.u32()? as usize;
            image.root_gap_ms = r.u32()?;
            let count = r.u32()? as usize;
            let frames = r.take(
                count
                    .checked_mul(12)
                    .ok_or_else(|| invalid("snapshot frame count overflow"))?,
            )?;
            valid &= bytes
                .and_then(|n| n.checked_mul(count + 1))
                .is_some_and(|n| n <= screen.graphics.limit);
            let mut frames = Slice::new(frames);
            for _ in 0..count {
                let gap_ms = frames.u32()?;
                let length = frames.u64()?;
                valid &= bytes.is_some_and(|n| n as u64 == length);
                if valid {
                    image.frames.push(AnimationFrame {
                        gap_ms,
                        pixels: Arc::new(Vec::new()),
                    });
                }
            }
            if image.current_frame > image.frames.len() {
                image.current_frame = 0;
            }
        }
        if !valid || bytes.is_none_or(|bytes| bytes > screen.graphics.limit) {
            continue;
        }
        let graphics = &mut screen.graphics;
        if graphics.reserve(image.bytes(), id).is_err() {
            continue;
        }
        graphics.generation = graphics.generation.wrapping_add(1);
        image.generation = graphics.generation;
        image.identity = graphics.generation;
        targets.insert(
            id,
            Target {
                storage: graphics.identity,
                identity: image.identity,
                format,
                pending: std::iter::once(&image.pixels)
                    .chain(image.frames.iter().map(|frame| &frame.pixels))
                    .map(Arc::downgrade)
                    .collect(),
            },
        );
        graphics.images.insert(id, image);
    }
    let pages: Vec<_> = screen
        .pages
        .pages
        .iter()
        .map(|page| {
            (
                page.columns,
                page.row_ids[..usize::from(page.rows)].to_vec(),
            )
        })
        .collect();
    decode_placements(
        &mut r,
        placement_count,
        Some(screen),
        &pages,
        true,
        &targets,
    )?;
    if loading {
        decode_loading(&mut r, &mut screen.graphics)?;
    }
    r.finish()?;
    screen.graphics.next_image = next_image;
    screen.graphics.next_placement = next_placement;
    Ok(targets)
}
fn read_key(r: &mut Slice<'_>) -> io::Result<Option<(u32, PlacementId)>> {
    let image = r.u32()?;
    let tag = r.u8()?;
    let id = r.u32()?;
    Ok(match tag {
        0 => Some((image, PlacementId::Internal(id))),
        1 if id != 0 => Some((image, PlacementId::External(id))),
        _ => None,
    })
}
fn decode_placements(
    r: &mut Slice<'_>,
    count: u32,
    mut screen: Option<&mut Screen>,
    pages: &[(u16, Vec<u64>)],
    resident: bool,
    targets: &Targets,
) -> io::Result<()> {
    for _ in 0..count {
        let key = read_key(r)?;
        let kind = r.u8()?;
        let mut location = None;
        let mut parent = None;
        let mut parent_offset = [0; 2];
        let valid_location = match kind {
            0 => {
                let page = if resident { usize::from(r.u16()?) } else { 0 };
                let y = usize::from(r.u16()?);
                let x = usize::from(r.u16()?);
                location = pages
                    .get(page)
                    .filter(|(width, _)| x < usize::from(*width))
                    .and_then(|(_, rows)| rows.get(y))
                    .map(|&row| (row, x));
                location.is_some()
            }
            1 => resident,
            2 => {
                parent = read_key(r)?;
                parent_offset = [r.u32()? as i32, r.u32()? as i32];
                parent.is_some()
            }
            _ => false,
        };
        let offset = [r.u32()?, r.u32()?];
        let source = [r.u32()?, r.u32()?, r.u32()?, r.u32()?];
        let columns = r.u32()?;
        let rows = r.u32()?;
        let z = r.u32()? as i32;
        let Some((image_id, placement_id)) = key.filter(|_| valid_location) else {
            continue;
        };
        let Some(screen) = screen.as_deref_mut() else {
            continue;
        };
        let graphics = &mut screen.graphics;
        if graphics.placements.get((image_id, placement_id)).is_some()
            || !graphics.images.get(&image_id).is_some_and(|image| {
                targets.get(&image_id).is_some_and(|target| {
                    target.storage == graphics.identity && target.identity == image.identity
                })
            })
            || parent.is_some_and(|key| graphics.placements.get(key).is_none())
        {
            continue;
        }
        if let Some((parent_id, parent_key)) = parent {
            // Verify depth and cycles against the actual parent, including internal keys.
            let mut current = (parent_id, parent_key);
            let mut depth = 0;
            while let Some(p) = graphics.placements.get(current) {
                depth += 1;
                if depth > PARENT_CHAIN_LIMIT {
                    break;
                }
                let Some(next) = p.parent else {
                    break;
                };
                current = next;
            }
            if depth > PARENT_CHAIN_LIMIT {
                continue;
            }
        }
        let (row, col) = location.unwrap_or((0, 0));
        graphics.placements.insert(Placement {
            image_id,
            placement_id,
            row,
            col,
            columns,
            rows,
            viewport_row: None,
            z,
            source,
            offset,
            virtual_placement: kind == 1,
            parent,
            parent_offset,
        });
        graphics.generation = graphics.generation.wrapping_add(1);
    }
    Ok(())
}
pub(crate) fn decode_page(
    data: &[u8],
    key: usize,
    screen: Option<&mut Screen>,
    page: Option<(u16, Vec<u64>)>,
    targets: &Targets,
) -> io::Result<()> {
    let mut r = Slice::new(data);
    if usize::from(r.u16()?) != key {
        return Err(invalid("unexpected snapshot graphics page screen"));
    }
    let count = r.u32()?;
    decode_placements(
        &mut r,
        count,
        screen,
        &page.into_iter().collect::<Vec<_>>(),
        false,
        targets,
    )?;
    r.finish()
}
pub(crate) fn decode_payload(
    source: &mut impl io::Read,
    header: [u8; 10],
    terminal: &mut Terminal,
    identities: &[Option<u64>; 2],
    targets: &[Targets; 2],
) -> io::Result<crate::snapshot::ImageProgress> {
    let payload_len = u32::from_le_bytes(header[2..6].try_into().unwrap()) as usize;
    if payload_len < 18 {
        return Err(invalid("short snapshot image payload"));
    }
    let mut metadata = [0; 18];
    source.read_exact(&mut metadata)?;
    let mut r = Slice::new(&metadata);
    let key = usize::from(r.u16()?);
    let id = r.u32()?;
    let frame = r.u32()? as usize;
    let length = r.u64()?;
    if key > 1 || identities[key].is_none() {
        return Err(invalid("unexpected snapshot graphics screen"));
    }
    let mut progress = crate::snapshot::ImageProgress {
        screen: key as u8,
        image_id: id,
        frame: frame as u32,
        bytes: 0,
    };
    let screen = if key == 0 {
        Some(&mut terminal.primary)
    } else {
        terminal.alternate.as_mut()
    }
    .filter(|screen| Some(screen.metadata.identity) == identities[key]);
    let ticket = screen.as_ref().and_then(|screen| {
        let target = targets[key].get(&id)?;
        let image = screen.graphics.images.get(&id)?;
        if target.storage != screen.graphics.identity
            || image.identity != target.identity
            || image.frame(frame).is_some()
        {
            return None;
        }
        let format = if frame == 1 {
            target.format
        } else if frame >= 2 && frame <= image.frames.len() + 1 {
            1
        } else {
            return None;
        };
        let current = if frame == 1 {
            &image.pixels
        } else {
            &image.frames[frame - 2].pixels
        };
        if !Weak::ptr_eq(target.pending.get(frame - 1)?, &Arc::downgrade(current)) {
            return None;
        }
        let expected = wire_len(image.width, image.height, format)?;
        (expected as u64 == length).then_some((format, expected))
    });
    let mut actual = crc32c::crc32c_append(crc32c::crc32c(&header[..6]), &metadata);
    let remaining = payload_len - metadata.len();
    let Some((format, expected)) = ticket else {
        let mut remaining = remaining;
        let mut buffer = [0; 8192];
        while remaining > 0 {
            let count = remaining.min(buffer.len());
            source.read_exact(&mut buffer[..count])?;
            actual = crc32c::crc32c_append(actual, &buffer[..count]);
            remaining -= count;
        }
        if actual != u32::from_le_bytes(header[6..].try_into().unwrap()) {
            return Err(invalid("snapshot checksum mismatch"));
        }
        return Ok(progress);
    };
    if remaining != expected {
        return Err(invalid("snapshot pixel payload length mismatch"));
    }
    let mut data = Vec::new();
    data.try_reserve_exact(expected)
        .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
    data.resize(expected, 0);
    source.read_exact(&mut data)?;
    actual = crc32c::crc32c_append(actual, &data);
    if actual != u32::from_le_bytes(header[6..].try_into().unwrap()) {
        return Err(invalid("snapshot checksum mismatch"));
    }
    let screen = screen.unwrap();
    let image = screen.graphics.images.get_mut(&id).unwrap();
    let rgba_len = rgba_len(image.width, image.height).unwrap();
    data.try_reserve_exact(rgba_len - data.len())
        .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
    data.resize(rgba_len, 0);
    if format != 1 {
        let stride = match format {
            0 => 3,
            3 => 2,
            4 => 1,
            _ => unreachable!(),
        };
        for index in (0..rgba_len / 4).rev() {
            let pixel = &data[index * stride..index * stride + stride];
            let rgba = match format {
                0 => [pixel[0], pixel[1], pixel[2], 255],
                3 => [pixel[0], pixel[0], pixel[0], pixel[1]],
                _ => [pixel[0], pixel[0], pixel[0], 255],
            };
            data[index * 4..index * 4 + 4].copy_from_slice(&rgba);
        }
    }
    image.set_frame(frame, pixels::intern(image.width, image.height, data));
    screen.graphics.generation = screen.graphics.generation.wrapping_add(1);
    image.generation = screen.graphics.generation;
    progress.bytes = expected;
    Ok(progress)
}

fn encode_format(command: &Command) -> io::Result<u8> {
    match command.values.get(&b'f').copied().unwrap_or(32) {
        24 => Ok(0),
        32 => Ok(1),
        100 => Ok(2),
        _ => Err(invalid("unsupported snapshot loading format")),
    }
}
fn encode_loading(out: &mut Vec<u8>, loading: &Loading) -> io::Result<()> {
    let command = &loading.command;
    for value in [
        loading.image_id,
        command.n(b'I'),
        command.n(b's'),
        command.n(b'v'),
    ] {
        word(out, value);
    }
    out.push(encode_format(command)?);
    out.push(u8::from(command.n(b'o') == u32::from(b'z')));
    out.push(
        u8::from(command.n(b'N') & 1 != 0)
            | (u8::from(command.n(b'i') == 0 && command.n(b'I') == 0) << 1),
    );
    out.push(command.quiet().min(2) as u8);
    for key in b"iIp" {
        word(out, command.n(*key));
    }
    word(
        out,
        if command.action() == b'f' {
            command.n(b'r')
        } else {
            0
        },
    );
    out.push(u8::from(command.action() == b'T'));
    if command.action() == b'T' {
        for key in b"iIpxywhXYcr" {
            word(out, command.n(*key));
        }
        out.push(u8::from(command.n(b'C') == 1));
        out.push(u8::from(command.n(b'U') != 0));
        for key in b"PQHVz" {
            word(out, command.n(*key));
        }
    }
    out.push(u8::from(command.action() == b'f'));
    if command.action() == b'f' {
        out.push(encode_format(command)?);
        out.push(0);
        for key in b"svSOiIp" {
            word(out, command.n(*key));
        }
        out.push(u8::from(command.n(b'o') == u32::from(b'z')));
        out.push(u8::from(command.n(b'm') != 0));
        for key in b"Nxycrz" {
            word(out, command.n(*key));
        }
        out.push(u8::from(command.n(b'X') == 1));
        word(out, command.n(b'Y'));
    }
    u32_bytes(out, loading.data.len())?;
    out.extend_from_slice(&loading.data);
    Ok(())
}
fn command_format(format: u8) -> Option<i64> {
    match format {
        0 => Some(24),
        1 => Some(32),
        2 => Some(100),
        _ => None,
    }
}
fn decode_loading(r: &mut Slice<'_>, graphics: &mut Graphics) -> io::Result<()> {
    let id = r.u32()?;
    let number = r.u32()?;
    let width = r.u32()?;
    let height = r.u32()?;
    let format = r.u8()?;
    let compression = r.u8()?;
    let flags = r.u8()?;
    let quiet = r.u8()?;
    let response_id = r.u32()?;
    let response_number = r.u32()?;
    let response_placement = r.u32()?;
    let response_frame = r.u32()?;
    let mut valid = command_format(format).is_some() && compression <= 1 && quiet <= 2;
    let mut command = Command::default();
    command.values.extend([
        (b'i', response_id.into()),
        (b'I', response_number.into()),
        (b'p', response_placement.into()),
        (b'r', response_frame.into()),
        (b's', width.into()),
        (b'v', height.into()),
        (b'f', command_format(format).unwrap_or(32)),
        (b'q', quiet.into()),
        (b'm', 1),
        (b'N', i64::from(flags & 1)),
    ]);
    if compression == 1 {
        command.values.insert(b'o', i64::from(b'z'));
    }
    if r.u8()? != 0 {
        command.values.insert(b'a', i64::from(b'T'));
        for key in b"iIpxywhXYcr" {
            command.values.insert(*key, r.u32()?.into());
        }
        let movement = r.u8()?;
        valid &= movement <= 1;
        command.values.insert(b'C', movement.into());
        command.values.insert(b'U', i64::from(r.u8()? != 0));
        for key in b"PQ" {
            command.values.insert(*key, r.u32()?.into());
        }
        for key in b"HVz" {
            command.values.insert(*key, i64::from(r.u32()? as i32));
        }
    }
    let mut image_generation = None;
    if r.u8()? != 0 {
        command.values.insert(b'a', i64::from(b'f'));
        let frame_format = r.u8()?;
        let medium = r.u8()?;
        valid &= command_format(frame_format).is_some() && medium <= 3;
        command
            .values
            .insert(b'f', command_format(frame_format).unwrap_or(32));
        for key in b"svSOiIp" {
            command.values.insert(*key, r.u32()?.into());
        }
        let compression = r.u8()?;
        valid &= compression <= 1;
        command.values.remove(&b'o');
        if compression == 1 {
            command.values.insert(b'o', i64::from(b'z'));
        }
        command.values.insert(b'm', i64::from(r.u8()? != 0));
        for key in b"Nxycr" {
            command.values.insert(*key, r.u32()?.into());
        }
        command.values.insert(b'z', i64::from(r.u32()? as i32));
        let mode = r.u8()?;
        valid &= mode <= 1;
        command.values.insert(b'X', mode.into());
        command.values.insert(b'Y', r.u32()?.into());
        image_generation = Some(
            graphics
                .resolve_id(id, number)
                .and_then(|id| graphics.images.get(&id))
                .map_or(0, |image| image.identity),
        );
    }
    let length = r.u32()? as usize;
    let data = r.take(length)?;
    if valid && graphics.limit != 0 && length <= MAX_DATA {
        graphics.loading = Some(Loading {
            command,
            data: data.to_vec(),
            image_id: id,
            image_generation,
        });
    }
    Ok(())
}
