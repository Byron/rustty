# Outliner and breadcrumbs

Open **View → Outliner**, or search for **Toggle Outliner** in the command palette.
The Outliner is a separate, movable, resizable floating window for the entire
workspace. Its position and size are remembered while the application runs.
It can also be bound with
`keybind = ctrl+alt+o=toggle_outliner`; no shortcut is assigned by default.

Each window has a root row, named `Window N · <active tab name>`. The Quick Terminal
appears after its first use in the current application session and stays listed
while hidden. Beneath each window, the hierarchy includes only the levels present:

- One tab without quads: panels.
- One tab with quads: quads → panels.
- Multiple tabs: tabs → optional quads → panels.

Panels use the same directory basename as their on-screen labels. A quad uses that
name when all its panels share it; otherwise it uses its position, such as
“Top left.” Tab rows show `Cmd+N` followed by the tab name, without activity counts
or repeated zoom suffixes. Tab accents carry through their descendants.

A square icon maps each panel's position within its parent. Quad icons use a double
square and map the quad's position within the tab. Corner brackets mark zoom while
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
names and icons. It omits the tab name and supports the same selection rules.
Tabs scroll horizontally when space is tight; the new-tab button stays accessible.

For a disposable native check with screenshots, run:

```sh
RUSTTY_SMOKE_DIR=/tmp/rustty-outliner-check RUSTTY_SMOKE_OFFSCREEN=1 \
  RUSTTY_SMOKE_OUTLINER=1 target/debug/rustty --config-file=/tmp/empty-rustty.conf
```

The fixture uses its own workspace and quiet child processes, disables USB, and
checks disclosure controls, navigation, zoom, native movement/resizing, quick-terminal visibility,
terminal typing, and input isolation between native windows.
