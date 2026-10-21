//! Server IDs are mapped to independent native views, including linked windows.
use crate::workspace::{
    Axis, Id, Node, Overlay, PaneRect, SavedPane, Tab, Tree, WindowState, Workspace,
};
use rustty::config::{Action, Direction};
use rustty::tmuxx::{Graph, Kind, Layout};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default)]
pub struct Mapping {
    ids: BTreeMap<String, Id>,
    /// Native clients keep their pane focus independent of shared server selection.
    pub local_focus: bool,
    pub server_focused: BTreeSet<Id>,
    pub panes: BTreeMap<Id, u64>,
    pub sessions: BTreeMap<Id, u64>,
    pub session_names: BTreeMap<Id, String>,
    pub windows: BTreeMap<Id, u64>,
    window_indices: BTreeMap<Id, u32>,
    pub sizes: BTreeMap<u64, (u16, u16)>,
    pub dividers: BTreeMap<Id, (u64, Axis, u32)>,
}
impl Mapping {
    fn id(&mut self, workspace: &mut Workspace, key: String) -> Id {
        *self.ids.entry(key).or_insert_with(|| workspace.id())
    }
    pub fn apply(
        &mut self,
        workspace: &mut Workspace,
        graph: &Graph,
        selected_sessions: &BTreeSet<u64>,
    ) -> Result<(), String> {
        let mut next = self.clone();
        let mut next_workspace = workspace.clone();
        next.apply_inner(&mut next_workspace, graph, selected_sessions)?;
        next_workspace
            .validate()
            .map_err(|error| error.to_string())?;
        *self = next;
        *workspace = next_workspace;
        Ok(())
    }
    fn apply_inner(
        &mut self,
        workspace: &mut Workspace,
        graph: &Graph,
        selected_sessions: &BTreeSet<u64>,
    ) -> Result<(), String> {
        let mut layouts = BTreeMap::new();
        for window in &graph.windows {
            let layout = rustty::tmuxx::Layout::parse(&window.layout).map_err(|e| e.to_string())?;
            layouts.insert(window.id, layout);
        }
        let previous = workspace.windows.clone();
        let owned: BTreeSet<_> = self.sessions.keys().copied().collect();
        let mut windows = Vec::new();
        self.panes.clear();
        self.sessions.clear();
        self.session_names.clear();
        self.windows.clear();
        self.window_indices.clear();
        self.dividers.clear();
        self.server_focused.clear();
        for session in graph
            .sessions
            .iter()
            .filter(|session| selected_sessions.contains(&session.id))
        {
            let id = self.id(workspace, format!("session:{}", session.id));
            self.sessions.insert(id, session.id);
            self.session_names.insert(id, session.name.clone());
            let old = previous.iter().find(|w| w.id == id);
            let mut tabs = Vec::new();
            let mut active_tab = 0;
            for (&window_index, &wid) in &session.windows {
                let Some(window) = graph.windows.iter().find(|w| w.id == wid) else {
                    return Err("missing server window".into());
                };
                let key = format!("{}:{window_index}:{wid}", session.id);
                let tab_id = self.id(workspace, format!("tab:{key}"));
                self.windows.insert(tab_id, wid);
                self.window_indices.insert(tab_id, window_index);
                let layout = &layouts[&wid];
                let root = if let Some(tiled) = layout.tiled_copy() {
                    self.tree(workspace, &tiled, &key, "root")?
                } else {
                    Tree::empty(self.id(workspace, format!("empty:{key}")))
                };
                let mut overlays = Vec::new();
                self.overlays(workspace, graph, layout, &key, &mut overlays)?;
                let mut panes = BTreeMap::new();
                for &pid in &window.panes {
                    let view = self.id(workspace, format!("pane:{key}:{pid}"));
                    self.panes.insert(view, pid);
                    let title = graph
                        .panes
                        .iter()
                        .find(|p| p.id == pid)
                        .and_then(|p| p.terminal["title"].as_array())
                        .map(|bytes| {
                            String::from_utf8_lossy(
                                &bytes
                                    .iter()
                                    .filter_map(|b| b.as_u64().map(|n| n as u8))
                                    .collect::<Vec<_>>(),
                            )
                            .into_owned()
                        });
                    panes.insert(
                        view,
                        SavedPane {
                            title,
                            ..Default::default()
                        },
                    );
                }
                let focused = self.id(workspace, format!("pane:{key}:{}", window.active));
                if !root.contains(focused)
                    && !overlays.iter().any(|overlay| overlay.pane == focused)
                {
                    return Err("active server pane is absent from layout".into());
                }
                if session.active == u64::from(window_index) {
                    active_tab = tabs.len();
                }
                let mut tab = old
                    .and_then(|w| w.tabs.iter().find(|t| t.id == tab_id))
                    .cloned()
                    .unwrap_or_else(|| Tab::new(tab_id, focused, Default::default()));
                tab.root = root;
                tab.overlays = overlays;
                tab.server_size = Some([u32::from(window.cols), u32::from(window.rows)]);
                tab.panes = panes;
                tab.title = Some(window.name.clone());
                let visible = if window.zoomed && !window.visible_layout.is_empty() {
                    Layout::parse(&window.visible_layout).map_err(|e| e.to_string())?
                } else {
                    layout.clone()
                };
                tab.zoom = window.zoomed.then(|| {
                    let target = window
                        .zoomed_pane
                        .or_else(|| {
                            if window.visible_layout.is_empty() {
                                return None;
                            }
                            // Stock tmux identifies the zoomed pane through its visible layout.
                            let mut panes =
                                visible.panes().into_iter().filter(|pane| pane.z.is_none());
                            let id = panes.next()?.id?;
                            panes.next().is_none().then_some(u64::from(id))
                        })
                        .unwrap_or(window.active);
                    self.id(workspace, format!("pane:{key}:{target}"))
                });
                tab.server_view = if (window.zoomed && !window.visible_layout.is_empty())
                    || graph
                        .panes
                        .iter()
                        .any(|pane| pane.window == wid && pane.x.is_some() && pane.y.is_some())
                {
                    let mut panes = Vec::new();
                    self.visible(workspace, graph, &visible, &key, &mut panes)?;
                    panes.sort_by_key(|(z, _)| {
                        z.map(|z| u32::MAX - z).map_or(0, |z| u64::from(z) + 1)
                    });
                    Some(panes.into_iter().map(|(_, rect)| rect).collect())
                } else {
                    None
                };
                tab.quadrant_zoom = None;
                if self.local_focus {
                    let visible = tab.visible_panes(false);
                    tab.focused = tab
                        .modal_pane()
                        .filter(|pane| visible.contains(pane))
                        .or_else(|| visible.contains(&tab.focused).then_some(tab.focused))
                        .or_else(|| visible.contains(&focused).then_some(focused))
                        .or_else(|| visible.first().copied())
                        .ok_or("server window has no visible pane")?;
                    self.server_focused.insert(focused);
                } else {
                    tab.focused = focused;
                }
                tabs.push(tab);
            }
            if !tabs.is_empty() {
                windows.push(WindowState {
                    id,
                    tabs,
                    active_tab,
                    frame: old.map_or([100.0, 100.0, 1000.0, 680.0], |w| w.frame),
                    quick: false,
                });
            }
        }
        let mut projected: BTreeMap<_, _> = windows.into_iter().map(|w| (w.id, w)).collect();
        workspace.windows = previous
            .into_iter()
            .filter_map(|window| {
                if owned.contains(&window.id) {
                    projected.remove(&window.id)
                } else {
                    Some(window)
                }
            })
            .collect();
        workspace.windows.extend(projected.into_values());
        Ok(())
    }
    fn overlays(
        &mut self,
        workspace: &mut Workspace,
        graph: &Graph,
        layout: &Layout,
        key: &str,
        out: &mut Vec<Overlay>,
    ) -> Result<(), String> {
        match &layout.kind {
            Kind::Pane(pane) => {
                if let Some(z) = pane.z {
                    let pid = u64::from(pane.id.ok_or("floating pane has no ID")?);
                    let server = graph
                        .panes
                        .iter()
                        .find(|pane| pane.id == pid)
                        .ok_or("missing floating pane")?;
                    out.push(Overlay {
                        pane: self.id(workspace, format!("pane:{key}:{pid}")),
                        x: layout.geometry.x,
                        y: layout.geometry.y,
                        cols: layout.geometry.width,
                        rows: layout.geometry.height,
                        z,
                        modal: server.modal,
                        capture_keys: server.modal_capture_keys,
                        close_click: server.modal_close_click,
                        close_cancel: server.modal_close_cancel,
                    });
                }
            }
            Kind::Split { children, .. } => {
                for child in children {
                    self.overlays(workspace, graph, child, key, out)?;
                }
            }
        }
        Ok(())
    }
    fn visible(
        &mut self,
        workspace: &mut Workspace,
        graph: &Graph,
        layout: &Layout,
        key: &str,
        out: &mut Vec<(Option<u32>, PaneRect)>,
    ) -> Result<(), String> {
        match &layout.kind {
            Kind::Pane(pane) => {
                let pid = u64::from(pane.id.ok_or("visible layout pane has no ID")?);
                let server = graph
                    .panes
                    .iter()
                    .find(|pane| pane.id == pid)
                    .ok_or("missing visible pane")?;
                // Raw layout slots include border status; pane geometry is the PTY content.
                let (x, y, cols, rows) = match (server.x, server.y) {
                    (Some(x), Some(y)) => (x, y, u32::from(server.cols), u32::from(server.rows)),
                    _ => (
                        layout.geometry.x,
                        layout.geometry.y,
                        layout.geometry.width,
                        layout.geometry.height,
                    ),
                };
                let reserved_top = (i64::from(y) - i64::from(layout.geometry.y))
                    .clamp(0, i64::from(layout.geometry.height))
                    as u32;
                let reserved_bottom = layout
                    .geometry
                    .height
                    .saturating_sub(reserved_top)
                    .saturating_sub(rows);
                out.push((
                    pane.z,
                    PaneRect {
                        pane: self.id(workspace, format!("pane:{key}:{pid}")),
                        x,
                        y,
                        cols,
                        rows,
                        reserved_top,
                        reserved_bottom,
                    },
                ));
            }
            Kind::Split { children, .. } => {
                for child in children {
                    self.visible(workspace, graph, child, key, out)?;
                }
            }
        }
        Ok(())
    }
    fn tree(
        &mut self,
        workspace: &mut Workspace,
        layout: &Layout,
        key: &str,
        path: &str,
    ) -> Result<Tree, String> {
        match &layout.kind {
            Kind::Pane(pane) => Ok(Tree::leaf(self.id(
                workspace,
                format!("pane:{key}:{}", pane.id.ok_or("layout pane has no ID")?),
            ))),
            Kind::Split { axis, children } => {
                if children.len() < 2 {
                    return Err("empty server layout".into());
                }
                self.children(workspace, *axis, children, key, path)
            }
        }
    }
    fn children(
        &mut self,
        workspace: &mut Workspace,
        axis: rustty::tmuxx::Axis,
        children: &[Layout],
        key: &str,
        path: &str,
    ) -> Result<Tree, String> {
        if children.len() == 1 {
            return self.tree(workspace, &children[0], key, path);
        }
        let native_axis = match axis {
            rustty::tmuxx::Axis::Horizontal => Axis::Horizontal,
            rustty::tmuxx::Axis::Vertical => Axis::Vertical,
        };
        let extent = |node: &Layout| match axis {
            rustty::tmuxx::Axis::Horizontal => node.geometry.width,
            rustty::tmuxx::Axis::Vertical => node.geometry.height,
        };
        let total: u32 = children.iter().map(extent).sum::<u32>() + children.len() as u32 - 1;
        let ratio = (extent(&children[0]) as f32 + 0.5) / total as f32;
        let id = self.id(workspace, format!("split:{key}:{path}"));
        let first = self.tree(workspace, &children[0], key, &format!("{path}/0"))?;
        let second = self.children(workspace, axis, &children[1..], key, &format!("{path}/1"))?;
        if let Some(first_pane) = children[0].panes().first() {
            self.dividers.insert(
                id,
                (
                    u64::from(first_pane.id.ok_or("layout pane has no ID")?),
                    native_axis,
                    total,
                ),
            );
        }
        Ok(Tree {
            id,
            kind: Node::Split {
                axis: native_axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            },
        })
    }
    pub fn focus_target(
        &self,
        workspace: &Workspace,
        host: Id,
        pane: Id,
    ) -> Option<(u64, u32, u64)> {
        let window = workspace.windows.iter().find(|window| window.id == host)?;
        let tab = window
            .tabs
            .get(window.active_tab)
            .filter(|tab| tab.panes.contains_key(&pane))?;
        Some((
            *self.sessions.get(&host)?,
            *self.window_indices.get(&tab.id)?,
            *self.panes.get(&pane)?,
        ))
    }
    pub fn action(
        &self,
        workspace: &Workspace,
        host: Id,
        action: &Action,
    ) -> Option<Result<Vec<String>, String>> {
        let pane = workspace
            .windows
            .iter()
            .find(|w| w.id == host)
            .and_then(|w| w.tabs.get(w.active_tab))
            .map(|t| t.focused);
        let pid = pane.and_then(|p| self.panes.get(&p)).copied();
        let sid = Some(*self.sessions.get(&host)?);
        let window = workspace.windows.iter().find(|w| w.id == host);
        let tab = window.and_then(|w| w.tabs.get(w.active_tab));
        let wid = tab.and_then(|t| self.windows.get(&t.id)).copied();
        let target = |command: &str, flag: &str, value: Option<u64>, prefix: char| -> Vec<String> {
            let mut argv = vec![command.into()];
            if !flag.is_empty() {
                argv.push(flag.into());
            }
            if let Some(value) = value {
                argv.extend(["-t".into(), format!("{prefix}{value}")]);
            }
            argv
        };
        let command = match action {
            Action::NewTab => target("new-window", "", sid, '$'),
            Action::NewFloatingPane => target("new-pane", "", pid, '%'),
            Action::NewSplit(direction) => {
                let mut argv = target(
                    "split-window",
                    if matches!(direction, Direction::Left | Direction::Right) {
                        "-h"
                    } else {
                        "-v"
                    },
                    pid,
                    '%',
                );
                if matches!(direction, Direction::Left | Direction::Up) {
                    argv.push("-b".into());
                }
                argv
            }
            Action::ResizeSplit { direction, amount } => {
                let flag = match direction {
                    Direction::Left => "-L",
                    Direction::Right => "-R",
                    Direction::Up => "-U",
                    _ => "-D",
                };
                let mut argv = target("resize-pane", flag, pid, '%');
                argv.push(amount.to_string());
                argv
            }
            Action::EqualizeSplits => {
                let mut argv = target("select-layout", "", wid, '@');
                argv.push("tiled".into());
                argv
            }
            Action::ToggleSplitZoom => target("resize-pane", "-Z", pid, '%'),
            Action::CloseSurface => target("kill-pane", "", pid, '%'),
            Action::CloseTab => target("kill-window", "", wid, '@'),
            Action::NextTab | Action::PreviousTab | Action::LastTab | Action::GotoTab(_) => {
                let w = window?;
                let n = w.tabs.len();
                if n == 0 {
                    return Some(Err("no server windows".into()));
                }
                let i = match action {
                    Action::NextTab => (w.active_tab + 1) % n,
                    Action::PreviousTab => (w.active_tab + n - 1) % n,
                    Action::LastTab => n - 1,
                    Action::GotoTab(i) => i.saturating_sub(1).min(n - 1),
                    _ => unreachable!(),
                };
                let target_index = self.window_indices.get(&w.tabs[i].id)?;
                vec![
                    "select-window".into(),
                    "-t".into(),
                    format!("${}:{target_index}", sid?),
                ]
            }
            Action::Undo | Action::Redo | Action::MoveTab(_) | Action::OpenLayout => {
                return Some(Err(
                    "this action is unavailable for a server-owned workspace".into(),
                ));
            }
            _ => return None,
        };
        Some(Ok(command))
    }
}

/// Compute the server grid from actual leaf content areas, including per-pane
/// padding/headers and the server's one-cell split borders.
// shortcut: A shared grid can clip asymmetric panes after padding; exact per-pane fit must also update layout and divider placement.
pub fn grid_size(
    tab: &Tab,
    bounds: crate::workspace::Rect,
    peek: bool,
    cell: [f32; 2],
    padding: [f32; 2],
) -> (u16, u16) {
    fn inner(
        tree: &Tree,
        width: f32,
        height: f32,
        cell: [f32; 2],
        padding: [f32; 2],
    ) -> (u32, u32) {
        match &tree.kind {
            Node::Empty | Node::Pane(_) => (
                ((width - padding[0]) / cell[0]).floor().max(1.0) as u32,
                ((height - padding[1]) / cell[1]).floor().max(1.0) as u32,
            ),
            Node::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let ((aw, ah), (bw, bh)) = match axis {
                    Axis::Horizontal => (
                        inner(first, width * ratio, height, cell, padding),
                        inner(second, width * (1.0 - ratio), height, cell, padding),
                    ),
                    Axis::Vertical => (
                        inner(first, width, height * ratio, cell, padding),
                        inner(second, width, height * (1.0 - ratio), cell, padding),
                    ),
                };
                match axis {
                    Axis::Horizontal => (aw + 1 + bw, ah.min(bh)),
                    Axis::Vertical => (aw.min(bw), ah + 1 + bh),
                }
            }
        }
    }
    let overlay;
    let tree = if !peek
        && let Some(id) = tab
            .zoom
            .filter(|id| tab.overlays.iter().any(|overlay| overlay.pane == *id))
    {
        overlay = Tree::leaf(id);
        &overlay
    } else {
        tab.visible_tree(peek)
    };
    let (cols, rows) = inner(tree, bounds.width, bounds.height, cell, padding);
    (cols.clamp(1, 10000) as u16, rows.clamp(1, 10000) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustty::tmuxx::{ServerPane, ServerSession, ServerWindow};
    fn graph() -> Graph {
        let layout = Layout::pane(7, 80, 24).to_legacy().unwrap();
        Graph {
            sessions: vec![
                ServerSession {
                    id: 0,
                    name: "first".into(),
                    active: 4,
                    windows: [(4, 9)].into(),
                },
                ServerSession {
                    id: 1,
                    name: "linked".into(),
                    active: 3,
                    windows: [(3, 9)].into(),
                },
            ],
            windows: vec![ServerWindow {
                id: 9,
                name: "shell".into(),
                active: 7,
                cols: 80,
                rows: 24,
                panes: vec![7],
                layout,
                zoomed: false,
                zoomed_pane: None,
                visible_layout: String::new(),
            }],
            panes: vec![ServerPane {
                id: 7,
                window: 9,
                x: None,
                y: None,
                cols: 80,
                rows: 24,
                status: None,
                persistent: true,
                pid: None,
                modal: false,
                modal_capture_keys: false,
                modal_close_click: false,
                modal_close_cancel: false,
                terminal: serde_json::Value::Null.into(),
            }],
        }
    }
    fn tiled_graph(layout: &str) -> Graph {
        let mut graph = graph();
        let parsed = Layout::parse(layout).unwrap();
        let template = graph.panes[0].clone();
        graph.panes = parsed
            .panes()
            .into_iter()
            .map(|pane| {
                let id = pane.id.unwrap();
                let geometry = parsed.find(id).unwrap().geometry;
                let mut pane = template.clone();
                pane.id = u64::from(id);
                pane.x = Some(geometry.x);
                pane.y = Some(geometry.y);
                pane.cols = geometry.width as u16;
                pane.rows = geometry.height as u16;
                pane
            })
            .collect();
        graph.windows[0].layout = layout.into();
        graph.windows[0].panes = graph.panes.iter().map(|pane| pane.id).collect();
        graph.windows[0].cols = parsed.geometry.width as u16;
        graph.windows[0].rows = parsed.geometry.height as u16;
        graph
    }
    const FOUR_TILES: &str = r#"{"V":2,"L":{"t":"h","w":81,"h":49,"x":0,"y":0,"c":[{"t":"v","w":40,"h":49,"x":0,"y":0,"c":[{"t":"p","w":40,"h":24,"x":0,"y":0,"i":0,"I":"%7"},{"t":"p","w":40,"h":24,"x":0,"y":25,"i":1,"I":"%8"}]},{"t":"v","w":40,"h":49,"x":41,"y":0,"c":[{"t":"p","w":40,"h":24,"x":41,"y":0,"i":2,"I":"%9"},{"t":"p","w":40,"h":24,"x":41,"y":25,"i":3,"I":"%10"}]}]}}"#;

    fn pane_id(mapping: &Mapping, tab: &Tab, server: u64) -> Id {
        tab.pane_ids()
            .into_iter()
            .find(|pane| mapping.panes[pane] == server)
            .unwrap()
    }

    #[test]
    fn shared_selection_keeps_independent_local_focus_and_shared_active_tabs() {
        let mut graph = tiled_graph(FOUR_TILES);
        graph.sessions[0].windows.insert(8, 9);
        let mut mapping = Mapping {
            local_focus: true,
            ..Default::default()
        };
        let mut workspace = Workspace::default();
        mapping
            .apply(&mut workspace, &graph, &[0, 1].into())
            .unwrap();
        let mut choices = [8, 9, 10].into_iter();
        for tab in workspace.windows.iter_mut().flat_map(|w| &mut w.tabs) {
            assert_eq!(mapping.panes[&tab.focused], 7);
            tab.focus(pane_id(&mapping, tab, choices.next().unwrap()));
        }

        graph.windows[0].active = 10;
        graph.sessions[0].active = 8;
        mapping
            .apply(&mut workspace, &graph, &[0, 1].into())
            .unwrap();
        assert_eq!(workspace.windows[0].active_tab, 1);
        assert_eq!(
            workspace
                .windows
                .iter()
                .flat_map(|window| &window.tabs)
                .map(|tab| mapping.panes[&tab.focused])
                .collect::<Vec<_>>(),
            [8, 9, 10]
        );
        assert_eq!(mapping.server_focused.len(), 3);
        for tab in workspace.windows.iter().flat_map(|window| &window.tabs) {
            assert!(mapping.server_focused.contains(&pane_id(&mapping, tab, 10)));
        }
        let window = &workspace.windows[0];
        assert_eq!(
            mapping.focus_target(&workspace, window.id, window.tabs[1].focused),
            Some((0, 8, 9))
        );
        assert_eq!(
            mapping
                .action(&workspace, window.id, &Action::NewSplit(Direction::Right))
                .unwrap()
                .unwrap(),
            ["split-window", "-h", "-t", "%9"]
        );
    }

    #[test]
    fn remote_creation_does_not_steal_focus_but_new_tabs_follow_server_selection() {
        let mut workspace = Workspace::default();
        let mut mapping = Mapping {
            local_focus: true,
            ..Default::default()
        };
        mapping
            .apply(&mut workspace, &graph(), &[0].into())
            .unwrap();

        let mut expanded = tiled_graph(FOUR_TILES);
        expanded.windows[0].active = 10;
        expanded.sessions[0].windows.insert(8, 9);
        mapping
            .apply(&mut workspace, &expanded, &[0].into())
            .unwrap();
        let window = &workspace.windows[0];
        assert_eq!(mapping.panes[&window.tabs[0].focused], 7);
        assert_eq!(mapping.panes[&window.tabs[1].focused], 10);

        let tab = &mut workspace.windows[0].tabs[0];
        tab.focus(pane_id(&mapping, tab, 8));
        mapping
            .apply(&mut workspace, &graph(), &[0].into())
            .unwrap();
        let window = &workspace.windows[0];
        assert_eq!(window.tabs.len(), 1);
        assert_eq!(mapping.panes[&window.tabs[0].focused], 7);
        assert_eq!(mapping.server_focused, [window.tabs[0].focused].into());
    }

    #[test]
    fn local_focus_follows_shared_zoom_only_when_its_pane_becomes_hidden() {
        for with_visible_layout in [false, true] {
            let mut graph = tiled_graph(FOUR_TILES);
            for pane in &mut graph.panes {
                pane.x = None;
                pane.y = None;
            }
            let mut mapping = Mapping {
                local_focus: true,
                ..Default::default()
            };
            let mut workspace = Workspace::default();
            mapping.apply(&mut workspace, &graph, &[0].into()).unwrap();
            let tab = &mut workspace.windows[0].tabs[0];
            tab.focus(pane_id(&mapping, tab, 8));

            graph.windows[0].zoomed = true;
            graph.windows[0].zoomed_pane = Some(7);
            // The shared selection can differ from the zoomed pane.
            graph.windows[0].active = 9;
            if with_visible_layout {
                graph.windows[0].visible_layout = Layout::pane(7, 81, 49).to_legacy().unwrap();
            }
            mapping.apply(&mut workspace, &graph, &[0].into()).unwrap();
            let tab = &workspace.windows[0].tabs[0];
            assert_eq!(mapping.panes[&tab.focused], 7);
            assert_eq!(tab.zoom, Some(tab.focused));
            assert_eq!(tab.server_view.is_some(), with_visible_layout);
            assert_eq!(mapping.server_focused, [pane_id(&mapping, tab, 9)].into());
        }
    }

    #[test]
    fn legacy_mapping_still_follows_server_focus_without_secondary_markers() {
        let mut graph = tiled_graph(FOUR_TILES);
        let mut mapping = Mapping::default();
        let mut workspace = Workspace::default();
        mapping.apply(&mut workspace, &graph, &[0].into()).unwrap();
        let tab = &mut workspace.windows[0].tabs[0];
        tab.focus(pane_id(&mapping, tab, 8));
        graph.windows[0].active = 9;
        mapping.apply(&mut workspace, &graph, &[0].into()).unwrap();
        assert_eq!(mapping.panes[&workspace.windows[0].tabs[0].focused], 9);
        assert!(mapping.server_focused.is_empty());

        mapping.local_focus = true;
        graph.windows[0].active = 10;
        mapping.apply(&mut workspace, &graph, &[0].into()).unwrap();
        assert_eq!(mapping.panes[&workspace.windows[0].tabs[0].focused], 9);
        assert_eq!(mapping.server_focused.len(), 1);

        mapping.local_focus = false;
        mapping.apply(&mut workspace, &graph, &[0].into()).unwrap();
        assert_eq!(mapping.panes[&workspace.windows[0].tabs[0].focused], 10);
        assert!(mapping.server_focused.is_empty());
    }

    #[test]
    fn tiled_views_fill_native_rectangles_without_server_cell_gutters() {
        let cases: [(&str, &[_]); 4] = [
            (
                r#"{"V":2,"L":{"t":"h","w":81,"h":24,"x":0,"y":0,"c":[{"t":"p","w":40,"h":24,"x":0,"y":0,"i":0,"I":"%7"},{"t":"p","w":40,"h":24,"x":41,"y":0,"i":1,"I":"%8"}]}}"#,
                &[(7, 8, Axis::Horizontal)],
            ),
            (
                r#"{"V":2,"L":{"t":"v","w":80,"h":49,"x":0,"y":0,"c":[{"t":"p","w":80,"h":24,"x":0,"y":0,"i":0,"I":"%7"},{"t":"p","w":80,"h":24,"x":0,"y":25,"i":1,"I":"%8"}]}}"#,
                &[(7, 8, Axis::Vertical)],
            ),
            (
                FOUR_TILES,
                &[
                    (7, 8, Axis::Vertical),
                    (9, 10, Axis::Vertical),
                    (7, 9, Axis::Horizontal),
                    (8, 10, Axis::Horizontal),
                ],
            ),
            (
                r#"{"V":2,"L":{"t":"h","w":96,"h":24,"x":0,"y":0,"c":[{"t":"p","w":67,"h":24,"x":0,"y":0,"i":0,"I":"%7"},{"t":"p","w":28,"h":24,"x":68,"y":0,"i":1,"I":"%8"}]}}"#,
                &[(7, 8, Axis::Horizontal)],
            ),
        ];
        let bounds = crate::workspace::Rect {
            x: 13.0,
            y: 17.0,
            width: 1000.0,
            height: 490.0,
        };
        for (layout, neighbors) in cases {
            let mut mapping = Mapping::default();
            let mut workspace = Workspace::default();
            mapping
                .apply(&mut workspace, &tiled_graph(layout), &[0].into())
                .unwrap();
            let rects: BTreeMap<_, _> = workspace.windows[0].tabs[0]
                .layout(bounds, false, 10.0)
                .into_iter()
                .map(|(id, rect)| (mapping.panes[&id], rect))
                .collect();
            let area: f32 = rects.values().map(|rect| rect.width * rect.height).sum();
            assert!((area - bounds.width * bounds.height).abs() < 0.01);
            for &(first, second, axis) in neighbors {
                let a = rects[&first];
                let b = rects[&second];
                let gap = match axis {
                    Axis::Horizontal => b.x - (a.x + a.width),
                    Axis::Vertical => b.y - (a.y + a.height),
                };
                assert!(gap.abs() < 0.001, "server cell left a {gap}-unit gutter");
                let inner = |rect: crate::workspace::Rect| {
                    egui::Rect::from_min_size(
                        egui::pos2(rect.x, rect.y),
                        egui::vec2(rect.width, rect.height),
                    )
                    .shrink(1.0)
                };
                let gap = match axis {
                    Axis::Horizontal => inner(b).left() - inner(a).right(),
                    Axis::Vertical => inner(b).top() - inner(a).bottom(),
                };
                assert!((gap - 2.0).abs() < 0.001);
            }
        }
    }

    #[test]
    fn view_grid_and_status_rows_fit_real_cells_after_headers_padding_and_frames() {
        let cell = [8.0, 10.0];
        // Horizontal padding 12; vertical padding 8 + header 20; frame 2 on both axes.
        let padding = [14.0, 30.0];
        for (width, height, cols, rows) in [(654.0, 270.0, 80, 24), (652.0, 268.0, 79, 23)] {
            for (top, bottom) in [(0, 0), (1, 0), (0, 1)] {
                let mut graph = graph();
                graph.windows[0].cols = cols;
                graph.windows[0].rows = rows;
                graph.windows[0].layout = Layout::pane(7, u32::from(cols), u32::from(rows))
                    .to_legacy()
                    .unwrap();
                graph.panes[0].x = Some(0);
                graph.panes[0].y = Some(top);
                graph.panes[0].cols = cols;
                graph.panes[0].rows = rows - (top + bottom) as u16;
                let mut mapping = Mapping::default();
                let mut workspace = Workspace::default();
                mapping.apply(&mut workspace, &graph, &[0].into()).unwrap();
                let tab = &workspace.windows[0].tabs[0];
                let bounds = crate::workspace::Rect {
                    x: 0.0,
                    y: 0.0,
                    width,
                    height,
                };
                assert_eq!(grid_size(tab, bounds, false, cell, padding), (cols, rows));
                let rect = tab.layout(bounds, false, cell[1])[0].1;
                assert_eq!(rect.y, top as f32 * cell[1]);
                assert_eq!(((rect.width - padding[0]) / cell[0]).floor() as u16, cols);
                assert_eq!(
                    ((rect.height - padding[1]) / cell[1]).floor() as u16,
                    graph.panes[0].rows
                );
                if top + bottom != 0 {
                    let tiny = tab.layout(
                        crate::workspace::Rect {
                            height: 5.0,
                            ..bounds
                        },
                        false,
                        cell[1],
                    )[0]
                    .1;
                    assert_eq!(tiny.height, 0.0);
                }
            }
        }
    }

    #[test]
    fn zoom_sizes_use_visible_content_while_peek_keeps_the_full_tree() {
        let mut graph = tiled_graph(FOUR_TILES);
        graph.windows[0].zoomed = true;
        graph.windows[0].zoomed_pane = Some(7);
        graph.windows[0].visible_layout = Layout::pane(7, 81, 49).to_legacy().unwrap();
        graph.panes[0].cols = 81;
        graph.panes[0].rows = 49;
        let mut mapping = Mapping::default();
        let mut workspace = Workspace::default();
        mapping.apply(&mut workspace, &graph, &[0].into()).unwrap();
        let tab = &workspace.windows[0].tabs[0];
        let bounds = crate::workspace::Rect {
            x: 0.0,
            y: 0.0,
            width: 654.0,
            height: 270.0,
        };
        assert_eq!(
            grid_size(tab, bounds, false, [8.0, 10.0], [14.0, 30.0]),
            (80, 24)
        );
        assert_eq!(
            grid_size(tab, bounds, true, [8.0, 10.0], [14.0, 30.0]),
            (79, 21)
        );
        let visible = tab.layout(bounds, false, 10.0);
        assert_eq!(visible.len(), 1);
        assert_eq!([visible[0].1.width, visible[0].1.height], [654.0, 270.0]);
    }
    #[test]
    fn linked_windows_get_independent_views_and_commands_use_server_ids() {
        let mut workspace = Workspace::default();
        let mut mapping = Mapping::default();
        mapping
            .apply(&mut workspace, &graph(), &[0, 1].into())
            .unwrap();
        assert_eq!(workspace.windows.len(), 2);
        let first = &workspace.windows[0];
        let second = &workspace.windows[1];
        assert_ne!(first.tabs[0].focused, second.tabs[0].focused);
        assert_eq!(mapping.panes[&first.tabs[0].focused], 7);
        assert_eq!(mapping.panes[&second.tabs[0].focused], 7);
        assert_eq!(mapping.session_names[&first.id], "first");
        assert_eq!(mapping.session_names[&second.id], "linked");
        assert_eq!(
            mapping
                .action(&workspace, first.id, &Action::NewSplit(Direction::Left))
                .unwrap()
                .unwrap(),
            ["split-window", "-h", "-t", "%7", "-b"]
        );
        let ids = first.tabs[0].root.panes();
        let (first_id, second_id) = (first.id, second.id);
        let mut renamed = graph();
        renamed.sessions[0].name = "renamed".into();
        mapping
            .apply(&mut workspace, &renamed, &[0, 1].into())
            .unwrap();
        assert_eq!(workspace.windows[0].tabs[0].root.panes(), ids);
        assert_eq!(mapping.session_names[&first_id], "renamed");
        assert_eq!(mapping.session_names[&second_id], "linked");
    }
    #[test]
    fn duplicate_window_links_in_one_session_have_distinct_views_and_targets() {
        let mut workspace = Workspace::default();
        let mut mapping = Mapping::default();
        let mut graph = graph();
        graph.sessions[0].windows.insert(8, 9);
        graph.sessions[0].active = 8;
        mapping
            .apply(&mut workspace, &graph, &[0, 1].into())
            .unwrap();
        let window = &workspace.windows[0];
        assert_eq!(window.tabs.len(), 2);
        assert_eq!(window.active_tab, 1);
        assert_ne!(window.tabs[0].id, window.tabs[1].id);
        assert_ne!(window.tabs[0].focused, window.tabs[1].focused);
        assert_eq!(mapping.panes[&window.tabs[0].focused], 7);
        assert_eq!(mapping.panes[&window.tabs[1].focused], 7);
        assert_eq!(
            mapping.focus_target(&workspace, window.id, window.tabs[1].focused),
            Some((0, 8, 7))
        );
        assert_eq!(
            mapping.focus_target(&workspace, window.id, window.tabs[0].focused),
            None
        );
        assert_eq!(
            mapping
                .action(&workspace, window.id, &Action::GotoTab(2))
                .unwrap()
                .unwrap(),
            ["select-window", "-t", "$0:8"]
        );
    }

    #[test]
    fn floating_panes_keep_signed_geometry_front_order_modal_focus_and_independent_views() {
        let mut graph = graph();
        let mut workspace = Workspace::default();
        let mut mapping = Mapping {
            local_focus: true,
            ..Default::default()
        };
        mapping
            .apply(&mut workspace, &graph, &[0, 1].into())
            .unwrap();
        graph.windows[0].layout = r#"{"V":2,"L":{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":80,"h":24,"x":0,"y":0,"i":0,"I":"%7"},{"t":"p","w":20,"h":5,"x":-2,"y":3,"i":1,"I":"%42","z":0},{"t":"p","w":30,"h":8,"x":2,"y":4,"i":2,"I":"%43","z":1}]}}"#.into();
        for (id, modal) in [(42, true), (43, false)] {
            let mut pane = graph.panes[0].clone();
            pane.id = id;
            pane.modal = modal;
            graph.panes.push(pane);
            graph.windows[0].panes.push(id);
        }
        graph.windows[0].active = 42;
        mapping
            .apply(&mut workspace, &graph, &[0, 1].into())
            .unwrap();
        workspace.validate().unwrap();
        let tab = &workspace.windows[0].tabs[0];
        let front = tab.focused;
        assert_eq!(mapping.panes[&front], 42);
        assert_ne!(front, workspace.windows[1].tabs[0].focused);
        assert_eq!(tab.root.panes().len(), 1);
        assert_eq!(tab.pane_ids().len(), 3);
        let layout = tab.layout(
            crate::workspace::Rect {
                x: 0.0,
                y: 24.0,
                width: 800.0,
                height: 240.0,
            },
            false,
            10.0,
        );
        assert_eq!(
            layout
                .iter()
                .map(|(id, _)| mapping.panes[id])
                .collect::<Vec<_>>(),
            [7, 43, 42]
        );
        let (_, rect) = layout.last().unwrap();
        assert_eq!(
            [rect.x, rect.y, rect.width, rect.height],
            [-20.0, 54.0, 200.0, 50.0]
        );
        assert_eq!(tab.modal_pane(), Some(front));
        let tile = tab.root.panes()[0];
        let tab = &mut workspace.windows[0].tabs[0];
        tab.focus(tile);
        assert_eq!(tab.focused, front);
        let rows = crate::outliner::rows(
            &workspace.windows[0],
            &Default::default(),
            &Default::default(),
        );
        assert!(rows.iter().any(
            |row| matches!(row.target, crate::outliner::Target::Pane { pane, .. } if pane == front)
        ));
        graph.windows[0].panes.retain(|id| *id != 7);
        graph.panes.retain(|pane| pane.id != 7);
        graph.windows[0].layout = graph.windows[0]
            .layout
            .replace(r#"{"t":"p","w":80,"h":24,"x":0,"y":0,"i":0,"I":"%7"},"#, "");
        mapping
            .apply(&mut workspace, &graph, &[0, 1].into())
            .unwrap();
        let tab = &workspace.windows[0].tabs[0];
        assert!(tab.root.panes().is_empty());
        assert_eq!(
            tab.layout(crate::workspace::Rect::UNIT, false, 0.0).len(),
            2
        );
        workspace.validate().unwrap();
    }
    #[test]
    fn zoomed_server_view_keeps_overlay_geometry_and_hidden_membership() {
        let mut graph = graph();
        graph.windows[0].layout = r#"{"V":2,"L":{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":39,"h":24,"x":0,"y":0,"i":0,"I":"%7"},{"t":"p","w":40,"h":24,"x":40,"y":0,"i":1,"I":"%43"},{"t":"p","w":20,"h":5,"x":-2,"y":3,"i":2,"I":"%42","z":0}]}}"#.into();
        graph.windows[0].visible_layout = r#"{"V":2,"L":{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":80,"h":24,"x":0,"y":0,"i":0,"I":"%7"},{"t":"p","w":20,"h":5,"x":-2,"y":3,"i":2,"I":"%42","z":0}]}}"#.into();
        for id in [42, 43] {
            let mut pane = graph.panes[0].clone();
            pane.id = id;
            graph.panes.push(pane);
            graph.windows[0].panes.push(id);
        }
        graph.windows[0].active = 42;
        graph.windows[0].zoomed = true;
        graph.windows[0].zoomed_pane = Some(7);
        graph.panes[1].modal_capture_keys = true;
        graph.panes[1].modal_close_click = true;
        graph.panes[1].modal_close_cancel = true;
        let mut workspace = Workspace::default();
        let mut mapping = Mapping::default();
        mapping
            .apply(&mut workspace, &graph, &[0, 1].into())
            .unwrap();
        let tab = &mut workspace.windows[0].tabs[0];
        let zoom = tab.zoom.unwrap();
        assert_eq!(mapping.panes[&zoom], 7);
        assert_eq!(tab.pane_ids().len(), 3);
        let visible = tab.layout(
            crate::workspace::Rect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 240.0,
            },
            false,
            10.0,
        );
        assert_eq!(
            visible
                .iter()
                .map(|(id, _)| mapping.panes[id])
                .collect::<Vec<_>>(),
            [7, 42]
        );
        assert_eq!([visible[0].1.width, visible[0].1.height], [800.0, 240.0]);
        assert_eq!(
            [
                visible[1].1.x,
                visible[1].1.y,
                visible[1].1.width,
                visible[1].1.height
            ],
            [-20.0, 30.0, 200.0, 50.0]
        );
        assert!(
            tab.overlays[0].capture_keys
                && tab.overlays[0].close_click
                && tab.overlays[0].close_cancel
        );
        tab.focus(tab.focused);
        assert_eq!(tab.zoom, Some(zoom));
        workspace.validate().unwrap();

        // Stock tmux omits zoomed_pane while the floating pane has focus.
        graph.windows[0].zoomed_pane = None;
        mapping
            .apply(&mut workspace, &graph, &[0, 1].into())
            .unwrap();
        let tab = &workspace.windows[0].tabs[0];
        assert_eq!(mapping.panes[&tab.zoom.unwrap()], 7);
        let bounds = crate::workspace::Rect {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 240.0,
        };
        let visible = tab.layout(bounds, false, 10.0);
        assert_eq!([visible[0].1.width, visible[0].1.height], [800.0, 240.0]);

        // A floating zoom target uses the full view, including its status-row reservation.
        graph.windows[0].zoomed_pane = Some(42);
        graph.windows[0].visible_layout = Layout::pane(42, 80, 24).to_legacy().unwrap();
        graph.panes[1].x = Some(0);
        graph.panes[1].y = Some(1);
        graph.panes[1].rows = 23;
        mapping
            .apply(&mut workspace, &graph, &[0, 1].into())
            .unwrap();
        let tab = &workspace.windows[0].tabs[0];
        let visible = tab.layout(bounds, false, 10.0);
        assert_eq!(visible.len(), 1);
        assert_eq!(mapping.panes[&visible[0].0], 42);
        assert_eq!(
            [
                visible[0].1.x,
                visible[0].1.y,
                visible[0].1.width,
                visible[0].1.height
            ],
            [0.0, 10.0, 800.0, 230.0]
        );
        assert_eq!(
            grid_size(tab, bounds, false, [8.0, 10.0], [14.0, 30.0]),
            (98, 21)
        );
        let before = serde_json::to_value(&workspace).unwrap();
        graph.windows[0].visible_layout = Layout::pane(999, 80, 24).to_legacy().unwrap();
        assert!(
            mapping
                .apply(&mut workspace, &graph, &[0, 1].into())
                .is_err()
        );
        assert_eq!(serde_json::to_value(&workspace).unwrap(), before);
    }

    #[test]
    fn server_content_rect_reserves_border_status_without_dropping_full_layout() {
        let mut graph = graph();
        graph.panes[0].x = Some(0);
        graph.panes[0].y = Some(1);
        graph.panes[0].rows = 23;
        let mut workspace = Workspace::default();
        let mut mapping = Mapping::default();
        mapping
            .apply(&mut workspace, &graph, &[0, 1].into())
            .unwrap();
        let tab = &workspace.windows[0].tabs[0];
        assert!(tab.root.contains(tab.focused));
        let rect = tab.layout(
            crate::workspace::Rect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 240.0,
            },
            false,
            10.0,
        )[0]
        .1;
        assert_eq!(
            [rect.x, rect.y, rect.width, rect.height],
            [0.0, 10.0, 800.0, 230.0]
        );
        graph.windows[0].zoomed = true;
        graph.windows[0].zoomed_pane = Some(7);
        graph.windows[0].visible_layout = graph.windows[0].layout.clone();
        mapping
            .apply(&mut workspace, &graph, &[0, 1].into())
            .unwrap();
        let rect = workspace.windows[0].tabs[0].layout(
            crate::workspace::Rect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 240.0,
            },
            false,
            10.0,
        )[0]
        .1;
        assert_eq!([rect.y, rect.height], [10.0, 230.0]);
    }

    #[test]
    fn malformed_graph_does_not_replace_live_workspace() {
        let mut workspace = Workspace::default();
        let mut mapping = Mapping {
            local_focus: true,
            ..Default::default()
        };
        let mut graph = tiled_graph(FOUR_TILES);
        mapping
            .apply(&mut workspace, &graph, &[0, 1].into())
            .unwrap();
        let before = serde_json::to_value(&workspace).unwrap();
        let markers = mapping.server_focused.clone();
        graph.windows[0].layout = "invalid".into();
        assert!(
            mapping
                .apply(&mut workspace, &graph, &[0, 1].into())
                .is_err()
        );
        assert_eq!(serde_json::to_value(&workspace).unwrap(), before);
        assert_eq!(mapping.server_focused, markers);

        graph.windows[0].layout = FOUR_TILES.into();
        graph.windows[0].active = 10;
        graph.sessions[1].windows.insert(8, 99);
        assert!(
            mapping
                .apply(&mut workspace, &graph, &[0, 1].into())
                .is_err()
        );
        assert_eq!(serde_json::to_value(&workspace).unwrap(), before);
        assert_eq!(mapping.server_focused, markers);
    }

    #[test]
    fn selected_sessions_preserve_local_and_other_connections_without_reattaching() {
        let mut workspace = Workspace::default();
        let local = WindowState {
            id: workspace.id(),
            tabs: vec![Tab::new(workspace.id(), workspace.id(), "/tmp".into())],
            active_tab: 0,
            frame: [10.0, 20.0, 800.0, 600.0],
            quick: false,
        };
        workspace.windows.push(local.clone());
        let mut first = Mapping::default();
        let mut second = Mapping::default();
        let mut graph = graph();
        first.apply(&mut workspace, &graph, &[0].into()).unwrap();
        let first_id = *first.sessions.keys().next().unwrap();
        second.apply(&mut workspace, &graph, &[0].into()).unwrap();
        let second_id = *second.sessions.keys().next().unwrap();
        assert_ne!(first_id, second_id);
        assert!(first.panes.keys().all(|id| !second.panes.contains_key(id)));

        let order: Vec<_> = workspace.windows.iter().map(|w| w.id).collect();
        let mut unselected = graph.sessions[0].clone();
        unselected.id = 2;
        graph.sessions.push(unselected);
        first.apply(&mut workspace, &graph, &[0].into()).unwrap();
        assert_eq!(
            workspace.windows.iter().map(|w| w.id).collect::<Vec<_>>(),
            order
        );
        assert_eq!(first.sessions.len(), 1);

        for _ in 0..2 {
            first
                .apply(&mut workspace, &graph, &BTreeSet::new())
                .unwrap();
            assert!(first.sessions.is_empty());
            assert_eq!(
                workspace.windows.iter().map(|w| w.id).collect::<Vec<_>>(),
                [local.id, second_id]
            );
        }
        first.apply(&mut workspace, &graph, &[0].into()).unwrap();
        assert_eq!(*first.sessions.keys().next().unwrap(), first_id);
        assert_eq!(
            serde_json::to_value(&workspace.windows[0]).unwrap(),
            serde_json::to_value(local).unwrap()
        );
        workspace.validate().unwrap();
    }

    #[test]
    fn pane_commands_target_the_server_and_window_lifecycle_stays_with_the_host() {
        let mut workspace = Workspace::default();
        let mut mapping = Mapping::default();
        mapping
            .apply(&mut workspace, &graph(), &[0].into())
            .unwrap();
        let host = workspace.windows[0].id;
        for (action, command) in [
            (Action::NewTab, vec!["new-window", "-t", "$0"]),
            (Action::NewFloatingPane, vec!["new-pane", "-t", "%7"]),
            (Action::CloseSurface, vec!["kill-pane", "-t", "%7"]),
            (Action::CloseTab, vec!["kill-window", "-t", "@9"]),
        ] {
            assert_eq!(
                mapping.action(&workspace, host, &action).unwrap().unwrap(),
                command
            );
            assert!(mapping.action(&workspace, u64::MAX, &action).is_none());
        }
        assert!(
            mapping
                .action(&workspace, host, &Action::NewWindow)
                .is_none()
        );
        assert!(
            mapping
                .action(&workspace, host, &Action::CloseWindow)
                .is_none()
        );
    }
}
