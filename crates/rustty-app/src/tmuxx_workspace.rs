//! Server IDs are mapped to independent native views, including linked windows.
use crate::workspace::{
    Axis, Id, Node, Overlay, PaneRect, SavedPane, Tab, Tree, WindowState, Workspace,
};
use rustty::config::{Action, Direction};
use rustty::tmuxx::{Graph, Kind, Layout};
use std::collections::BTreeMap;

#[derive(Clone, Default)]
pub struct Mapping {
    ids: BTreeMap<String, Id>,
    pub panes: BTreeMap<Id, u64>,
    pub sessions: BTreeMap<Id, u64>,
    pub windows: BTreeMap<Id, u64>,
    window_indices: BTreeMap<Id, u32>,
    pub sizes: BTreeMap<u64, (u16, u16)>,
    pub dividers: BTreeMap<Id, (u64, Axis, u32)>,
}
impl Mapping {
    fn id(&mut self, workspace: &mut Workspace, key: String) -> Id {
        *self.ids.entry(key).or_insert_with(|| workspace.id())
    }
    pub fn apply(&mut self, workspace: &mut Workspace, graph: &Graph) -> Result<(), String> {
        let mut next = self.clone();
        let mut next_workspace = workspace.clone();
        next.apply_inner(&mut next_workspace, graph)?;
        next_workspace
            .validate()
            .map_err(|error| error.to_string())?;
        *self = next;
        *workspace = next_workspace;
        Ok(())
    }
    fn apply_inner(&mut self, workspace: &mut Workspace, graph: &Graph) -> Result<(), String> {
        let mut layouts = BTreeMap::new();
        for window in &graph.windows {
            let layout = rustty::tmuxx::Layout::parse(&window.layout).map_err(|e| e.to_string())?;
            layouts.insert(window.id, layout);
        }
        let previous = workspace.windows.clone();
        let mut windows = Vec::new();
        self.panes.clear();
        self.sessions.clear();
        self.windows.clear();
        self.window_indices.clear();
        self.dividers.clear();
        for session in &graph.sessions {
            let id = self.id(workspace, format!("session:{}", session.id));
            self.sessions.insert(id, session.id);
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
                tab.focused = focused;
                tab.title = Some(window.name.clone());
                tab.zoom = window.zoomed.then(|| {
                    self.id(
                        workspace,
                        format!("pane:{key}:{}", window.zoomed_pane.unwrap_or(window.active)),
                    )
                });
                let visible = if window.zoomed && !window.visible_layout.is_empty() {
                    Layout::parse(&window.visible_layout).map_err(|e| e.to_string())?
                } else {
                    layout.clone()
                };
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
        workspace.windows = windows;
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
                out.push((
                    pane.z,
                    PaneRect {
                        pane: self.id(workspace, format!("pane:{key}:{pid}")),
                        x,
                        y,
                        cols,
                        rows,
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
        let sid = self.sessions.get(&host).copied();
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
            Action::NewWindow => vec!["new-session".into(), "-d".into()],
            Action::NewTab => target("new-window", "", sid, '$'),
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
            Action::CloseWindow => target("kill-session", "", sid, '$'),
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
            Action::Undo
            | Action::Redo
            | Action::MoveTab(_)
            | Action::OpenLayout
            | Action::CloseAllWindows => {
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
pub fn grid_size(
    tree: &Tree,
    bounds: crate::workspace::Rect,
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
                terminal: serde_json::Value::Null,
            }],
        }
    }
    #[test]
    fn linked_windows_get_independent_views_and_commands_use_server_ids() {
        let mut workspace = Workspace::default();
        let mut mapping = Mapping::default();
        mapping.apply(&mut workspace, &graph()).unwrap();
        assert_eq!(workspace.windows.len(), 2);
        let first = &workspace.windows[0];
        let second = &workspace.windows[1];
        assert_ne!(first.tabs[0].focused, second.tabs[0].focused);
        assert_eq!(mapping.panes[&first.tabs[0].focused], 7);
        assert_eq!(mapping.panes[&second.tabs[0].focused], 7);
        assert_eq!(
            mapping
                .action(&workspace, first.id, &Action::NewSplit(Direction::Left))
                .unwrap()
                .unwrap(),
            ["split-window", "-h", "-t", "%7", "-b"]
        );
        let ids = first.tabs[0].root.panes();
        mapping.apply(&mut workspace, &graph()).unwrap();
        assert_eq!(workspace.windows[0].tabs[0].root.panes(), ids);
    }
    #[test]
    fn duplicate_window_links_in_one_session_have_distinct_views_and_targets() {
        let mut workspace = Workspace::default();
        let mut mapping = Mapping::default();
        let mut graph = graph();
        graph.sessions[0].windows.insert(8, 9);
        graph.sessions[0].active = 8;
        mapping.apply(&mut workspace, &graph).unwrap();
        let window = &workspace.windows[0];
        assert_eq!(window.tabs.len(), 2);
        assert_eq!(window.active_tab, 1);
        assert_ne!(window.tabs[0].id, window.tabs[1].id);
        assert_ne!(window.tabs[0].focused, window.tabs[1].focused);
        assert_eq!(mapping.panes[&window.tabs[0].focused], 7);
        assert_eq!(mapping.panes[&window.tabs[1].focused], 7);
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
        graph.windows[0].layout = r#"{"V":2,"L":{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":80,"h":24,"x":0,"y":0,"i":0,"I":"%7"},{"t":"p","w":20,"h":5,"x":-2,"y":3,"i":1,"I":"%42","z":0},{"t":"p","w":30,"h":8,"x":2,"y":4,"i":2,"I":"%43","z":1}]}}"#.into();
        for (id, modal) in [(42, true), (43, false)] {
            let mut pane = graph.panes[0].clone();
            pane.id = id;
            pane.modal = modal;
            graph.panes.push(pane);
            graph.windows[0].panes.push(id);
        }
        graph.windows[0].active = 42;
        let mut workspace = Workspace::default();
        let mut mapping = Mapping::default();
        mapping.apply(&mut workspace, &graph).unwrap();
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
        mapping.apply(&mut workspace, &graph).unwrap();
        let tab = &workspace.windows[0].tabs[0];
        assert!(tab.root.panes().is_empty());
        assert_eq!(tab.layout(crate::workspace::Rect::UNIT, false).len(), 2);
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
        mapping.apply(&mut workspace, &graph).unwrap();
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
        let before = serde_json::to_value(&workspace).unwrap();
        graph.windows[0].visible_layout = graph.windows[0].visible_layout.replace("%42", "%999");
        assert!(mapping.apply(&mut workspace, &graph).is_err());
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
        mapping.apply(&mut workspace, &graph).unwrap();
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
        )[0]
        .1;
        assert_eq!(
            [rect.x, rect.y, rect.width, rect.height],
            [0.0, 10.0, 800.0, 230.0]
        );
        graph.windows[0].zoomed = true;
        graph.windows[0].zoomed_pane = Some(7);
        graph.windows[0].visible_layout = graph.windows[0].layout.clone();
        mapping.apply(&mut workspace, &graph).unwrap();
        let rect = workspace.windows[0].tabs[0].layout(
            crate::workspace::Rect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 240.0,
            },
            false,
        )[0]
        .1;
        assert_eq!([rect.y, rect.height], [10.0, 230.0]);
    }

    #[test]
    fn malformed_graph_does_not_replace_live_workspace() {
        let mut workspace = Workspace::default();
        let mut mapping = Mapping::default();
        let mut graph = graph();
        mapping.apply(&mut workspace, &graph).unwrap();
        let before = serde_json::to_value(&workspace).unwrap();
        graph.windows[0].layout = "invalid".into();
        assert!(mapping.apply(&mut workspace, &graph).is_err());
        assert_eq!(serde_json::to_value(&workspace).unwrap(), before);
    }
}
