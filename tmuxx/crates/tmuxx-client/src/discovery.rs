//! Numeric discovery avoids putting unescaped names into line-delimited tables.
//! Retrieve names with `name_command`, which gives each object's bytes its own reply.

use crate::{Error, Pane, Session, Window};
use std::collections::BTreeMap;
use tmuxx_command::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryKind {
    Sessions,
    Windows,
    Panes,
}

impl DiscoveryKind {
    pub fn command(self) -> Command {
        let args: &[&[u8]] = match self {
            Self::Sessions => &[b"list-sessions", b"-F", b"#{session_id}"],
            Self::Windows => &[b"list-windows", b"-a", b"-F", b"#{session_id}|#{window_id}|#{window_index}|#{window_active}|#{window_layout}|#{window_visible_layout}|#{window_flags}"],
            Self::Panes => &[b"list-panes", b"-a", b"-F", b"#{pane_id}|#{window_id}|#{pane_width}|#{pane_height}|#{pane_left}|#{pane_top}|#{pane_active}|#{pane_modal_flag}"],
        };
        Command {
            blocks: Default::default(),
            args: args.iter().map(|arg| arg.to_vec()).collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoverySnapshot {
    pub sessions: BTreeMap<u64, Session>,
    pub windows: BTreeMap<u64, Window>,
    pub panes: BTreeMap<u64, Pane>,
}

impl DiscoverySnapshot {
    pub(crate) fn validate(&mut self) -> Result<(), Error> {
        for (&id, window) in &mut self.windows {
            if window.id != id {
                return Err(Error::InvalidDiscovery);
            }
            window.panes.clear();
        }
        for (&id, session) in &self.sessions {
            if session.id != id
                || session
                    .windows
                    .values()
                    .any(|id| !self.windows.contains_key(id))
                || session
                    .active_window
                    .is_some_and(|id| !session.windows.values().any(|linked| *linked == id))
                || session.active_index.is_some_and(|index| {
                    session.windows.get(&index).copied() != session.active_window
                })
            {
                return Err(Error::InvalidDiscovery);
            }
        }
        for (&id, pane) in &self.panes {
            if pane.id != id || pane.width == 0 || pane.height == 0 {
                return Err(Error::InvalidDiscovery);
            }
            let window = self
                .windows
                .get_mut(&pane.window_id)
                .ok_or(Error::InvalidDiscovery)?;
            window.panes.insert(id);
        }
        if self.windows.values().any(|window| {
            window
                .active_pane
                .is_some_and(|id| !window.panes.contains(&id))
        }) {
            return Err(Error::InvalidDiscovery);
        }
        Ok(())
    }
}

pub struct Discovery {
    snapshot: DiscoverySnapshot,
    active: BTreeMap<u64, u64>,
    max_objects: usize,
    failed: bool,
}

impl Default for Discovery {
    fn default() -> Self {
        Self::with_limit(100_000)
    }
}

impl Discovery {
    /// Bound unique sessions + windows + panes + window links. This is an explicit client policy.
    pub fn with_limit(max_objects: usize) -> Self {
        Self {
            snapshot: DiscoverySnapshot::default(),
            active: BTreeMap::new(),
            max_objects,
            failed: false,
        }
    }

    /// Feed command-output lines from each numeric discovery command.
    /// Feed sessions, then windows, then panes. An error invalidates this attempt.
    pub fn add_line(&mut self, kind: DiscoveryKind, line: &[u8]) -> Result<(), Error> {
        if self.failed {
            return Err(Error::InvalidDiscovery);
        }
        let result = self.add_inner(kind, line);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn add_inner(&mut self, kind: DiscoveryKind, line: &[u8]) -> Result<(), Error> {
        let fields: Vec<_> = line.splitn(9, |&b| b == b'|').collect();
        let invalid = Error::InvalidDiscovery;
        match kind {
            DiscoveryKind::Sessions => {
                if fields.len() != 1 {
                    return Err(invalid);
                }
                let id = prefixed(fields[0], b'$')?;
                self.snapshot.sessions.entry(id).or_insert_with(|| Session {
                    id,
                    ..Session::default()
                });
            }
            DiscoveryKind::Windows => {
                if fields.len() != 7 {
                    return Err(invalid);
                }
                let session_id = prefixed(fields[0], b'$')?;
                let id = prefixed(fields[1], b'@')?;
                let index = integer(fields[2])?;
                let active = boolean(fields[3])?;
                let session = self.snapshot.sessions.get_mut(&session_id).ok_or(invalid)?;
                if session
                    .windows
                    .insert(index, id)
                    .is_some_and(|old| old != id)
                {
                    return Err(Error::InvalidDiscovery);
                }
                if active {
                    if session.active_index.is_some_and(|old| old != index) {
                        return Err(Error::InvalidDiscovery);
                    }
                    session.active_index = Some(index);
                    session.active_window = Some(id);
                }
                let window = Window {
                    id,
                    layout: fields[4].to_vec(),
                    visible_layout: fields[5].to_vec(),
                    flags: fields[6].to_vec(),
                    ..Window::default()
                };
                if let Some(old) = self.snapshot.windows.get(&id) {
                    // Flags such as active/last are properties of the session link.
                    if old.layout != window.layout || old.visible_layout != window.visible_layout {
                        return Err(Error::InvalidDiscovery);
                    }
                } else {
                    self.snapshot.windows.insert(id, window);
                }
            }
            DiscoveryKind::Panes => {
                if !(7..=8).contains(&fields.len()) {
                    return Err(invalid);
                }
                let id = prefixed(fields[0], b'%')?;
                let window_id = prefixed(fields[1], b'@')?;
                let pane = Pane {
                    id,
                    window_id,
                    width: integer(fields[2])?,
                    height: integer(fields[3])?,
                    left: signed(fields[4])?,
                    top: signed(fields[5])?,
                    modal: match fields.get(7).copied() {
                        None | Some(b"") => false,
                        Some(value) => boolean(value)?,
                    },
                    paused: false,
                    needs_snapshot: true,
                };
                if pane.width == 0
                    || pane.height == 0
                    || i64::from(pane.left) + i64::from(pane.width) > i64::from(i32::MAX)
                    || i64::from(pane.top) + i64::from(pane.height) > i64::from(i32::MAX)
                    || !self.snapshot.windows.contains_key(&window_id)
                {
                    return Err(Error::InvalidDiscovery);
                }
                if boolean(fields[6])?
                    && self
                        .active
                        .insert(window_id, id)
                        .is_some_and(|old| old != id)
                {
                    return Err(Error::InvalidDiscovery);
                }
                if let Some(old) = self.snapshot.panes.get(&id) {
                    if old != &pane {
                        return Err(Error::InvalidDiscovery);
                    }
                } else {
                    self.snapshot.panes.insert(id, pane);
                }
            }
        }
        if self
            .snapshot
            .sessions
            .len()
            .saturating_add(self.snapshot.windows.len())
            .saturating_add(self.snapshot.panes.len())
            .saturating_add(
                self.snapshot
                    .sessions
                    .values()
                    .map(|s| s.windows.len())
                    .fold(0usize, usize::saturating_add),
            )
            > self.max_objects
        {
            return Err(Error::DiscoveryLimit);
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<DiscoverySnapshot, Error> {
        if self.failed {
            return Err(Error::InvalidDiscovery);
        }
        self.snapshot.validate()?;
        for (window_id, pane_id) in self.active {
            self.snapshot
                .windows
                .get_mut(&window_id)
                .ok_or(Error::InvalidDiscovery)?
                .active_pane = Some(pane_id);
        }
        Ok(self.snapshot)
    }
}

fn prefixed(bytes: &[u8], prefix: u8) -> Result<u64, Error> {
    if bytes.first() != Some(&prefix) {
        return Err(Error::InvalidDiscovery);
    }
    unsigned(&bytes[1..])
}

fn unsigned(bytes: &[u8]) -> Result<u64, Error> {
    if bytes.is_empty() {
        return Err(Error::InvalidDiscovery);
    }
    bytes.iter().try_fold(0_u64, |number, byte| {
        if !byte.is_ascii_digit() {
            return Err(Error::InvalidDiscovery);
        }
        number
            .checked_mul(10)
            .and_then(|n| n.checked_add(u64::from(byte - b'0')))
            .ok_or(Error::InvalidDiscovery)
    })
}

fn integer(bytes: &[u8]) -> Result<u32, Error> {
    unsigned(bytes)?
        .try_into()
        .map_err(|_| Error::InvalidDiscovery)
}

fn signed(bytes: &[u8]) -> Result<i32, Error> {
    let (negative, digits) = bytes
        .strip_prefix(b"-")
        .map_or((false, bytes), |digits| (true, digits));
    let value = i64::try_from(unsigned(digits)?).map_err(|_| Error::InvalidDiscovery)?;
    i32::try_from(if negative { -value } else { value }).map_err(|_| Error::InvalidDiscovery)
}

fn boolean(bytes: &[u8]) -> Result<bool, Error> {
    match bytes {
        b"0" => Ok(false),
        b"1" => Ok(true),
        _ => Err(Error::InvalidDiscovery),
    }
}
