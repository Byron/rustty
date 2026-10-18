# Outliner and breadcrumbs

Open **View → Outliner**, or search for **Toggle Outliner** in the command palette.
The Outliner is a separate, movable, resizable floating window for the entire
workspace. Its position and size are remembered while the application runs.
It can also be bound with
`keybind = ctrl+alt+o=toggle_outliner`; no shortcut is assigned by default.

When more than one window is included, each has a root row named
`Window N · <active tab name>`. With just one included window, that level is omitted.
The Quick Terminal appears after its first use in the current application session
and stays listed while hidden; an unused Quick Terminal does not add a window level.
Within each window, the hierarchy includes only the levels present:

- One tab without quads: panels.
- One tab with quads: quads → panels.
- Multiple tabs: tabs → optional quads → panels.

Panels use the same directory basename as their on-screen labels. A quad lists its
distinct panel names in alphabetical order, separated by commas, whether expanded
or collapsed. Tab rows show `Cmd+N` followed by the tab name, without activity counts
or repeated zoom suffixes. Tab accents carry through their descendants.

Search for **Toggle Pin** in the command palette to pin or unpin the focused panel.
It can also be bound with `keybind = ctrl+alt+p=toggle_pin`; no shortcut is assigned
by default. Pinned panels appear in a numbered section above the tree. On macOS,
Cmd+Shift+1 through Cmd+Shift+9 focus the corresponding pinned panel, even with
the Outliner closed or its search field focused. A missing pin number leaves
focus unchanged. Custom bindings can use `goto_pin:N` with a positive pin number.
macOS screenshot shortcuts may intercept Cmd+Shift+3, 4, or 5 before Rustty receives them.

Configure each pin shortcut with a `keybind` entry. For example, move pin 1 to
Ctrl+Alt+1 and disable its default shortcut:

```ini
keybind = cmd+shift+1=unbind
keybind = ctrl+alt+1=goto_pin:1
```

Repeat for other pin numbers. Adding a new shortcut keeps the default unless you
unbind it; assigning another action to the same shortcut replaces it. Numbers
above 9 also work, for example `keybind = ctrl+alt+0=goto_pin:10`. Reload the
configuration with Cmd+Shift+, on macOS, or **Reload Configuration** in the command
palette. The configured shortcuts also work while the Outliner's search is focused.

Each pin shows a neutral breadcrumb omitting the Window level, making panels
with matching names distinguishable. Click a breadcrumb level to select it using
the tree's navigation rules. A neutral pin also marks the panel
in the tree and beside its terminal label, or in its corner while the label is hidden.

New pins are appended to the list. Drag a pin icon to change its order and shortcut
number; a focused pin icon also supports Alt+↑/↓. Clicking the pin icon or toggling
an existing pin removes it. Pins and their order are saved with the workspace,
and closing a panel removes its pin.

With the Outliner focused, press `/` to open a search field at the bottom. The tree
stays visible until you type, then becomes a list of matching panel breadcrumbs.
Search ignores case and permits gaps between letters; matched characters in panel
names are highlighted. Pinned entries remain above the results.

Breadcrumbs represent quads with just their position icon and zoom indicator;
quad names remain visible in the tree. Tab breadcrumbs show just the tab name;
their `Cmd+N` shortcuts remain in the tree and tooltips. Other repeated names
appear only at their deepest level, keeping each ancestor's icon available for navigation.

Every panel has its own row in the tree, even when names match. In search results,
panels with the same name under the same quad or tab share one breadcrumb. Each
keeps its own clickable position icon; clicking the shared name selects the first
icon's panel. Matching names in different parent groups remain separate. Pins
retain their individual entries and order.

`Ctrl+N` and `Ctrl+P` select the next or previous match, wrapping at either end.
The terminal follows selection changes while keyboard input stays in the Outliner.
Editing the query selects the first match; clicking a result selects and focuses it
while keeping search open.

Enter, Escape, and `Ctrl+[` focus the selected result and exit search, using the
first result until you select another. With no matches, they exit without changing
focus. The `×` button closes search without changing panel focus.

A square icon maps each panel's position within its parent using a filled shape.
Quad icons use the same geometry with an outlined position shape and map the quad's
position within the tab. Corner brackets mark zoom while
preserving this position map: brackets around a quad mean quad zoom, brackets
around a panel mean panel zoom, and brackets around both mean a panel zoomed within
a quad. Tooltips name the exact zoom state.

The tab bar uses compact zoom symbols: an empty square for quad zoom, a filled
square for panel zoom, and a filled square inside an empty square for panel zoom
within a quad. The same symbols appear in a single-tab window's header.

The focused panel has a solid accent background. Its window, tab, and quad have a subtle
accent background and an accent line, making the path easy to follow. Focus changes
scroll only when the selected panel is outside the visible area, moving just enough
to reveal it; ordinary scrolling remains under your control.

Branches start collapsed except for the path to the focused panel. Click a branch's
`+` or `−` button to expand or collapse one level; Shift-click applies the same change
to all its descendants. These buttons only change the tree's visibility, leaving
terminal focus and zoom alone. When panel focus changes, its ancestor branches open
automatically so the focused panel stays visible. A manually collapsed branch stays
collapsed until focus moves into it or its focused path changes.

Clicking the focused panel or one of its ancestors leaves focus and zoom alone.
Clicking another window, tab, quad, or panel selects its first descendant panel and
reveals it, unwinding panel zoom or moving quad zoom when needed. The terminal window
is brought forward, including minimized windows and the hidden Quick Terminal.
The Outliner keeps the last terminal's focus trail while you interact with it.
Closing it leaves all terminal windows and panels intact.

When multiple tabs are present, a monochrome breadcrumb appears at the right end
of the tab bar. It shows the focused quad (if present) and panel using the same
compact icons and panel names. It omits the tab name and supports the same
selection rules.
Tabs scroll horizontally when space is tight; the new-tab button stays accessible.

For a disposable native check with screenshots, run:

```sh
RUSTTY_SMOKE_DIR=/tmp/rustty-outliner-check RUSTTY_SMOKE_OFFSCREEN=1 \
  RUSTTY_SMOKE_OUTLINER=1 target/debug/rustty --config-file=/tmp/empty-rustty.conf
```

The fixture uses its own workspace and quiet child processes, disables USB, and
checks pinning and drag order, fuzzy search, disclosure controls, navigation, zoom,
native movement/resizing, quick-terminal visibility,
terminal typing, and input isolation between native windows.
Offscreen runs keep all native windows hidden and simulate focus inside the fixture;
the report marks native focus and minimization checks as skipped. Omit
`RUSTTY_SMOKE_OFFSCREEN` only for an attended run on an idle desktop, since the
foreground checks repeatedly show and focus windows.
