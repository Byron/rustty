//! Window/tab/split state, independent of native windows and running processes.
use rustty::config::{Direction, Modifiers};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

mod saved_layout;
pub use saved_layout::{SavedLayout, load_layout, saved_layouts};

pub type Id = u64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
impl Rect {
    pub const UNIT: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };
    pub fn center(self) -> [f32; 2] {
        [self.x + self.width / 2.0, self.y + self.height / 2.0]
    }
    pub fn contains(self, point: [f32; 2]) -> bool {
        point[0] >= self.x
            && point[0] < self.x + self.width
            && point[1] >= self.y
            && point[1] < self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tree {
    pub id: Id,
    pub kind: Node,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Node {
    Empty,
    Pane(Id),
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Tree>,
        second: Box<Tree>,
    },
}

impl Tree {
    pub fn empty(id: Id) -> Self {
        Self {
            id,
            kind: Node::Empty,
        }
    }
    pub fn leaf(id: Id) -> Self {
        Self {
            id,
            kind: Node::Pane(id),
        }
    }
    pub fn panes(&self) -> Vec<Id> {
        let mut panes = Vec::new();
        self.visit(&mut |node| {
            if let Node::Pane(id) = node.kind {
                panes.push(id)
            }
        });
        panes
    }
    fn visit(&self, visitor: &mut impl FnMut(&Tree)) {
        visitor(self);
        if let Node::Split { first, second, .. } = &self.kind {
            first.visit(visitor);
            second.visit(visitor);
        }
    }
    pub fn node(&self, id: Id) -> Option<&Tree> {
        if self.id == id {
            return Some(self);
        }
        match &self.kind {
            Node::Empty | Node::Pane(_) => None,
            Node::Split { first, second, .. } => first.node(id).or_else(|| second.node(id)),
        }
    }
    pub fn contains(&self, pane: Id) -> bool {
        match &self.kind {
            Node::Empty => false,
            Node::Pane(id) => *id == pane,
            Node::Split { first, second, .. } => first.contains(pane) || second.contains(pane),
        }
    }
    pub fn split(&mut self, pane: Id, new_pane: Id, split_id: Id, direction: Direction) -> bool {
        match &mut self.kind {
            Node::Pane(id) if *id == pane => {
                let original = self.clone();
                let new = Self::leaf(new_pane);
                let before = matches!(direction, Direction::Left | Direction::Up);
                let (first, second) = if before {
                    (new, original)
                } else {
                    (original, new)
                };
                self.id = split_id;
                self.kind = Node::Split {
                    axis: if matches!(direction, Direction::Left | Direction::Right) {
                        Axis::Horizontal
                    } else {
                        Axis::Vertical
                    },
                    ratio: 0.5,
                    first: Box::new(first),
                    second: Box::new(second),
                };
                true
            }
            Node::Empty | Node::Pane(_) => false,
            Node::Split { first, second, .. } => {
                first.split(pane, new_pane, split_id, direction)
                    || second.split(pane, new_pane, split_id, direction)
            }
        }
    }
    pub fn remove(self, pane: Id) -> Option<Self> {
        match self.kind {
            Node::Empty => None,
            Node::Pane(id) => (id != pane).then_some(Self::leaf(id)),
            Node::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.remove(pane), second.remove(pane)) {
                (Some(first), Some(second)) => Some(Self {
                    id: self.id,
                    kind: Node::Split {
                        axis,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    },
                }),
                (first, second) => first.or(second),
            },
        }
    }
    pub fn layout(&self, rect: Rect) -> Vec<(Id, Rect)> {
        self.layout_with_minimum(rect, [0.0; 2])
    }
    pub fn layout_with_minimum(&self, rect: Rect, minimum: [f32; 2]) -> Vec<(Id, Rect)> {
        let mut out = Vec::new();
        self.layout_into(rect, &mut out, false, minimum);
        out
    }
    pub fn minimum_size(&self, pane: [f32; 2]) -> [f32; 2] {
        match &self.kind {
            Node::Empty => [0.0; 2],
            Node::Pane(_) => pane,
            Node::Split {
                axis,
                first,
                second,
                ..
            } => {
                let a = first.minimum_size(pane);
                let b = second.minimum_size(pane);
                match axis {
                    Axis::Horizontal => [a[0] + b[0], a[1].max(b[1])],
                    Axis::Vertical => [a[0].max(b[0]), a[1] + b[1]],
                }
            }
        }
    }
    fn split_bounds(&self, rect: Rect, minimum: [f32; 2]) -> Option<(f32, Rect, Rect)> {
        let Node::Split {
            axis,
            ratio,
            first,
            second,
        } = &self.kind
        else {
            return None;
        };
        let a = first.minimum_size(minimum);
        let b = second.minimum_size(minimum);
        let (size, first_min, second_min) = match axis {
            Axis::Horizontal => (rect.width, a[0], b[0]),
            Axis::Vertical => (rect.height, a[1], b[1]),
        };
        let desired = *ratio;
        let ratio = constrained_ratio(desired, size, first_min, second_min);
        let constrained = first_min > 0.0 || second_min > 0.0;
        let fits = constrained && first_min + second_min <= size;
        // Clamp pixel extents directly so roundoff cannot hide part of the last cell.
        let position = if fits {
            (size * desired).clamp(first_min, (size - second_min).max(first_min))
        } else {
            size * ratio
        };
        let remaining = if constrained {
            (size - position).max(if fits { second_min } else { 0.0 })
        } else {
            size * (1.0 - ratio)
        };
        let (a, b) = match axis {
            Axis::Horizontal => (
                Rect {
                    width: position,
                    ..rect
                },
                Rect {
                    x: rect.x + position,
                    width: remaining,
                    ..rect
                },
            ),
            Axis::Vertical => (
                Rect {
                    height: position,
                    ..rect
                },
                Rect {
                    y: rect.y + position,
                    height: remaining,
                    ..rect
                },
            ),
        };
        Some((
            if constrained && size > 0.0 {
                position / size
            } else {
                ratio
            },
            a,
            b,
        ))
    }
    fn layout_into(
        &self,
        rect: Rect,
        out: &mut Vec<(Id, Rect)>,
        include_splits: bool,
        minimum: [f32; 2],
    ) {
        if include_splits && matches!(self.kind, Node::Split { .. }) {
            out.push((self.id, rect));
        }
        match &self.kind {
            Node::Empty => {}
            Node::Pane(pane) => out.push((*pane, rect)),
            Node::Split { first, second, .. } => {
                let (_, a, b) = self.split_bounds(rect, minimum).unwrap();
                first.layout_into(a, out, include_splits, minimum);
                second.layout_into(b, out, include_splits, minimum);
            }
        }
    }
    // Ghostty's spatial navigation uses top-left distances on a grid whose
    // dimensions come from the split topology, including structural nodes.
    fn spatial(&self) -> Vec<(Id, Rect)> {
        fn dimensions(tree: &Tree) -> [f32; 2] {
            match &tree.kind {
                Node::Empty => [0.0, 0.0],
                Node::Pane(_) => [1.0, 1.0],
                Node::Split {
                    axis,
                    first,
                    second,
                    ..
                } => {
                    let a = dimensions(first);
                    let b = dimensions(second);
                    match axis {
                        Axis::Horizontal => [a[0] + b[0], a[1].max(b[1])],
                        Axis::Vertical => [a[0].max(b[0]), a[1] + b[1]],
                    }
                }
            }
        }
        let [width, height] = dimensions(self);
        let mut out = Vec::new();
        self.layout_into(
            Rect {
                width,
                height,
                ..Rect::UNIT
            },
            &mut out,
            true,
            [0.0; 2],
        );
        out
    }
    fn weight(&self, direction: Axis) -> usize {
        match &self.kind {
            Node::Split {
                axis,
                first,
                second,
                ..
            } if *axis == direction => first.weight(direction) + second.weight(direction),
            _ => 1,
        }
    }
    pub fn equalize(&mut self) {
        if let Node::Split { axis, .. } = self.kind {
            self.equalize_inner(Some(axis));
        }
    }
    fn equalize_inner(&mut self, outer_axis: Option<Axis>) {
        if let Node::Split {
            axis,
            ratio,
            first,
            second,
        } = &mut self.kind
        {
            // Crossing both axes starts quadrants; their contents must not move this divider.
            let boundary = outer_axis.is_some_and(|outer| outer != *axis);
            let (a, b) = if boundary {
                (1, 1)
            } else {
                (first.weight(*axis), second.weight(*axis))
            };
            *ratio = a as f32 / (a + b) as f32;
            let outer_axis = outer_axis.filter(|_| !boundary);
            first.equalize_inner(outer_axis);
            second.equalize_inner(outer_axis);
        }
    }
    pub fn resize(&mut self, pane: Id, direction: Direction, pixels: f32, bounds: Rect) -> bool {
        self.resize_with_minimum(pane, direction, pixels, bounds, [0.0; 2])
    }
    pub fn resize_with_minimum(
        &mut self,
        pane: Id,
        direction: Direction,
        pixels: f32,
        bounds: Rect,
        minimum: [f32; 2],
    ) -> bool {
        let Some((displayed, a, b)) = self.split_bounds(bounds, minimum) else {
            return false;
        };
        let Node::Split {
            axis,
            ratio,
            first,
            second,
        } = &mut self.kind
        else {
            return false;
        };
        let in_first = first.contains(pane);
        if !in_first && !second.contains(pane) {
            return false;
        }
        let child = if in_first { &mut *first } else { &mut *second };
        if child.resize_with_minimum(
            pane,
            direction,
            pixels,
            if in_first { a } else { b },
            minimum,
        ) {
            return true;
        }
        let horizontal = matches!(direction, Direction::Left | Direction::Right);
        if horizontal != (*axis == Axis::Horizontal) {
            return false;
        }
        let size = if horizontal {
            bounds.width
        } else {
            bounds.height
        };
        if size <= 0.0 || !size.is_finite() || !pixels.is_finite() {
            return false;
        }
        let delta = pixels / size;
        let positive = matches!(direction, Direction::Right | Direction::Down);
        let desired = (displayed + if positive { delta } else { -delta }).clamp(0.1, 0.9);
        let index = usize::from(!horizontal);
        *ratio = constrained_ratio(
            desired,
            size,
            first.minimum_size(minimum)[index],
            second.minimum_size(minimum)[index],
        )
        .clamp(0.01, 0.99);
        true
    }
    pub fn set_ratio(&mut self, id: Id, value: f32) -> bool {
        self.set_ratio_with_minimum(id, value, Rect::UNIT, [0.0; 2])
    }
    /// The bounds belong to the displayed divider, including when its subtree is zoomed.
    pub fn set_ratio_with_minimum(
        &mut self,
        id: Id,
        value: f32,
        bounds: Rect,
        minimum: [f32; 2],
    ) -> bool {
        if !value.is_finite() {
            return false;
        }
        match &mut self.kind {
            Node::Split {
                axis,
                ratio,
                first,
                second,
            } if self.id == id => {
                let (size, index) = match axis {
                    Axis::Horizontal => (bounds.width, 0),
                    Axis::Vertical => (bounds.height, 1),
                };
                *ratio = constrained_ratio(
                    value.clamp(0.1, 0.9),
                    size,
                    first.minimum_size(minimum)[index],
                    second.minimum_size(minimum)[index],
                )
                .clamp(0.01, 0.99);
                true
            }
            Node::Split { first, second, .. } => {
                first.set_ratio_with_minimum(id, value, bounds, minimum)
                    || second.set_ratio_with_minimum(id, value, bounds, minimum)
            }
            _ => false,
        }
    }
    /// Return the deepest divider under the pointer and its parent bounds.
    pub fn divider_at(
        &self,
        bounds: Rect,
        point: [f32; 2],
        tolerance: f32,
    ) -> Option<(Id, Axis, Rect)> {
        self.divider_at_with_minimum(bounds, point, tolerance, [0.0; 2])
    }
    pub fn divider_at_with_minimum(
        &self,
        bounds: Rect,
        point: [f32; 2],
        tolerance: f32,
        minimum: [f32; 2],
    ) -> Option<(Id, Axis, Rect)> {
        let mut nodes = Vec::new();
        self.layout_into(bounds, &mut nodes, true, minimum);
        nodes.into_iter().rev().find_map(|(id, rect)| {
            let node = self.node(id)?;
            let Node::Split { axis, .. } = node.kind else {
                return None;
            };
            let (_, first, _) = node.split_bounds(rect, minimum)?;
            let near = match axis {
                Axis::Horizontal => (point[0] - first.x - first.width).abs() <= tolerance,
                Axis::Vertical => (point[1] - first.y - first.height).abs() <= tolerance,
            };
            (rect.contains(point) && near).then_some((id, axis, rect))
        })
    }
    pub fn quadrant(&self, pane: Id) -> Option<Id> {
        fn find(tree: &Tree, pane: Id, horizontal: bool, vertical: bool) -> Option<Id> {
            let Node::Split {
                axis,
                first,
                second,
                ..
            } = &tree.kind
            else {
                return None;
            };
            let child = if first.node(pane).is_some() {
                first
            } else if second.node(pane).is_some() {
                second
            } else {
                return None;
            };
            let h = horizontal || *axis == Axis::Horizontal;
            let v = vertical || *axis == Axis::Vertical;
            if h && v {
                Some(child.id)
            } else {
                find(child, pane, h, v)
            }
        }
        find(self, pane, false, false)
    }
}

fn constrained_ratio(ratio: f32, size: f32, first: f32, second: f32) -> f32 {
    if size <= 0.0 {
        return ratio;
    }
    if first + second > size {
        return first / (first + second);
    }
    ratio.clamp(first / size, (1.0 - second / size).max(first / size))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SavedPane {
    pub working_directory: PathBuf,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub title_override: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct Peek {
    pub chord: Modifiers,
    pub target: Id,
    pub full_zoom: Option<Id>,
    pub quadrant: Id,
    pub navigation_used: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Overlay {
    pub pane: Id,
    pub x: i32,
    pub y: i32,
    pub cols: u32,
    pub rows: u32,
    pub z: u32,
    pub modal: bool,
    #[serde(default)]
    pub capture_keys: bool,
    #[serde(default)]
    pub close_click: bool,
    #[serde(default)]
    pub close_cancel: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneRect {
    pub pane: Id,
    pub x: i32,
    pub y: i32,
    pub cols: u32,
    pub rows: u32,
    /// Server status rows inside the layout slot, outside the terminal content.
    #[serde(default)]
    pub reserved_top: u32,
    #[serde(default)]
    pub reserved_bottom: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tab {
    pub id: Id,
    pub title: Option<String>,
    pub color: Option<[u8; 3]>,
    pub root: Tree,
    pub focused: Id,
    pub zoom: Option<Id>,
    pub quadrant_zoom: Option<Id>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub quad_roots: BTreeSet<Id>,
    pub remembered: BTreeMap<Id, Id>,
    pub panes: BTreeMap<Id, SavedPane>,
    #[serde(default)]
    pub overlays: Vec<Overlay>,
    #[serde(default)]
    pub server_size: Option<[u32; 2]>,
    #[serde(default)]
    pub server_view: Option<Vec<PaneRect>>,
}

impl Tab {
    pub fn new(id: Id, pane: Id, directory: PathBuf) -> Self {
        Self {
            id,
            title: None,
            color: None,
            root: Tree::leaf(pane),
            focused: pane,
            zoom: None,
            quadrant_zoom: None,
            quad_roots: BTreeSet::new(),
            remembered: BTreeMap::new(),
            overlays: Vec::new(),
            server_size: None,
            server_view: None,
            panes: [(
                pane,
                SavedPane {
                    working_directory: directory,
                    ..SavedPane::default()
                },
            )]
            .into(),
        }
    }
    pub fn visible_tree(&self, peek: bool) -> &Tree {
        if peek {
            &self.root
        } else {
            self.zoom
                .and_then(|id| self.root.node(id))
                .unwrap_or(&self.root)
        }
    }
    pub fn contains(&self, pane: Id) -> bool {
        self.root.contains(pane) || self.overlays.iter().any(|overlay| overlay.pane == pane)
    }
    pub fn pane_ids(&self) -> Vec<Id> {
        self.root
            .panes()
            .into_iter()
            .chain(self.overlays.iter().map(|overlay| overlay.pane))
            .collect()
    }
    pub fn modal_overlay(&self) -> Option<&Overlay> {
        self.overlays
            .iter()
            .filter(|overlay| overlay.modal)
            .min_by_key(|overlay| overlay.z)
    }
    pub fn modal_pane(&self) -> Option<Id> {
        self.overlays
            .iter()
            .filter(|overlay| overlay.modal)
            .min_by_key(|overlay| overlay.z)
            .map(|overlay| overlay.pane)
    }
    pub fn visible_panes(&self, peek: bool) -> Vec<Id> {
        if !peek && let Some(view) = &self.server_view {
            return view.iter().map(|rect| rect.pane).collect();
        }
        if !peek
            && let Some(id) = self.zoom
            && self.overlays.iter().any(|overlay| overlay.pane == id)
        {
            return vec![id];
        }
        self.visible_tree(peek)
            .panes()
            .into_iter()
            .chain(
                self.overlays
                    .iter()
                    .filter(|_| peek || self.zoom.is_none())
                    .map(|overlay| overlay.pane),
            )
            .collect()
    }
    pub fn layout(&self, rect: Rect, peek: bool, cell_height: f32) -> Vec<(Id, Rect)> {
        self.layout_with_minimum(rect, peek, cell_height, [0.0; 2])
    }
    pub fn layout_with_minimum(
        &self,
        rect: Rect,
        peek: bool,
        cell_height: f32,
        minimum: [f32; 2],
    ) -> Vec<(Id, Rect)> {
        if !peek
            && let Some(view) = &self.server_view
            && let Some([cols, rows]) = self.server_size
        {
            let cell = [
                rect.width / cols.max(1) as f32,
                rect.height / rows.max(1) as f32,
            ];
            let tiles = self.visible_tree(false).layout(rect);
            return view
                .iter()
                .filter_map(|pane| {
                    let overlay = self
                        .overlays
                        .iter()
                        .any(|overlay| overlay.pane == pane.pane);
                    let tile = tiles
                        .iter()
                        .find(|(id, _)| *id == pane.pane)
                        .map(|(_, rect)| *rect)
                        .or_else(|| (overlay && self.zoom == Some(pane.pane)).then_some(rect));
                    if let Some(mut tile) = tile {
                        let top = (pane.reserved_top as f32 * cell_height)
                            .clamp(0.0, tile.height.max(0.0));
                        let bottom = (pane.reserved_bottom as f32 * cell_height)
                            .clamp(0.0, (tile.height - top).max(0.0));
                        tile.y += top;
                        tile.height = (tile.height - top - bottom).max(0.0);
                        return Some((pane.pane, tile));
                    }
                    overlay.then_some((
                        pane.pane,
                        Rect {
                            x: rect.x + pane.x as f32 * cell[0],
                            y: rect.y + pane.y as f32 * cell[1],
                            width: pane.cols as f32 * cell[0],
                            height: pane.rows as f32 * cell[1],
                        },
                    ))
                })
                .collect();
        }
        if !peek
            && let Some(id) = self.zoom
            && self.overlays.iter().any(|overlay| overlay.pane == id)
        {
            return vec![(id, rect)];
        }
        let mut layout = self.visible_tree(peek).layout_with_minimum(
            rect,
            if self.server_size.is_some() {
                [0.0; 2]
            } else {
                minimum
            },
        );
        if (peek || self.zoom.is_none())
            && let Some([cols, rows]) = self.server_size
        {
            let cell = [
                rect.width / cols.max(1) as f32,
                rect.height / rows.max(1) as f32,
            ];
            let mut overlays = self.overlays.iter().collect::<Vec<_>>();
            overlays.sort_by_key(|overlay| std::cmp::Reverse(overlay.z));
            layout.extend(overlays.into_iter().map(|overlay| {
                (
                    overlay.pane,
                    Rect {
                        x: rect.x + overlay.x as f32 * cell[0],
                        y: rect.y + overlay.y as f32 * cell[1],
                        width: overlay.cols as f32 * cell[0],
                        height: overlay.rows as f32 * cell[1],
                    },
                )
            }));
        }
        layout
    }
    pub fn focus(&mut self, pane: Id) {
        if !self.contains(pane)
            || self.modal_pane().is_some_and(|modal| modal != pane)
            || self
                .server_view
                .as_ref()
                .is_some_and(|view| !view.iter().any(|rect| rect.pane == pane))
        {
            return;
        }
        let previous_path = self.quad_path(self.focused);
        for quadrant in &previous_path {
            self.remembered.insert(*quadrant, self.focused);
        }
        self.focused = pane;
        if self.server_view.is_none()
            && self
                .zoom
                .is_some_and(|id| self.root.node(id).is_none_or(|node| !node.contains(pane)))
        {
            self.quadrant_zoom = self.quadrant_zoom.and_then(|zoom| {
                let depth = previous_path.iter().position(|id| *id == zoom).unwrap_or(0);
                let path = self.quad_path(pane);
                path.get(depth).or_else(|| path.last()).copied()
            });
            self.zoom = self.quadrant_zoom;
        }
        for quadrant in self.quad_path(pane) {
            self.remembered.insert(quadrant, pane);
        }
    }
    pub fn begin_peek(&mut self, chord: Modifiers) -> Option<Peek> {
        let quadrant = self.quadrant_zoom?;
        if chord == Modifiers::default() {
            return None;
        }
        let peek = Peek {
            chord,
            target: self.focused,
            full_zoom: self.zoom.filter(|zoom| Some(*zoom) != self.quadrant_zoom),
            quadrant,
            navigation_used: false,
        };
        self.zoom = None;
        self.quadrant_zoom = None;
        Some(peek)
    }
    pub fn finish_peek(&mut self, peek: Peek) -> Id {
        if self.zoom.is_some() || self.quadrant_zoom.is_some() || !self.root.contains(peek.target) {
            return self.focused;
        }
        let quadrant = self.root.quadrant(peek.target);
        let target = peek
            .full_zoom
            .filter(|original| {
                quadrant.is_some()
                    && self.root.quadrant(*original) == quadrant
                    && self.root.contains(*original)
            })
            .unwrap_or(peek.target);
        self.quadrant_zoom = self
            .root
            .node(peek.quadrant)
            .filter(|node| node.contains(target))
            .map(|node| node.id)
            .or(quadrant);
        self.zoom = if Some(target) == peek.full_zoom {
            Some(target)
        } else {
            self.quadrant_zoom
        };
        target
    }
    pub fn remembered_for(&self, pane: Id) -> Option<Id> {
        let quadrant = self.root.node(self.root.quadrant(pane)?)?;
        self.remembered
            .get(&quadrant.id)
            .copied()
            .filter(|id| quadrant.contains(*id))
    }
    /// Inferred outer quads and designated roots, ordered from outside to inside.
    pub fn quad_path(&self, pane: Id) -> Vec<Id> {
        let inferred = self.root.quadrant(pane);
        if self.quad_roots.is_empty() {
            return inferred.into_iter().collect();
        }
        let mut path = Vec::new();
        let mut node = &self.root;
        while node.contains(pane) {
            if Some(node.id) == inferred || self.quad_roots.contains(&node.id) {
                path.push(node.id);
            }
            let Node::Split { first, second, .. } = &node.kind else {
                break;
            };
            node = if first.contains(pane) { first } else { second };
        }
        path
    }
    fn parent_quad(&self, quad: Id) -> Option<Id> {
        let pane = *self.root.node(quad)?.panes().first()?;
        let path = self.quad_path(pane);
        let index = path.iter().position(|id| *id == quad)?;
        index.checked_sub(1).map(|index| path[index])
    }
    /// At a root, designate the focused sub-quad; from its parent, remove it.
    pub fn toggle_quad_root(&mut self) -> bool {
        if self.server_size.is_some() {
            return false;
        }
        let scope = self.quadrant_zoom.unwrap_or(self.root.id);
        let Some(tree) = self.root.node(scope) else {
            return false;
        };
        let child = self.quad_path(self.focused).into_iter().find(|id| {
            Some(*id) != self.quadrant_zoom
                && tree.node(*id).is_some()
                && self.quad_roots.contains(id)
        });
        if let Some(child) = child {
            let subtree = tree.node(child).unwrap();
            self.quad_roots.retain(|id| subtree.node(*id).is_none());
            self.quadrant_zoom = (scope != self.root.id).then_some(scope);
        } else {
            // A designation adds a level inside an already zoomed quad.
            if self.quadrant_zoom.is_none() {
                return false;
            }
            let Some(child) = tree.quadrant(self.focused) else {
                return false;
            };
            self.quad_roots.insert(child);
            self.quadrant_zoom = Some(child);
        }
        self.zoom = self.quadrant_zoom;
        true
    }
    fn navigation_tree(&self, peek: bool) -> &Tree {
        if !peek && !self.quad_roots.is_empty() {
            let scope = self
                .quadrant_zoom
                .and_then(|id| self.root.node(id))
                .filter(|tree| self.quad_roots.iter().any(|id| tree.node(*id).is_some()));
            if let Some(scope) = scope {
                if self.navigation_quad(scope, self.focused, false).is_some() {
                    return scope;
                }
                if let Some(parent) = self.parent_quad(scope.id).and_then(|id| self.root.node(id)) {
                    return parent;
                }
            }
        }
        &self.root
    }
    fn navigation_quad(&self, tree: &Tree, pane: Id, peek: bool) -> Option<Id> {
        if !peek && tree.id != self.root.id {
            let pane = self
                .root
                .node(pane)
                .and_then(|node| node.panes().first().copied())
                .unwrap_or(pane);
            return self
                .quad_path(pane)
                .into_iter()
                .find(|id| *id != tree.id && tree.node(*id).is_some())
                .or_else(|| tree.quadrant(pane));
        }
        tree.quadrant(pane)
    }
    pub fn remembered_for_view(&self, pane: Id, peek: bool) -> Option<Id> {
        let tree = self.navigation_tree(peek);
        let quad = self.root.node(self.navigation_quad(tree, pane, peek)?)?;
        self.remembered
            .get(&quad.id)
            .copied()
            .filter(|id| quad.contains(*id))
    }
    pub fn label_quad(&self, pane: Id, peek: bool) -> Option<Id> {
        if !peek
            && !self.quad_roots.is_empty()
            && let Some(scope) = self.quadrant_zoom.and_then(|id| self.root.node(id))
            && scope.contains(pane)
            && self.quad_roots.iter().any(|id| scope.node(*id).is_some())
        {
            return self.navigation_quad(scope, pane, false).or(Some(scope.id));
        }
        self.root.quadrant(pane)
    }
    pub fn activate_quadrant(&self, pane: Id) -> Option<Id> {
        let quadrant = self.root.node(self.root.quadrant(pane)?)?;
        self.remembered_for(pane)
            .or_else(|| quadrant.panes().first().copied())
    }
    /// Choose the zoom layer before focusing an ordinary pane-navigation target.
    pub fn pane_navigation_zoom(&self, target: Id) -> Option<Id> {
        if self.quadrant_zoom.is_some()
            && self.zoom == Some(self.focused)
            && self.zoom != self.quadrant_zoom
        {
            Some(target)
        } else {
            self.quadrant_zoom
        }
    }
    pub fn unzoom_after_blocked_navigation(&mut self) -> bool {
        let Some(zoom) = self.zoom else {
            return false;
        };
        if Some(zoom) == self.quadrant_zoom && !self.root.contains(zoom) {
            return false;
        }
        self.zoom = self.quadrant_zoom.filter(|id| *id != zoom).or_else(|| {
            (!self.quad_roots.is_empty())
                .then(|| self.parent_quad(zoom))
                .flatten()
        });
        if self.zoom != self.quadrant_zoom {
            self.quadrant_zoom = self.zoom;
        }
        if self.zoom.is_none() {
            self.quadrant_zoom = None;
        }
        true
    }
    pub fn toggle_zoom(&mut self) {
        if self.zoom == Some(self.focused) {
            if self.quadrant_zoom == self.zoom && self.quad_roots.contains(&self.focused) {
                self.quadrant_zoom = self.parent_quad(self.focused);
                self.zoom = self.quadrant_zoom;
                return;
            }
            self.zoom = self.quadrant_zoom.filter(|id| *id != self.focused);
        } else {
            self.zoom = Some(self.focused);
        }
    }
    pub fn toggle_quadrant_zoom(&mut self) {
        if self.quadrant_zoom.is_some() && self.zoom != self.quadrant_zoom {
            self.zoom = self.quadrant_zoom;
            return;
        }
        let path = self.quad_path(self.focused);
        self.quadrant_zoom = match self
            .quadrant_zoom
            .and_then(|id| path.iter().position(|quad| *quad == id))
        {
            Some(index) => path
                .get(index + 1)
                .copied()
                .or_else(|| index.checked_sub(1).map(|index| path[index])),
            None => path.first().copied(),
        };
        self.zoom = self.quadrant_zoom;
    }
    pub fn target(&self, from: Id, direction: Direction) -> Option<Id> {
        self.target_for_view(from, direction, false)
    }
    pub fn target_for_view(&self, from: Id, direction: Direction, peek: bool) -> Option<Id> {
        let quadrant_only = matches!(
            direction,
            Direction::QuadrantLeft
                | Direction::QuadrantRight
                | Direction::QuadrantUp
                | Direction::QuadrantDown
        );
        let root = if quadrant_only {
            self.navigation_tree(peek)
        } else {
            self.quadrant_zoom
                .and_then(|id| self.root.node(id))
                .unwrap_or(&self.root)
        };
        let panes = root.panes();
        let index = panes.iter().position(|id| *id == from)?;
        if matches!(direction, Direction::Previous | Direction::Next) {
            return Some(
                panes[(index
                    + if direction == Direction::Next {
                        1
                    } else {
                        panes.len() - 1
                    })
                    % panes.len()],
            );
        }
        let slots = root.spatial();
        let adjacent = |from: Id| -> Vec<Id> {
            let Some((_, source)) = slots.iter().find(|(id, _)| *id == from) else {
                return Vec::new();
            };
            let mut candidates = slots
                .iter()
                .filter(|(id, rect)| {
                    *id != from
                        && match direction {
                            Direction::Left | Direction::QuadrantLeft => {
                                rect.x + rect.width <= source.x
                            }
                            Direction::Right | Direction::QuadrantRight => {
                                rect.x >= source.x + source.width
                            }
                            Direction::Up | Direction::QuadrantUp => {
                                rect.y + rect.height <= source.y
                            }
                            _ => rect.y >= source.y + source.height,
                        }
                })
                .collect::<Vec<_>>();
            let distance = |rect: &Rect| (rect.x - source.x).powi(2) + (rect.y - source.y).powi(2);
            candidates.sort_by(|a, b| distance(&a.1).total_cmp(&distance(&b.1)));
            candidates.into_iter().map(|(id, _)| *id).collect()
        };
        let target_quadrant = if quadrant_only {
            let current = self.navigation_quad(root, from, peek)?;
            let target = adjacent(current)
                .into_iter()
                .find(|id| self.navigation_quad(root, *id, peek) == Some(*id))?;
            Some(root.node(target)?)
        } else {
            None
        };
        adjacent(from).into_iter().find(|id| {
            panes.contains(id) && target_quadrant.is_none_or(|quadrant| quadrant.contains(*id))
        })
    }
    pub fn split(&mut self, new_pane: Id, split_id: Id, direction: Direction, directory: PathBuf) {
        if !self.root.split(self.focused, new_pane, split_id, direction) {
            return;
        }
        if self.quad_roots.remove(&self.focused) {
            self.quad_roots.insert(split_id);
            if self.quadrant_zoom == Some(self.focused) {
                self.quadrant_zoom = Some(split_id);
                if self.zoom == Some(self.focused) {
                    self.zoom = Some(split_id);
                }
            }
            if let Some(pane) = self.remembered.remove(&self.focused) {
                self.remembered.insert(split_id, pane);
            }
        }
        self.panes.insert(
            new_pane,
            SavedPane {
                working_directory: directory,
                ..SavedPane::default()
            },
        );
        self.focus(new_pane);
    }
    /// Returns false when the tab's last pane has closed.
    pub fn close(&mut self, pane: Id) -> bool {
        let next = if self.focused == pane {
            let neighbor = |tree: &Tree| {
                let panes = tree.panes();
                let index = panes.iter().position(|id| *id == pane)?;
                if index > 0 {
                    Some(panes[index - 1])
                } else {
                    panes.get(1).copied()
                }
            };
            self.quadrant_zoom
                .and_then(|id| self.root.node(id))
                .and_then(neighbor)
                .or_else(|| neighbor(&self.root))
        } else {
            None
        };
        let mut quadrant_zoom = self
            .quadrant_zoom
            .and_then(|id| self.root.node(id))
            .and_then(|node| node.clone().remove(pane))
            .filter(|node| {
                self.quadrant_zoom
                    .is_some_and(|id| self.quad_roots.contains(&id))
                    || matches!(node.kind, Node::Split { .. })
            })
            .map(|node| node.id);
        if quadrant_zoom.is_none() && !self.quad_roots.is_empty() {
            let mut parent = self.quadrant_zoom.and_then(|id| self.parent_quad(id));
            while let Some(id) = parent {
                if let Some(node) = self
                    .root
                    .node(id)
                    .and_then(|node| node.clone().remove(pane))
                {
                    quadrant_zoom = Some(node.id);
                    break;
                }
                parent = self.parent_quad(id);
            }
        }
        let mut remembered = self.remembered.clone();
        self.quad_roots = self
            .quad_roots
            .iter()
            .filter_map(|id| {
                let node = self.root.node(*id)?.clone().remove(pane)?;
                if let Some(focused) = self.remembered.get(id) {
                    remembered.insert(node.id, *focused);
                }
                Some(node.id)
            })
            .collect();
        self.remembered = remembered;
        self.overlays.retain(|overlay| overlay.pane != pane);
        let Some(root) = self
            .root
            .clone()
            .remove(pane)
            .or_else(|| (!self.overlays.is_empty()).then(|| Tree::empty(self.root.id)))
        else {
            return false;
        };
        self.root = root;
        self.panes.remove(&pane);
        if let Some(view) = &mut self.server_view {
            view.retain(|rect| rect.pane != pane);
            if view.is_empty() {
                self.server_view = None;
            }
        }
        self.remembered.retain(|node, pane| {
            self.root
                .node(*node)
                .is_some_and(|node| node.contains(*pane))
        });
        self.zoom = if self.zoom == self.quadrant_zoom {
            quadrant_zoom
        } else {
            self.zoom
                .filter(|id| self.root.node(*id).is_some() || self.contains(*id))
                .or(quadrant_zoom)
        };
        self.quadrant_zoom = quadrant_zoom;
        if self.focused == pane {
            self.focused = next
                .or_else(|| self.modal_pane())
                .or_else(|| self.root.panes().first().copied())
                .or_else(|| {
                    self.overlays
                        .iter()
                        .min_by_key(|overlay| overlay.z)
                        .map(|overlay| overlay.pane)
                })
                .expect("a surviving root or overlay has a pane");
        }
        true
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowState {
    pub id: Id,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub frame: [f64; 4],
    pub quick: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedAttachment {
    pub endpoint: PathBuf,
    #[serde(default)]
    pub instance_id: String,
    pub session_id: u64,
    pub session_name: String,
    pub frame: [f64; 4],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    version: u32,
    next_id: Id,
    pub windows: Vec<WindowState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tmuxx: Vec<SavedAttachment>,
    /// Logical dashboard positions; new reporters may reclaim inactive reservations.
    #[serde(default, deserialize_with = "deck_positions")]
    pub deck_positions: Vec<Option<Id>>,
    /// User order; newly pinned panes append to the end.
    #[serde(default, deserialize_with = "pinned_panes")]
    pub pinned_panes: Vec<Id>,
}

pub const MAX_DECK_SLOTS: usize = 4096;
const MAX_PINNED_PANES: usize = 4096;

fn pinned_panes<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Id>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_array().map_or_else(Vec::new, |pins| {
        pins.iter()
            .take(MAX_PINNED_PANES)
            .filter_map(serde_json::Value::as_u64)
            .collect()
    }))
}

fn deck_positions<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Option<Id>>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_array().map_or_else(Vec::new, |slots| {
        slots
            .iter()
            .take(MAX_DECK_SLOTS)
            .map(|id| id.as_u64())
            .collect()
    }))
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            version: 1,
            next_id: 1,
            windows: Vec::new(),
            tmuxx: Vec::new(),
            deck_positions: Vec::new(),
            pinned_panes: Vec::new(),
        }
    }
}

impl Workspace {
    pub fn reconcile_pinned_panes(&mut self) -> bool {
        let live: HashSet<_> = self
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .flat_map(|tab| tab.panes.keys().copied())
            .collect();
        let previous = self.pinned_panes.clone();
        let mut seen = HashSet::new();
        self.pinned_panes.truncate(MAX_PINNED_PANES);
        self.pinned_panes
            .retain(|pane| live.contains(pane) && seen.insert(*pane));
        previous != self.pinned_panes
    }

    pub fn toggle_pin(&mut self, pane: Id) -> bool {
        let changed = self.reconcile_pinned_panes();
        if let Some(index) = self.pinned_panes.iter().position(|id| *id == pane) {
            self.pinned_panes.remove(index);
            return true;
        }
        if self.pinned_panes.len() < MAX_PINNED_PANES
            && self
                .windows
                .iter()
                .flat_map(|window| &window.tabs)
                .any(|tab| tab.panes.contains_key(&pane))
        {
            self.pinned_panes.push(pane);
            return true;
        }
        changed
    }

    /// Resolve a pin and preserve the source's quad-zoom mode before selection.
    pub fn prepare_pin_navigation(
        &mut self,
        index: usize,
        from: Id,
        peek: &mut Option<Peek>,
    ) -> Option<(Id, Id, Id)> {
        let pane = *self.pinned_panes.get(index.checked_sub(1)?)?;
        let (window, tab) = self
            .windows
            .iter()
            .enumerate()
            .find_map(|(window, state)| {
                state
                    .tabs
                    .iter()
                    .position(|tab| tab.panes.contains_key(&pane) && tab.contains(pane))
                    .map(|tab| (window, tab))
            })?;
        let source = self.windows.iter_mut().find(|window| window.id == from)?;
        let source = source.tabs.get_mut(source.active_tab)?;
        let quad_zoom = source.quadrant_zoom.is_some() || peek.is_some();
        if let Some(peek) = peek.take() {
            let target = source.finish_peek(peek);
            source.focus(target);
        }
        let destination = &mut self.windows[window].tabs[tab];
        if destination.quadrant_zoom.is_some() != quad_zoom {
            if quad_zoom {
                destination.quadrant_zoom = destination.root.quadrant(pane);
                destination.zoom = destination.quadrant_zoom;
            } else {
                if destination.zoom == destination.quadrant_zoom {
                    destination.zoom = None;
                }
                destination.quadrant_zoom = None;
            }
        }
        Some((
            self.windows[window].id,
            self.windows[window].tabs[tab].id,
            pane,
        ))
    }

    pub fn reorder_pin(&mut self, pane: Id, before: Option<Id>) -> bool {
        let changed = self.reconcile_pinned_panes();
        let Some(from) = self.pinned_panes.iter().position(|id| *id == pane) else {
            return changed;
        };
        let to = match before {
            Some(id) => {
                let Some(index) = self.pinned_panes.iter().position(|pane| *pane == id) else {
                    return changed;
                };
                index
            }
            None => self.pinned_panes.len(),
        };
        let to = to.saturating_sub(usize::from(from < to));
        if from == to {
            return changed;
        }
        self.pinned_panes.remove(from);
        self.pinned_panes.insert(to, pane);
        true
    }

    /// Auxiliary data must never prevent the terminal workspace from loading.
    pub fn reconcile_deck_positions(&mut self) -> bool {
        let live: HashSet<_> = self
            .windows
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| t.panes.keys().copied())
            .collect();
        let previous = self.deck_positions.clone();
        let mut seen = HashSet::new();
        self.deck_positions.truncate(MAX_DECK_SLOTS);
        for position in &mut self.deck_positions {
            if position.is_some_and(|id| !live.contains(&id) || !seen.insert(id)) {
                *position = None;
            }
        }
        while self.deck_positions.last() == Some(&None) {
            self.deck_positions.pop();
        }
        previous != self.deck_positions
    }

    /// Open saved windows alongside live sessions, assigning fresh identifiers.
    /// Validation and remapping finish before the current workspace changes.
    pub fn append_layout(&mut self, mut layout: Self) -> io::Result<Vec<Id>> {
        layout.validate()?;
        layout.reconcile_pinned_panes();
        let mut next_id = self.next_id;
        let mut remap = BTreeMap::new();
        for window in &layout.windows {
            let mut old_ids = vec![window.id];
            for tab in &window.tabs {
                old_ids.push(tab.id);
                tab.root.visit(&mut |node| old_ids.push(node.id));
                old_ids.extend(tab.overlays.iter().map(|overlay| overlay.pane));
            }
            for old in old_ids {
                let id = next_id;
                next_id = next_id
                    .checked_add(1)
                    .ok_or_else(|| invalid("workspace ID space exhausted"))?;
                remap.insert(old, id);
            }
        }
        fn remap_tree(tree: &mut Tree, remap: &BTreeMap<Id, Id>) {
            tree.id = remap[&tree.id];
            match &mut tree.kind {
                Node::Empty => {}
                Node::Pane(pane) => *pane = remap[pane],
                Node::Split { first, second, .. } => {
                    remap_tree(first, remap);
                    remap_tree(second, remap);
                }
            }
        }
        for window in &mut layout.windows {
            window.id = remap[&window.id];
            // Imported quick terminals open as ordinary windows; a workspace
            // can only have one live quick terminal controlled by the hotkey.
            window.quick = false;
            for tab in &mut window.tabs {
                tab.id = remap[&tab.id];
                tab.focused = remap[&tab.focused];
                tab.zoom = tab.zoom.map(|id| remap[&id]);
                tab.quadrant_zoom = tab.quadrant_zoom.map(|id| remap[&id]);
                tab.quad_roots = std::mem::take(&mut tab.quad_roots)
                    .into_iter()
                    .map(|id| remap[&id])
                    .collect();
                tab.remembered = std::mem::take(&mut tab.remembered)
                    .into_iter()
                    .filter_map(|(node, pane)| Some((*remap.get(&node)?, *remap.get(&pane)?)))
                    .collect();
                tab.panes = std::mem::take(&mut tab.panes)
                    .into_iter()
                    .map(|(id, pane)| (remap[&id], pane))
                    .collect();
                remap_tree(&mut tab.root, &remap);
                for overlay in &mut tab.overlays {
                    overlay.pane = remap[&overlay.pane];
                }
                for pane in tab.server_view.iter_mut().flatten() {
                    pane.pane = remap[&pane.pane];
                }
            }
        }
        let windows = layout.windows.iter().map(|window| window.id).collect();
        let mut merged = self.clone();
        merged.next_id = next_id;
        merged.windows.extend(layout.windows);
        merged
            .pinned_panes
            .extend(layout.pinned_panes.into_iter().map(|pane| remap[&pane]));
        merged.reconcile_pinned_panes();
        merged.validate()?;
        *self = merged;
        Ok(windows)
    }

    /// Restoring a layout must not reuse IDs issued after that snapshot.
    pub fn restore(&mut self, mut previous: Self) -> Self {
        previous.next_id = previous.next_id.max(self.next_id);
        previous.tmuxx.clone_from(&self.tmuxx);
        previous.deck_positions.clone_from(&self.deck_positions);
        previous.reconcile_deck_positions();
        previous.pinned_panes.clone_from(&self.pinned_panes);
        previous.reconcile_pinned_panes();
        std::mem::replace(self, previous)
    }
    pub fn close_pane(&mut self, pane: Id) -> bool {
        let Some((window, tab)) = self.windows.iter().enumerate().find_map(|(w, window)| {
            window
                .tabs
                .iter()
                .position(|tab| tab.panes.contains_key(&pane))
                .map(|tab| (w, tab))
        }) else {
            return false;
        };
        let window_state = &mut self.windows[window];
        if !window_state.tabs[tab].close(pane) {
            window_state.tabs.remove(tab);
            if window_state.tabs.is_empty() {
                self.windows.remove(window);
            } else if window_state.active_tab >= tab {
                window_state.active_tab = window_state
                    .active_tab
                    .saturating_sub(1)
                    .min(window_state.tabs.len() - 1);
            }
        }
        self.reconcile_deck_positions();
        self.reconcile_pinned_panes();
        true
    }
    pub fn id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("workspace ID space exhausted");
        id
    }
    pub fn load(path: &Path) -> io::Result<Option<Self>> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut bytes = Vec::new();
        file.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(invalid("workspace state exceeds 8 MiB"));
        }
        let mut state: Self = serde_json::from_slice(&bytes).map_err(invalid)?;
        state.validate()?;
        state.reconcile_deck_positions();
        state.reconcile_pinned_panes();
        Ok(Some(state))
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        let parent = path
            .parent()
            .ok_or_else(|| invalid("workspace path has no parent"))?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".workspace-{}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        let result = (|| {
            let bytes = serde_json::to_vec(self).map_err(invalid)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
    /// Check persisted or imported workspace metadata before creating windows.
    pub fn validate(&self) -> io::Result<()> {
        if self.version != 1
            || self.next_id == 0
            || self.windows.len() > 64
            || self.tmuxx.len() > 64
        {
            return Err(invalid("unsupported or invalid workspace state"));
        }
        for attachment in &self.tmuxx {
            if !attachment.endpoint.is_absolute()
                || attachment
                    .endpoint
                    .as_os_str()
                    .as_encoded_bytes()
                    .contains(&0)
                || attachment.session_name.is_empty()
                || attachment.session_name.contains('\0')
                || attachment.frame.iter().any(|value| !value.is_finite())
                || attachment.frame[2] <= 0.0
                || attachment.frame[3] <= 0.0
            {
                return Err(invalid("invalid saved tmuxx attachment"));
            }
        }
        let mut ids = HashSet::new();
        let mut maximum = 0;
        let mut count = 0;
        for window in &self.windows {
            if !ids.insert(window.id)
                || window.tabs.is_empty()
                || window.active_tab >= window.tabs.len()
                || window.tabs.len() > 512
                || window.frame.iter().any(|value| !value.is_finite())
                || window.frame[2] <= 0.0
                || window.frame[3] <= 0.0
            {
                return Err(invalid("invalid saved window"));
            }
            maximum = maximum.max(window.id);
            for tab in &window.tabs {
                if !ids.insert(tab.id) || !tab.contains(tab.focused) {
                    return Err(invalid("invalid saved tab"));
                }
                maximum = maximum.max(tab.id);
                let mut valid = true;
                tab.root.visit(&mut |node| {
                    maximum = maximum.max(node.id);
                    valid &= ids.insert(node.id);
                    if let Node::Pane(pane) = node.kind {
                        valid &= pane == node.id;
                    }
                    if let Node::Split { ratio, .. } = node.kind {
                        valid &= ratio.is_finite() && (0.01..=0.99).contains(&ratio);
                    }
                });
                for overlay in &tab.overlays {
                    maximum = maximum.max(overlay.pane);
                    valid &= ids.insert(overlay.pane)
                        && overlay.cols > 0
                        && overlay.rows > 0
                        && overlay.cols <= 10_000
                        && overlay.rows <= 10_000;
                }
                if let Some(view) = &tab.server_view {
                    let mut visible = HashSet::new();
                    valid &= !view.is_empty()
                        && view.iter().all(|pane| {
                            tab.contains(pane.pane)
                                && visible.insert(pane.pane)
                                && pane.cols > 0
                                && pane.rows > 0
                                && pane.cols <= 10_000
                                && pane.rows <= 10_000
                                && pane.reserved_top.saturating_add(pane.reserved_bottom) <= 10_000
                        });
                }
                valid &= (tab.overlays.is_empty() && tab.server_view.is_none())
                    || tab
                        .server_size
                        .is_some_and(|size| size.iter().all(|size| *size > 0 && *size <= 10_000));
                let panes = tab.pane_ids();
                count += panes.len();
                if !valid
                    || count > 4096
                    || panes.len() != tab.panes.len()
                    || panes.iter().any(|id| !tab.panes.contains_key(id))
                    || tab
                        .zoom
                        .is_some_and(|id| tab.root.node(id).is_none() && !tab.contains(id))
                    || tab
                        .quadrant_zoom
                        .is_some_and(|id| tab.root.node(id).is_none())
                    || tab.quad_roots.iter().any(|id| {
                        tab.root
                            .node(*id)
                            .is_none_or(|node| matches!(node.kind, Node::Empty))
                    })
                {
                    return Err(invalid("invalid saved split tree"));
                }
            }
        }
        if self.next_id <= maximum {
            return Err(invalid("saved workspace contains reused identifiers"));
        }
        Ok(())
    }
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quadrants() -> Tab {
        let mut tab = Tab::new(1, 2, PathBuf::from("/tmp"));
        tab.split(3, 4, Direction::Right, PathBuf::from("/tmp"));
        tab.split(5, 6, Direction::Down, PathBuf::from("/tmp"));
        tab.focus(2);
        tab.split(7, 8, Direction::Down, PathBuf::from("/tmp"));
        tab
    }
    fn recursive_quads(depth: usize) -> Tab {
        fn grid(next: &mut Id, depth: usize) -> Tree {
            let id = *next;
            *next += 1;
            if depth == 0 {
                return Tree::leaf(id);
            }
            let left_id = *next;
            *next += 1;
            let first = grid(next, depth - 1);
            let second = grid(next, 0);
            let right_id = *next;
            *next += 1;
            let third = grid(next, 0);
            let fourth = grid(next, 0);
            Tree {
                id,
                kind: Node::Split {
                    axis: Axis::Horizontal,
                    ratio: 0.5,
                    first: Box::new(Tree {
                        id: left_id,
                        kind: Node::Split {
                            axis: Axis::Vertical,
                            ratio: 0.5,
                            first: Box::new(first),
                            second: Box::new(second),
                        },
                    }),
                    second: Box::new(Tree {
                        id: right_id,
                        kind: Node::Split {
                            axis: Axis::Vertical,
                            ratio: 0.5,
                            first: Box::new(third),
                            second: Box::new(fourth),
                        },
                    }),
                },
            }
        }
        let mut next = 2;
        let root = grid(&mut next, depth);
        let panes = root.panes();
        let mut tab = Tab::new(1, panes[0], "/tmp".into());
        tab.root = root;
        tab.panes = panes
            .into_iter()
            .map(|pane| {
                (
                    pane,
                    SavedPane {
                        working_directory: "/tmp".into(),
                        ..SavedPane::default()
                    },
                )
            })
            .collect();
        tab
    }
    #[test]
    fn quad_roots_repeat_inference_and_toggle_between_parent_and_child() {
        let mut tab = recursive_quads(6);
        assert!(!tab.toggle_quad_root());
        tab.toggle_quadrant_zoom();
        let mut path = vec![tab.quadrant_zoom.unwrap()];
        for _ in 0..5 {
            let expected = tab
                .root
                .node(*path.last().unwrap())
                .unwrap()
                .quadrant(tab.focused)
                .unwrap();
            assert!(tab.toggle_quad_root());
            path.push(expected);
            assert_eq!(tab.quad_path(tab.focused), path);
            assert_eq!(
                (tab.zoom, tab.quadrant_zoom),
                (Some(expected), Some(expected))
            );
        }
        assert!(
            !tab.toggle_quad_root(),
            "an unsplit leaf has no deeper inferred quad"
        );
        let child = path[5];
        let parent = path[4];
        tab.toggle_quadrant_zoom();
        assert_eq!(tab.zoom, Some(parent));
        tab.toggle_quadrant_zoom();
        assert_eq!(tab.zoom, Some(child));
        tab.toggle_quadrant_zoom();
        assert!(tab.toggle_quad_root());
        assert!(!tab.quad_roots.contains(&child));
        assert_eq!(tab.zoom, Some(parent));
        assert!(tab.toggle_quad_root());
        assert_eq!(tab.zoom, Some(child));
    }
    #[test]
    fn quad_zoom_stays_inside_a_parent_when_all_children_are_designated() {
        let mut tab = recursive_quads(2);
        tab.toggle_quadrant_zoom();
        let parent = tab.quadrant_zoom.unwrap();
        let panes = tab.root.node(parent).unwrap().panes();
        for pane in &panes {
            tab.focus(*pane);
            assert!(tab.toggle_quad_root());
            assert_eq!(tab.zoom, Some(*pane));
            tab.toggle_quadrant_zoom();
            assert_eq!(tab.zoom, Some(parent));
        }
        for pane in panes {
            tab.focus(pane);
            tab.toggle_quadrant_zoom();
            assert_eq!(tab.zoom, Some(pane));
            tab.toggle_zoom();
            assert_eq!((tab.zoom, tab.quadrant_zoom), (Some(parent), Some(parent)));
            tab.toggle_quadrant_zoom();
            assert_eq!(tab.zoom, Some(pane));
            tab.toggle_quadrant_zoom();
            assert_eq!(tab.zoom, Some(parent));
        }
    }
    #[test]
    fn removing_a_quad_root_from_another_panel_clears_nested_designations() {
        let mut tab = recursive_quads(4);
        tab.toggle_quadrant_zoom();
        let parent = tab.quadrant_zoom.unwrap();
        assert!(tab.toggle_quad_root());
        let child = tab.quadrant_zoom.unwrap();
        assert!(tab.toggle_quad_root());
        let grandchild = tab.quadrant_zoom.unwrap();
        let sibling = *tab.root.node(child).unwrap().panes().last().unwrap();
        tab.zoom = Some(parent);
        tab.quadrant_zoom = Some(parent);
        tab.focus(sibling);
        assert!(tab.toggle_quad_root());
        assert!(!tab.quad_roots.contains(&child));
        assert!(!tab.quad_roots.contains(&grandchild));
        assert_eq!(tab.zoom, Some(parent));
        let pane = *tab.root.node(grandchild).unwrap().panes().first().unwrap();
        assert_eq!(tab.quad_path(pane), [parent]);
        tab.focus(pane);
        tab.toggle_quadrant_zoom();
        assert_eq!(tab.zoom, None);
    }
    #[test]
    fn designated_leaf_follows_splits_and_collapses_and_closing_it_returns_to_parent() {
        let mut tab = recursive_quads(2);
        tab.toggle_quadrant_zoom();
        let parent = tab.quadrant_zoom.unwrap();
        let original = tab.focused;
        assert!(tab.toggle_quad_root());
        assert_eq!(tab.zoom, Some(original));
        tab.split(100, 101, Direction::Right, "/tmp".into());
        assert_eq!(tab.zoom, Some(101));
        assert_eq!(tab.quad_roots, BTreeSet::from([101]));
        assert_eq!(tab.root.node(101).unwrap().panes(), [original, 100]);
        assert!(tab.close(original));
        assert_eq!(tab.zoom, Some(100));
        assert_eq!(tab.quad_roots, BTreeSet::from([100]));
        assert!(tab.close(100));
        assert_eq!(tab.zoom, Some(parent));
        assert!(tab.quad_roots.is_empty());
        assert!(tab.root.contains(tab.focused));
    }
    #[test]
    fn quad_root_navigation_uses_visible_children_but_peek_uses_original_quads() {
        let mut tab = recursive_quads(3);
        tab.toggle_quadrant_zoom();
        let parent = tab.quadrant_zoom.unwrap();
        assert!(tab.toggle_quad_root());
        let child = tab.quadrant_zoom.unwrap();
        tab.toggle_quadrant_zoom();
        let right = tab.target(tab.focused, Direction::QuadrantRight).unwrap();
        assert!(tab.root.node(parent).unwrap().contains(right));
        assert!(!tab.root.node(child).unwrap().contains(right));
        tab.toggle_quadrant_zoom();
        let pane = tab.focused;
        let peek = tab
            .begin_peek(Modifiers {
                alt: true,
                ..Modifiers::default()
            })
            .unwrap();
        assert_eq!(tab.visible_tree(true).id, tab.root.id);
        let right = tab
            .target_for_view(pane, Direction::QuadrantRight, true)
            .unwrap();
        assert_ne!(tab.root.quadrant(right), Some(parent));
        assert_eq!(tab.finish_peek(peek), pane);
        assert_eq!(tab.zoom, Some(child));
        let mut peek = tab
            .begin_peek(Modifiers {
                alt: true,
                ..Modifiers::default()
            })
            .unwrap();
        peek.target = right;
        peek.navigation_used = true;
        assert_eq!(tab.finish_peek(peek), right);
        assert_eq!(tab.zoom, tab.root.quadrant(right));
    }
    #[test]
    fn quad_roots_roundtrip_import_and_validate_with_legacy_defaults() {
        let mut tab = recursive_quads(4);
        tab.toggle_quadrant_zoom();
        tab.toggle_quad_root();
        tab.toggle_quad_root();
        let mut state = Workspace {
            next_id: 100,
            ..Workspace::default()
        };
        state.windows.push(WindowState {
            id: 90,
            tabs: vec![tab],
            active_tab: 0,
            frame: [0.0, 0.0, 800.0, 600.0],
            quick: false,
        });
        state.validate().unwrap();
        let value = serde_json::to_value(&state).unwrap();
        let decoded: Workspace = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            decoded.windows[0].tabs[0].quad_roots,
            state.windows[0].tabs[0].quad_roots
        );
        let mut legacy = value.clone();
        legacy["windows"][0]["tabs"][0]
            .as_object_mut()
            .unwrap()
            .remove("quad_roots");
        let legacy: Workspace = serde_json::from_value(legacy).unwrap();
        assert!(legacy.windows[0].tabs[0].quad_roots.is_empty());
        let mut invalid_roots = decoded.clone();
        invalid_roots.windows[0].tabs[0].quad_roots.insert(999);
        assert!(invalid_roots.validate().is_err());
        let before = state.clone();
        let ids = state.append_layout(decoded).unwrap();
        state.validate().unwrap();
        let imported = state
            .windows
            .iter()
            .find(|window| window.id == ids[0])
            .unwrap();
        let imported = &imported.tabs[0];
        assert_eq!(imported.quad_roots.len(), 2);
        assert!(
            imported
                .quad_roots
                .is_disjoint(&before.windows[0].tabs[0].quad_roots)
        );
        assert_eq!(imported.quad_path(imported.focused).len(), 3);
        state.restore(before.clone());
        assert_eq!(
            state.windows[0].tabs[0].quad_roots,
            before.windows[0].tabs[0].quad_roots
        );
        assert_eq!(state.windows.len(), 1);
    }
    #[test]
    fn quadrants_follow_first_split_on_each_axis_and_nested_focus() {
        let mut tab = quadrants();
        assert_eq!(tab.root.quadrant(2), Some(2));
        assert_eq!(tab.root.quadrant(5), Some(5));
        tab.focus(2);
        tab.split(9, 10, Direction::Right, PathBuf::from("/tmp"));
        assert_eq!(tab.root.quadrant(2), Some(10));
        assert_eq!(tab.root.quadrant(9), Some(10));
        tab.toggle_quadrant_zoom();
        assert_eq!(tab.zoom, Some(10));
        tab.toggle_zoom();
        assert_eq!(tab.zoom, Some(9));
        tab.toggle_zoom();
        assert_eq!(tab.zoom, Some(10));
        tab.focus(5);
        assert_eq!(tab.zoom, Some(5));
        assert_eq!(tab.remembered.get(&10), Some(&9));
    }
    #[test]
    fn quadrant_zoom_toggle_unwinds_full_pane_zoom_first() {
        let mut tab = quadrants();
        tab.focus(2);
        tab.split(9, 10, Direction::Right, PathBuf::from("/tmp"));
        tab.toggle_quadrant_zoom();
        tab.toggle_zoom();
        assert_eq!((tab.zoom, tab.quadrant_zoom), (Some(9), Some(10)));
        tab.toggle_quadrant_zoom();
        assert_eq!((tab.zoom, tab.quadrant_zoom), (Some(10), Some(10)));
        assert_eq!(tab.visible_tree(false).panes(), [2, 9]);
        tab.toggle_quadrant_zoom();
        assert_eq!((tab.zoom, tab.quadrant_zoom), (None, None));
    }
    #[test]
    fn navigation_skips_current_quadrant_and_blocked_edges_keep_focus() {
        let mut tab = quadrants();
        tab.focus(2);
        tab.split(9, 10, Direction::Right, PathBuf::from("/tmp"));
        assert_eq!(tab.target(2, Direction::Right), Some(9));
        assert_eq!(tab.target(2, Direction::QuadrantRight), Some(3));
        assert_eq!(tab.target(2, Direction::Left), None);
        assert!(tab.close(9));
        assert_eq!(tab.root.quadrant(2), Some(2));
    }
    #[test]
    fn closing_panes_keeps_quadrant_zoom_until_one_remains() {
        for order in [[2, 9, 11], [11, 9, 2]] {
            for focus_closed in [false, true] {
                let mut tab = quadrants();
                tab.focus(2);
                tab.split(9, 10, Direction::Right, PathBuf::from("/tmp"));
                tab.split(11, 12, Direction::Down, PathBuf::from("/tmp"));
                tab.toggle_quadrant_zoom();
                for (index, pane) in order.into_iter().enumerate() {
                    if focus_closed {
                        tab.focus(pane);
                    }
                    assert!(tab.close(pane));
                    assert!(tab.root.contains(tab.focused));
                    if index == 0 {
                        let quadrant = tab.root.quadrant(tab.focused);
                        assert!(quadrant.is_some());
                        assert_eq!((tab.zoom, tab.quadrant_zoom), (quadrant, quadrant));
                        assert_eq!(tab.visible_tree(false).panes().len(), 2);
                    } else {
                        assert_eq!((tab.zoom, tab.quadrant_zoom), (None, None));
                    }
                    if index < 2 {
                        assert!(order[index + 1..].contains(&tab.focused));
                    }
                }
            }
        }
    }
    #[test]
    fn closing_full_zoomed_pane_restores_surviving_quadrant_zoom() {
        let mut tab = quadrants();
        tab.focus(2);
        tab.split(9, 10, Direction::Right, PathBuf::from("/tmp"));
        tab.split(11, 12, Direction::Down, PathBuf::from("/tmp"));
        tab.toggle_quadrant_zoom();
        tab.focus(2);
        tab.toggle_zoom();
        assert!(tab.close(2));
        assert_eq!(
            (tab.focused, tab.zoom, tab.quadrant_zoom),
            (9, Some(12), Some(12))
        );
        assert_eq!(tab.visible_tree(false).panes(), [9, 11]);
        tab.toggle_zoom();
        assert!(tab.close(9));
        assert_eq!((tab.focused, tab.zoom, tab.quadrant_zoom), (11, None, None));
    }
    #[test]
    fn closing_other_panes_preserves_full_pane_zoom() {
        for quadrant_zoom in [false, true] {
            let mut tab = quadrants();
            tab.focus(2);
            tab.split(9, 10, Direction::Right, PathBuf::from("/tmp"));
            tab.split(11, 12, Direction::Down, PathBuf::from("/tmp"));
            if quadrant_zoom {
                tab.toggle_quadrant_zoom();
            }
            tab.focus(9);
            tab.toggle_zoom();
            for pane in [5, 11, 2] {
                assert!(tab.close(pane));
                assert_eq!((tab.focused, tab.zoom), (9, Some(9)));
                assert_eq!(
                    tab.quadrant_zoom,
                    (quadrant_zoom && pane != 2).then_some(10)
                );
                assert_eq!(tab.visible_tree(false).panes(), [9]);
            }
        }
    }
    #[test]
    fn directional_navigation_uses_edges_and_stays_in_zoomed_quadrant() {
        let mut tab = Tab::new(1, 2, PathBuf::from("/tmp"));
        tab.split(3, 4, Direction::Right, PathBuf::from("/tmp"));
        tab.split(5, 6, Direction::Down, PathBuf::from("/tmp"));
        if let Node::Split { second, .. } = &mut tab.root.kind
            && let Node::Split { ratio, .. } = &mut second.kind
        {
            *ratio = 0.1;
        }
        // A full-height pane moves to the topmost neighboring pane, and has
        // nothing below it even if another pane has a lower center point.
        assert_eq!(tab.target(2, Direction::Right), Some(3));
        assert_eq!(tab.target(2, Direction::Down), None);
        let mut tab = quadrants();
        tab.focus(2);
        tab.split(9, 10, Direction::Right, PathBuf::from("/tmp"));
        tab.toggle_quadrant_zoom();
        assert_eq!(tab.target(2, Direction::Right), Some(9));
        assert_eq!(tab.target(9, Direction::Right), None);
        assert_eq!(tab.target(9, Direction::Next), Some(2));
        assert_eq!(tab.target(9, Direction::QuadrantRight), Some(3));
    }
    #[test]
    fn pane_navigation_keeps_full_zoom_within_zoomed_quadrant() {
        for (forward, backward) in [
            (Direction::Right, Direction::Left),
            (Direction::Down, Direction::Up),
        ] {
            let mut tab = quadrants();
            tab.focus(2);
            tab.split(9, 10, forward, PathBuf::from("/tmp"));
            tab.toggle_quadrant_zoom();
            tab.toggle_zoom();
            assert_eq!((tab.zoom, tab.quadrant_zoom), (Some(9), Some(10)));
            for (direction, expected) in [
                (backward, 2),
                (forward, 9),
                (Direction::Next, 2),
                (Direction::Next, 9),
                (Direction::Previous, 2),
                (Direction::Previous, 9),
            ] {
                let target = tab.target(tab.focused, direction).unwrap();
                assert_eq!(target, expected);
                tab.zoom = tab.pane_navigation_zoom(target);
                tab.focus(target);
                assert_eq!(
                    (tab.focused, tab.zoom, tab.quadrant_zoom),
                    (expected, Some(expected), Some(10)),
                    "{direction:?}"
                );
                assert_eq!(tab.visible_tree(false).panes(), [expected]);
            }
            assert_eq!(tab.target(tab.focused, forward), None);
            assert!(tab.unzoom_after_blocked_navigation());
            assert_eq!(
                (tab.focused, tab.zoom, tab.quadrant_zoom),
                (9, Some(10), Some(10))
            );
            assert_eq!(tab.visible_tree(false).panes(), [2, 9]);
            assert_eq!(tab.target(tab.focused, forward), None);
            assert!(!tab.unzoom_after_blocked_navigation());
            assert_eq!(
                (tab.focused, tab.zoom, tab.quadrant_zoom),
                (9, Some(10), Some(10))
            );
        }
    }
    #[test]
    fn pane_navigation_without_combined_zoom_keeps_existing_behavior() {
        for (zoom, quadrant_zoom, expected_zoom) in [
            (None, None, None),
            (Some(9), None, None),
            (Some(10), Some(10), Some(10)),
        ] {
            let mut tab = quadrants();
            tab.focus(2);
            tab.split(9, 10, Direction::Right, PathBuf::from("/tmp"));
            tab.zoom = zoom;
            tab.quadrant_zoom = quadrant_zoom;
            let target = tab.target(tab.focused, Direction::Left).unwrap();
            tab.zoom = tab.pane_navigation_zoom(target);
            tab.focus(target);
            assert_eq!(
                (tab.focused, tab.zoom, tab.quadrant_zoom),
                (2, expected_zoom, quadrant_zoom)
            );
            assert_eq!(
                tab.visible_tree(false).panes().len(),
                if quadrant_zoom.is_some() { 2 } else { 5 }
            );
        }
    }
    #[test]
    fn peek_restores_full_zoom_only_within_original_quadrant() {
        let mut tab = quadrants();
        tab.focus(2);
        tab.split(9, 10, Direction::Right, PathBuf::from("/tmp"));
        tab.toggle_quadrant_zoom();
        tab.toggle_zoom();
        let chord = Modifiers {
            super_key: true,
            ..Modifiers::default()
        };
        let mut peek = tab.begin_peek(chord).unwrap();
        assert_eq!((tab.zoom, tab.quadrant_zoom), (None, None));
        assert_eq!(tab.activate_quadrant(2), Some(9));
        peek.target = 2;
        let target = tab.finish_peek(peek);
        tab.focus(target);
        assert_eq!(
            (target, tab.zoom, tab.quadrant_zoom),
            (9, Some(9), Some(10))
        );
        let mut peek = tab.begin_peek(chord).unwrap();
        peek.target = 5;
        let target = tab.finish_peek(peek);
        tab.focus(target);
        assert_eq!((target, tab.zoom, tab.quadrant_zoom), (5, Some(5), Some(5)));
        assert!(tab.unzoom_after_blocked_navigation());
        assert_eq!((tab.zoom, tab.quadrant_zoom), (None, None));
        tab.toggle_zoom();
        assert!(
            tab.begin_peek(chord).is_none(),
            "full zoom alone does not trigger peek"
        );
        assert!(tab.unzoom_after_blocked_navigation());
        tab.focus(2);
        tab.toggle_quadrant_zoom();
        assert!(
            !tab.unzoom_after_blocked_navigation(),
            "multi-pane quadrant stays zoomed at its edge"
        );
    }
    #[test]
    fn split_sizes_use_axis_weights_pixels_and_clamped_dividers() {
        let mut tree = Tree::leaf(1);
        tree.split(1, 2, 3, Direction::Right);
        tree.split(2, 4, 5, Direction::Right);
        tree.split(4, 6, 7, Direction::Down);
        tree.equalize();
        let bounds = Rect {
            width: 900.0,
            height: 600.0,
            ..Rect::UNIT
        };
        for (_, rect) in tree.layout(bounds) {
            assert!((rect.width - 300.0).abs() < 0.001);
        }
        assert!(tree.resize(4, Direction::Left, 60.0, bounds));
        let widths = tree.layout(bounds);
        assert!((widths[1].1.width - 240.0).abs() < 0.001);
        assert!((widths[2].1.width - 360.0).abs() < 0.001);
        let (id, axis, parent) = tree.divider_at(bounds, [540.0, 20.0], 3.0).unwrap();
        assert_eq!((id, axis), (5, Axis::Horizontal));
        assert!((parent.width - 600.0).abs() < 0.001);
        assert!(tree.set_ratio(id, 2.0));
        assert!((tree.layout(bounds)[1].1.width - 540.0).abs() < 0.001);
        assert!(!tree.set_ratio(id, f32::NAN));
        assert!(!tree.resize(99, Direction::Left, 20.0, bounds));
    }

    #[test]
    fn nested_split_minima_preserve_ratios_and_follow_geometry_changes() {
        let mut tree = Tree::leaf(1);
        tree.split(1, 2, 3, Direction::Right);
        tree.split(1, 4, 5, Direction::Right);
        tree.set_ratio(3, 0.1);
        tree.set_ratio(5, 0.1);
        let bounds = Rect {
            width: 300.0,
            height: 150.0,
            ..Rect::UNIT
        };
        let desired = tree.layout(bounds);
        for minimum in [[30.0, 40.0], [45.0, 65.0], [30.0, 40.0]] {
            let layout = tree.layout_with_minimum(bounds, minimum);
            assert!(
                layout
                    .iter()
                    .all(|(_, r)| r.width >= minimum[0] && r.height >= minimum[1]),
                "{layout:?}"
            );
            assert!(
                (layout.iter().map(|(_, r)| r.width).sum::<f32>() - bounds.width).abs() < 0.001
            );
            assert_eq!(tree.layout(bounds), desired);
        }
        let grown = Rect {
            width: 4000.0,
            ..bounds
        };
        assert_eq!(
            tree.layout_with_minimum(grown, [30.0, 40.0]),
            tree.layout(grown)
        );
        for width in [0.0, 1.0, 20.0] {
            let tiny = Rect { width, ..bounds };
            assert!(
                tree.layout_with_minimum(tiny, [30.0, 40.0])
                    .iter()
                    .all(|(_, r)| r.width.is_finite() && r.width >= 0.0)
            );
        }
    }

    #[test]
    fn constrained_dividers_and_keyboard_resizes_use_the_visible_position() {
        let mut tree = Tree::leaf(1);
        tree.split(1, 2, 3, Direction::Right);
        tree.set_ratio(3, 0.1);
        let bounds = Rect {
            width: 100.0,
            height: 80.0,
            ..Rect::UNIT
        };
        let minimum = [30.0, 40.0];
        assert!(
            tree.divider_at_with_minimum(bounds, [10.0, 20.0], 1.0, minimum)
                .is_none()
        );
        assert_eq!(
            tree.divider_at_with_minimum(bounds, [30.0, 20.0], 1.0, minimum)
                .unwrap()
                .0,
            3
        );
        assert!(tree.resize_with_minimum(1, Direction::Right, 10.0, bounds, minimum));
        assert!((tree.layout_with_minimum(bounds, minimum)[0].1.width - 40.0).abs() < 0.001);
        for (ratio, width) in [(-100.0, 30.0), (100.0, 70.0)] {
            assert!(tree.set_ratio_with_minimum(3, ratio, bounds, minimum));
            assert!((tree.layout(bounds)[0].1.width - width).abs() < 0.001);
        }
        assert!(tree.resize_with_minimum(1, Direction::Left, 10000.0, bounds, minimum));
        assert!((tree.layout(bounds)[0].1.width - 30.0).abs() < 0.001);
        let grown = Rect {
            width: 1000.0,
            ..bounds
        };
        assert!((tree.layout_with_minimum(grown, minimum)[0].1.width - 300.0).abs() < 0.001);
        assert!(!tree.resize_with_minimum(1, Direction::Left, f32::NAN, bounds, minimum));
        tree.set_ratio(3, 0.9);
        let fractional = Rect {
            width: 63.0,
            ..bounds
        };
        assert_eq!(
            tree.layout_with_minimum(fractional, minimum)[1].1.width,
            30.0
        );
    }

    #[test]
    fn local_minima_do_not_change_server_owned_geometry() {
        let mut tab = Tab::new(10, 1, PathBuf::from("/tmp"));
        tab.split(2, 3, Direction::Right, PathBuf::from("/tmp"));
        tab.root.set_ratio(3, 0.1);
        tab.server_size = Some([100, 40]);
        let bounds = Rect {
            width: 100.0,
            height: 80.0,
            ..Rect::UNIT
        };
        assert_eq!(
            tab.layout_with_minimum(bounds, false, 10.0, [40.0, 40.0]),
            tab.layout(bounds, false, 10.0)
        );
    }

    #[test]
    fn dragging_a_zoomed_subtree_uses_its_visible_parent_bounds() {
        let mut tree = Tree::leaf(1);
        tree.split(1, 2, 3, Direction::Right);
        tree.split(1, 4, 5, Direction::Right);
        let bounds = Rect {
            width: 300.0,
            height: 100.0,
            ..Rect::UNIT
        };
        let minimum = [100.0, 20.0];
        let (divider, _, parent) = tree
            .node(5)
            .unwrap()
            .divider_at_with_minimum(bounds, [150.0, 30.0], 1.0, minimum)
            .unwrap();
        assert!(tree.set_ratio_with_minimum(divider, 0.9, parent, minimum));
        let visible = tree.node(5).unwrap().layout_with_minimum(bounds, minimum);
        assert!((visible[0].1.width - 200.0).abs() < 0.001, "{visible:?}");
        assert!((visible[1].1.width - 100.0).abs() < 0.001, "{visible:?}");
    }

    #[test]
    fn equalize_aligns_quadrants_independently_of_their_interiors() {
        for transpose in [false, true] {
            let (right, down) = if transpose {
                (Direction::Down, Direction::Right)
            } else {
                (Direction::Right, Direction::Down)
            };
            let mut tree = Tree::leaf(1);
            for (pane, new, split, direction) in [
                (1, 2, 3, right),
                (1, 4, 5, down),
                (2, 6, 7, down),
                (1, 8, 9, down),
                (8, 10, 11, down),
                (1, 12, 13, right),
                (12, 14, 15, right),
                (2, 16, 17, right),
                (16, 18, 19, right),
            ] {
                assert!(tree.split(pane, new, split, direction));
                assert!(tree.set_ratio(split, 0.2));
            }
            let quadrants: Vec<_> = tree.panes().iter().map(|id| tree.quadrant(*id)).collect();
            tree.equalize();
            assert_eq!(
                tree.panes()
                    .iter()
                    .map(|id| tree.quadrant(*id))
                    .collect::<Vec<_>>(),
                quadrants
            );
            let mut layout = Vec::new();
            tree.layout_into(Rect::UNIT, &mut layout, true, [0.0; 2]);
            let rects: BTreeMap<_, _> = layout.iter().copied().collect();
            let check = |id, [x, y, width, height]: [f32; 4]| {
                let expected = if transpose {
                    [y, x, height, width]
                } else {
                    [x, y, width, height]
                };
                let actual = rects[&id];
                for (actual, expected) in [actual.x, actual.y, actual.width, actual.height]
                    .into_iter()
                    .zip(expected)
                {
                    assert!(
                        (actual - expected).abs() < 0.000001,
                        "pane {id}: {actual} != {expected}"
                    );
                }
            };
            for (id, x, y) in [(9, 0.0, 0.0), (17, 0.5, 0.0), (4, 0.0, 0.5), (6, 0.5, 0.5)] {
                check(id, [x, y, 0.5, 0.5]);
            }
            // The first quadrant has three rows, with three panes in its first row.
            for (column, id) in [1, 12, 14].into_iter().enumerate() {
                check(id, [column as f32 / 6.0, 0.0, 1.0 / 6.0, 1.0 / 6.0]);
            }
            for (row, id) in [(1, 8), (2, 10)] {
                check(id, [0.0, row as f32 / 6.0, 0.5, 1.0 / 6.0]);
            }
            for (column, id) in [2, 16, 18].into_iter().enumerate() {
                check(id, [0.5 + column as f32 / 6.0, 0.0, 1.0 / 6.0, 0.5]);
            }
            tree.equalize();
            let mut repeated = Vec::new();
            tree.layout_into(Rect::UNIT, &mut repeated, true, [0.0; 2]);
            assert_eq!(layout, repeated);
        }
    }

    #[test]
    fn equalize_preserves_outer_lanes_with_incomplete_quadrants() {
        let mut tree = Tree::leaf(1);
        tree.split(1, 2, 3, Direction::Right);
        tree.split(2, 4, 5, Direction::Right);
        tree.split(1, 6, 7, Direction::Down);
        tree.split(4, 8, 9, Direction::Down);
        tree.split(4, 10, 11, Direction::Down);
        tree.equalize();
        let layout: BTreeMap<_, _> = tree.layout(Rect::UNIT).into_iter().collect();
        for rect in layout.values() {
            assert!((rect.width - 1.0 / 3.0).abs() < 0.000001);
        }
        assert_eq!(layout[&1].height, 0.5);
        assert_eq!(layout[&6].height, 0.5);
        assert_eq!(layout[&2].height, 1.0);
        assert_eq!(layout[&8].height, 0.5);
        assert_eq!(layout[&4].height, 0.25);
        assert_eq!(layout[&10].height, 0.25);
    }

    #[test]
    fn restored_layouts_do_not_reuse_ids_and_closing_keeps_nearby_focus() {
        let mut workspace = Workspace::default();
        workspace.id();
        let saved = workspace.clone();
        let issued = workspace.id();
        workspace.restore(saved);
        assert!(workspace.id() > issued);
        let mut tab = quadrants();
        tab.focus(5);
        assert!(tab.close(5));
        assert_eq!(tab.focused, 3);
        workspace.windows.push(WindowState {
            id: 100,
            tabs: vec![tab],
            active_tab: 0,
            frame: [0.0, 0.0, 100.0, 100.0],
            quick: false,
        });
        assert!(workspace.close_pane(2));
        assert!(workspace.close_pane(3));
        assert!(workspace.close_pane(7));
        assert!(workspace.windows.is_empty());
        assert!(!workspace.close_pane(7));
    }

    fn workspace_with_overlays() -> Workspace {
        let mut tab = Tab::new(1, 2, PathBuf::from("/tmp"));
        tab.server_size = Some([100, 50]);
        for pane in [3, 4] {
            tab.panes.insert(pane, SavedPane::default());
            tab.overlays.push(Overlay {
                pane,
                x: 5,
                y: 7,
                cols: 30,
                rows: 10,
                z: pane as u32,
                modal: false,
                capture_keys: false,
                close_click: false,
                close_cancel: false,
            });
        }
        Workspace {
            next_id: 20,
            windows: vec![WindowState {
                id: 10,
                tabs: vec![tab],
                active_tab: 0,
                frame: [0.0, 0.0, 800.0, 600.0],
                quick: false,
            }],
            ..Workspace::default()
        }
    }

    #[test]
    fn imported_overlays_reject_splits_and_close_without_dangling_state() {
        let mut workspace = workspace_with_overlays();
        let tab = &mut workspace.windows[0].tabs[0];
        tab.focused = 3;
        tab.zoom = Some(3);
        let before = serde_json::to_vec(tab).unwrap();
        tab.split(5, 6, Direction::Down, PathBuf::from("/tmp"));
        assert_eq!(serde_json::to_vec(tab).unwrap(), before);
        assert!(workspace.close_pane(3));
        workspace.validate().unwrap();
        let tab = &workspace.windows[0].tabs[0];
        assert_eq!(tab.focused, 2);
        assert_eq!(tab.zoom, None);
        assert!(!tab.panes.contains_key(&3));
        assert_eq!(tab.overlays.len(), 1);
        assert_eq!(tab.overlays[0].pane, 4);
        assert_eq!(tab.pane_ids(), [2, 4]);
    }

    #[test]
    fn closing_last_tiled_pane_retains_floating_geometry_and_focus() {
        let mut workspace = workspace_with_overlays();
        let original = workspace.windows[0].tabs[0].layout(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 500.0,
            },
            false,
            10.0,
        );
        assert!(workspace.close_pane(2));
        workspace.validate().unwrap();
        let tab = &workspace.windows[0].tabs[0];
        assert!(matches!(tab.root.kind, Node::Empty));
        assert_eq!(tab.focused, 3);
        assert_eq!(tab.pane_ids(), [3, 4]);
        let floating = tab.layout(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 500.0,
            },
            false,
            10.0,
        );
        assert_eq!(
            floating,
            original
                .into_iter()
                .filter(|(pane, _)| *pane != 2)
                .collect::<Vec<_>>()
        );
        assert!(workspace.close_pane(3));
        workspace.validate().unwrap();
        assert_eq!(workspace.windows[0].tabs[0].focused, 4);
        assert!(workspace.close_pane(4));
        workspace.validate().unwrap();
        assert!(workspace.windows.is_empty());
    }

    #[test]
    fn serialized_workspace_preserves_layout_focus_and_directories() {
        let state = Workspace {
            next_id: 100,
            version: 1,
            deck_positions: vec![Some(7), None, Some(2)],
            pinned_panes: vec![7, 2],
            tmuxx: vec![],
            windows: vec![WindowState {
                id: 20,
                tabs: vec![quadrants()],
                active_tab: 0,
                frame: [10.0, 20.0, 800.0, 600.0],
                quick: false,
            }],
        };
        state.validate().unwrap();
        let bytes = serde_json::to_vec(&state).unwrap();
        let decoded: Workspace = serde_json::from_slice(&bytes).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded.deck_positions, state.deck_positions);
        assert_eq!(decoded.pinned_panes, state.pinned_panes);
        let tab = &decoded.windows[0].tabs[0];
        assert_eq!(tab.root.panes(), [2, 7, 3, 5]);
        assert_eq!(tab.focused, 7);
        assert_eq!(tab.panes[&7].working_directory, PathBuf::from("/tmp"));
    }

    #[test]
    fn saved_tmuxx_attachments_are_optional_local_and_independent_of_layout_undo() {
        let mut state = Workspace::default();
        let legacy = serde_json::to_value(&state).unwrap();
        assert!(legacy.get("tmuxx").is_none());
        assert!(
            serde_json::from_value::<Workspace>(legacy)
                .unwrap()
                .tmuxx
                .is_empty()
        );
        let previous = state.clone();
        state.tmuxx.push(SavedAttachment {
            endpoint: std::env::temp_dir().join("rustty-tmuxx.native"),
            instance_id: "server-one".into(),
            session_id: 7,
            session_name: "work".into(),
            frame: [10.0, 20.0, 800.0, 600.0],
        });
        state.validate().unwrap();
        let decoded: Workspace =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        assert_eq!(decoded.tmuxx, state.tmuxx);
        state.restore(previous);
        assert_eq!(state.tmuxx, decoded.tmuxx);

        for invalid in 0..4 {
            let mut state = decoded.clone();
            match invalid {
                0 => state.tmuxx[0].endpoint = "relative.native".into(),
                1 => state.tmuxx[0].session_name = "bad\0name".into(),
                2 => state.tmuxx[0].frame[2] = 0.0,
                _ => state.tmuxx.resize(65, state.tmuxx[0].clone()),
            }
            assert!(state.validate().is_err());
        }
    }

    #[test]
    fn deck_positions_are_optional_bounded_and_independent_of_layout_undo() {
        let mut state = Workspace::default();
        let window = state.id();
        let tab = state.id();
        let pane = state.id();
        state.windows.push(WindowState {
            id: window,
            tabs: vec![Tab::new(tab, pane, PathBuf::from("/tmp"))],
            active_tab: 0,
            frame: [0.0, 0.0, 800.0, 600.0],
            quick: false,
        });
        let prior = state.clone();
        state.deck_positions = vec![None, Some(pane), Some(pane), Some(999)];
        assert!(state.reconcile_deck_positions());
        assert_eq!(state.deck_positions, [None, Some(pane)]);
        state.restore(prior.clone());
        assert_eq!(state.deck_positions, [None, Some(pane)]);
        state.append_layout(prior).unwrap();
        assert_eq!(state.deck_positions, [None, Some(pane)]);
        let mut json = serde_json::to_value(&state).unwrap();
        json.as_object_mut().unwrap().remove("deck_positions");
        assert!(
            serde_json::from_value::<Workspace>(json.clone())
                .unwrap()
                .deck_positions
                .is_empty()
        );
        json["deck_positions"] = serde_json::json!({"invalid": true});
        assert!(
            serde_json::from_value::<Workspace>(json.clone())
                .unwrap()
                .deck_positions
                .is_empty()
        );
        json["deck_positions"] = serde_json::json!([pane, "bad", null]);
        assert_eq!(
            serde_json::from_value::<Workspace>(json.clone())
                .unwrap()
                .deck_positions,
            [Some(pane), None, None]
        );
        json["deck_positions"] = serde_json::json!(vec![pane; MAX_DECK_SLOTS + 10]);
        assert_eq!(
            serde_json::from_value::<Workspace>(json)
                .unwrap()
                .deck_positions
                .len(),
            MAX_DECK_SLOTS
        );
        state.close_pane(pane);
        assert!(state.deck_positions.is_empty());
    }

    #[test]
    fn pin_navigation_preserves_starting_quad_zoom_with_and_without_peek() {
        fn tab(offset: Id, mode: usize) -> Tab {
            let mut tab = Tab::new(offset + 1, offset + 2, "/tmp".into());
            tab.split(offset + 3, offset + 4, Direction::Right, "/tmp".into());
            tab.split(offset + 5, offset + 6, Direction::Down, "/tmp".into());
            tab.focus(offset + 2);
            tab.split(offset + 7, offset + 8, Direction::Down, "/tmp".into());
            tab.focus(offset + 5);
            tab.split(offset + 11, offset + 12, Direction::Right, "/tmp".into());
            tab.focus(offset + 2);
            tab.split(offset + 9, offset + 10, Direction::Right, "/tmp".into());
            if mode >= 2 {
                tab.toggle_quadrant_zoom();
            }
            if mode == 1 || mode == 3 {
                tab.toggle_zoom();
            }
            tab
        }
        let chord = Modifiers {
            super_key: true,
            shift: true,
            ..Modifiers::default()
        };
        for source_mode in 0..4 {
            for destination_mode in 0..4 {
                for location in 0..3 {
                    for target in [2, 9, 11] {
                        let offset = if location == 0 { 0 } else { 100 };
                        let pane = offset + target;
                        let mut initial = Workspace {
                            next_id: 1000,
                            windows: vec![WindowState {
                                id: 20,
                                tabs: vec![tab(0, source_mode)],
                                active_tab: 0,
                                frame: [0.0, 0.0, 800.0, 600.0],
                                quick: false,
                            }],
                            pinned_panes: vec![pane],
                            ..Workspace::default()
                        };
                        if location == 1 {
                            initial.windows[0].tabs.push(tab(offset, destination_mode));
                        } else if location == 2 {
                            let mut window = initial.windows[0].clone();
                            window.id = 200;
                            window.tabs = vec![tab(offset, destination_mode)];
                            initial.windows.push(window);
                        }
                        let mut without_peek = None;
                        for overlap in [false, true] {
                            let mut state = initial.clone();
                            let mut peek = if overlap {
                                state.windows[0].tabs[0].begin_peek(chord)
                            } else {
                                None
                            };
                            assert_eq!(peek.is_some(), overlap && source_mode >= 2);
                            let (window, tab, resolved) =
                                state.prepare_pin_navigation(1, 20, &mut peek).unwrap();
                            assert_eq!(resolved, pane);
                            assert!(peek.is_none(), "pin navigation must end the peek");
                            crate::outliner::select_workspace(
                                &mut state,
                                Some(20),
                                crate::outliner::Target::Pane { tab, pane },
                            );
                            let window = state.windows.iter().find(|w| w.id == window).unwrap();
                            let destination = &window.tabs[window.active_tab];
                            assert_eq!(destination.focused, pane);
                            assert_eq!(
                                destination.quadrant_zoom,
                                if source_mode >= 2 {
                                    destination.root.quadrant(pane)
                                } else {
                                    None
                                }
                            );
                            assert!(destination.visible_tree(false).contains(pane));
                            state.validate().unwrap();
                            let windows = serde_json::to_value(&state.windows).unwrap();
                            if overlap {
                                assert_eq!(Some(windows), without_peek);
                            } else {
                                without_peek = Some(windows);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn missing_pin_navigation_leaves_peek_intact_for_modifier_release() {
        let mut tab = quadrants();
        tab.toggle_quadrant_zoom();
        let mut peek = tab.begin_peek(Modifiers {
            super_key: true,
            ..Modifiers::default()
        });
        let mut state = Workspace {
            windows: vec![WindowState {
                id: 20,
                tabs: vec![tab],
                active_tab: 0,
                frame: [0.0, 0.0, 800.0, 600.0],
                quick: false,
            }],
            pinned_panes: vec![999],
            ..Workspace::default()
        };
        let before = serde_json::to_value(&state.windows).unwrap();
        for index in [0, 1, 2, usize::MAX] {
            assert!(state.prepare_pin_navigation(index, 20, &mut peek).is_none());
            assert!(peek.is_some());
            assert_eq!(serde_json::to_value(&state.windows).unwrap(), before);
        }
        let tab = &mut state.windows[0].tabs[0];
        let target = tab.finish_peek(peek.take().unwrap());
        tab.focus(target);
        assert_eq!((tab.zoom, tab.quadrant_zoom), (Some(target), Some(target)));
    }

    #[test]
    fn pins_keep_their_order_and_ignore_layout_undo_while_pruning_closed_panes() {
        let mut state = Workspace {
            next_id: 100,
            windows: vec![WindowState {
                id: 20,
                tabs: vec![quadrants()],
                active_tab: 0,
                frame: [0.0, 0.0, 800.0, 600.0],
                quick: false,
            }],
            pinned_panes: vec![7, 999, 2, 7, 1],
            ..Workspace::default()
        };
        assert!(state.reconcile_pinned_panes());
        assert_eq!(state.pinned_panes, [7, 2]);
        let prior = state.clone();
        assert!(state.toggle_pin(5));
        assert!(state.toggle_pin(2));
        assert!(state.toggle_pin(2));
        assert_eq!(state.pinned_panes, [7, 5, 2]);
        assert!(state.reorder_pin(2, Some(7)));
        assert_eq!(state.pinned_panes, [2, 7, 5]);
        assert!(state.reorder_pin(2, None));
        assert_eq!(state.pinned_panes, [7, 5, 2]);
        assert!(!state.reorder_pin(5, Some(2)));
        assert!(!state.reorder_pin(5, Some(5)));
        assert!(!state.reorder_pin(5, Some(999)));
        assert!(!state.reorder_pin(999, None));
        assert!(!state.toggle_pin(999));
        state.restore(prior);
        assert_eq!(state.pinned_panes, [7, 5, 2]);
        state.close_pane(7);
        assert_eq!(state.pinned_panes, [5, 2]);
    }

    #[test]
    fn malformed_or_oversized_pin_metadata_does_not_prevent_workspace_loading() {
        let mut value = serde_json::to_value(Workspace::default()).unwrap();
        value.as_object_mut().unwrap().remove("pinned_panes");
        assert!(
            serde_json::from_value::<Workspace>(value.clone())
                .unwrap()
                .pinned_panes
                .is_empty()
        );
        for malformed in [
            serde_json::Value::Null,
            serde_json::json!({"invalid": true}),
        ] {
            value["pinned_panes"] = malformed;
            assert!(
                serde_json::from_value::<Workspace>(value.clone())
                    .unwrap()
                    .pinned_panes
                    .is_empty()
            );
        }
        value["pinned_panes"] = serde_json::json!([7, "bad", null, -1, 2]);
        assert_eq!(
            serde_json::from_value::<Workspace>(value.clone())
                .unwrap()
                .pinned_panes,
            [7, 2]
        );
        value["pinned_panes"] = serde_json::json!(vec![7; MAX_PINNED_PANES + 10]);
        let mut decoded = serde_json::from_value::<Workspace>(value).unwrap();
        assert_eq!(decoded.pinned_panes.len(), MAX_PINNED_PANES);
        assert!(decoded.reconcile_pinned_panes());
        assert!(decoded.pinned_panes.is_empty());
    }
}
