//! Rustty's retained native zgui window, including its chrome and transient surfaces.

#[path = "zgui_ui/model.rs"]
mod model;
pub use model::*;

use std::{
    cell::RefCell,
    collections::HashMap,
    hash::Hash,
    rc::{Rc, Weak},
};

use rustty::config::Action;
use rustty_app::{outliner::Target, workspace::Id};
use zgui::prelude::*;
use zgui::reactive::{ArcRwSignal, LocalStorage, RenderEffect, StoredValue, UnsyncCallback};
use zgui_ui::MenuItemProps as NativeMenuItemProps;
use zgui_ui::prelude::*;
use zgui_ui_tokens::prelude::{ColorScheme, ThemeProviderProps};

type Cell<T> = RwSignal<T, LocalStorage>;
pub type Bounds = zgui::geom::Rect<zgui::geom::DevicePx, zgui::geom::Device>;

pub const STYLESHEET: &str = css!(
    ":root { font-family: sans-serif; font-size: 13px; color: var(--zui-color-foreground); background: transparent; }
     .rustty-window { width: 100%; height: 100%; min-width: 0; min-height: 0; }
     .rustty-header { flex-shrink: 0; min-height: 42px; gap: 6px; padding: 4px 8px; align-items: center; border-bottom: 1px solid var(--zui-color-border); background: var(--zui-color-background); }
     .rustty-tabs { flex: 1; min-width: 0; }
     .rustty-tab-list { justify-content: flex-start; overflow-x: auto; overflow-y: hidden; }
     .rustty-tab { gap: 5px; border-bottom: 2px solid transparent; }
     .rustty-tab-name { max-width: 220px; overflow: hidden; white-space: nowrap; }
     .rustty-tab-activity { color: var(--zui-color-primary); }
     .rustty-body { flex: 1; min-height: 0; width: 100%; }
     .rustty-page { flex: 1; min-width: 0; min-height: 0; }
     .rustty-breadcrumb { padding: 4px 10px; flex-shrink: 0; min-height: 26px; background: var(--zui-color-background); }
     .rustty-workspace { flex: 1; min-width: 0; min-height: 0; position: relative; overflow: hidden; }
     .rustty-pane { position: absolute; overflow: hidden; border: 1px solid var(--zui-color-border); background: transparent; }
     .rustty-pane-focused { border-color: var(--zui-color-ring); }
     .rustty-pane-header { height: 26px; flex-shrink: 0; align-items: center; padding: 0 7px; gap: 6px; background: var(--zui-color-muted); }
     .rustty-pane-title { flex: 1; white-space: nowrap; overflow: hidden; }
     .rustty-terminal { flex: 1; min-width: 0; min-height: 0; }
     .rustty-pane-content { flex: 1; min-width: 0; min-height: 0; position: relative; }
     .rustty-pane-status { position: absolute; bottom: 0; left: 0; right: 0; }
     .rustty-pane-hint { position: absolute; top: 32px; left: 8px; }
     .rustty-link-hint { position: absolute; padding: 0 2px; }
     .rustty-outliner-row { align-items: center; gap: 2px; }
     .rustty-outliner-title { flex: 1; min-width: 0; }
     .rustty-detail { color: var(--zui-color-muted-foreground); font-size: 11px; }
     .rustty-search { padding: 6px 10px; gap: 6px; align-items: center; border-bottom: 1px solid var(--zui-color-border); }
     .rustty-search-input { flex: 1; }
     .rustty-check { align-items: center; gap: 8px; }
     .rustty-fields { gap: 12px; }
     .rustty-messages { padding: 6px 10px; gap: 4px; background: var(--zui-color-muted); }
     .rustty-message-text { flex: 1; }
     .rustty-preview { max-height: 260px; overflow: auto; gap: 6px; }
     .rustty-preview-text { white-space: pre-wrap; font-family: monospace; }
     .rustty-preview-image { max-width: 100%; max-height: 220px; object-fit: contain; }
     .rustty-dialog { max-width: 680px; }
     .rustty-sidebar-heading { align-items: center; justify-content: space-between; }
     .rustty-close { padding: 2px 5px; min-width: 20px; height: 20px; }
    "
);

/// Changing an item writes only that item's signal. Reordering writes only the keyed collection.
/// Reference-counted values allow controller updates outside a reactive owner without leaks.
#[derive(Clone)]
struct RetainedList<K, T> {
    cells: Rc<RefCell<HashMap<K, Weak<ArcRwSignal<T>>>>>,
    order: Cell<Vec<(K, Rc<ArcRwSignal<T>>)>>,
}

impl<K: Clone + Eq + Hash + 'static, T: Clone + PartialEq + 'static> RetainedList<K, T> {
    fn new() -> Self {
        Self {
            cells: Default::default(),
            order: RwSignal::new_local(Vec::new()),
        }
    }

    fn update(&self, values: Vec<T>, key: impl Fn(&T) -> K) {
        let mut cells = self.cells.borrow_mut();
        let mut order = Vec::with_capacity(values.len());
        for value in values {
            let id = key(&value);
            let cell = cells.get(&id).and_then(Weak::upgrade).unwrap_or_else(|| {
                let cell = Rc::new(ArcRwSignal::new(value.clone()));
                cells.insert(id.clone(), Rc::downgrade(&cell));
                cell
            });
            if cell.with_untracked(|old| old != &value) {
                cell.set(value);
            }
            order.push((id, cell));
        }
        let unchanged = self.order.with_untracked(|old| {
            old.len() == order.len() && old.iter().zip(&order).all(|(old, new)| old.0 == new.0)
        });
        if !unchanged {
            self.order.set(order);
        }
        cells.retain(|_, cell| cell.strong_count() > 0);
    }
}

fn mounted_cell<T: 'static>(cell: Rc<ArcRwSignal<T>>) -> Cell<T> {
    // Keep identity until keyed unmount, including removal and reinsertion before a flush.
    let signal = RwSignal::from_local((*cell).clone());
    on_cleanup_local(move || drop(cell));
    signal
}

fn set_changed<T: PartialEq + 'static>(cell: Cell<T>, value: T) {
    if cell.with_untracked(|old| old != &value) {
        cell.set(value);
    }
}

fn memo_field<T: 'static, V: Clone + PartialEq + Send + Sync + 'static>(
    cell: Cell<T>,
    field: impl Fn(&T) -> &V + Send + Sync + 'static,
) -> Memo<V> {
    // Geometry-only writes must not clone menus or wake metadata bindings.
    Memo::new_owning(move |previous: Option<V>| {
        cell.with(|model| {
            let value = field(model);
            if previous.as_ref() == Some(value) {
                (previous.expect("compared an existing value"), false)
            } else {
                (value.clone(), true)
            }
        })
    })
}

#[derive(Clone, Copy)]
struct Sink(
    StoredValue<Rc<RefCell<Vec<Command>>>, LocalStorage>,
    Cell<usize>,
);

impl Sink {
    fn send(self, command: Command) {
        self.0.with_value(|queue| queue.borrow_mut().push(command));
    }

    fn popup(self, open: bool) {
        self.1.update(|count| {
            *count = if open {
                *count + 1
            } else {
                count.saturating_sub(1)
            }
        });
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaneModel {
    pub id: Id,
    /// Fractions of the native workspace, in x/y/width/height order.
    pub rect: [f32; 4],
    pub label: String,
    pub focused: bool,
    pub opacity: f32,
    pub progress: Option<f64>,
    pub badges: String,
    pub status: String,
    pub retry: bool,
    pub hint: Option<String>,
    pub link_hints: Vec<(String, [f32; 4])>,
    pub menu: Vec<model::MenuItem>,
}

#[derive(Clone, Copy)]
struct PaneNodes {
    outer: NodeRef,
    content: NodeRef,
}

/// Signals stay mounted for the lifetime of the native window. Terminal output never writes them.
#[derive(Clone)]
pub struct WindowSignals {
    pub root: NodeRef,
    pub workspace: NodeRef,
    tabs: RetainedList<Id, TabModel>,
    breadcrumbs: RetainedList<Target, BreadcrumbModel>,
    rows: RetainedList<Target, OutlinerRow>,
    dialogs: RetainedList<String, DialogModel>,
    panes: RetainedList<Id, PaneModel>,
    pane_nodes: Rc<RefCell<HashMap<Id, PaneNodes>>>,
    surfaces: Rc<RefCell<HashMap<Id, SurfaceHandle>>>,
    controls: Rc<RefCell<HashMap<&'static str, NodeRef>>>,
    selected_tab: Cell<String>,
    outliner: Cell<Option<OutlinerModel>>,
    search: Cell<Option<SearchModel>>,
    palette: Cell<Option<PaletteModel>>,
    messages: Cell<Vec<String>>,
    titlebar_inset: Cell<f32>,
    theme: Cell<zgui_ui_tokens::Theme>,
    queue: Rc<RefCell<Vec<Command>>>,
    sink: Sink,
}

impl WindowSignals {
    pub fn new() -> Self {
        let queue = Rc::new(RefCell::new(Vec::new()));
        Self {
            root: NodeRef::new(),
            workspace: NodeRef::new(),
            tabs: RetainedList::new(),
            breadcrumbs: RetainedList::new(),
            rows: RetainedList::new(),
            dialogs: RetainedList::new(),
            panes: RetainedList::new(),
            pane_nodes: Default::default(),
            surfaces: Default::default(),
            controls: Default::default(),
            selected_tab: RwSignal::new_local(String::new()),
            outliner: RwSignal::new_local(None),
            search: RwSignal::new_local(None),
            palette: RwSignal::new_local(None),
            messages: RwSignal::new_local(Vec::new()),
            titlebar_inset: RwSignal::new_local(0.0),
            theme: RwSignal::new_local(zgui_ui_tokens::Theme::dark()),
            sink: Sink(
                StoredValue::new_local(Rc::clone(&queue)),
                RwSignal::new_local(0),
            ),
            queue,
        }
    }

    pub fn update(&self, model: WindowModel) {
        set_changed(
            self.theme,
            native_theme(model.background, model.foreground, model.accent),
        );
        self.tabs.update(model.tabs, |tab| tab.id);
        self.breadcrumbs
            .update(model.breadcrumbs, |crumb| crumb.target);
        self.rows.update(
            model
                .outliner
                .as_ref()
                .map_or_else(Vec::new, |outline| outline.rows.clone()),
            |row| row.target,
        );
        self.dialogs
            .update(model.dialogs, |dialog| dialog.id.clone());
        set_changed(
            self.selected_tab,
            model
                .selected_tab
                .map_or_else(String::new, |id| id.to_string()),
        );
        set_changed(self.outliner, model.outliner);
        set_changed(self.search, model.search);
        set_changed(self.palette, model.palette);
        set_changed(self.messages, model.messages);
        set_changed(self.titlebar_inset, model.titlebar_inset);
    }

    pub fn update_panes(&self, panes: Vec<PaneModel>) {
        self.pane_nodes
            .borrow_mut()
            .retain(|id, _| panes.iter().any(|pane| pane.id == *id));
        self.surfaces
            .borrow_mut()
            .retain(|id, _| panes.iter().any(|pane| pane.id == *id));
        self.panes.update(panes, |pane| pane.id);
    }

    pub fn update_pane_geometry(&self, id: Id, rect: [f32; 4]) {
        if let Some(pane) = self.panes.cells.borrow().get(&id).and_then(Weak::upgrade)
            && pane.with_untracked(|pane| pane.rect != rect)
        {
            pane.update(|pane| pane.rect = rect);
        }
    }

    pub fn update_search(&self, search: Option<SearchModel>) {
        set_changed(self.search, search);
    }

    pub fn search_model(&self) -> Option<SearchModel> {
        self.search.get_untracked()
    }

    pub fn pane_model(&self, id: Id) -> Option<PaneModel> {
        self.panes
            .cells
            .borrow()
            .get(&id)
            .and_then(Weak::upgrade)
            .map(|pane| pane.get_untracked())
    }

    /// Install before adding the corresponding pane; the retained surface holds this handle.
    pub fn set_surface(&self, id: Id, surface: SurfaceHandle) {
        self.surfaces.borrow_mut().insert(id, surface);
    }
    pub fn surface_node(&self, id: Id) -> Option<zgui::view::NodeId> {
        self.pane_nodes.borrow().get(&id)?.content.get_untracked()
    }

    fn control(&self, name: &'static str) -> NodeRef {
        *self.controls.borrow_mut().entry(name).or_default()
    }

    pub fn control_bounds(&self, name: &str) -> Option<Bounds> {
        self.controls.borrow().get(name)?.window_bounds()
    }
    pub fn workspace_bounds(&self) -> Option<Bounds> {
        self.workspace.window_bounds()
    }
    pub fn pane_bounds(&self) -> Vec<(Id, Bounds, Bounds)> {
        self.pane_nodes
            .borrow()
            .iter()
            .filter_map(|(&id, nodes)| {
                Some((
                    id,
                    nodes.outer.window_bounds()?,
                    nodes.content.window_bounds()?,
                ))
            })
            .collect()
    }
    pub fn drain_commands(&self) -> Vec<Command> {
        std::mem::take(&mut *self.queue.borrow_mut())
    }

    pub fn popup_open(&self) -> bool {
        self.sink.1.get_untracked() > 0
            || self.palette.with_untracked(Option::is_some)
            || !self.dialogs.order.with_untracked(Vec::is_empty)
    }

    pub fn focus_pane(&self, id: Id) {
        if let Some(nodes) = self.pane_nodes.borrow().get(&id) {
            nodes.content.focus();
        }
    }

    pub fn focus_search(&self) {
        if let Some(input) = self.controls.borrow().get("search") {
            input.focus();
        }
    }
}

fn native_theme(
    background: [u8; 3],
    foreground: [u8; 3],
    accent: [u8; 3],
) -> zgui_ui_tokens::Theme {
    let dark = u32::from(background[0]) * 299
        + u32::from(background[1]) * 587
        + u32::from(background[2]) * 114
        < 128_000;
    let mut theme = if dark {
        zgui_ui_tokens::Theme::dark()
    } else {
        zgui_ui_tokens::Theme::light()
    };
    let color = |rgb: [u8; 3]| format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
    theme.color.background = color(background);
    theme.color.foreground = color(foreground);
    theme.color.primary = color(accent);
    theme.color.ring = color(accent);
    theme
}

pub fn window_view(state: WindowSignals) -> impl IntoView {
    install_stylesheet("rustty-native", STYLESHEET);
    let root = state.root;
    let workspace = state.workspace;
    let sink = state.sink;
    let selected = state.selected_tab;
    let tabs = state.tabs.order;
    let breadcrumbs = state.breadcrumbs.order;
    let dialogs = state.dialogs.order;
    let outliner = state.outliner;
    let palette = state.palette;
    let search = state.search;
    let messages = state.messages;
    let inset = state.titlebar_inset;
    let theme = state.theme;
    let sidebar_state = state.clone();
    let pane_state = state.clone();
    let new_tab = state.control("new-tab");
    let palette_button = state.control("command-palette");
    let outliner_button = state.control("outliner");
    let search_input = state.control("search");
    view! {
        ThemeProvider(scheme = ColorScheme::Dark, dark = theme) {
            column(class = "rustty-window", node_ref = root) {
                row(class = "rustty-header", style:padding-left = move || Some(format!("{}px", inset.get() + 8.0))) {
                    Button(node_ref = outliner_button, variant = ButtonVariant::Ghost, size = ButtonSize::Sm,
                        a11y:label = "Toggle outliner", on:click = move |_| sink.send(Command::Action(Action::ToggleOutliner))) {"☰"}
                    Tabs(value = selected, class = "rustty-tabs", label = "Terminal tabs",
                        on_change = UnsyncCallback::new(move |value: String| { if let Ok(id) = value.parse() { sink.send(Command::SelectTab(id)); } })) {
                        TabsList(class = "rustty-tab-list") {
                            for entry in move || tabs.get(), key = |entry| entry.0 {
                                {tab_view(mounted_cell(entry.1), sink)}
                            }
                        }
                    }
                    Button(node_ref = new_tab, variant = ButtonVariant::Ghost, size = ButtonSize::Sm,
                        a11y:label = "New tab", on:click = move |_| sink.send(Command::Action(Action::NewTab))) {"+"}
                    Button(node_ref = palette_button, variant = ButtonVariant::Ghost, size = ButtonSize::Sm,
                        a11y:label = "Command palette", on:click = move |_| sink.send(Command::Action(Action::ToggleCommandPalette))) {"⌘"}
                }
                SidebarProvider(open = Binding::controlled(Signal::derive_local(move || outliner.with(Option::is_some)), move |_| sink.send(Command::Action(Action::ToggleOutliner))),
                    collapse = SidebarCollapse::Offcanvas, shortcut = false, class = "rustty-body") {
                    {outliner_view(sidebar_state)}
                    column(class = "rustty-page") {
                        Breadcrumb(class = "rustty-breadcrumb") {
                            BreadcrumbList {
                                for entry in move || breadcrumbs.get(), key = |entry| entry.0 {
                                    {breadcrumb_view(entry.0, mounted_cell(entry.1), sink)}
                                }
                            }
                        }
                        for _pane in move || search.with(|value| value.as_ref().map(|model| model.pane)).into_iter().collect::<Vec<_>>(), key = |pane| *pane {
                            {search_view(search, search_input, sink)}
                        }
                        column(class = "rustty-workspace", node_ref = workspace, a11y:role = Role::Group, a11y:label = "Terminal workspace") {
                            {workspace_view(pane_state)}
                        }
                        if move || !messages.with(Vec::is_empty) {
                            row(class = "rustty-messages") {
                                text(class = "rustty-message-text") {{move || messages.with(|messages| messages.join("\n"))}}
                                Button(variant = ButtonVariant::Ghost, size = ButtonSize::Sm, on:click = move |_| sink.send(Command::DismissMessages)) {"Dismiss"}
                            }
                        } else {}
                    }
                }
                for _mode in move || palette.with(|value| value.as_ref().map(|model| model.preview)).into_iter().collect::<Vec<_>>(), key = |mode| *mode {
                    {palette_view(palette, sink)}
                }
                for entry in move || dialogs.get(), key = |entry| entry.0.clone() { {dialog_view(mounted_cell(entry.1), sink)} }
            }
        }
    }
}

fn breadcrumb_view(target: Target, crumb: Cell<BreadcrumbModel>, sink: Sink) -> impl IntoView {
    view! {
        BreadcrumbItem {
            BreadcrumbLink(on:click = move |_| sink.send(Command::SelectTarget(target))) {{move || crumb.with(|model| model.label.clone())}}
            BreadcrumbSeparator()
        }
    }
}

fn tab_view(tab: Cell<TabModel>, sink: Sink) -> impl IntoView {
    let id = tab.with_untracked(|model| model.id);
    view! {
        ContextMenu(on_open_change = UnsyncCallback::new(move |open: bool| sink.popup(open))) {
            ContextMenuTrigger {
                TabsTrigger(value = id.to_string(), class = "rustty-tab", style:border-bottom-color = move || tab.with(|model| model.color).map(css_color)) {
                    text(class = "rustty-tab-name") {{move || tab.with(|model| model.label.clone())}}
                    if move || tab.with(|model| model.activity) { text(class = "rustty-tab-activity") {"●"} } else {}
                    Button(variant = ButtonVariant::Ghost, class = "rustty-close", a11y:label = "Close tab",
                        on:click:stop = move |_| sink.send(Command::CloseTab(id))) {"×"}
                }
            }
            ContextMenuContent { {menu_view(Signal::derive_local(move || tab.with(|model| model.menu.clone())), sink)} }
        }
    }
}

fn menu_view(items: Signal<Vec<model::MenuItem>, LocalStorage>, sink: Sink) -> impl IntoView {
    view! {
        for item in move || items.get().into_iter().enumerate().collect::<Vec<_>>(), key = |entry| entry.0 {
            {menu_item_view(item.1, sink)}
        }
    }
}

fn menu_item_view(item: model::MenuItem, sink: Sink) -> impl IntoView {
    let command = StoredValue::new_local(item.command);
    view! {
        NativeMenuItem(shortcut = item.shortcut, destructive = item.destructive, disabled = item.disabled,
            on_select = UnsyncCallback::new(move |()| sink.send(command.get_value()))) {{item.label}}
    }
}

fn outliner_view(state: WindowSignals) -> impl IntoView {
    let sink = state.sink;
    let outliner = state.outliner;
    let rows = state.rows.order;
    view! {
        Sidebar(label = "Workspace outliner") {
            SidebarHeader {
                row(class = "rustty-sidebar-heading") {
                    label {"Workspace"}
                    Button(variant = ButtonVariant::Ghost, size = ButtonSize::Sm, a11y:label = "Close outliner",
                        on:click = move |_| sink.send(Command::Action(Action::ToggleOutliner))) {"×"}
                }
                SidebarInput(placeholder = "Find a pane…", a11y:label = "Search workspace",
                    value = Binding::controlled(Signal::derive_local(move || outliner.with(|value| value.as_ref().map_or_else(String::new, |model| model.query.clone()))), move |query| sink.send(Command::OutlinerQuery(query))))
                row(class = "rustty-check") {
                    Checkbox(checked = Binding::controlled(Signal::derive_local(move || if outliner.with(|value| value.as_ref().is_some_and(|model| model.agents_only)) { Checked::Yes } else { Checked::No }),
                        move |value| sink.send(Command::OutlinerAgentsOnly(value == Checked::Yes))), a11y:label = "Agents only")
                    text {"Agents only"}
                }
            }
            SidebarContent {
                SidebarMenu {
                    for entry in move || rows.get(), key = |entry| entry.0 { {outliner_row(mounted_cell(entry.1), sink)} }
                }
            }
        }
    }
}

fn outliner_row(row: Cell<OutlinerRow>, sink: Sink) -> impl IntoView {
    let target = row.with_untracked(|model| model.target);
    view! {
        ContextMenu(on_open_change = UnsyncCallback::new(move |open: bool| sink.popup(open))) {
          ContextMenuTrigger {
           SidebarMenuItem {
            row(class = "rustty-outliner-row", style:padding-left = move || Some(format!("{}px", row.with(|model| model.depth) * 12))) {
                if move || row.with(|model| model.has_children) {
                    Button(variant = ButtonVariant::Ghost, class = "rustty-close", a11y:label = "Expand or collapse",
                        on:click = move |_| sink.send(Command::ExpandTarget(target))) {{move || if row.with(|model| model.expanded) { "⌄" } else { "›" }}}
                } else {}
                SidebarMenuButton(active = Signal::derive_local(move || row.with(|model| model.selected)), class = "rustty-outliner-title",
                    on:click = move |_| sink.send(Command::SelectTarget(target)),
                    on:pointer_enter = move |_| sink.send(Command::PreviewTarget(Some(target))),
                    on:pointer_leave = move |_| sink.send(Command::PreviewTarget(None))) {
                    column {
                        text {{move || row.with(|model| model.label.clone())}}
                        if move || row.with(|model| !model.detail.is_empty()) {text(class = "rustty-detail") {{move || row.with(|model| model.detail.clone())}}} else {}
                    }
                }
                if move || matches!(target, Target::Pane { .. }) {
                    Button(variant = ButtonVariant::Ghost, class = "rustty-close", a11y:label = "Pin or unpin pane",
                        on:click = move |_| sink.send(Command::PinTarget(target))) {{move || if row.with(|model| model.pinned) { "◆" } else { "◇" }}}
                } else {}
            }
           }
          }
          ContextMenuContent {
            NativeMenuItem(on_select = UnsyncCallback::new(move |()| sink.send(Command::SelectTarget(target)))) {"Focus"}
            if move || row.with(|model| model.pinned) {
                NativeMenuItem(on_select = UnsyncCallback::new(move |()| if let Target::Pane { pane, .. } = target { sink.send(Command::Unpin(pane)); })) {"Unpin"}
                NativeMenuItem(on_select = UnsyncCallback::new(move |()| if let Target::Pane { pane, .. } = target { sink.send(Command::MovePin(pane, -1)); })) {"Move pinned pane up"}
                NativeMenuItem(on_select = UnsyncCallback::new(move |()| if let Target::Pane { pane, .. } = target { sink.send(Command::MovePin(pane, 1)); })) {"Move pinned pane down"}
            } else {}
          }
        }
    }
}

fn search_view(search: Cell<Option<SearchModel>>, input: NodeRef, sink: Sink) -> impl IntoView {
    let pane = search.with_untracked(|value| value.as_ref().expect("mounted search").pane);
    let focus = RenderEffect::new(move |_| {
        if input.get().is_some() {
            input.focus();
        }
    });
    on_cleanup_local(move || drop(focus));
    view! {
        row(class = "rustty-search") {
            Input(node_ref = input, class = "rustty-search-input", label = "Search terminal", placeholder = "Find in terminal…",
                value = Binding::controlled(Signal::derive_local(move || search.with(|value| value.as_ref().map_or_else(String::new, |model| model.query.clone()))), move |query| sink.send(Command::SearchQuery(pane, query))),
                on:focus_in = move |_| sink.send(Command::SearchFocus(Some(pane))),
                on:focus_out = move |_| sink.send(Command::SearchFocus(None)),
                on:key_down = move |event| {
                    if event.key == Key::Named(NamedKey::Enter) { sink.send(Command::SearchNext(pane, !event.modifiers.contains(Modifiers::SHIFT))); event.prevent_default(); }
                    if event.key == Key::Named(NamedKey::Escape) { sink.send(Command::SearchClose(pane)); event.prevent_default(); }
                })
            text(class = "rustty-detail") {{move || search.with(|value| value.as_ref().map_or_else(String::new, |model| model.status.clone()))}}
            Button(variant = ButtonVariant::Ghost, size = ButtonSize::Sm, a11y:label = "Previous match", on:click = move |_| sink.send(Command::SearchNext(pane, false))) {"↑"}
            Button(variant = ButtonVariant::Ghost, size = ButtonSize::Sm, a11y:label = "Next match", on:click = move |_| sink.send(Command::SearchNext(pane, true))) {"↓"}
            Button(variant = ButtonVariant::Ghost, size = ButtonSize::Sm, a11y:label = "Close search", on:click = move |_| sink.send(Command::SearchClose(pane))) {"×"}
        }
    }
}

fn palette_view(palette: Cell<Option<PaletteModel>>, sink: Sink) -> impl IntoView {
    let model = palette.get_untracked().expect("mounted palette");
    let items = StoredValue::new_local(model.items);
    let placeholder = StoredValue::new_local(model.placeholder);
    view! {
        CommandDialog(default_open = true, title = model.title,
            on_open_change = UnsyncCallback::new(move |open: bool| { if !open { sink.send(Command::PaletteClose); } })) {
            {palette_observer(model.preview, sink)}
            CommandInput(placeholder = placeholder.get_value())
            CommandList {
                CommandEmpty {"No matching commands"}
                for item in move || items.get_value(), key = |item| item.index {
                    {palette_item(item, sink)}
                }
            }
        }
    }
}

fn palette_item(item: PaletteItem, sink: Sink) -> impl IntoView {
    let index = item.index;
    let label = StoredValue::new_local(item.label);
    let shortcut = StoredValue::new_local(item.shortcut);
    view! {
        CommandItem(value = index.to_string(), text = label.get_value(),
            on_select = UnsyncCallback::new(move |()| sink.send(Command::PaletteChoose(index)))) {
            text {{label.get_value()}}
            CommandShortcut {{shortcut.get_value()}}
        }
    }
}

fn palette_observer(preview: bool, sink: Sink) -> impl IntoView {
    if preview && let Some(listbox) = zgui_ui::listbox::Listbox::current() {
        let watching = RenderEffect::new(move |previous: Option<Option<usize>>| {
            let active = listbox.active();
            let index = listbox
                .entries()
                .iter()
                .find(|entry| Some(entry.id()) == active)
                .and_then(|entry| entry.option().value().parse().ok());
            if previous != Some(index)
                && let Some(index) = index
            {
                sink.send(Command::PalettePreview(index));
            }
            index
        });
        on_cleanup_local(move || drop(watching));
    }
}

fn workspace_view(state: WindowSignals) -> impl IntoView {
    let panes = state.panes.order;
    let state = StoredValue::new_local(state);
    view! { for entry in move || panes.get(), key = |entry| entry.0 { {pane_view(mounted_cell(entry.1), state.get_value())} } }
}

fn pane_view(pane: Cell<PaneModel>, state: WindowSignals) -> impl IntoView {
    let id = pane.with_untracked(|model| model.id);
    let rect = memo_field(pane, |model| &model.rect);
    let label = memo_field(pane, |model| &model.label);
    let focused = memo_field(pane, |model| &model.focused);
    let opacity = memo_field(pane, |model| &model.opacity);
    let badges = memo_field(pane, |model| &model.badges);
    let progress = memo_field(pane, |model| &model.progress);
    let menu = memo_field(pane, |model| &model.menu);
    let link_hints = memo_field(pane, |model| &model.link_hints);
    let status = memo_field(pane, |model| &model.status);
    let retry = memo_field(pane, |model| &model.retry);
    let hint = memo_field(pane, |model| &model.hint);
    let sink = state.sink;
    let nodes = PaneNodes {
        outer: NodeRef::new(),
        content: NodeRef::new(),
    };
    state.pane_nodes.borrow_mut().insert(id, nodes);
    let source = state
        .surfaces
        .borrow()
        .get(&id)
        .cloned()
        .expect("surface installed before pane");
    let surface = zgui::elements::surface()
        .source(&source)
        .node_ref(nodes.content)
        .class("rustty-terminal")
        .tabindex(Focus::Programmatic)
        .a11y(A11yBinding::new(Role::Terminal).label(move || label.get()))
        .on(events::POINTER_DOWN, move |_| nodes.content.focus());
    view! {
        column(class = "rustty-pane", class:rustty-pane-focused = move || focused.get(), node_ref = nodes.outer,
            style:left = move || Some(format!("{}%", rect.get()[0] * 100.0)),
            style:top = move || Some(format!("{}%", rect.get()[1] * 100.0)),
            style:width = move || Some(format!("{}%", rect.get()[2] * 100.0)),
            style:height = move || Some(format!("{}%", rect.get()[3] * 100.0)),
            style:opacity = move || Some(opacity.get().to_string()),
            on:pointer_down = move |_| sink.send(Command::FocusPane(id)), a11y:role = Role::Group) {
            ContextMenu(on_open_change = UnsyncCallback::new(move |open: bool| sink.popup(open))) {
                ContextMenuTrigger {
                    row(class = "rustty-pane-header") {
                        text(class = "rustty-pane-title") {{move || label.get()}}
                        text(class = "rustty-detail") {{move || badges.get()}}
                        if move || progress.get().is_some() {text(class = "rustty-detail") {{move || format!("{}%", progress.get().unwrap_or(0.0) as u32)}}} else {}
                        Button(variant = ButtonVariant::Ghost, class = "rustty-close", a11y:label = "Zoom pane",
                            on:click = move |_| sink.send(Command::Action(Action::ToggleSplitZoom))) {"□"}
                    }
                }
                ContextMenuContent { {menu_view(Signal::derive_local(move || menu.get()), sink)} }
            }
            column(class = "rustty-pane-content") {
                {surface}
                for entry in move || link_hints.get().into_iter().enumerate().collect::<Vec<_>>(), key = |entry| entry.0 {
                    Badge(class = "rustty-link-hint", style:left = format!("{}px", entry.1.1[0]), style:top = format!("{}px", entry.1.1[1])) {{entry.1.0}}
                }
            }
            if move || !status.with(String::is_empty) {
                row(class = "rustty-messages rustty-pane-status") {
                    text {{move || status.get()}}
                    if move || retry.get() { Button(on:click = move |_| sink.send(Command::RetryPane(id))) {"Retry"} } else {}
                }
            } else {}
            if move || hint.with(Option::is_some) { Badge(class = "rustty-pane-hint") {{move || hint.get().unwrap_or_default()}} } else {}
        }
    }
}

fn dialog_view(dialog: Cell<DialogModel>, sink: Sink) -> impl IntoView {
    let id = StoredValue::new_local(dialog.with_untracked(|model| model.id.clone()));
    let fields = RetainedList::new();
    let checks = RetainedList::new();
    let buttons = RetainedList::new();
    let fields_order = fields.order;
    let checks_order = checks.order;
    let buttons_order = buttons.order;
    let watching = RenderEffect::new(move |_| {
        dialog.with(|model| {
            fields.update(model.fields.clone(), |field| field.id.clone());
            checks.update(model.checks.clone(), |check| check.id.clone());
            buttons.update(model.buttons.clone(), |button| button.id.clone());
        });
    });
    on_cleanup_local(move || drop(watching));
    view! {
        Dialog(default_open = true, on_open_change = UnsyncCallback::new(move |open: bool| { if !open { sink.send(Command::DialogClose(id.get_value())); } })) {
            DialogContent(class = "rustty-dialog", dismiss_on_outside_press = Signal::derive_local(move || dialog.with(|model| model.dismissible)),
                dismiss_on_escape = Signal::derive_local(move || dialog.with(|model| model.dismissible)), dismiss_control = false) {
                DialogHeader {
                    DialogTitle {{move || dialog.with(|model| model.title.clone())}}
                    DialogDescription {{move || dialog.with(|model| model.description.clone())}}
                }
                column(class = "rustty-fields") {
                    for entry in move || fields_order.get(), key = |entry| entry.0.clone() { {dialog_field(dialog, mounted_cell(entry.1), sink)} }
                    for entry in move || checks_order.get(), key = |entry| entry.0.clone() { {dialog_check(id, mounted_cell(entry.1), sink)} }
                    for entry in move || dialog.with(|model| model.preview.clone()).into_iter().enumerate().collect::<Vec<_>>(), key = |entry| entry.0 { {preview_view(entry.1)} }
                }
                DialogFooter {
                    for entry in move || buttons_order.get(), key = |entry| entry.0.clone() { {dialog_button(id, mounted_cell(entry.1), sink)} }
                }
            }
        }
    }
}

fn dialog_field(model: Cell<DialogModel>, field: Cell<FieldModel>, sink: Sink) -> impl IntoView {
    let dialog = StoredValue::new_local(model.with_untracked(|model| model.id.clone()));
    let initial = field.get_untracked();
    let id = StoredValue::new_local(initial.id);
    let label = NodeRef::new();
    let input = NodeRef::new();
    let placeholder = StoredValue::new_local(initial.placeholder);
    let value = Binding::controlled(
        Signal::derive_local(move || field.with(|model| model.value.clone())),
        move |value| {
            sink.send(Command::DialogField {
                dialog: dialog.get_value(),
                field: id.get_value(),
                value,
            })
        },
    );
    view! {
        column {
            Label(node_ref = label, control = input) {{move || field.with(|model| model.label.clone())}}
            if move || initial.multiline {
                Textarea(node_ref = input, value = value, labelled_by = label, placeholder = placeholder.get_value())
            } else {
                Input(node_ref = input, value = value, labelled_by = label, placeholder = placeholder.get_value(),
                    on:key_down = move |event| {
                        if event.key == Key::Named(NamedKey::Enter)
                            && let Some(action) = model.with_untracked(|model| model.buttons.iter().find(|button| button.primary && !button.disabled).map(|button| button.id.clone())) {
                            sink.send(Command::DialogAction { dialog: dialog.get_value(), action });
                            event.prevent_default();
                        }
                    })
            }
        }
    }
}

fn dialog_check(
    dialog: StoredValue<String, LocalStorage>,
    check: Cell<CheckModel>,
    sink: Sink,
) -> impl IntoView {
    let id = StoredValue::new_local(check.with_untracked(|model| model.id.clone()));
    let label = NodeRef::new();
    let input = NodeRef::new();
    view! {
        row(class = "rustty-check") {
            Checkbox(node_ref = input, labelled_by = label,
                checked = Binding::controlled(Signal::derive_local(move || if check.with(|model| model.checked) { Checked::Yes } else { Checked::No }),
                    move |value| sink.send(Command::DialogCheck { dialog: dialog.get_value(), check: id.get_value(), checked: value == Checked::Yes })))
            Label(node_ref = label, control = input) {{move || check.with(|model| model.label.clone())}}
        }
    }
}

fn dialog_button(
    dialog: StoredValue<String, LocalStorage>,
    button: Cell<DialogButton>,
    sink: Sink,
) -> impl IntoView {
    let initial = button.get_untracked();
    let id = StoredValue::new_local(initial.id);
    view! {
        Button(variant = if initial.destructive { ButtonVariant::Destructive } else if initial.primary { ButtonVariant::Default } else { ButtonVariant::Outline },
            disabled = Signal::derive_local(move || button.with(|model| model.disabled)), on:click = move |_| sink.send(Command::DialogAction { dialog: dialog.get_value(), action: id.get_value() })) {{move || button.with(|model| model.label.clone())}}
    }
}

fn css_color(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

fn preview_view(preview: PreviewPart) -> impl IntoView {
    let image = preview.image.and_then(|(width, height, pixels)| {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .ok()?
            .write_image_data(&pixels)
            .ok()?;
        Some(zgui_image::ImageBytes::new(bytes))
    });
    let source = image.as_ref().map(zgui_image::ImageBytes::url);
    on_cleanup_local(move || drop(image));
    view! {
        column(class = "rustty-preview") {
            label {{preview.label}}
            text(class = "rustty-detail") {{preview.mime}}
            {preview.text.map(|text| view! {text(class = "rustty-preview-text") {{text}}})}
            {source.map(|source| view! {image(class = "rustty-preview-image", src = source)})}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_items_are_released_after_unowned_updates() {
        zgui::reactive::install().ok();
        let owner = zgui::reactive::Mounted::new();
        let list = owner.with(RetainedList::new);
        // The controller publishes snapshots outside the mounted window's owner.
        for id in 0..100 {
            let payload = Rc::new(());
            let dropped = Rc::downgrade(&payload);
            list.update(vec![(id, payload)], |value| value.0);
            assert!(dropped.upgrade().is_some());
            list.update(Vec::new(), |value| value.0);
            zgui::reactive::flush();
            assert!(dropped.upgrade().is_none(), "removed item {id} leaked");
        }
        owner.unmount();
    }

    #[test]
    fn removed_items_live_until_keyed_unmount_and_reuse_pending_identity() {
        zgui::reactive::install().ok();
        let owner = zgui::reactive::Mounted::new();
        let list = owner.with(RetainedList::new);
        for id in 0..100 {
            let payload = Rc::new(());
            let dropped = Rc::downgrade(&payload);
            list.update(vec![(id, 0, payload.clone())], |value| value.0);
            let child = owner.with(zgui::reactive::Mounted::new);
            let cell = child.with(|| mounted_cell(list.order.get_untracked().pop().unwrap().1));

            list.update(Vec::new(), |value| value.0);
            assert_eq!(cell.with_untracked(|value| value.1), 0);
            // A keyed list has not reconciled yet, so the existing child must see the new value.
            list.update(vec![(id, 1, payload)], |value| value.0);
            assert_eq!(cell.with_untracked(|value| value.1), 1);
            list.update(Vec::new(), |value| value.0);
            assert!(dropped.upgrade().is_some());
            child.unmount();
            assert!(dropped.upgrade().is_none(), "unmounted item {id} leaked");
        }
        assert!(list.cells.borrow().len() <= 1, "dead identities accumulate");
        owner.unmount();
    }

    #[test]
    fn window_unmount_releases_current_items() {
        zgui::reactive::install().ok();
        let owner = zgui::reactive::Mounted::new();
        let list = owner.with(RetainedList::new);
        let payload = Rc::new(());
        let dropped = Rc::downgrade(&payload);
        list.update(vec![(1, payload)], |value| value.0);
        let child = owner.with(zgui::reactive::Mounted::new);
        child.with(|| mounted_cell(list.order.get_untracked().pop().unwrap().1));
        owner.unmount();
        assert!(dropped.upgrade().is_none());
    }

    #[test]
    fn field_memos_skip_cloning_and_notifying_unchanged_fields() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        struct TrackedValue(u64, Arc<AtomicUsize>);
        impl PartialEq for TrackedValue {
            fn eq(&self, other: &Self) -> bool {
                self.0 == other.0
            }
        }
        impl Clone for TrackedValue {
            fn clone(&self) -> Self {
                self.1.fetch_add(1, Ordering::Relaxed);
                Self(self.0, Arc::clone(&self.1))
            }
        }

        zgui::reactive::install().ok();
        let owner = zgui::reactive::Mounted::new();
        owner.with(|| {
            let clones = Arc::new(AtomicUsize::new(0));
            let model = RwSignal::new_local((0_u64, TrackedValue(1, Arc::clone(&clones))));
            let field = memo_field(model, |model| &model.1);
            let runs = Rc::new(std::cell::Cell::new(0));
            let observed = Rc::clone(&runs);
            let effect = RenderEffect::new(move |_| {
                field.with(|_| ());
                observed.set(observed.get() + 1);
            });
            assert_eq!(clones.load(Ordering::Relaxed), 1);
            let initial = runs.get();
            model.update(|model| model.0 += 1);
            zgui::reactive::flush();
            assert_eq!(runs.get(), initial);
            assert_eq!(clones.load(Ordering::Relaxed), 1);

            model.update(|model| model.1.0 += 1);
            zgui::reactive::flush();
            assert_eq!(runs.get(), initial + 1);
            assert_eq!(clones.load(Ordering::Relaxed), 2);
            drop(effect);
        });
        owner.unmount();
    }

    #[test]
    fn equal_snapshot_does_not_invalidate_retained_items() {
        zgui::reactive::install().ok();
        let owner = zgui::reactive::Mounted::new();
        owner.with(|| {
            let list = RetainedList::new();
            list.update(vec![(1_u64, "one"), (2, "two")], |value| value.0);
            let first = list.order.get_untracked()[0].1.clone();
            let runs = Rc::new(std::cell::Cell::new(0));
            let observed = Rc::clone(&runs);
            let observed_cell = first.clone();
            let effect = RenderEffect::new(move |_| {
                observed_cell.get();
                observed.set(observed.get() + 1);
            });
            let initial = runs.get();
            list.update(vec![(1, "one"), (2, "changed")], |value| value.0);
            zgui::reactive::flush();
            assert_eq!(runs.get(), initial, "an unrelated item must not redraw");
            assert_eq!(
                list.order.get_untracked()[0].1,
                first,
                "identity survives updates"
            );
            list.update(vec![(1, "changed"), (2, "changed")], |value| value.0);
            zgui::reactive::flush();
            assert!(
                runs.get() > initial,
                "changing the observed item must redraw it"
            );
            drop(effect);
        });
        owner.unmount();
    }
}
