//! Window/tab/split state, independent of native windows and running processes.
use rustty::config::Direction;
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
        self.layout_into(rect, &mut out);
        out
    }
    fn layout_into(&self, rect: Rect, out: &mut Vec<(Id, Rect)>) {
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
                first.layout_into(a, out);
                second.layout_into(b, out);
            }
        }
    }
    pub fn equalize(&mut self) {
        if let Node::Split {
            ratio,
            first,
            second,
            ..
        } = &mut self.kind
        {
            *ratio = 0.5;
            first.equalize();
            second.equalize();
        }
    }
    pub fn resize(&mut self, pane: Id, direction: Direction, delta: f32) -> bool {
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
        if if in_first {
            first.resize(pane, direction, delta)
        } else {
            second.resize(pane, direction, delta)
        } {
            return true;
        }
        let horizontal = matches!(direction, Direction::Left | Direction::Right);
        if horizontal != (*axis == Axis::Horizontal) {
            return false;
        }
        let positive = matches!(direction, Direction::Right | Direction::Down);
        *ratio = (*ratio + if positive { delta } else { -delta }).clamp(0.05, 0.95);
        true
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
            let child = if first.contains(pane) {
                first
            } else if second.contains(pane) {
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
        let layout = self.root.layout(Rect::UNIT);
        let index = layout.iter().position(|(id, _)| *id == from)?;
        if matches!(direction, Direction::Previous | Direction::Next) {
            return Some(
                layout[(index
                    + if direction == Direction::Next {
                        1
                    } else {
                        layout.len() - 1
                    })
                    % layout.len()]
                .0,
            );
        }
        let quadrant_only = matches!(
            direction,
            Direction::QuadrantLeft
                | Direction::QuadrantRight
                | Direction::QuadrantUp
                | Direction::QuadrantDown
        );
        let source_quadrant = self.root.quadrant(from);
        let source = layout[index].1.center();
        let mut targets = layout
            .into_iter()
            .filter_map(|(id, rect)| {
                if id == from || quadrant_only && self.root.quadrant(id) == source_quadrant {
                    return None;
                }
                let center = rect.center();
                let (along, across) = match direction {
                    Direction::Left | Direction::QuadrantLeft => {
                        (source[0] - center[0], source[1] - center[1])
                    }
                    Direction::Right | Direction::QuadrantRight => {
                        (center[0] - source[0], source[1] - center[1])
                    }
                    Direction::Up | Direction::QuadrantUp => {
                        (source[1] - center[1], source[0] - center[0])
                    }
                    _ => (center[1] - source[1], source[0] - center[0]),
                };
                (along > 0.0001).then_some((id, across.abs() * 2.0 + along))
            })
            .collect::<Vec<_>>();
        targets.sort_by(|a, b| a.1.total_cmp(&b.1));
        targets.first().map(|(id, _)| *id)
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
            self.focused = self.root.panes()[0];
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
