//! The native window's data and commands. No UI-framework geometry leaks into the controller.

use rustty::config::Action;
use rustty_app::{outliner::Target, workspace::Id};

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Action(Action),
    SelectTab(Id),
    CloseTab(Id),
    FocusPane(Id),
    PaneAction(Id, Action),
    SelectTarget(Target),
    PreviewTarget(Option<Target>),
    ExpandTarget(Target),
    PinTarget(Target),
    Unpin(Id),
    MovePin(Id, i32),
    SearchQuery(Id, String),
    SearchNext(Id, bool),
    SearchClose(Id),
    SearchFocus(Option<Id>),
    RetryPane(Id),
    OutlinerQuery(String),
    OutlinerAgentsOnly(bool),
    PaletteChoose(usize),
    PalettePreview(usize),
    PaletteClose,
    DialogField {
        dialog: String,
        field: String,
        value: String,
    },
    DialogCheck {
        dialog: String,
        check: String,
        checked: bool,
    },
    DialogAction {
        dialog: String,
        action: String,
    },
    DialogClose(String),
    DismissMessages,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowModel {
    pub background: [u8; 3],
    pub foreground: [u8; 3],
    pub accent: [u8; 3],
    pub tabs: Vec<TabModel>,
    pub selected_tab: Option<Id>,
    pub breadcrumbs: Vec<BreadcrumbModel>,
    pub outliner: Option<OutlinerModel>,
    pub search: Option<SearchModel>,
    pub palette: Option<PaletteModel>,
    pub dialogs: Vec<DialogModel>,
    pub messages: Vec<String>,
    /// Logical pixels reserved for the native macOS window buttons.
    pub titlebar_inset: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TabModel {
    pub id: Id,
    pub label: String,
    pub activity: bool,
    pub color: Option<[u8; 3]>,
    pub menu: Vec<MenuItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    pub label: String,
    pub shortcut: String,
    pub command: Command,
    pub destructive: bool,
    pub disabled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BreadcrumbModel {
    pub target: Target,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutlinerModel {
    pub query: String,
    pub agents_only: bool,
    pub rows: Vec<OutlinerRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutlinerRow {
    pub target: Target,
    pub label: String,
    pub detail: String,
    pub depth: usize,
    pub selected: bool,
    pub expanded: bool,
    pub has_children: bool,
    pub pinned: bool,
    pub color: Option<[u8; 3]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchModel {
    pub pane: Id,
    pub query: String,
    pub status: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaletteModel {
    pub title: String,
    pub placeholder: String,
    pub items: Vec<PaletteItem>,
    pub preview: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaletteItem {
    pub index: usize,
    pub label: String,
    pub shortcut: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DialogModel {
    pub id: String,
    pub title: String,
    pub description: String,
    pub fields: Vec<FieldModel>,
    pub checks: Vec<CheckModel>,
    pub preview: Vec<PreviewPart>,
    pub buttons: Vec<DialogButton>,
    pub dismissible: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldModel {
    pub id: String,
    pub label: String,
    pub value: String,
    pub placeholder: String,
    pub multiline: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CheckModel {
    pub id: String,
    pub label: String,
    pub checked: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreviewPart {
    pub mime: String,
    pub label: String,
    pub text: Option<String>,
    /// RGBA8 pixels, width, and height of an already decoded safe preview.
    pub image: Option<(u32, u32, Vec<u8>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DialogButton {
    pub id: String,
    pub label: String,
    pub destructive: bool,
    pub primary: bool,
    pub disabled: bool,
}
