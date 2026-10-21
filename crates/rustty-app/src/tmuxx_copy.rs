//! Copy a local window's arrangement into a new tmuxx session with fresh shells.

use crate::workspace::{Axis, Id, Node, Tab, Tree, WindowState};
use rustty::tmuxx::{Axis as LayoutAxis, Connection, Kind as LayoutKind, Layout};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

const MAX_DIMENSION: u16 = 10_000;
const IDS: &str = "#{session_id} #{window_id} #{pane_id}";
type Size = (u16, u16);

#[derive(Debug)]
pub struct WindowCopy {
    pub window: WindowState,
    pub size: (u16, u16),
}

#[derive(Debug)]
pub struct CopiedWindow {
    pub session: u64,
    pub window: WindowState,
    pub tabs: BTreeMap<Id, u64>,
    pub panes: BTreeMap<Id, u64>,
}

impl WindowCopy {
    pub fn validate(&self) -> Result<(), String> {
        if self.window.tabs.is_empty() || self.window.active_tab >= self.window.tabs.len() {
            return Err("The window has no valid active tab to copy".into());
        }
        let mut tabs = BTreeSet::new();
        let mut panes = BTreeSet::new();
        for tab in &self.window.tabs {
            if !tabs.insert(tab.id) {
                return Err("The window contains duplicate tab IDs".into());
            }
            if !tab.overlays.is_empty() {
                return Err(
                    "Copying windows with floating or overlay panes is not supported".into(),
                );
            }
            minimum(&tab.root)?;
            let ids = tab.root.panes();
            if !ids.contains(&tab.focused)
                || tab.zoom.is_some_and(|id| tab.root.node(id).is_none())
                || tab
                    .quadrant_zoom
                    .is_some_and(|id| tab.root.node(id).is_none())
                || ids.len() != tab.panes.len()
            {
                return Err("A tab contains invalid pane references".into());
            }
            if tab.title.as_ref().is_some_and(|title| title.contains('\0')) {
                return Err("A tab name contains NUL".into());
            }
            for id in ids {
                if !panes.insert(id) {
                    return Err("The window contains duplicate pane IDs".into());
                }
                let pane = tab.panes.get(&id).ok_or("A tab is missing pane metadata")?;
                let directory = &pane.working_directory;
                let path = directory
                    .to_str()
                    .ok_or("tmuxx cannot represent a non-UTF-8 working directory")?;
                if path.contains('\0') || !directory.is_absolute() || !directory.is_dir() {
                    return Err(format!(
                        "Pane working directory is not an existing absolute directory: {}",
                        directory.display()
                    ));
                }
                if pane
                    .title
                    .iter()
                    .chain(&pane.title_override)
                    .any(|title| title.contains('\0'))
                {
                    return Err("A pane title contains NUL".into());
                }
            }
        }
        Ok(())
    }

    /// Blocks for command replies; callers should run this outside the UI thread.
    pub fn run(self, connection: &Connection, name: &str) -> Result<CopiedWindow, String> {
        self.run_with(name, &mut |argv| {
            let command = String::from_utf8_lossy(&argv[0]).into_owned();
            connection
                .command_with_reply(argv)
                .map_err(|error| error.to_string())?
                .recv_timeout(Duration::from_secs(10))
                .map_err(|error| format!("{command}: no confirmed reply ({error})"))?
                .map_err(|error| format!("{command}: {error}"))
        })
    }

    fn run_with(
        self,
        name: &str,
        send: &mut impl FnMut(Vec<Vec<u8>>) -> Result<Vec<u8>, String>,
    ) -> Result<CopiedWindow, String> {
        self.validate()?;
        if name.is_empty() || name.contains('\0') {
            return Err("The new session needs a nonempty name without NUL".into());
        }
        let mut session = None;
        let mut tabs = BTreeMap::new();
        let mut panes = BTreeMap::new();
        let result: Result<(), String> = (|| {
            for tab in &self.window.tabs {
                let minimum = minimum(&tab.root)?;
                let size = (
                    self.size.0.clamp(minimum.0, MAX_DIMENSION),
                    self.size.1.clamp(minimum.1, MAX_DIMENSION),
                );
                let first = first_pane(&tab.root);
                let directory = tab.panes[&first].working_directory.to_str().unwrap();
                let title = format_literal(tab.title.as_deref().unwrap_or("Terminal"));
                let output = if let Some(session) = session {
                    command(
                        send,
                        &[
                            "new-window",
                            "-d",
                            "-P",
                            "-F",
                            IDS,
                            "-t",
                            &format!("${session}:"),
                            "-n",
                            &title,
                            "-c",
                            directory,
                        ],
                    )?
                } else {
                    command(
                        send,
                        &[
                            "new-session",
                            "-d",
                            "-P",
                            "-F",
                            IDS,
                            "-s",
                            &format_literal(name),
                            "-n",
                            &title,
                            "-c",
                            &format_literal(directory),
                            "-x",
                            &size.0.to_string(),
                            "-y",
                            &size.1.to_string(),
                        ],
                    )?
                };
                let (sid, window, pane) = parse_ids(&output)?;
                if session.is_some_and(|expected| expected != sid) {
                    return Err("tmuxx returned a window in a different session".into());
                }
                session = Some(sid);
                tabs.insert(tab.id, window);
                panes.insert(first, pane);
                let target = format!("${sid}:@{window}");
                for option in ["automatic-rename", "allow-rename", "pane-border-status"] {
                    command(send, &["set-window-option", "-t", &target, option, "off"])?;
                }
                command(
                    send,
                    &[
                        "resize-window",
                        "-t",
                        &target,
                        "-x",
                        &size.0.to_string(),
                        "-y",
                        &size.1.to_string(),
                    ],
                )?;
                let layout = copy_tree(send, tab, &tab.root, pane, size, (0, 0), &mut panes)?;
                command(
                    send,
                    &[
                        "select-layout",
                        "-t",
                        &target,
                        &layout.to_json_v2().map_err(|error| error.to_string())?,
                    ],
                )?;
                // resize-window temporarily forces manual sizing while copying the layout.
                command(
                    send,
                    &["set-window-option", "-u", "-t", &target, "window-size"],
                )?;
                for (source, saved) in &tab.panes {
                    if let Some(title) = saved.title_override.as_ref().or(saved.title.as_ref()) {
                        command(
                            send,
                            &[
                                "select-pane",
                                "-t",
                                &format!("%{}", panes[source]),
                                "-T",
                                &format_literal(title),
                            ],
                        )?;
                    }
                }
                // shortcut: quadrant zoom needs tmuxx subtree zoom support before it can be copied.
                let zoom = tab.zoom.and_then(|id| panes.get(&id));
                let active = zoom.unwrap_or(&panes[&tab.focused]);
                command(send, &["select-pane", "-t", &format!("%{active}")])?;
                if zoom.is_some() {
                    command(send, &["resize-pane", "-Z", "-t", &format!("%{active}")])?;
                }
            }
            command(
                send,
                &[
                    "select-window",
                    "-t",
                    &format!(
                        "${}:@{}",
                        session.unwrap(),
                        tabs[&self.window.tabs[self.window.active_tab].id]
                    ),
                ],
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            return Err(match session {
                Some(session) => {
                    match command(send, &["kill-session", "-t", &format!("${session}")]) {
                        Ok(_) => {
                            format!("Copy failed: {error}. The incomplete new session was removed.")
                        }
                        Err(cleanup) => format!(
                            "Copy failed: {error}. Session {name:?} (${session}) may be incomplete; cleanup was not confirmed: {cleanup}"
                        ),
                    }
                }
                None => format!(
                    "Copy failed: {error}. Creation of session {name:?} could not be confirmed; check for an incomplete session before retrying."
                ),
            });
        }
        Ok(CopiedWindow {
            session: session.unwrap(),
            window: self.window,
            tabs,
            panes,
        })
    }
}

fn command(
    send: &mut impl FnMut(Vec<Vec<u8>>) -> Result<Vec<u8>, String>,
    args: &[&str],
) -> Result<Vec<u8>, String> {
    send(args.iter().map(|arg| argument(arg)).collect())
}

fn argument(value: &str) -> Vec<u8> {
    let mut bytes = value.as_bytes().to_vec();
    // Native argv still treats a trailing semicolon as a command separator.
    if bytes.last() == Some(&b';') {
        bytes.insert(bytes.len() - 1, b'\\');
    }
    bytes
}

fn format_literal(value: &str) -> String {
    value.replace('#', "##")
}

fn parse_ids(output: &[u8]) -> Result<(u64, u64, u64), String> {
    let output = std::str::from_utf8(output).map_err(|_| "tmuxx returned invalid IDs")?;
    let parts: Vec<_> = output.split_whitespace().collect();
    let parse = |value: &str, prefix| value.strip_prefix(prefix).and_then(|id| id.parse().ok());
    if let [session, window, pane] = parts.as_slice()
        && let (Some(session), Some(window), Some(pane)) =
            (parse(session, '$'), parse(window, '@'), parse(pane, '%'))
    {
        return Ok((session, window, pane));
    }
    Err("tmuxx did not confirm the new session, window and pane IDs".into())
}

fn minimum(tree: &Tree) -> Result<Size, String> {
    let size = match &tree.kind {
        Node::Empty => return Err("Copying tabs without tiled panes is not supported".into()),
        Node::Pane(_) => (1, 1),
        Node::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            if !ratio.is_finite() || !(0.0..=1.0).contains(ratio) {
                return Err("A tab contains an invalid split ratio".into());
            }
            let first = minimum(first)?;
            let second = minimum(second)?;
            match axis {
                Axis::Horizontal => (
                    first.0.saturating_add(second.0).saturating_add(1),
                    first.1.max(second.1),
                ),
                Axis::Vertical => (
                    first.0.max(second.0),
                    first.1.saturating_add(second.1).saturating_add(1),
                ),
            }
        }
    };
    if size.0 > MAX_DIMENSION || size.1 > MAX_DIMENSION {
        return Err("The pane layout exceeds tmuxx's 10000-cell window limit".into());
    }
    Ok(size)
}

fn first_pane(tree: &Tree) -> Id {
    match &tree.kind {
        Node::Pane(id) => *id,
        Node::Split { first, .. } => first_pane(first),
        Node::Empty => unreachable!("validated tiled tree"),
    }
}

fn split_sizes(
    axis: Axis,
    ratio: f32,
    size: Size,
    first: &Tree,
    second: &Tree,
) -> Result<(Size, Size), String> {
    let a = minimum(first)?;
    let b = minimum(second)?;
    let (extent, min_a, min_b) = match axis {
        Axis::Horizontal => (size.0, a.0, b.0),
        Axis::Vertical => (size.1, a.1, b.1),
    };
    let available = extent - 1;
    let first = ((f32::from(extent) * ratio - 0.5).round() as u16).clamp(min_a, available - min_b);
    let second = available - first;
    Ok(match axis {
        Axis::Horizontal => ((first, size.1), (second, size.1)),
        Axis::Vertical => ((size.0, first), (size.0, second)),
    })
}

fn copy_tree(
    send: &mut impl FnMut(Vec<Vec<u8>>) -> Result<Vec<u8>, String>,
    tab: &Tab,
    tree: &Tree,
    target: u64,
    size: Size,
    origin: (i32, i32),
    panes: &mut BTreeMap<Id, u64>,
) -> Result<Layout, String> {
    let mut layout = Layout::pane(
        target
            .try_into()
            .map_err(|_| "tmuxx pane ID exceeds the layout limit")?,
        size.0.into(),
        size.1.into(),
    );
    layout.geometry.x = origin.0;
    layout.geometry.y = origin.1;
    let Node::Split {
        axis,
        ratio,
        first,
        second,
    } = &tree.kind
    else {
        return Ok(layout);
    };
    let (a, b) = split_sizes(*axis, *ratio, size, first, second)?;
    let source = first_pane(second);
    let length = match axis {
        Axis::Horizontal => b.0,
        Axis::Vertical => b.1,
    };
    let output = command(
        send,
        &[
            "split-window",
            "-d",
            "-P",
            "-F",
            IDS,
            "-t",
            &format!("%{target}"),
            if *axis == Axis::Horizontal {
                "-h"
            } else {
                "-v"
            },
            "-l",
            &length.to_string(),
            "-c",
            tab.panes[&source].working_directory.to_str().unwrap(),
        ],
    )?;
    let (_, _, pane) = parse_ids(&output)?;
    panes.insert(source, pane);
    let second_origin = match axis {
        Axis::Horizontal => (origin.0 + i32::from(a.0) + 1, origin.1),
        Axis::Vertical => (origin.0, origin.1 + i32::from(a.1) + 1),
    };
    // split-window flattens matching axes; restore the original subtree grouping afterward.
    layout.kind = LayoutKind::Split {
        axis: match axis {
            Axis::Horizontal => LayoutAxis::Horizontal,
            Axis::Vertical => LayoutAxis::Vertical,
        },
        children: vec![
            copy_tree(send, tab, first, target, a, origin, panes)?,
            copy_tree(send, tab, second, pane, b, second_origin, panes)?,
        ],
    };
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::SavedPane;
    use std::path::Path;

    fn sample(directory: &Path) -> WindowCopy {
        let mut tab = Tab::new(10, 1, directory.to_owned());
        tab.title = Some("editor #{session_name};".into());
        tab.root = Tree {
            id: 12,
            kind: Node::Split {
                axis: Axis::Horizontal,
                ratio: 0.3,
                first: Box::new(Tree::leaf(1)),
                second: Box::new(Tree {
                    id: 13,
                    kind: Node::Split {
                        axis: Axis::Vertical,
                        ratio: 0.84,
                        first: Box::new(Tree::leaf(2)),
                        second: Box::new(Tree::leaf(3)),
                    },
                }),
            },
        };
        for id in 1..=3 {
            tab.panes.insert(
                id,
                SavedPane {
                    working_directory: directory.to_owned(),
                    title: Some(format!("pane {id} #{{pane_id}};")),
                    title_override: None,
                },
            );
        }
        tab.focused = 3;
        tab.zoom = Some(3);
        let mut second = Tab::new(20, 4, directory.to_owned());
        second.title = Some("build #(printf changed);".into());
        WindowCopy {
            window: WindowState {
                id: 30,
                tabs: vec![tab, second],
                active_tab: 1,
                frame: [0.0, 0.0, 800.0, 600.0],
                quick: false,
            },
            size: (80, 24),
        }
    }

    #[test]
    fn split_rounding_reserves_space_for_descendants_and_rejects_invalid_metadata() {
        let mut copy = sample(&std::env::current_dir().unwrap());
        assert!(copy.validate().is_ok());
        let root = &copy.window.tabs[0].root;
        assert_eq!(minimum(root).unwrap(), (3, 3));
        let Node::Split { first, second, .. } = &root.kind else {
            unreachable!()
        };
        assert_eq!(
            split_sizes(Axis::Horizontal, 0.99, (3, 3), first, second).unwrap(),
            ((1, 3), (1, 3))
        );
        let Node::Split { first, second, .. } = &second.kind else {
            unreachable!()
        };
        assert_eq!(
            split_sizes(Axis::Vertical, 0.84, (55, 24), first, second).unwrap(),
            ((55, 20), (55, 3))
        );
        copy.window.tabs[0].panes.remove(&2);
        assert!(copy.validate().unwrap_err().contains("pane references"));
        copy.window.tabs.remove(0);
        copy.window.active_tab = 0;
        copy.window.tabs[0]
            .panes
            .get_mut(&4)
            .unwrap()
            .working_directory = std::env::current_dir()
            .unwrap()
            .join("missing-rustty-copy-directory");
        assert!(copy.validate().unwrap_err().contains("working directory"));
        copy.window.tabs[0].root = Tree::empty(4);
        assert!(copy.validate().unwrap_err().contains("without tiled panes"));
    }

    #[test]
    fn untrusted_arguments_keep_format_expressions_and_trailing_semicolons_literal() {
        assert_eq!(argument("a;"), b"a\\;");
        assert_eq!(argument("a\\;"), b"a\\\\;");
        assert_eq!(argument("a; kill-session"), b"a; kill-session");
        assert_eq!(
            format_literal("#{pane_id} #(printf changed);"),
            "##{pane_id} ##(printf changed);"
        );
        assert_eq!(parse_ids(b"$9 @31 %407\n").unwrap(), (9, 31, 407));
        assert!(parse_ids(b"9 31 407").is_err());
    }

    #[test]
    fn failures_remove_only_the_acknowledged_new_session() {
        let mut commands = Vec::new();
        let error = sample(&std::env::current_dir().unwrap())
            .run_with("copy", &mut |args| {
                commands.push(args.clone());
                match args[0].as_slice() {
                    b"new-session" => Ok(b"$8 @25 %97\n".to_vec()),
                    b"kill-session" => Ok(Vec::new()),
                    _ => Err("deliberate failure".into()),
                }
            })
            .unwrap_err();
        assert!(error.contains("incomplete new session was removed"));
        assert_eq!(
            commands.last().unwrap(),
            &[b"kill-session".to_vec(), b"-t".to_vec(), b"$8".to_vec()]
        );

        let mut calls = 0;
        let error = sample(&std::env::current_dir().unwrap())
            .run_with("uncertain", &mut |_| {
                calls += 1;
                Err("connection lost".into())
            })
            .unwrap_err();
        assert_eq!(calls, 1);
        assert!(error.contains("uncertain"));
        assert!(error.contains("could not be confirmed"));
    }

    #[cfg(unix)]
    #[test]
    fn native_copy_preserves_tabs_layout_directories_titles_and_focus() {
        use rustty::tmuxx::{Graph, Layout};
        use std::sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        };
        use std::time::Instant;

        fn wait(connection: &Connection, predicate: impl Fn(&Graph) -> bool) -> Graph {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(graph) = connection.graph()
                    && predicate(&graph)
                {
                    return graph;
                }
                assert!(
                    Instant::now() < deadline,
                    "graph did not catch up: {:?}",
                    connection.errors()
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::path::PathBuf::from(format!(
            "/tmp/rustty-copy-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let cwd = directory.join("work #{pane_id};");
        std::fs::create_dir(&cwd).unwrap();
        let connection = Connection::embedded_with_command(
            &directory.join("server.native"),
            Arc::new(|| {}),
            vec![b"/bin/sleep".to_vec(), b"60".to_vec()],
        )
        .unwrap();
        let original = wait(&connection, |graph| graph.sessions.len() == 1);
        let worker = Connection::native_commands(
            &directory.join("server.native"),
            &connection.instance(),
            Arc::new(|| {}),
        )
        .unwrap();
        let mut send = |argv| {
            worker
                .command_with_reply(argv)
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .map_err(|e| e.to_string())
        };
        for (option, value) in [
            ("default-shell", "/bin/sh"),
            ("default-command", ""),
            ("base-index", "7"),
            ("window-size", "largest"),
        ] {
            command(&mut send, &["set-option", "-g", option, value]).unwrap();
        }
        command(
            &mut send,
            &["set-window-option", "-g", "pane-border-status", "top"],
        )
        .unwrap();
        let mut copy = sample(&cwd);
        copy.window.tabs[1]
            .panes
            .get_mut(&4)
            .unwrap()
            .working_directory = directory.clone();
        let saved = serde_json::to_value(&copy.window).unwrap();
        let copied = copy.run(&worker, "copy #{pane_id};").unwrap();
        assert_eq!(serde_json::to_value(&copied.window).unwrap(), saved);
        let graph = wait(&connection, |graph| {
            graph.sessions.iter().any(|s| {
                s.id == copied.session && s.windows.get(&(s.active as u32)) == copied.tabs.get(&20)
            })
        });
        let source = &original.sessions[0];
        let retained = graph.sessions.iter().find(|s| s.id == source.id).unwrap();
        assert_eq!(retained.windows, source.windows);
        assert_eq!(retained.active, source.active);
        let new = graph
            .sessions
            .iter()
            .find(|s| s.id == copied.session)
            .unwrap();
        assert_eq!(new.name, "copy #{pane_id};");
        assert_eq!(new.windows.keys().copied().collect::<Vec<_>>(), [7, 8]);
        for tab in &copied.window.tabs {
            let window = graph
                .windows
                .iter()
                .find(|w| w.id == copied.tabs[&tab.id])
                .unwrap();
            assert_eq!(&window.name, tab.title.as_ref().unwrap());
            assert_eq!(window.active, copied.panes[&tab.focused]);
            assert_eq!(window.zoomed, tab.zoom.is_some());
            assert_eq!((window.cols, window.rows), (80, 24));
            assert_eq!(
                command(
                    &mut send,
                    &[
                        "display-message",
                        "-p",
                        "-t",
                        &format!("@{}", window.id),
                        "#{window-size}",
                    ],
                )
                .unwrap(),
                b"largest\n"
            );
        }
        let first = graph
            .windows
            .iter()
            .find(|w| w.id == copied.tabs[&10])
            .unwrap();
        let layout = Layout::parse(&first.layout).unwrap();
        for (source, size, position) in [
            (1, (24, 24), (0, 0)),
            (2, (55, 20), (25, 0)),
            (3, (55, 3), (25, 21)),
        ] {
            let pane = layout.find(copied.panes[&source] as u32).unwrap();
            assert_eq!((pane.geometry.width, pane.geometry.height), size);
            assert_eq!((pane.geometry.x, pane.geometry.y), position);
        }
        for (source, saved) in copied.window.tabs.iter().flat_map(|tab| &tab.panes) {
            let output = command(
                &mut send,
                &[
                    "display-message",
                    "-p",
                    "-t",
                    &format!("%{}", copied.panes[source]),
                    "#{pane_current_path}\n#{pane_title}",
                ],
            )
            .unwrap();
            let text = String::from_utf8(output).unwrap();
            assert!(
                text.starts_with(&format!("{}\n", saved.working_directory.display())),
                "{text}"
            );
            if let Some(title) = &saved.title {
                assert!(text.ends_with(&format!("{title}\n")), "{text}");
            }
        }
        for size in [(100, 40), (60, 20)] {
            connection
                .view_size(copied.session, size.0, size.1, 8, 16)
                .unwrap();
            wait(&connection, |graph| {
                copied.tabs.values().all(|id| {
                    graph
                        .windows
                        .iter()
                        .any(|window| window.id == *id && (window.cols, window.rows) == size)
                })
            });
        }

        let mut tiny = sample(&cwd);
        tiny.size = (1, 1);
        let tiny = tiny.run(&worker, "tiny").unwrap();
        let graph = wait(&connection, |g| {
            g.windows
                .iter()
                .any(|w| w.id == tiny.tabs[&10] && w.panes.len() == 3)
        });
        let window = graph
            .windows
            .iter()
            .find(|w| w.id == tiny.tabs[&10])
            .unwrap();
        assert_eq!((window.cols, window.rows), (3, 3));

        let mut grouped = sample(&cwd);
        grouped.window.tabs[0].root = Tree {
            id: 12,
            kind: Node::Split {
                axis: Axis::Horizontal,
                ratio: 0.7,
                first: Box::new(Tree {
                    id: 13,
                    kind: Node::Split {
                        axis: Axis::Horizontal,
                        ratio: 0.25,
                        first: Box::new(Tree::leaf(1)),
                        second: Box::new(Tree::leaf(2)),
                    },
                }),
                second: Box::new(Tree::leaf(3)),
            },
        };
        grouped.window.tabs[0]
            .panes
            .get_mut(&2)
            .unwrap()
            .working_directory = directory.clone();
        let grouped = grouped.run(&worker, "grouped").unwrap();
        let graph = wait(&connection, |g| {
            g.sessions
                .iter()
                .any(|s| s.id == grouped.session && s.windows.len() == 2)
        });
        let window = graph
            .windows
            .iter()
            .find(|w| w.id == grouped.tabs[&10])
            .unwrap();
        let layout = Layout::parse(&window.layout).unwrap();
        for (source, width, x) in [(1, 14, 0), (2, 41, 15), (3, 23, 57)] {
            let pane = layout.find(grouped.panes[&source] as u32).unwrap();
            assert_eq!((pane.geometry.width, pane.geometry.x), (width, x));
        }
        let mut mapping = crate::tmuxx_workspace::Mapping::default();
        let mut workspace = crate::workspace::Workspace::default();
        mapping
            .apply(&mut workspace, &graph, &[grouped.session].into())
            .unwrap();
        let tab = workspace
            .windows
            .iter()
            .flat_map(|w| &w.tabs)
            .find(|tab| mapping.windows.get(&tab.id) == grouped.tabs.get(&10))
            .unwrap();
        let Node::Split {
            axis: Axis::Horizontal,
            first,
            second: third,
            ..
        } = &tab.root.kind
        else {
            panic!("lost root grouping");
        };
        let Node::Split {
            axis: Axis::Horizontal,
            first,
            second,
            ..
        } = &first.kind
        else {
            panic!("left subtree was flattened");
        };
        for (node, source) in [(first, 1), (second, 2), (third, 3)] {
            let Node::Pane(id) = node.kind else {
                panic!("expected a leaf");
            };
            assert_eq!(mapping.panes[&id], grouped.panes[&source]);
            let output = command(
                &mut send,
                &[
                    "display-message",
                    "-p",
                    "-t",
                    &format!("%{}", grouped.panes[&source]),
                    "#{pane_current_path}",
                ],
            )
            .unwrap();
            assert_eq!(
                String::from_utf8(output).unwrap().trim_end(),
                grouped.window.tabs[0].panes[&source]
                    .working_directory
                    .to_str()
                    .unwrap()
            );
        }

        let error = sample(&cwd)
            .run_with("rollback", &mut |argv| {
                if argv[0] == b"split-window" {
                    Err("deliberate split failure".into())
                } else {
                    send(argv)
                }
            })
            .unwrap_err();
        assert!(error.contains("incomplete new session was removed"));
        wait(&connection, |g| {
            g.sessions.iter().all(|s| s.name != "rollback")
        });
        assert!(
            connection
                .graph()
                .unwrap()
                .sessions
                .iter()
                .any(|s| s.id == source.id)
        );
        assert!(
            worker.graph().is_none(),
            "copy worker retained pane snapshots"
        );
    }
}
