//! Tmux legacy/checksummed and JSON-v2 layouts with integer cell geometry.
//! The wire representation retains server IDs independently from pane indexes.

use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};
use std::{error::Error as StdError, fmt, fmt::Write};
mod floating;
mod presets;
pub use presets::PRESETS;

/// Bounds untrusted layouts before recursive construction.
pub const MAX_DEPTH: usize = 1000;
pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_DIMENSION: u32 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pane {
    pub id: Option<u32>,
    pub index: Option<u32>,
    pub active: bool,
    pub last: Option<u32>,
    pub z: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Pane(Pane),
    Split { axis: Axis, children: Vec<Layout> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub geometry: Geometry,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);
impl fmt::Display for Error {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(&self.0)
    }
}
impl StdError for Error {}
type Result<T> = std::result::Result<T, Error>;
fn invalid(message: &str) -> Error {
    Error(message.to_owned())
}

pub fn checksum(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0u16, |sum, byte| {
        sum.rotate_right(1).wrapping_add(u16::from(*byte))
    })
}

pub fn parse(input: &str) -> Result<Layout> {
    Layout::parse(input)
}

impl Layout {
    pub fn pane(id: u32, width: u32, height: u32) -> Self {
        Self {
            geometry: Geometry {
                width,
                height,
                x: 0,
                y: 0,
            },
            kind: Kind::Pane(Pane {
                id: Some(id),
                ..Pane::default()
            }),
        }
    }

    pub fn parse(input: &str) -> Result<Self> {
        if input.len() > MAX_BYTES {
            return Err(invalid("layout exceeds byte limit"));
        }
        if input.starts_with('{') {
            return parse_json(input, true);
        }
        let (prefix, body) = input
            .split_once(',')
            .ok_or_else(|| invalid("missing layout checksum"))?;
        if prefix.len() != 4
            || u16::from_str_radix(prefix, 16).ok() != Some(checksum(body.as_bytes()))
        {
            return Err(invalid("layout checksum mismatch"));
        }
        let mut parser = Parser {
            bytes: body.as_bytes(),
            at: 0,
        };
        let mut layout = parser.node(0)?;
        if parser.bytes.get(parser.at) == Some(&b'<') {
            parser.at += 1;
            let mut z = 0;
            let mut seen = std::collections::HashSet::new();
            loop {
                let mut floating = parser.node(1)?;
                let Kind::Pane(pane) = &mut floating.kind else {
                    return Err(invalid("floating suffix must contain panes"));
                };
                let id = pane
                    .id
                    .ok_or_else(|| invalid("floating pane needs an ID"))?;
                if !seen.insert(id) {
                    return Err(invalid("duplicate floating pane ID"));
                }
                pane.z = Some(z);
                z += 1;
                // C3.7c writes floating leaves in the main tree and again in
                // a suffix describing stacking order. Preserve one identity.
                if let Some(existing) = layout.find_mut(id) {
                    if existing.geometry != floating.geometry {
                        return Err(invalid("conflicting floating pane geometry"));
                    }
                    existing.kind = floating.kind;
                } else {
                    layout.append_floating(vec![floating], false)?;
                }
                match parser.bytes.get(parser.at) {
                    Some(b',') => parser.at += 1,
                    Some(b'>') => {
                        parser.at += 1;
                        break;
                    }
                    _ => return Err(invalid("invalid floating suffix")),
                }
            }
        }
        if parser.at != parser.bytes.len() {
            return Err(invalid("trailing layout data"));
        }
        Ok(layout)
    }

    /// C can emit unary legacy groups as JSON, but rejects them as command input.
    pub fn parse_command(input: &str) -> Result<Self> {
        if input.len() > MAX_BYTES {
            return Err(invalid("layout exceeds byte limit"));
        }
        if input.starts_with('{') {
            parse_json(input, false)
        } else {
            Self::parse(input)
        }
    }

    pub fn to_legacy(&self) -> Result<String> {
        let mut body = String::new();
        let Some(tiled) = self.tiled_copy() else {
            return Ok("0000,".into());
        };
        tiled.write_legacy(&mut body, 0)?;
        Ok(format!("{:04x},{body}", checksum(body.as_bytes())))
    }

    pub fn to_json_v2(&self) -> Result<String> {
        let mut index = 0;
        Ok(format!(
            "{{\"V\":2,\"L\":{}}}",
            self.json_node(0, &mut index)?
        ))
    }

    pub fn panes(&self) -> Vec<&Pane> {
        let mut panes = Vec::new();
        self.visit(&mut |layout| {
            if let Kind::Pane(pane) = &layout.kind {
                panes.push(pane);
            }
        });
        panes
    }

    pub fn find(&self, id: u32) -> Option<&Layout> {
        match &self.kind {
            Kind::Pane(pane) => (pane.id == Some(id)).then_some(self),
            Kind::Split { children, .. } => children.iter().find_map(|child| child.find(id)),
        }
    }

    pub fn split(&mut self, id: u32, new_id: u32, axis: Axis) -> Result<()> {
        self.split_sized(id, new_id, axis, None, false)
    }

    /// Split with tmux's cell arithmetic: the one-cell separator belongs to
    /// neither pane. An explicit size describes the new pane, before or after.
    pub fn split_sized(
        &mut self,
        id: u32,
        new_id: u32,
        axis: Axis,
        new_size: Option<u32>,
        before: bool,
    ) -> Result<()> {
        self.validate_tiled()?;
        if self.find(new_id).is_some() {
            return Err(invalid("pane ID already exists"));
        }
        let pane = self.find(id).ok_or_else(|| invalid("pane not found"))?;
        if pane.is_floating() {
            return Err(invalid("can't split a floating pane"));
        }
        let size = match axis {
            Axis::Horizontal => pane.geometry.width,
            Axis::Vertical => pane.geometry.height,
        };
        if size < 3 {
            return Err(invalid("pane is too small to split"));
        }
        let second = match new_size {
            None => size.div_ceil(2) - 1,
            Some(requested) if before => size.wrapping_sub(requested).wrapping_sub(1),
            Some(requested) => requested.min(size - 2),
        }
        .clamp(1, size - 2);
        let first = size - 1 - second;
        let mut layout = self.clone();
        layout.split_inner(id, new_id, axis, first, second, before);
        layout.validate_tiled()?;
        *self = layout;
        Ok(())
    }

    /// Split the entire window, proportionally shrinking the existing tree.
    /// Full-window splitting uses different rounding from ordinary resize.
    pub fn split_full_sized(
        &mut self,
        new_id: u32,
        axis: Axis,
        new_size: Option<u32>,
        before: bool,
    ) -> Result<()> {
        self.validate_tiled()?;
        if self.find(new_id).is_some() {
            return Err(invalid("pane ID already exists"));
        }
        let geometry = self.geometry;
        let mut pair = Self::pane(new_id.wrapping_add(1), geometry.width, geometry.height);
        pair.split_sized(new_id.wrapping_add(1), new_id, axis, new_size, before)?;
        let Kind::Split { children, .. } = pair.kind else {
            unreachable!()
        };
        let old_geometry = children[usize::from(before)].geometry;
        let new_geometry = children[usize::from(!before)].geometry;
        let mut old = self.clone();
        old.position_proportional(old_geometry)?;
        let mut new = Self::pane(new_id, new_geometry.width, new_geometry.height);
        new.geometry = new_geometry;
        let mut children = match old.kind {
            Kind::Split {
                axis: old_axis,
                children,
            } if old_axis == axis => children,
            _ => vec![old],
        };
        if before {
            children.insert(0, new);
        } else {
            children.push(new);
        }
        let mut layout = Self {
            geometry,
            kind: Kind::Split { axis, children },
        };
        layout.position(geometry);
        layout.validate_tiled()?;
        *self = layout;
        Ok(())
    }

    fn position_proportional(&mut self, geometry: Geometry) -> Result<()> {
        if !self.has_tiled() {
            return Ok(());
        }
        let minimum = self.minimum();
        if geometry.width < minimum.0 || geometry.height < minimum.1 {
            return Err(invalid("no space for a new pane"));
        }
        let Kind::Split { axis, children } = &mut self.kind else {
            self.geometry = geometry;
            return Ok(());
        };
        let extent = |g: Geometry| {
            if *axis == Axis::Horizontal {
                g.width
            } else {
                g.height
            }
        };
        let previous = extent(self.geometry);
        let size = extent(geometry);
        let mut children: Vec<_> = children.iter_mut().filter(|c| c.has_tiled()).collect();
        let count = children.len();
        let mut available = size;
        let mut offset = 0;
        for (index, child) in children.iter_mut().enumerate() {
            let minimum = child.minimum();
            let minimum = if *axis == Axis::Horizontal {
                minimum.0
            } else {
                minimum.1
            };
            let length = if index + 1 == count {
                available
            } else {
                let reserved = (2 * (count - index - 1) as u32).max(minimum);
                let max = available
                    .checked_sub(reserved)
                    .ok_or_else(|| invalid("no space for a new pane"))?;
                ((u64::from(extent(child.geometry)) * u64::from(size) / u64::from(previous)) as u32)
                    .min(max)
                    .max(1)
            };
            let next = match axis {
                Axis::Horizontal => Geometry {
                    width: length,
                    x: geometry.x + offset as i32,
                    ..geometry
                },
                Axis::Vertical => Geometry {
                    height: length,
                    y: geometry.y + offset as i32,
                    ..geometry
                },
            };
            child.position_proportional(next)?;
            if index + 1 != count {
                available = available
                    .checked_sub(length + 1)
                    .ok_or_else(|| invalid("no space for a new pane"))?;
            }
            offset += length + 1;
        }
        self.geometry = geometry;
        Ok(())
    }

    fn split_inner(
        &mut self,
        id: u32,
        new_id: u32,
        axis: Axis,
        first: u32,
        second: u32,
        before: bool,
    ) -> bool {
        let make_children = |original: Layout| {
            let geometry = original.geometry;
            let new = Self::pane(new_id, geometry.width, geometry.height);
            let mut children = if before {
                vec![new, original]
            } else {
                vec![original, new]
            };
            children[0].geometry = match axis {
                Axis::Horizontal => Geometry {
                    width: first,
                    ..geometry
                },
                Axis::Vertical => Geometry {
                    height: first,
                    ..geometry
                },
            };
            children[1].geometry = match axis {
                Axis::Horizontal => Geometry {
                    width: second,
                    x: geometry.x + first as i32 + 1,
                    ..geometry
                },
                Axis::Vertical => Geometry {
                    height: second,
                    y: geometry.y + first as i32 + 1,
                    ..geometry
                },
            };
            children
        };
        if matches!(&self.kind, Kind::Pane(pane) if pane.id == Some(id)) {
            self.kind = Kind::Split {
                axis,
                children: make_children(self.clone()),
            };
            true
        } else if let Kind::Split {
            axis: parent_axis,
            children,
        } = &mut self.kind
        {
            if *parent_axis == axis
                && let Some(index) = children.iter().position(
                    |child| matches!(&child.kind, Kind::Pane(pane) if pane.id == Some(id)),
                )
            {
                let original = children.remove(index);
                children.splice(index..index, make_children(original));
                true
            } else {
                children
                    .iter_mut()
                    .any(|child| child.split_inner(id, new_id, axis, first, second, before))
            }
        } else {
            false
        }
    }

    /// Validate the tiled footprint and independent floating pane geometry.
    pub fn validate_tiled(&self) -> Result<()> {
        fn visit(
            layout: &Layout,
            depth: usize,
            ids: &mut std::collections::HashSet<u32>,
        ) -> Result<()> {
            check_depth(depth)?;
            check_node_geometry(layout.geometry, matches!(layout.kind, Kind::Pane(_)))?;
            match &layout.kind {
                Kind::Pane(pane) => {
                    if let Some(id) = pane.id
                        && !ids.insert(id)
                    {
                        return Err(invalid("duplicate pane ID"));
                    }
                }
                Kind::Split { axis, children } => {
                    if children.is_empty() {
                        return Err(invalid("split needs a child"));
                    }
                    let mut offset = match axis {
                        Axis::Horizontal => layout.geometry.x,
                        Axis::Vertical => layout.geometry.y,
                    };
                    for child in children {
                        visit(child, depth + 1, ids)?;
                        if !child.has_tiled() {
                            continue;
                        }
                        let fits = match axis {
                            Axis::Horizontal => {
                                child.geometry.x == offset
                                    && child.geometry.y == layout.geometry.y
                                    && child.geometry.height == layout.geometry.height
                            }
                            Axis::Vertical => {
                                child.geometry.y == offset
                                    && child.geometry.x == layout.geometry.x
                                    && child.geometry.width == layout.geometry.width
                            }
                        };
                        if !fits {
                            return Err(invalid("child geometry leaves a gap or overlap"));
                        }
                        offset += match axis {
                            Axis::Horizontal => child.geometry.width,
                            Axis::Vertical => child.geometry.height,
                        } as i32
                            + 1;
                    }
                    let end = match axis {
                        Axis::Horizontal => layout.geometry.x + layout.geometry.width as i32,
                        Axis::Vertical => layout.geometry.y + layout.geometry.height as i32,
                    };
                    if children.iter().any(Layout::has_tiled) && offset - 1 != end {
                        return Err(invalid("children do not fill their parent"));
                    }
                }
            }
            Ok(())
        }
        visit(self, 0, &mut std::collections::HashSet::new())
    }

    /// Assign existing pane IDs in traversal order; count mismatches are atomic errors.
    pub fn assign_panes(&mut self, ids: &[u32]) -> Result<()> {
        if self.panes().len() != ids.len()
            || ids
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != ids.len()
        {
            return Err(invalid(
                "pane IDs must be unique and match the layout leaf count",
            ));
        }
        fn assign(layout: &mut Layout, ids: &mut impl Iterator<Item = u32>) {
            match &mut layout.kind {
                Kind::Pane(pane) => {
                    pane.id = ids.next();
                }
                Kind::Split { children, .. } => {
                    for child in children {
                        assign(child, ids);
                    }
                }
            }
        }
        assign(self, &mut ids.iter().copied());
        Ok(())
    }

    /// Install a validated tiled layout over the current panes, preserving IDs.
    pub fn set_layout(&mut self, mut layout: Layout) -> Result<()> {
        let ids = self
            .panes()
            .into_iter()
            .map(|pane| pane.id.ok_or_else(|| invalid("pane has no ID")))
            .collect::<Result<Vec<_>>>()?;
        layout.assign_panes(&ids)?;
        layout.validate_tiled()?;
        *self = layout;
        Ok(())
    }

    /// Apply server command semantics: trim excess cells from bottom-right and
    /// assign current pane order to JSON indexes (or legacy traversal order).
    pub fn for_panes(mut self, ids: &[u32]) -> Result<Self> {
        let panes = self.panes();
        let count = panes.len();
        if ids.is_empty() || ids.len() > count {
            return Err(Error(format!("have {} panes but need {count}", ids.len())));
        }
        if panes.iter().filter(|p| p.active).count() > 1 {
            return Err(invalid("more than one active pane"));
        }
        for (name, values) in [
            (
                "pane index",
                panes.iter().filter_map(|p| p.index).collect::<Vec<_>>(),
            ),
            (
                "pane last index",
                panes.iter().filter_map(|p| p.last).collect(),
            ),
        ] {
            let mut unique = std::collections::BTreeSet::new();
            if values.iter().any(|value| !unique.insert(*value)) {
                return Err(Error(format!("duplicate {name}")));
            }
        }
        self.assign_panes(&(0..count as u32).collect::<Vec<_>>())?;
        self.validate_tiled()?;
        while self.panes().len() > ids.len() {
            let id = self.panes().last().unwrap().id.unwrap();
            if !self.remove(id) {
                return Err(invalid("cannot remove excess layout cell"));
            }
        }
        let panes = self.panes();
        let mut order: Vec<_> = panes
            .iter()
            .enumerate()
            .map(|(at, p)| (p.index.unwrap_or(at as u32), p.id.unwrap()))
            .collect();
        order.sort_unstable();
        let mapping: std::collections::BTreeMap<_, _> = order
            .iter()
            .zip(ids)
            .map(|((_, old), id)| (*old, *id))
            .collect();
        fn assign(layout: &mut Layout, mapping: &std::collections::BTreeMap<u32, u32>) {
            match &mut layout.kind {
                Kind::Pane(pane) => pane.id = Some(mapping[&pane.id.unwrap()]),
                Kind::Split { children, .. } => {
                    for child in children {
                        assign(child, mapping);
                    }
                }
            }
        }
        assign(&mut self, &mapping);
        self.normalize_z();
        self.validate_tiled()?;
        Ok(self)
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let geometry = Geometry {
            width,
            height,
            ..self.geometry
        };
        self.validate_tiled()?;
        check_geometry(geometry)?;
        let minimum = self.minimum();
        if width < minimum.0 || height < minimum.1 {
            return Err(invalid("layout does not fit requested size"));
        }
        if matches!(self.kind, Kind::Split { .. }) {
            self.geometry = geometry;
        }
        self.position(geometry);
        Ok(())
    }

    /// Move the pane's right/bottom separator; for a last child this is its
    /// left/top separator. Ancestors perpendicular to the axis move together.
    pub fn resize_border(&mut self, id: u32, axis: Axis, change: i32) -> Result<()> {
        self.validate_tiled()?;
        if self.find(id).is_none() {
            return Err(invalid("pane not found"));
        }
        self.resize_border_inner(id, axis, change, None);
        Ok(())
    }

    pub fn resize_pane_to(&mut self, id: u32, axis: Axis, size: u32) -> Result<()> {
        self.validate_tiled()?;
        if size == 0 || size > MAX_DIMENSION {
            return Err(invalid("invalid pane size"));
        }
        if self.find(id).is_none() {
            return Err(invalid("pane not found"));
        }
        self.resize_border_inner(id, axis, 0, Some(size));
        Ok(())
    }

    fn resize_border_inner(&mut self, id: u32, axis: Axis, change: i32, size: Option<u32>) -> bool {
        let Kind::Split {
            axis: parent_axis,
            children,
        } = &mut self.kind
        else {
            return false;
        };
        let Some(index) = children.iter().position(|child| child.find(id).is_some()) else {
            return false;
        };
        if children[index].resize_border_inner(id, axis, change, size) {
            return true;
        }
        if *parent_axis != axis {
            return false;
        }
        let mut children: Vec<_> = children.iter_mut().filter(|c| c.has_tiled()).collect();
        if children.len() < 2 {
            return false;
        }
        let Some(index) = children.iter().position(|child| child.find(id).is_some()) else {
            return false;
        };
        let extent = |child: &Layout| match axis {
            Axis::Horizontal => child.geometry.width,
            Axis::Vertical => child.geometry.height,
        };
        let min = |child: &Layout| match axis {
            Axis::Horizontal => child.minimum().0,
            Axis::Vertical => child.minimum().1,
        };
        let last = index + 1 == children.len();
        let border = if last { index - 1 } else { index };
        let mut change = size
            .map(|size| {
                let delta = size as i32 - extent(children[index]) as i32;
                if last { -delta } else { delta }
            })
            .unwrap_or(change);
        let resize = |child: &mut Layout, delta: i32| {
            let mut geometry = child.geometry;
            match axis {
                Axis::Horizontal => geometry.width = (geometry.width as i64 + delta as i64) as u32,
                Axis::Vertical => geometry.height = (geometry.height as i64 + delta as i64) as u32,
            }
            child.position(geometry);
        };
        if change > 0 {
            let donors: Vec<_> = ((border + 1)..children.len())
                .chain((0..border).rev())
                .collect();
            for donor in donors {
                let take =
                    (extent(children[donor]) - min(children[donor])).min(change as u32) as i32;
                resize(children[donor], -take);
                resize(children[border], take);
                change -= take;
                if change == 0 {
                    break;
                }
            }
        } else {
            for donor in (0..=border).rev() {
                let take = (extent(children[donor]) - min(children[donor]))
                    .min(change.unsigned_abs()) as i32;
                resize(children[donor], -take);
                resize(children[border + 1], take);
                change += take;
                if change == 0 {
                    break;
                }
            }
        }
        self.position(self.geometry);
        true
    }

    /// Remove a descendant pane and collapse redundant split nodes. The root
    /// pane cannot be removed: its owning window must be removed by the caller.
    pub fn remove(&mut self, id: u32) -> bool {
        if self.validate_tiled().is_err() {
            return false;
        }
        let geometry = self.geometry;
        let Kind::Split { axis, children } = &mut self.kind else {
            return false;
        };
        let Some(index) = children.iter().position(|c| c.find(id).is_some()) else {
            return false;
        };
        let old = children[index].geometry;
        let was_tiled = children[index].has_tiled();
        let removed_leaf = matches!(&children[index].kind, Kind::Pane(_));
        if removed_leaf {
            children.remove(index);
        } else if !children[index].remove(id) {
            return false;
        }
        if was_tiled && (removed_leaf || !children[index].has_tiled()) {
            let start = index + usize::from(!removed_leaf);
            if let Some(neighbor) = (start..children.len())
                .chain((0..index).rev())
                .find(|i| children[*i].has_tiled())
            {
                let mut next = children[neighbor].geometry;
                match axis {
                    Axis::Horizontal => next.width += old.width + 1,
                    Axis::Vertical => next.height += old.height + 1,
                }
                children[neighbor].position(next);
            }
        }
        if children.len() == 1 {
            *self = children.remove(0);
        }
        self.position(geometry);
        self.normalize_z();
        true
    }

    fn minimum(&self) -> (u32, u32) {
        if !self.has_tiled() {
            return (0, 0);
        }
        match &self.kind {
            Kind::Pane(_) => (1, 1),
            Kind::Split { axis, children } => {
                let mut sum = children
                    .iter()
                    .filter(|c| c.has_tiled())
                    .count()
                    .saturating_sub(1) as u32;
                let mut cross = 1;
                for child in children {
                    if !child.has_tiled() {
                        continue;
                    }
                    let (width, height) = child.minimum();
                    let (along, other) = match axis {
                        Axis::Horizontal => (width, height),
                        Axis::Vertical => (height, width),
                    };
                    sum = sum.saturating_add(along);
                    cross = cross.max(other);
                }
                match axis {
                    Axis::Horizontal => (sum, cross),
                    Axis::Vertical => (cross, sum),
                }
            }
        }
    }

    fn position(&mut self, geometry: Geometry) {
        if !self.has_tiled() {
            return;
        }
        self.geometry = geometry;
        let Kind::Split { axis, children } = &mut self.kind else {
            return;
        };
        if children.len() == 1 {
            children[0].position(geometry);
            return;
        }
        let mut children: Vec<_> = children.iter_mut().filter(|c| c.has_tiled()).collect();
        let available = match axis {
            Axis::Horizontal => geometry.width,
            Axis::Vertical => geometry.height,
        } - children.len().saturating_sub(1) as u32;
        let minimum: Vec<u32> = children
            .iter()
            .map(|child| {
                let value = child.minimum();
                match axis {
                    Axis::Horizontal => value.0,
                    Axis::Vertical => value.1,
                }
            })
            .collect();
        let mut lengths: Vec<u32> = children
            .iter()
            .enumerate()
            .map(|(i, child)| {
                let size = match axis {
                    Axis::Horizontal => child.geometry.width,
                    Axis::Vertical => child.geometry.height,
                };
                size.max(minimum[i])
            })
            .collect();
        let mut change =
            i64::from(available) - lengths.iter().map(|size| i64::from(*size)).sum::<i64>();
        // Tmux distributes size changes in pane order, preserving the existing
        // split rather than scaling each pane proportionally.
        while change != 0 {
            for (i, length) in lengths.iter_mut().enumerate() {
                if change > 0 {
                    *length += 1;
                    change -= 1;
                } else if change < 0 && *length > minimum[i] {
                    *length -= 1;
                    change += 1;
                }
                if change == 0 {
                    break;
                }
            }
        }
        let mut offset = 0;
        for (index, child) in children.iter_mut().enumerate() {
            let length = lengths[index];
            let next = match axis {
                Axis::Horizontal => Geometry {
                    width: length,
                    x: geometry.x + offset as i32,
                    ..geometry
                },
                Axis::Vertical => Geometry {
                    height: length,
                    y: geometry.y + offset as i32,
                    ..geometry
                },
            };
            child.position(next);
            offset += length + 1;
        }
    }

    fn visit<'a>(&'a self, visitor: &mut impl FnMut(&'a Layout)) {
        visitor(self);
        if let Kind::Split { children, .. } = &self.kind {
            for child in children {
                child.visit(visitor);
            }
        }
    }

    fn write_legacy(&self, result: &mut String, depth: usize) -> Result<()> {
        check_depth(depth)?;
        check_geometry(self.geometry)?;
        if self.geometry.x < 0 || self.geometry.y < 0 {
            return Err(invalid("negative coordinates cannot use legacy layout"));
        }
        write!(
            result,
            "{}x{},{},{}",
            self.geometry.width, self.geometry.height, self.geometry.x, self.geometry.y
        )
        .unwrap();
        match &self.kind {
            Kind::Pane(pane) => {
                if pane.z.is_some() {
                    return Err(invalid("floating panes cannot use legacy layout"));
                }
                write!(
                    result,
                    ",{}",
                    pane.id
                        .ok_or_else(|| invalid("legacy layout needs pane IDs"))?
                )
                .unwrap();
            }
            Kind::Split { axis, children } => {
                if children.is_empty() {
                    return Err(invalid("split needs a child"));
                }
                let (open, close) = match axis {
                    Axis::Horizontal => ('{', '}'),
                    Axis::Vertical => ('[', ']'),
                };
                result.push(open);
                for (index, child) in children.iter().enumerate() {
                    if index != 0 {
                        result.push(',');
                    }
                    child.write_legacy(result, depth + 1)?;
                }
                result.push(close);
            }
        }
        Ok(())
    }

    fn json_node(&self, depth: usize, next_index: &mut u32) -> Result<String> {
        check_depth(depth)?;
        check_node_geometry(self.geometry, matches!(self.kind, Kind::Pane(_)))?;
        let g = self.geometry;
        let kind = match &self.kind {
            Kind::Pane(_) => "p",
            Kind::Split {
                axis: Axis::Horizontal,
                ..
            } => "h",
            Kind::Split {
                axis: Axis::Vertical,
                ..
            } => "v",
        };
        let mut value = format!(
            "{{\"t\":\"{kind}\",\"w\":{},\"h\":{},\"x\":{},\"y\":{}",
            g.width, g.height, g.x, g.y
        );
        match &self.kind {
            Kind::Pane(pane) => {
                if pane.active {
                    value.push_str(",\"a\":true");
                } else if let Some(last) = pane.last {
                    value.push_str(&format!(",\"l\":{last}"));
                }
                value.push_str(&format!(",\"i\":{}", pane.index.unwrap_or(*next_index)));
                *next_index += 1;
                if let Some(z) = pane.z {
                    value.push_str(&format!(",\"z\":{z}"));
                }
                if let Some(id) = pane.id {
                    value.push_str(&format!(",\"I\":\"%{id}\""));
                }
            }
            Kind::Split { children, .. } => {
                if children.is_empty() {
                    return Err(invalid("split needs a child"));
                }
                value.push_str(",\"c\":[");
                for (i, child) in children.iter().enumerate() {
                    if i != 0 {
                        value.push(',');
                    }
                    value.push_str(&child.json_node(depth + 1, next_index)?);
                }
                value.push(']');
            }
        }
        value.push('}');
        Ok(value)
    }
}

fn check_depth(depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        Err(invalid("layout exceeds nesting limit"))
    } else {
        Ok(())
    }
}
fn check_geometry(geometry: Geometry) -> Result<()> {
    check_node_geometry(geometry, true)
}
fn check_node_geometry(geometry: Geometry, pane: bool) -> Result<()> {
    if (pane && (geometry.width == 0 || geometry.height == 0))
        || geometry.width > MAX_DIMENSION
        || geometry.height > MAX_DIMENSION
        || geometry.x.unsigned_abs() > MAX_DIMENSION
        || geometry.y.unsigned_abs() > MAX_DIMENSION
    {
        Err(invalid("invalid layout geometry"))
    } else {
        Ok(())
    }
}
fn unsigned(value: &Value, key: &str) -> Result<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| Error(format!("invalid {key}")))
}
fn parse_json(input: &str, allow_unary: bool) -> Result<Layout> {
    #[derive(Deserialize)]
    struct Root<'a> {
        #[serde(rename = "V")]
        version: u32,
        #[serde(rename = "L", borrow)]
        layout: &'a RawValue,
    }
    let root: Root<'_> = serde_json::from_str(input).map_err(|error| Error(error.to_string()))?;
    if root.version != 2 {
        return Err(invalid("unsupported layout version"));
    }
    // RawValue scans nested values without recursive deserialization.
    let mut pending = vec![(root.layout, 0)];
    let mut nodes = Vec::new();
    while let Some((raw, depth)) = pending.pop() {
        check_depth(depth)?;
        let (node, children) = from_json_node(raw.get(), allow_unary)?;
        nodes.push((node, children.len()));
        pending.extend(children.into_iter().rev().map(|child| (child, depth + 1)));
    }
    let mut built = Vec::new();
    for (mut node, count) in nodes.into_iter().rev() {
        if let Kind::Split { children, .. } = &mut node.kind {
            children.extend((0..count).map(|_| built.pop().unwrap()));
        }
        built.push(node);
    }
    Ok(built.pop().unwrap())
}

fn from_json_node(input: &str, allow_unary: bool) -> Result<(Layout, Vec<&RawValue>)> {
    let mut fields: std::collections::BTreeMap<String, &RawValue> =
        serde_json::from_str(input).map_err(|error| Error(error.to_string()))?;
    let raw_children = fields.remove("c");
    let value = Value::Object(
        fields
            .into_iter()
            .map(|(key, raw)| serde_json::from_str(raw.get()).map(|value| (key, value)))
            .collect::<std::result::Result<_, _>>()
            .map_err(|error| Error(error.to_string()))?,
    );
    let mut children = Vec::new();
    let position = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| Error(format!("invalid {key}")))
    };
    let geometry = Geometry {
        width: unsigned(&value, "w")?,
        height: unsigned(&value, "h")?,
        x: position("x")?,
        y: position("y")?,
    };
    check_node_geometry(
        geometry,
        value.get("t").and_then(Value::as_str) == Some("p"),
    )?;
    let kind = match value.get("t").and_then(Value::as_str) {
        Some("p") => {
            if raw_children.is_some() {
                return Err(invalid("pane cannot have children"));
            }
            let id = value
                .get("I")
                .map(|id| {
                    id.as_str()
                        .and_then(|id| id.strip_prefix('%'))
                        .and_then(|id| id.parse().ok())
                        .ok_or_else(|| invalid("invalid pane ID"))
                })
                .transpose()?;
            let active = value
                .get("a")
                .map(|value| {
                    value
                        .as_bool()
                        .ok_or_else(|| invalid("invalid active flag"))
                })
                .transpose()?
                .unwrap_or(false);
            let optional = |key| value.get(key).map(|_| unsigned(&value, key)).transpose();
            Kind::Pane(Pane {
                id,
                index: Some(unsigned(&value, "i")?),
                active,
                last: optional("l")?,
                z: optional("z")?,
            })
        }
        Some(axis @ ("h" | "v")) => {
            children = serde_json::from_str(
                raw_children
                    .ok_or_else(|| invalid("missing split children"))?
                    .get(),
            )
            .map_err(|error| Error(error.to_string()))?;
            if children.is_empty() || (!allow_unary && children.len() < 2) {
                return Err(invalid("nodes must have more than one child"));
            }
            Kind::Split {
                axis: if axis == "h" {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                },
                children: Vec::new(),
            }
        }
        _ => return Err(invalid("unknown layout type")),
    };
    Ok((Layout { geometry, kind }, children))
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Parser<'_> {
    fn expect(&mut self, expected: u8) -> Result<()> {
        if self.bytes.get(self.at) != Some(&expected) {
            return Err(invalid("invalid layout syntax"));
        }
        self.at += 1;
        Ok(())
    }
    fn number(&mut self) -> Result<u32> {
        let start = self.at;
        let mut value = 0u32;
        while let Some(&digit @ b'0'..=b'9') = self.bytes.get(self.at) {
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u32::from(digit - b'0')))
                .ok_or_else(|| invalid("layout integer overflow"))?;
            self.at += 1;
        }
        if self.at == start {
            return Err(invalid("missing layout number"));
        }
        Ok(value)
    }
    fn coordinate(&mut self) -> Result<i32> {
        let negative = self.bytes.get(self.at) == Some(&b'-');
        if negative {
            self.at += 1;
        }
        let value = i64::from(self.number()?);
        i32::try_from(if negative { -value } else { value })
            .map_err(|_| invalid("layout coordinate overflow"))
    }
    fn node(&mut self, depth: usize) -> Result<Layout> {
        let mut parents = Vec::<(Layout, u8)>::new();
        loop {
            check_depth(depth + parents.len())?;
            let width = self.number()?;
            self.expect(b'x')?;
            let height = self.number()?;
            self.expect(b',')?;
            let x = self.coordinate()?;
            self.expect(b',')?;
            let y = self.coordinate()?;
            let geometry = Geometry {
                width,
                height,
                x,
                y,
            };
            check_geometry(geometry)?;
            let mut pane = Pane::default();
            if self.bytes.get(self.at) == Some(&b',') {
                let saved = self.at;
                self.at += 1;
                let id = self.number()?;
                if self.bytes.get(self.at) == Some(&b'x') {
                    self.at = saved;
                } else {
                    pane.id = Some(id);
                }
            }
            if let Some(open @ (b'{' | b'[')) = self.bytes.get(self.at).copied() {
                self.at += 1;
                parents.push((
                    Layout {
                        geometry,
                        kind: Kind::Split {
                            axis: if open == b'{' {
                                Axis::Horizontal
                            } else {
                                Axis::Vertical
                            },
                            children: Vec::new(),
                        },
                    },
                    if open == b'{' { b'}' } else { b']' },
                ));
                continue;
            }
            let mut node = Layout {
                geometry,
                kind: Kind::Pane(pane),
            };
            loop {
                let Some((parent, close)) = parents.last_mut() else {
                    return Ok(node);
                };
                let Kind::Split { children, .. } = &mut parent.kind else {
                    unreachable!()
                };
                children.push(node);
                if self.bytes.get(self.at) == Some(&b',') {
                    self.at += 1;
                    break;
                }
                let close = *close;
                self.expect(close)?;
                node = parents.pop().unwrap().0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn legacy(body: &str) -> String {
        format!("{:04x},{body}", checksum(body.as_bytes()))
    }

    #[test]
    fn legacy_known_checksum_and_nested_roundtrip() {
        assert_eq!(checksum(b"80x24,0,0,0"), 0xb25d);
        let wire = legacy("80x24,0,0{39x24,0,0,4,40x24,40,0[40x11,40,0,8,40x12,40,12,9]}");
        let layout = Layout::parse(&wire).unwrap();
        assert_eq!(layout.to_legacy().unwrap(), wire);
        assert_eq!(
            layout
                .panes()
                .iter()
                .map(|pane| pane.id.unwrap())
                .collect::<Vec<_>>(),
            [4, 8, 9]
        );
    }

    #[test]
    fn json_v2_keeps_indexes_ids_and_negative_floating_geometry() {
        let wire = r#"{"V":2,"L":{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":80,"h":24,"x":0,"y":0,"i":0,"I":"%7","a":true},{"t":"p","w":20,"h":5,"x":-2,"y":3,"i":1,"I":"%42","z":0}]}}"#;
        let layout = Layout::parse(wire).unwrap();
        assert_eq!(layout.find(42).unwrap().geometry.x, -2);
        assert_eq!(
            Layout::parse(&layout.to_json_v2().unwrap()).unwrap(),
            layout
        );
        assert_eq!(
            layout.to_legacy().unwrap(),
            Layout::pane(7, 80, 24).to_legacy().unwrap()
        );
    }

    #[test]
    fn c37_floating_suffix_keeps_signed_geometry_and_z_without_duplicate_ids() {
        let wire = "4081,80x24,0,0[80x12,0,0,0,80x11,0,13,2,24x8,-3,2,4]<24x8,-3,2,4>";
        let layout = Layout::parse(wire).unwrap();
        layout.validate_tiled().unwrap();
        assert_eq!(
            layout
                .panes()
                .iter()
                .map(|pane| pane.id.unwrap())
                .collect::<Vec<_>>(),
            [0, 2, 4]
        );
        assert_eq!(
            layout.find(4).unwrap().geometry,
            Geometry {
                width: 24,
                height: 8,
                x: -3,
                y: 2
            }
        );
        assert!(layout.find(4).unwrap().is_floating());
        let all_floating = Layout::parse("5568,20x8,-3,2,1<20x8,-3,2,1>").unwrap();
        assert_eq!(all_floating.panes().len(), 1);
        assert!(all_floating.is_floating());
        assert_eq!(
            Layout::parse(&layout.to_json_v2().unwrap())
                .unwrap()
                .to_json_v2()
                .unwrap(),
            layout.to_json_v2().unwrap()
        );
        let stacked =
            legacy("80x24,0,0{80x24,0,0,0,10x3,2,-1,1,12x4,-2,2,2}<12x4,-2,2,2,10x3,2,-1,1>");
        let layout = Layout::parse(&stacked).unwrap();
        assert_eq!(
            layout.panes().iter().map(|pane| pane.z).collect::<Vec<_>>(),
            [None, Some(1), Some(0)]
        );
        layout.validate_tiled().unwrap();
        for body in [
            "80x24,0,0,0<10x3,0,0,0>",
            "80x24,0,0,0<10x3,0,0,1,10x3,0,0,1>",
            "80x24,0,0,0<>",
            "80x24,0,0,0<10x3,0,0,1",
        ] {
            assert!(Layout::parse(&legacy(body)).is_err(), "{body}");
        }
    }

    #[test]
    fn legacy_nesting_limit_preserves_unary_groups_without_recursive_parsing() {
        for (open, close) in [('{', '}'), ('[', ']')] {
            for depth in [1000, 1001, 1500] {
                let body = format!(
                    "{}1x1,0,0{}",
                    format!("1x1,0,0{open}").repeat(depth),
                    close.to_string().repeat(depth)
                );
                let parsed = Layout::parse(&legacy(&body));
                if depth > MAX_DEPTH {
                    assert_eq!(parsed.unwrap_err().0, "layout exceeds nesting limit");
                    continue;
                }
                let mut layout = parsed.unwrap().for_panes(&[7]).unwrap();
                assert_eq!(layout.panes().len(), 1);
                assert_eq!(layout.to_legacy().unwrap(), legacy("1x1,0,0,7"));
                let json = layout.to_json_v2().unwrap();
                assert_eq!(json.matches("\"c\":[").count(), depth);
                assert_eq!(Layout::parse(&json).unwrap().to_json_v2().unwrap(), json);
                assert!(Layout::parse_command(&json).is_err());
                layout.resize(80, 24).unwrap();
                assert_eq!(layout.find(7).unwrap().geometry.width, 80);
            }
        }
    }
    #[test]
    fn malformed_layouts_fail_without_panicking() {
        for body in [
            "",
            "0x2,0,0,0",
            "10x2,0,0,4294967296",
            "10x2,0,0,0garbage",
            "10x2,0,0[4x2,0,0,1,5x2,5,0,2}",
        ] {
            assert!(Layout::parse(&legacy(body)).is_err(), "{body}");
        }
        assert!(Layout::parse("0000,80x24,0,0,0").is_err());
        assert!(Layout::parse(r#"{"V":3,"L":{}}"#).is_err());
    }

    #[test]
    fn splitting_resizing_and_removal_preserve_cell_borders() {
        let mut layout = Layout::pane(1, 80, 24);
        layout.split(1, 2, Axis::Horizontal).unwrap();
        layout.split(2, 3, Axis::Vertical).unwrap();
        layout.resize(101, 30).unwrap();
        let one = layout.find(1).unwrap().geometry;
        let two = layout.find(2).unwrap().geometry;
        let three = layout.find(3).unwrap().geometry;
        assert_eq!(one.width + two.width + 1, 101);
        assert_eq!(two.x, one.width as i32 + 1);
        assert_eq!(two.height + three.height + 1, 30);
        assert_eq!(Layout::parse(&layout.to_legacy().unwrap()).unwrap(), layout);
        assert!(layout.resize(2, 2).is_err());
        assert!(layout.remove(3));
        assert!(layout.remove(2));
        assert_eq!(layout.find(1).unwrap().geometry.width, 101);
        assert!(!layout.remove(1));
    }

    #[test]
    fn split_sizes_match_tmux_layout_split_sizes() {
        for (width, requested, before, old, new) in [
            (80, None, false, 40, 39),
            (80, None, true, 39, 40),
            (81, None, false, 40, 40),
            (80, Some(20), false, 59, 20),
            (80, Some(20), true, 59, 20),
            (80, Some(100), false, 1, 78),
            (80, Some(100), true, 78, 1),
            (80, Some(0), false, 78, 1),
            (80, Some(0), true, 78, 1),
        ] {
            let mut layout = Layout::pane(0, width, 24);
            layout
                .split_sized(0, 1, Axis::Horizontal, requested, before)
                .unwrap();
            assert_eq!(layout.find(0).unwrap().geometry.width, old);
            assert_eq!(layout.find(1).unwrap().geometry.width, new);
            layout.validate_tiled().unwrap();
        }
        let mut layout = Layout::pane(0, 80, 24);
        layout.split(0, 1, Axis::Horizontal).unwrap();
        layout.split(1, 2, Axis::Horizontal).unwrap();
        assert!(matches!(&layout.kind, Kind::Split { children, .. } if children.len() == 3));
    }

    #[test]
    fn replacement_validates_geometry_before_mutating() {
        let mut layout = Layout::pane(7, 80, 24);
        layout.split(7, 42, Axis::Horizontal).unwrap();
        let mut replacement = Layout::pane(100, 80, 24);
        replacement.split(100, 101, Axis::Vertical).unwrap();
        layout.set_layout(replacement).unwrap();
        assert_eq!(
            layout
                .panes()
                .iter()
                .map(|pane| pane.id.unwrap())
                .collect::<Vec<_>>(),
            [7, 42]
        );
        let mut invalid = layout.clone();
        if let Kind::Split { children, .. } = &mut invalid.kind {
            children[1].geometry.y += 1;
        }
        let original = layout.clone();
        assert!(layout.set_layout(invalid).is_err());
        assert_eq!(layout, original);
    }
}
