//! The window hierarchy shared by the outliner and the focused-pane breadcrumb.
use crate::workspace::{Id, Rect, Tab, Tree, WindowState, Workspace};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Window(Id),
    Tab(Id),
    Quad { tab: Id, node: Id },
    Pane { tab: Id, pane: Id },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Zoom {
    #[default]
    None,
    Quad,
    Pane,
    PaneInQuad,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub target: Target,
    pub depth: usize,
    pub label: String,
    pub tab_index: Option<usize>,
    /// Location within the parent, independent of the current zoom.
    pub rect: Rect,
    pub focused: bool,
    pub ancestor: bool,
    pub color: Option<[u8; 3]>,
    pub zoom: Zoom,
}

#[derive(Clone, Default)]
pub struct Expansion {
    open: HashSet<Target>,
    focused_path: Vec<Target>,
}

impl Expansion {
    /// Open a changed focus path and report whether it needs revealing.
    pub fn sync(&mut self, rows: &[Row]) -> bool {
        let branches: HashSet<_> = rows
            .windows(2)
            .filter(|pair| pair[1].depth > pair[0].depth)
            .map(|pair| pair[0].target)
            .collect();
        self.open.retain(|target| branches.contains(target));
        let path: Vec<_> = rows
            .iter()
            .filter(|row| row.ancestor || row.focused)
            .map(|row| row.target)
            .collect();
        let changed = path != self.focused_path;
        if changed {
            self.open.extend(
                path.iter()
                    .filter(|target| branches.contains(target))
                    .copied(),
            );
            self.focused_path = path;
        }
        changed
    }

    pub fn is_open(&self, target: Target) -> bool {
        self.open.contains(&target)
    }

    pub fn toggle(&mut self, rows: &[Row], index: usize, recursive: bool) {
        if !has_children(rows, index) {
            return;
        }
        let row = &rows[index];
        let open = !self.is_open(row.target);
        let end = if recursive {
            rows[index + 1..]
                .iter()
                .position(|child| child.depth <= row.depth)
                .map_or(rows.len(), |offset| index + 1 + offset)
        } else {
            index + 1
        };
        for index in index..end {
            if has_children(rows, index) {
                if open {
                    self.open.insert(rows[index].target);
                } else {
                    self.open.remove(&rows[index].target);
                }
            }
        }
    }

    pub fn visible(&self, rows: &[Row]) -> Vec<usize> {
        let mut visible = Vec::new();
        let mut closed_depth = None;
        for (index, row) in rows.iter().enumerate() {
            if closed_depth.is_some_and(|depth| row.depth > depth) {
                continue;
            }
            closed_depth = None;
            visible.push(index);
            if has_children(rows, index) && !self.is_open(row.target) {
                closed_depth = Some(row.depth);
            }
        }
        visible
    }
}

pub fn has_children(rows: &[Row], index: usize) -> bool {
    rows.get(index)
        .zip(rows.get(index.saturating_add(1)))
        .is_some_and(|(row, next)| next.depth > row.depth)
}

pub fn workspace_rows(
    workspace: &Workspace,
    active_window: Option<Id>,
    labels: &HashMap<Id, String>,
    tab_labels: &HashMap<Id, String>,
    quick_used: &HashSet<Id>,
) -> Vec<Row> {
    let mut outline = Vec::new();
    let mut number = 0;
    let windows: Vec<_> = workspace
        .windows
        .iter()
        .filter(|window| {
            (!window.quick || quick_used.contains(&window.id))
                && window.tabs.get(window.active_tab).is_some()
        })
        .collect();
    let has_windows = windows.len() > 1;
    for window in windows {
        let Some(tab) = window.tabs.get(window.active_tab) else {
            continue;
        };
        let active = active_window == Some(window.id);
        let label = if window.quick {
            "Quick Terminal".to_owned()
        } else {
            number += 1;
            format!("Window {number} · {}", tab_label(tab, labels, tab_labels))
        };
        if has_windows {
            outline.push(Row {
                target: Target::Window(window.id),
                depth: 0,
                label,
                tab_index: None,
                rect: Rect::UNIT,
                focused: false,
                ancestor: active,
                color: tab.color,
                zoom: Zoom::None,
            });
        }
        outline.extend(rows(window, labels, tab_labels).into_iter().map(|mut row| {
            row.depth += usize::from(has_windows);
            row.focused &= active;
            row.ancestor &= active;
            row
        }));
    }
    outline
}

pub fn pinned_paths(rows: &[Row], pins: &[Id]) -> Vec<Vec<Row>> {
    let pins_set: HashSet<_> = pins.iter().copied().collect();
    let mut parents: Vec<&Row> = Vec::new();
    let mut paths = HashMap::new();
    for row in rows {
        while parents
            .last()
            .is_some_and(|parent| parent.depth >= row.depth)
        {
            parents.pop();
        }
        parents.push(row);
        if let Target::Pane { pane, .. } = row.target
            && pins_set.contains(&pane)
        {
            paths.insert(pane, parents.iter().map(|row| (*row).clone()).collect());
        }
    }
    pins.iter().filter_map(|pane| paths.remove(pane)).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchResult {
    pub path: Vec<Row>,
    pub highlights: Vec<Range<usize>>,
}

pub fn search(rows: &[Row], query: &str) -> Vec<SearchResult> {
    let query: Vec<_> = query
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    let mut matches: Vec<_> = rows
        .iter()
        .filter_map(|row| {
            let Target::Pane { pane, .. } = row.target else {
                return None;
            };
            let (rank, highlights) = match_label(&row.label, &query)?;
            Some((rank, pane, highlights))
        })
        .collect();
    matches.sort_by_key(|(rank, ..)| *rank);
    let panes: Vec<_> = matches.iter().map(|(_, pane, _)| *pane).collect();
    pinned_paths(rows, &panes)
        .into_iter()
        .zip(matches)
        .map(|(path, (_, _, highlights))| SearchResult { path, highlights })
        .collect()
}

fn match_label(label: &str, query: &[char]) -> Option<([usize; 3], Vec<Range<usize>>)> {
    let Some(&first) = query.first() else {
        return Some(([0; 3], Vec::new()));
    };
    let letters: Vec<_> = label
        .char_indices()
        .filter(|(_, character)| !character.is_whitespace())
        .flat_map(|(byte, character)| {
            character
                .to_lowercase()
                .map(move |lowercase| (lowercase, byte..byte + character.len_utf8()))
        })
        .collect();
    let mut best = None;
    'starts: for (start, (letter, _)) in letters.iter().enumerate() {
        if *letter != first {
            continue;
        }
        let mut positions = vec![start];
        let mut cursor = start + 1;
        for needle in &query[1..] {
            let Some(offset) = letters[cursor..]
                .iter()
                .position(|(letter, _)| letter == needle)
            else {
                continue 'starts;
            };
            positions.push(cursor + offset);
            cursor += offset + 1;
        }
        let rank = [cursor - start - query.len(), start, letters.len()];
        if best.as_ref().is_none_or(|(previous, _)| rank < *previous) {
            best = Some((rank, positions));
        }
    }
    let (rank, positions) = best?;
    let mut highlights: Vec<Range<usize>> = Vec::new();
    for position in positions {
        let range = letters[position].1.clone();
        if let Some(previous) = highlights.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            highlights.push(range);
        }
    }
    Some((rank, highlights))
}

pub fn rows(
    window: &WindowState,
    labels: &HashMap<Id, String>,
    tab_labels: &HashMap<Id, String>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let has_tabs = window.tabs.len() > 1;
    for (index, tab) in window.tabs.iter().enumerate() {
        let active = index == window.active_tab;
        if has_tabs {
            rows.push(Row {
                target: Target::Tab(tab.id),
                depth: 0,
                label: tab_label(tab, labels, tab_labels),
                tab_index: Some(index),
                rect: Rect::UNIT,
                focused: false,
                ancestor: active,
                color: tab.color,
                zoom: zoom(tab),
            });
        }
        rows.extend(tab_rows(tab, labels, usize::from(has_tabs), active));
    }
    rows
}

pub fn breadcrumb(tab: &Tab, labels: &HashMap<Id, String>) -> Vec<Row> {
    tab_rows(tab, labels, 0, true)
        .into_iter()
        .filter(|row| row.focused || row.ancestor)
        .collect()
}

fn tab_rows(tab: &Tab, labels: &HashMap<Id, String>, depth: usize, active: bool) -> Vec<Row> {
    let layout = tab.root.layout(Rect::UNIT);
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    for &(pane, rect) in &layout {
        if let Some(quad) = tab.root.quadrant(pane) {
            if !seen.insert(quad) {
                continue;
            }
            let node = tab.root.node(quad).unwrap();
            let children = node.layout(Rect::UNIT);
            let mut names: Vec<_> = children
                .iter()
                .map(|(pane, _)| pane_label(*pane, labels))
                .collect();
            names.sort_unstable();
            names.dedup();
            let rect = layout
                .iter()
                .filter(|(pane, _)| node.contains(*pane))
                .map(|(_, rect)| *rect)
                .reduce(|a, b| Rect {
                    x: a.x.min(b.x),
                    y: a.y.min(b.y),
                    width: (a.x + a.width).max(b.x + b.width) - a.x.min(b.x),
                    height: (a.y + a.height).max(b.y + b.height) - a.y.min(b.y),
                })
                .unwrap();
            rows.push(Row {
                target: Target::Quad {
                    tab: tab.id,
                    node: quad,
                },
                depth,
                label: names.join(", "),
                tab_index: None,
                rect,
                focused: false,
                ancestor: active && node.contains(tab.focused),
                color: tab.color,
                zoom: if tab.quadrant_zoom == Some(quad) {
                    Zoom::Quad
                } else {
                    Zoom::None
                },
            });
            for (pane, rect) in children {
                rows.push(pane_row(tab, labels, pane, rect, depth + 1, active));
            }
        } else {
            rows.push(pane_row(tab, labels, pane, rect, depth, active));
        }
    }
    rows
}

fn pane_row(
    tab: &Tab,
    labels: &HashMap<Id, String>,
    pane: Id,
    rect: Rect,
    depth: usize,
    active: bool,
) -> Row {
    Row {
        target: Target::Pane { tab: tab.id, pane },
        depth,
        label: pane_label(pane, labels).to_owned(),
        tab_index: None,
        rect,
        focused: active && tab.focused == pane,
        ancestor: false,
        color: tab.color,
        zoom: if tab.zoom == Some(pane) && tab.zoom != tab.quadrant_zoom {
            zoom(tab)
        } else {
            Zoom::None
        },
    }
}

fn pane_label(pane: Id, labels: &HashMap<Id, String>) -> &str {
    labels
        .get(&pane)
        .map(String::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("Terminal")
}

fn tab_label(tab: &Tab, labels: &HashMap<Id, String>, tab_labels: &HashMap<Id, String>) -> String {
    tab_labels
        .get(&tab.id)
        .map(String::as_str)
        .or(tab.title.as_deref())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| pane_label(tab.focused, labels))
        .to_owned()
}

fn zoom(tab: &Tab) -> Zoom {
    match (tab.zoom, tab.quadrant_zoom) {
        (None, _) => Zoom::None,
        (Some(_), None) => Zoom::Pane,
        (Some(pane), Some(quad)) if pane == quad => Zoom::Quad,
        _ => Zoom::PaneInQuad,
    }
}

/// Select the first descendant unless focus is already in this branch.
pub fn select(window: &mut WindowState, target: Target) -> Option<Id> {
    select_in_window(window, target, true)
}

pub fn target_window(workspace: &Workspace, target: Target) -> Option<Id> {
    workspace.windows.iter().find_map(|window| {
        match target {
            Target::Window(id) => window.id == id,
            Target::Tab(tab) | Target::Quad { tab, .. } | Target::Pane { tab, .. } => window
                .tabs
                .iter()
                .any(|candidate| candidate.id == tab && target_node(candidate, target).is_some()),
        }
        .then_some(window.id)
    })
}

pub fn select_workspace(
    workspace: &mut Workspace,
    active_window: Option<Id>,
    target: Target,
) -> Option<(Id, Id)> {
    let id = target_window(workspace, target)?;
    let window = workspace
        .windows
        .iter_mut()
        .find(|window| window.id == id)?;
    select_in_window(window, target, active_window == Some(id)).map(|pane| (id, pane))
}

fn select_in_window(window: &mut WindowState, target: Target, active: bool) -> Option<Id> {
    let index = match target {
        Target::Window(id) => {
            if window.id != id || active {
                return None;
            }
            0
        }
        Target::Tab(id) | Target::Quad { tab: id, .. } | Target::Pane { tab: id, .. } => {
            window.tabs.iter().position(|tab| tab.id == id)?
        }
    };
    let tab = window.tabs.get_mut(index)?;
    let node = target_node(tab, target)?;
    if active && window.active_tab == index && node.contains(tab.focused) {
        return None;
    }
    let pane = *node.panes().first()?;
    window.active_tab = index;
    tab.focus(pane);
    Some(pane)
}

fn target_node(tab: &Tab, target: Target) -> Option<&Tree> {
    match target {
        Target::Window(_) | Target::Tab(_) => Some(&tab.root),
        Target::Quad { node, .. } => {
            let node = tab.root.node(node)?;
            let pane = *node.panes().first()?;
            (tab.root.quadrant(pane) == Some(node.id)).then_some(node)
        }
        Target::Pane { pane, .. } => {
            let node = tab.root.node(pane)?;
            node.contains(pane).then_some(node)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustty::config::Direction;

    fn window() -> WindowState {
        WindowState {
            id: 100,
            tabs: vec![Tab::new(1, 2, "/tmp".into())],
            active_tab: 0,
            frame: [0.0; 4],
            quick: false,
        }
    }

    fn quads() -> WindowState {
        let mut window = window();
        let tab = &mut window.tabs[0];
        tab.split(3, 4, Direction::Right, "/tmp".into());
        tab.split(5, 6, Direction::Down, "/tmp".into());
        tab.focus(2);
        tab.split(7, 8, Direction::Down, "/tmp".into());
        tab.focus(2);
        tab.split(9, 10, Direction::Right, "/tmp".into());
        window
    }

    fn expansion_rows() -> Vec<Row> {
        let mut first = quads();
        first.tabs.push(Tab::new(11, 12, "/tmp".into()));
        let second = WindowState {
            id: 200,
            tabs: vec![Tab::new(21, 22, "/tmp".into())],
            ..window()
        };
        let mut workspace = Workspace::default();
        workspace.windows = vec![first, second];
        workspace_rows(
            &workspace,
            Some(100),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        )
    }

    fn row_index(rows: &[Row], target: Target) -> usize {
        rows.iter().position(|row| row.target == target).unwrap()
    }

    #[test]
    fn expansion_starts_closed_and_opens_only_the_focused_path() {
        let rows = expansion_rows();
        let mut expansion = Expansion::default();
        assert_eq!(
            expansion
                .visible(&rows)
                .iter()
                .map(|&index| rows[index].target)
                .collect::<Vec<_>>(),
            vec![Target::Window(100), Target::Window(200)]
        );
        expansion.sync(&rows);
        assert_eq!(
            expansion.open,
            [
                Target::Window(100),
                Target::Tab(1),
                Target::Quad { tab: 1, node: 10 }
            ]
            .into()
        );
        assert!(
            expansion
                .visible(&rows)
                .contains(&row_index(&rows, Target::Pane { tab: 1, pane: 9 }))
        );
        assert!(
            !expansion
                .visible(&rows)
                .contains(&row_index(&rows, Target::Pane { tab: 11, pane: 12 }))
        );
    }

    #[test]
    fn shallow_toggle_preserves_children_and_recursive_toggle_changes_the_whole_branch() {
        let rows = expansion_rows();
        let tab = row_index(&rows, Target::Tab(1));
        let quad = Target::Quad { tab: 1, node: 10 };
        let mut expansion = Expansion::default();
        expansion.sync(&rows);
        expansion.toggle(&rows, tab, false);
        assert!(!expansion.is_open(Target::Tab(1)));
        assert!(expansion.is_open(quad));
        expansion.toggle(&rows, tab, false);
        assert!(expansion.is_open(Target::Tab(1)) && expansion.is_open(quad));
        expansion.toggle(&rows, tab, true);
        assert!(!expansion.is_open(quad));
        expansion.toggle(&rows, tab, true);
        for row in &rows {
            if matches!(row.target, Target::Quad { tab: 1, .. }) {
                assert!(expansion.is_open(row.target));
            }
        }
        assert!(!expansion.is_open(Target::Tab(11)));
        assert!(!expansion.is_open(Target::Window(200)));
    }

    #[test]
    fn manual_collapse_stays_until_focus_moves_then_only_the_new_path_opens() {
        let mut rows = expansion_rows();
        let quad = Target::Quad { tab: 1, node: 10 };
        let mut expansion = Expansion::default();
        expansion.sync(&rows);
        expansion.toggle(&rows, row_index(&rows, quad), false);
        rows[0].label = "Renamed window".into();
        expansion.sync(&rows);
        assert!(!expansion.is_open(quad));
        for row in &mut rows {
            row.focused = row.target == Target::Pane { tab: 1, pane: 2 };
        }
        expansion.sync(&rows);
        assert!(expansion.is_open(quad));
        let before = expansion.open.clone();
        for row in &mut rows {
            row.focused = row.target == Target::Pane { tab: 1, pane: 7 };
            if matches!(row.target, Target::Quad { .. }) {
                row.ancestor = row.target == Target::Quad { tab: 1, node: 7 };
            }
        }
        expansion.sync(&rows);
        assert_eq!(
            expansion
                .open
                .difference(&before)
                .copied()
                .collect::<HashSet<_>>(),
            [Target::Quad { tab: 1, node: 7 }].into()
        );
    }

    #[test]
    fn changed_ancestry_opens_the_new_path_for_the_same_focused_panel_and_prunes_old_keys() {
        let mut rows = expansion_rows();
        let previous = Target::Quad { tab: 1, node: 10 };
        let next = Target::Quad { tab: 1, node: 20 };
        let mut expansion = Expansion::default();
        expansion.sync(&rows);
        expansion.toggle(&rows, row_index(&rows, Target::Tab(1)), false);
        let index = row_index(&rows, previous);
        rows[index].target = next;
        expansion.sync(&rows);
        assert!(!expansion.is_open(previous));
        assert!(expansion.is_open(next));
        assert!(expansion.is_open(Target::Tab(1)));
        assert!(
            expansion
                .visible(&rows)
                .contains(&row_index(&rows, Target::Pane { tab: 1, pane: 9 }))
        );
    }

    #[test]
    fn flat_panels_gain_only_the_hierarchy_levels_that_exist() {
        let mut window = window();
        window.tabs[0].split(3, 4, Direction::Right, "/tmp".into());
        let labels = [(2, "left".into()), (3, "right".into())].into();
        let flat = rows(&window, &labels, &HashMap::new());
        assert_eq!(flat.len(), 2);
        assert_eq!((flat[0].depth, flat[0].label.as_str()), (0, "left"));
        assert_eq!(flat[0].rect.width, 0.5);
        assert_eq!(flat[1].rect.x, 0.5);
        assert!(flat[1].focused);

        window.tabs[0].title = Some("Work".into());
        window.tabs[0].color = Some([40, 100, 220]);
        window.tabs.push(Tab::new(11, 12, "/tmp".into()));
        let nested = rows(&window, &labels, &HashMap::new());
        assert_eq!(
            (nested[0].label.as_str(), nested[0].tab_index),
            ("Work", Some(0))
        );
        assert_eq!(nested[0].color, Some([40, 100, 220]));
        assert!(nested[0].ancestor);
        assert_eq!(nested[1].depth, 1);
        assert!(!nested[4].focused);
        assert_eq!(breadcrumb(&window.tabs[0], &labels).len(), 1);
        let titles = [(1, "Full tab title".into())].into();
        assert_eq!(rows(&window, &labels, &titles)[0].label, "Full tab title");
    }

    #[test]
    fn quad_labels_and_icons_share_the_unzoomed_topology_with_breadcrumbs() {
        let mut window = quads();
        let labels = [(2, "editor".into()), (9, "editor".into())].into();
        let first = rows(&window, &labels, &HashMap::new());
        assert_eq!(first[0].target, Target::Quad { tab: 1, node: 10 });
        assert_eq!((first[0].depth, first[0].label.as_str()), (0, "editor"));
        assert_eq!(
            first[0].rect,
            Rect {
                width: 0.5,
                height: 0.5,
                ..Rect::UNIT
            }
        );
        assert_eq!(
            first[1].rect,
            Rect {
                width: 0.5,
                ..Rect::UNIT
            }
        );
        assert_eq!(
            first[2].rect,
            Rect {
                x: 0.5,
                width: 0.5,
                ..Rect::UNIT
            }
        );

        let tab = &mut window.tabs[0];
        tab.toggle_quadrant_zoom();
        tab.toggle_zoom();
        let crumb = breadcrumb(tab, &labels);
        assert_eq!(crumb.len(), 2);
        assert_eq!(
            (crumb[0].zoom, crumb[1].zoom),
            (Zoom::Quad, Zoom::PaneInQuad)
        );
        assert!(crumb[0].ancestor && crumb[1].focused);
        assert_eq!(crumb[0].rect, first[0].rect);
        assert_eq!(crumb[1].rect, first[2].rect);

        let labels = [(2, "editor".into()), (9, "tests".into())].into();
        assert_eq!(
            rows(&window, &labels, &HashMap::new())[0].label,
            "editor, tests"
        );
    }

    #[test]
    fn quad_names_sort_and_deduplicate_panel_labels_independent_of_focus_and_zoom() {
        let mut window = quads();
        window.tabs[0].split(11, 12, Direction::Down, "/tmp".into());
        for (labels, expected) in [
            (
                [
                    (2, "zebra".into()),
                    (9, "Alpha".into()),
                    (11, "zebra".into()),
                ]
                .into(),
                "Alpha, zebra",
            ),
            (
                [(2, "editor".into()), (9, String::new())].into(),
                "Terminal, editor",
            ),
        ] {
            for pane in [2, 9, 11] {
                let tab = &mut window.tabs[0];
                tab.zoom = None;
                tab.quadrant_zoom = None;
                tab.focus(pane);
                for zoomed in [false, true] {
                    if zoomed {
                        window.tabs[0].toggle_quadrant_zoom();
                        window.tabs[0].toggle_zoom();
                    }
                    let outline = rows(&window, &labels, &HashMap::new());
                    assert_eq!(outline[0].label, expected);
                    assert_eq!(breadcrumb(&window.tabs[0], &labels)[0].label, expected);
                    let mut expansion = Expansion::default();
                    expansion.sync(&outline);
                    expansion.toggle(&outline, 0, false);
                    assert_eq!(outline[expansion.visible(&outline)[0]].label, expected);
                }
            }
        }
    }

    #[test]
    fn selection_preserves_focused_ancestors_and_reveals_other_branches() {
        let mut window = quads();
        window.tabs[0].toggle_quadrant_zoom();
        window.tabs[0].toggle_zoom();
        for target in [
            Target::Tab(1),
            Target::Quad { tab: 1, node: 10 },
            Target::Pane { tab: 1, pane: 9 },
        ] {
            assert_eq!(select(&mut window, target), None);
            assert_eq!(
                (window.tabs[0].zoom, window.tabs[0].quadrant_zoom),
                (Some(9), Some(10))
            );
        }
        assert_eq!(
            select(&mut window, Target::Pane { tab: 1, pane: 2 }),
            Some(2)
        );
        assert_eq!(
            (window.tabs[0].zoom, window.tabs[0].quadrant_zoom),
            (Some(10), Some(10))
        );
        assert_eq!(
            select(&mut window, Target::Quad { tab: 1, node: 5 }),
            Some(5)
        );
        assert_eq!(
            (window.tabs[0].zoom, window.tabs[0].quadrant_zoom),
            (Some(5), Some(5))
        );

        window.tabs.insert(0, Tab::new(11, 12, "/tmp".into()));
        assert_eq!(select(&mut window, Target::Tab(1)), Some(2));
        assert_eq!(window.active_tab, 1);
        assert_eq!(window.tabs[1].focused, 2);
        assert_eq!(window.tabs[1].zoom, Some(10));
        assert_eq!(select(&mut window, Target::Quad { tab: 1, node: 4 }), None);
        assert_eq!(select(&mut window, Target::Pane { tab: 1, pane: 99 }), None);
        assert_eq!(window.tabs[1].focused, 2);
    }

    #[test]
    fn incomplete_quads_keep_unsplit_lanes_as_panels() {
        let mut window = window();
        window.tabs[0].split(3, 4, Direction::Right, "/tmp".into());
        window.tabs[0].split(5, 6, Direction::Down, "/tmp".into());
        let rows = rows(&window, &HashMap::new(), &HashMap::new());
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].target, Target::Pane { tab: 1, pane: 2 });
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].target, Target::Quad { tab: 1, node: 3 });
        assert_eq!(rows[2].depth, 1);
        assert_eq!(rows[3].target, Target::Quad { tab: 1, node: 5 });
    }

    #[test]
    fn icon_regions_follow_uneven_dividers_at_each_hierarchy_level() {
        let mut window = quads();
        window.tabs[0].root.set_ratio(4, 0.3);
        window.tabs[0].root.set_ratio(10, 0.75);
        let rows = rows(&window, &HashMap::new(), &HashMap::new());
        assert_eq!(rows[0].rect.width, 0.3);
        assert_eq!(rows[1].rect.width, 0.75);
        assert_eq!(rows[2].rect.x, 0.75);
        assert_eq!(rows[2].rect.width, 0.25);
    }

    #[test]
    fn global_outline_highlights_one_window_and_hides_unused_quick_terminal() {
        let mut first = quads();
        first.tabs[0].color = Some([80, 120, 200]);
        let second = WindowState {
            id: 200,
            tabs: vec![Tab::new(21, 22, "/tmp".into())],
            ..window()
        };
        let quick = WindowState {
            id: 300,
            tabs: vec![Tab::new(31, 32, "/tmp".into())],
            quick: true,
            ..window()
        };
        let mut workspace = Workspace::default();
        workspace.windows = vec![first, quick, second];
        let titles = [(1, "Work".into()), (21, "Research".into())].into();
        let rows = workspace_rows(
            &workspace,
            Some(100),
            &HashMap::new(),
            &titles,
            &HashSet::new(),
        );
        assert_eq!(rows[0].target, Target::Window(100));
        assert_eq!(rows[0].label, "Window 1 · Work");
        assert!(rows[0].ancestor);
        assert_eq!(rows[0].color, Some([80, 120, 200]));
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows.iter().filter(|row| row.focused).count(), 1);
        assert_eq!(
            rows.last().unwrap().target,
            Target::Pane { tab: 21, pane: 22 }
        );
        assert!(!rows.last().unwrap().focused);
        assert_eq!(rows[rows.len() - 2].label, "Window 2 · Research");
        assert!(!rows.iter().any(|row| row.target == Target::Window(300)));

        let rows = workspace_rows(
            &workspace,
            Some(300),
            &HashMap::new(),
            &titles,
            &[300].into(),
        );
        let quick = rows
            .iter()
            .find(|row| row.target == Target::Window(300))
            .unwrap();
        assert_eq!(quick.label, "Quick Terminal");
        assert!(quick.ancestor);
        assert_eq!(
            rows.iter().find(|row| row.focused).unwrap().target,
            Target::Pane { tab: 31, pane: 32 }
        );
        assert_eq!(rows.iter().filter(|row| row.focused).count(), 1);
    }

    #[test]
    fn global_selection_only_preserves_ancestors_in_the_active_window() {
        let mut first = quads();
        first.tabs[0].toggle_quadrant_zoom();
        first.tabs[0].toggle_zoom();
        first.tabs.push(Tab::new(11, 12, "/tmp".into()));
        first.active_tab = 1;
        let mut second = WindowState {
            id: 200,
            tabs: vec![Tab::new(21, 22, "/tmp".into())],
            ..window()
        };
        second.tabs[0].split(23, 24, Direction::Right, "/tmp".into());
        second.tabs[0].toggle_zoom();
        let mut workspace = Workspace::default();
        workspace.windows = vec![first, second];

        assert_eq!(
            select_workspace(&mut workspace, Some(100), Target::Window(100)),
            None
        );
        assert_eq!(workspace.windows[0].active_tab, 1);
        assert_eq!(target_window(&workspace, Target::Tab(21)), Some(200));
        assert_eq!(
            select_workspace(&mut workspace, Some(100), Target::Tab(21)),
            Some((200, 22))
        );
        assert_eq!(workspace.windows[1].tabs[0].zoom, None);
        assert_eq!(
            select_workspace(&mut workspace, Some(200), Target::Tab(21)),
            None
        );
        assert_eq!(
            select_workspace(&mut workspace, Some(200), Target::Window(100)),
            Some((100, 2))
        );
        assert_eq!(workspace.windows[0].active_tab, 0);
        assert_eq!(workspace.windows[0].tabs[0].zoom, Some(10));
        workspace.windows[0].tabs[0].focus(9);
        assert_eq!(
            select_workspace(&mut workspace, Some(200), Target::Quad { tab: 1, node: 10 }),
            Some((100, 2))
        );
        assert_eq!(target_window(&workspace, Target::Window(999)), None);
        assert_eq!(
            select_workspace(
                &mut workspace,
                Some(100),
                Target::Pane { tab: 1, pane: 999 }
            ),
            None
        );
    }

    #[test]
    fn single_visible_window_omits_its_level_until_a_used_quick_terminal_joins() {
        let mut workspace = Workspace::default();
        workspace.windows = vec![
            window(),
            WindowState {
                id: 300,
                quick: true,
                tabs: vec![Tab::new(31, 32, "/tmp".into())],
                ..window()
            },
        ];
        let rows = workspace_rows(
            &workspace,
            Some(100),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].target, rows[0].depth),
            (Target::Pane { tab: 1, pane: 2 }, 0)
        );
        assert_eq!(pinned_paths(&rows, &[2])[0].len(), 1);

        let rows = workspace_rows(
            &workspace,
            Some(100),
            &HashMap::new(),
            &HashMap::new(),
            &[300].into(),
        );
        assert_eq!(rows[0].target, Target::Window(100));
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].target, Target::Window(300));
        assert_eq!(pinned_paths(&rows, &[2])[0].len(), 2);

        workspace.windows = vec![quads()];
        let rows = workspace_rows(
            &workspace,
            Some(100),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        assert_eq!(rows[0].target, Target::Quad { tab: 1, node: 10 });
        assert_eq!(rows[0].depth, 0);
        assert_eq!(pinned_paths(&rows, &[9])[0].len(), 2);
    }

    #[test]
    fn pinned_paths_follow_pin_order_and_use_only_each_panes_current_ancestors() {
        let rows = expansion_rows();
        let paths = pinned_paths(&rows, &[22, 9, 999, 12, 22]);
        let targets: Vec<Vec<_>> = paths
            .iter()
            .map(|path| path.iter().map(|row| row.target).collect())
            .collect();
        assert_eq!(
            targets,
            vec![
                vec![Target::Window(200), Target::Pane { tab: 21, pane: 22 }],
                vec![
                    Target::Window(100),
                    Target::Tab(1),
                    Target::Quad { tab: 1, node: 10 },
                    Target::Pane { tab: 1, pane: 9 }
                ],
                vec![
                    Target::Window(100),
                    Target::Tab(11),
                    Target::Pane { tab: 11, pane: 12 }
                ],
            ]
        );
        assert!(paths[1].last().unwrap().focused);
        assert!(!paths[0].last().unwrap().focused);
    }

    #[test]
    fn search_matches_only_panel_names_and_preserves_their_optional_ancestor_paths() {
        let mut rows = expansion_rows();
        rows[0].label = "AncestorKeyword".into();
        let panel = row_index(&rows, Target::Pane { tab: 1, pane: 9 });
        rows[panel].label = "Rustty".into();
        assert!(search(&rows, "AncestorKeyword").is_empty());
        let matches = search(&rows, "rs");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, pinned_paths(&rows, &[9])[0]);
        assert_eq!(matches[0].highlights, [0..1, 2..3]);

        let labels = [(2, "Rustty".into())].into();
        let flat = super::rows(&window(), &labels, &HashMap::new());
        assert_eq!(search(&flat, "rust")[0].path.len(), 1);
        let all = search(&rows, " \t\u{2003} ");
        assert!(all.iter().all(|result| result.highlights.is_empty()));
        assert_eq!(
            all.iter()
                .map(|result| result.path.last().unwrap().target)
                .collect::<Vec<_>>(),
            rows.iter()
                .filter(|row| matches!(row.target, Target::Pane { .. }))
                .map(|row| row.target)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    #[expect(clippy::single_range_in_vec_init)]
    fn search_ranks_contiguous_prefixes_then_shorter_names_and_keeps_stable_ties() {
        let template = super::rows(&window(), &HashMap::new(), &HashMap::new()).remove(0);
        let rows: Vec<_> = ["fo o", "xxfoo", "f-x-o-o", "foo", "FOO", "fxxfoo", "food"]
            .iter()
            .enumerate()
            .map(|(pane, label)| Row {
                target: Target::Pane {
                    tab: 1,
                    pane: pane as Id,
                },
                label: (*label).into(),
                ..template.clone()
            })
            .collect();
        let matches = search(&rows, "fOo");
        assert_eq!(
            matches
                .iter()
                .map(|result| result.path.last().unwrap().target)
                .collect::<Vec<_>>(),
            [0, 3, 4, 6, 1, 5, 2].map(|pane| Target::Pane { tab: 1, pane })
        );
        let later_match = matches
            .iter()
            .find(|result| result.path.last().unwrap().label == "fxxfoo")
            .unwrap();
        assert_eq!(later_match.highlights, [3..6]);
        assert!(search(&rows, "of").is_empty());
        assert!(search(&rows, "ffff").is_empty());
    }

    #[test]
    #[expect(clippy::single_range_in_vec_init)]
    fn search_highlights_original_utf8_scalars_and_coalesces_lowercase_expansions() {
        let template = super::rows(&window(), &HashMap::new(), &HashMap::new()).remove(0);
        for (label, query, expected) in [
            ("Été東京", "ét東", vec![0..3, 5..8]),
            ("İstanbul", "i\u{307}s", vec![0..3]),
            ("Ru\u{2003}St\tTy", "rU st tY", vec![0..2, 5..7, 8..10]),
        ] {
            let rows = [Row {
                label: label.into(),
                ..template.clone()
            }];
            let matches = search(&rows, query);
            assert_eq!(matches.len(), 1, "{label:?} {query:?}");
            assert_eq!(matches[0].highlights, expected);
            for range in &matches[0].highlights {
                assert!(label.get(range.clone()).is_some());
            }
        }
    }
}
