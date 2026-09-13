//! Window/tab/split state, independent of native windows and running processes.
use rustty::config::{Direction, Modifiers};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

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
    Pane(Id),
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Tree>,
        second: Box<Tree>,
    },
}

impl Tree {
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
            Node::Pane(_) => None,
            Node::Split { first, second, .. } => first.node(id).or_else(|| second.node(id)),
        }
    }
    pub fn contains(&self, pane: Id) -> bool {
        match &self.kind {
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
            Node::Pane(_) => false,
            Node::Split { first, second, .. } => {
                first.split(pane, new_pane, split_id, direction)
                    || second.split(pane, new_pane, split_id, direction)
            }
        }
    }
    pub fn remove(self, pane: Id) -> Option<Self> {
        match self.kind {
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
        let mut out = Vec::new();
        self.layout_into(rect, &mut out, false);
        out
    }
    fn layout_into(&self, rect: Rect, out: &mut Vec<(Id, Rect)>, include_splits: bool) {
        if include_splits && matches!(self.kind, Node::Split { .. }) {
            out.push((self.id, rect));
        }
        match &self.kind {
            Node::Pane(pane) => out.push((*pane, rect)),
            Node::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (a, b) = match axis {
                    Axis::Horizontal => (
                        Rect {
                            width: rect.width * ratio,
                            ..rect
                        },
                        Rect {
                            x: rect.x + rect.width * ratio,
                            width: rect.width * (1.0 - ratio),
                            ..rect
                        },
                    ),
                    Axis::Vertical => (
                        Rect {
                            height: rect.height * ratio,
                            ..rect
                        },
                        Rect {
                            y: rect.y + rect.height * ratio,
                            height: rect.height * (1.0 - ratio),
                            ..rect
                        },
                    ),
                };
                first.layout_into(a, out, include_splits);
                second.layout_into(b, out, include_splits);
            }
        }
    }
    // Ghostty's spatial navigation uses top-left distances on a grid whose
    // dimensions come from the split topology, including structural nodes.
    fn spatial(&self) -> Vec<(Id, Rect)> {
        fn dimensions(tree: &Tree) -> [f32; 2] {
            match &tree.kind {
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
        if let Node::Split {
            axis,
            ratio,
            first,
            second,
        } = &mut self.kind
        {
            let a = first.weight(*axis);
            let b = second.weight(*axis);
            *ratio = a as f32 / (a + b) as f32;
            first.equalize();
            second.equalize();
        }
    }
    pub fn resize(&mut self, pane: Id, direction: Direction, pixels: f32, bounds: Rect) -> bool {
        let Node::Split {
            axis,
            ratio,
            first,
            second,
        } = &mut self.kind
        else {
            return false;
        };
        let mut child_bounds = bounds;
        let in_first = first.contains(pane);
        if !in_first && !second.contains(pane) {
            return false;
        }
        match axis {
            Axis::Horizontal => {
                child_bounds.width *= if in_first { *ratio } else { 1.0 - *ratio };
                if !in_first {
                    child_bounds.x += bounds.width * *ratio;
                }
            }
            Axis::Vertical => {
                child_bounds.height *= if in_first { *ratio } else { 1.0 - *ratio };
                if !in_first {
                    child_bounds.y += bounds.height * *ratio;
                }
            }
        }
        let child = if in_first { first } else { second };
        if child.resize(pane, direction, pixels, child_bounds) {
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
        *ratio = (*ratio + if positive { delta } else { -delta }).clamp(0.1, 0.9);
        true
    }
    pub fn set_ratio(&mut self, id: Id, value: f32) -> bool {
        if !value.is_finite() {
            return false;
        }
        match &mut self.kind {
            Node::Split { ratio, .. } if self.id == id => {
                *ratio = value.clamp(0.1, 0.9);
                true
            }
            Node::Split { first, second, .. } => {
                first.set_ratio(id, value) || second.set_ratio(id, value)
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
        let mut nodes = Vec::new();
        self.layout_into(bounds, &mut nodes, true);
        nodes.into_iter().rev().find_map(|(id, rect)| {
            let Node::Split { axis, ratio, .. } = self.node(id)?.kind else {
                return None;
            };
            let near = match axis {
                Axis::Horizontal => (point[0] - rect.x - rect.width * ratio).abs() <= tolerance,
                Axis::Vertical => (point[1] - rect.y - rect.height * ratio).abs() <= tolerance,
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SavedPane {
    pub working_directory: PathBuf,
}

#[derive(Clone, Copy, Debug)]
pub struct Peek {
    pub chord: Modifiers,
    pub target: Id,
    pub full_zoom: Option<Id>,
    pub navigation_used: bool,
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
    pub remembered: BTreeMap<Id, Id>,
    pub panes: BTreeMap<Id, SavedPane>,
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
            remembered: BTreeMap::new(),
            panes: [(
                pane,
                SavedPane {
                    working_directory: directory,
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
    pub fn focus(&mut self, pane: Id) {
        if !self.root.contains(pane) {
            return;
        }
        if let Some(quadrant) = self.root.quadrant(self.focused) {
            self.remembered.insert(quadrant, self.focused);
        }
        self.focused = pane;
        if self
            .zoom
            .is_some_and(|id| self.root.node(id).is_none_or(|node| !node.contains(pane)))
        {
            self.quadrant_zoom = self.quadrant_zoom.and_then(|_| self.root.quadrant(pane));
            self.zoom = self.quadrant_zoom;
        }
        if let Some(quadrant) = self.root.quadrant(pane) {
            self.remembered.insert(quadrant, pane);
        }
    }
    pub fn begin_peek(&mut self, chord: Modifiers) -> Option<Peek> {
        self.quadrant_zoom?;
        if chord == Modifiers::default() {
            return None;
        }
        let peek = Peek {
            chord,
            target: self.focused,
            full_zoom: self.zoom.filter(|zoom| Some(*zoom) != self.quadrant_zoom),
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
        self.quadrant_zoom = quadrant;
        self.zoom = if Some(target) == peek.full_zoom {
            Some(target)
        } else {
            quadrant
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
    pub fn activate_quadrant(&self, pane: Id) -> Option<Id> {
        let quadrant = self.root.node(self.root.quadrant(pane)?)?;
        self.remembered_for(pane)
            .or_else(|| quadrant.panes().first().copied())
    }
    pub fn unzoom_after_blocked_navigation(&mut self) -> bool {
        let Some(zoom) = self.zoom else {
            return false;
        };
        if Some(zoom) == self.quadrant_zoom && !self.root.contains(zoom) {
            return false;
        }
        self.zoom = self.quadrant_zoom.filter(|id| *id != zoom);
        if self.zoom.is_none() {
            self.quadrant_zoom = None;
        }
        true
    }
    pub fn toggle_zoom(&mut self) {
        if self.zoom == Some(self.focused) {
            self.zoom = self.quadrant_zoom.filter(|id| *id != self.focused);
        } else {
            self.zoom = Some(self.focused);
        }
    }
    pub fn toggle_quadrant_zoom(&mut self) {
        let quadrant = self.root.quadrant(self.focused);
        if quadrant == self.quadrant_zoom && self.zoom.is_some() {
            self.zoom = None;
            self.quadrant_zoom = None;
        } else {
            self.zoom = quadrant;
            self.quadrant_zoom = quadrant;
        }
    }
    pub fn target(&self, from: Id, direction: Direction) -> Option<Id> {
        let quadrant_only = matches!(
            direction,
            Direction::QuadrantLeft
                | Direction::QuadrantRight
                | Direction::QuadrantUp
                | Direction::QuadrantDown
        );
        let root = if quadrant_only {
            &self.root
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
            let current = root.quadrant(from)?;
            let target = adjacent(current)
                .into_iter()
                .find(|id| root.quadrant(*id) == Some(*id))?;
            Some(root.node(target)?)
        } else {
            None
        };
        adjacent(from).into_iter().find(|id| {
            panes.contains(id) && target_quadrant.is_none_or(|quadrant| quadrant.contains(*id))
        })
    }
    pub fn split(&mut self, new_pane: Id, split_id: Id, direction: Direction, directory: PathBuf) {
        self.root.split(self.focused, new_pane, split_id, direction);
        self.panes.insert(
            new_pane,
            SavedPane {
                working_directory: directory,
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
        let Some(root) = self.root.clone().remove(pane) else {
            return false;
        };
        self.root = root;
        self.panes.remove(&pane);
        self.remembered.retain(|node, pane| {
            self.root
                .node(*node)
                .is_some_and(|node| node.contains(*pane))
        });
        self.zoom = self.zoom.filter(|id| self.root.node(*id).is_some());
        self.quadrant_zoom = self
            .quadrant_zoom
            .filter(|id| self.root.node(*id).is_some());
        if self.focused == pane {
            self.focused = next.unwrap_or_else(|| self.root.panes()[0]);
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    version: u32,
    next_id: Id,
    pub windows: Vec<WindowState>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            version: 1,
            next_id: 1,
            windows: Vec::new(),
        }
    }
}

impl Workspace {
    /// Restoring a layout must not reuse IDs issued after that snapshot.
    pub fn restore(&mut self, mut previous: Self) -> Self {
        previous.next_id = previous.next_id.max(self.next_id);
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
        let state: Self = serde_json::from_slice(&bytes).map_err(invalid)?;
        state.validate()?;
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
    fn validate(&self) -> io::Result<()> {
        if self.version != 1 || self.next_id == 0 || self.windows.len() > 64 {
            return Err(invalid("unsupported or invalid workspace state"));
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
                if !ids.insert(tab.id) || !tab.root.contains(tab.focused) {
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
                let panes = tab.root.panes();
                count += panes.len();
                if !valid
                    || count > 4096
                    || panes.len() != tab.panes.len()
                    || panes.iter().any(|id| !tab.panes.contains_key(id))
                    || tab.zoom.is_some_and(|id| tab.root.node(id).is_none())
                    || tab
                        .quadrant_zoom
                        .is_some_and(|id| tab.root.node(id).is_none())
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
    #[test]
    fn serialized_workspace_preserves_layout_focus_and_directories() {
        let state = Workspace {
            next_id: 100,
            version: 1,
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
        let tab = &decoded.windows[0].tabs[0];
        assert_eq!(tab.root.panes(), [2, 7, 3, 5]);
        assert_eq!(tab.focused, 7);
        assert_eq!(tab.panes[&7].working_directory, PathBuf::from("/tmp"));
    }
}
