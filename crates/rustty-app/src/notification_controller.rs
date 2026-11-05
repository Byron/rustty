//! OSC 99 lifetimes are scoped to their issuing terminal, separate from agent alerts.
use rustty::vt::notification::{Action, Command, Message, Reply};
use rustty_app::platform::TuiNotificationEvent;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
};

const MAX_LIVE: usize = 128;
const MAX_QUERIES: usize = 32;

fn token() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("notification tokens exhausted")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Native {
    Show(u64),
    Close(u64),
    Query { token: u64, requested: Vec<u64> },
}

#[derive(Debug)]
pub(super) enum Operation {
    Native(Native),
    Focus {
        scope: u64,
        pane: u64,
    },
    Reply {
        scope: u64,
        pane: u64,
        command: Command,
        reply: Reply,
    },
}

struct Live {
    scope: u64,
    pane: u64,
    command: Command,
    untracked: bool,
}

impl Live {
    fn reply(self, reply: Reply) -> Operation {
        Operation::Reply {
            scope: self.scope,
            pane: self.pane,
            command: self.command,
            reply,
        }
    }
}

struct Query {
    scope: u64,
    pane: u64,
    command: Command,
    requested: Vec<u64>,
}

impl Query {
    fn reply(self, reply: Reply) -> Operation {
        Operation::Reply {
            scope: self.scope,
            pane: self.pane,
            command: self.command,
            reply,
        }
    }
}

#[derive(Default)]
pub(super) struct Controller {
    live: BTreeMap<u64, Live>,
    identifiers: BTreeMap<(u64, String), u64>,
    queries: BTreeMap<u64, Query>,
}

impl Controller {
    pub fn is_empty(&self) -> bool {
        self.live.is_empty() && self.queries.is_empty()
    }

    pub fn message(&self, token: u64) -> Option<&Message> {
        match &self.live.get(&token)?.command.action {
            Action::Show(message) => Some(message),
            _ => None,
        }
    }

    fn remove(&mut self, token: u64) -> Option<Live> {
        let live = self.live.remove(&token)?;
        if let Action::Show(message) = &live.command.action
            && let Some(identifier) = &message.identifier
        {
            self.identifiers.remove(&(live.scope, identifier.clone()));
        }
        Some(live)
    }

    pub fn command(&mut self, scope: u64, pane: u64, command: Command) -> Vec<Operation> {
        let mut operations = Vec::new();
        match &command.action {
            Action::Show(message) => {
                if self
                    .live
                    .values()
                    .any(|live| live.scope == scope && live.command.serial == command.serial)
                {
                    return operations;
                }
                if let Some(identifier) = &message.identifier
                    && let Some(old) = self.identifiers.get(&(scope, identifier.clone())).copied()
                    && let Some(live) = self.remove(old)
                {
                    operations.push(Operation::Native(Native::Close(old)));
                    operations.push(live.reply(Reply::Finished));
                }
                if self.live.len() == MAX_LIVE {
                    let old = *self.live.first_key_value().unwrap().0;
                    let live = self.remove(old).unwrap();
                    operations.push(Operation::Native(Native::Close(old)));
                    operations.push(live.reply(Reply::Closed));
                }
                let token = token();
                if let Some(identifier) = &message.identifier {
                    self.identifiers.insert((scope, identifier.clone()), token);
                }
                self.live.insert(
                    token,
                    Live {
                        scope,
                        pane,
                        command,
                        untracked: false,
                    },
                );
                operations.push(Operation::Native(Native::Show(token)));
            }
            Action::Close { identifier } => {
                if let Some(old) = self.identifiers.get(&(scope, identifier.clone())).copied()
                    && let Some(live) = self.remove(old)
                {
                    operations.push(Operation::Native(Native::Close(old)));
                    operations.push(live.reply(Reply::Closed));
                }
                operations.push(Operation::Reply {
                    scope,
                    pane,
                    command,
                    reply: Reply::Finished,
                });
            }
            Action::Alive { .. } => {
                if self.queries.len() == MAX_QUERIES {
                    let (_, old) = self.queries.pop_first().unwrap();
                    operations.push(old.reply(Reply::Failed));
                }
                let token = token();
                let requested: Vec<_> = self
                    .live
                    .iter()
                    .filter_map(|(&token, live)| (live.scope == scope).then_some(token))
                    .collect();
                self.queries.insert(
                    token,
                    Query {
                        scope,
                        pane,
                        command,
                        requested: requested.clone(),
                    },
                );
                operations.push(Operation::Native(Native::Query { token, requested }));
            }
        }
        operations
    }

    pub fn event(&mut self, token: u64, event: TuiNotificationEvent) -> Vec<Operation> {
        let Some(live) = self.live.get_mut(&token) else {
            return Vec::new();
        };
        if event == TuiNotificationEvent::Untracked {
            if std::mem::replace(&mut live.untracked, true) {
                return Vec::new();
            }
            return vec![Operation::Reply {
                scope: live.scope,
                pane: live.pane,
                command: live.command.clone(),
                reply: Reply::Untracked,
            }];
        }
        let focus = if let TuiNotificationEvent::Activated { button } = event {
            let Action::Show(message) = &live.command.action else {
                unreachable!()
            };
            if usize::from(button) > message.buttons.len() {
                return Vec::new();
            }
            message.focus.then_some(Operation::Focus {
                scope: live.scope,
                pane: live.pane,
            })
        } else {
            None
        };
        let live = self.remove(token).unwrap();
        let reply = match event {
            TuiNotificationEvent::Activated { button } => Reply::Activated { button },
            TuiNotificationEvent::Closed => Reply::Closed,
            TuiNotificationEvent::Failed => Reply::Failed,
            TuiNotificationEvent::Untracked => unreachable!(),
        };
        let close = matches!(event, TuiNotificationEvent::Activated { .. })
            .then_some(Operation::Native(Native::Close(token)));
        close
            .into_iter()
            .chain(focus)
            .chain(Some(live.reply(reply)))
            .collect()
    }

    pub fn alive(&mut self, token: u64, alive: &[u64]) -> Vec<Operation> {
        let Some(query) = self.queries.remove(&token) else {
            return Vec::new();
        };
        let identifiers = query
            .requested
            .iter()
            .filter(|token| alive.contains(token))
            .filter_map(|token| self.live.get(token))
            .filter(|live| live.scope == query.scope)
            .filter_map(|live| match &live.command.action {
                Action::Show(message) => message.identifier.clone(),
                _ => None,
            })
            .collect();
        vec![query.reply(Reply::Alive { identifiers })]
    }

    pub fn query_failed(&mut self, token: u64) -> Vec<Operation> {
        self.queries
            .remove(&token)
            .map(|query| query.reply(Reply::Failed))
            .into_iter()
            .collect()
    }

    pub fn prune(&mut self, valid: impl Fn(u64) -> bool) -> Vec<Operation> {
        let stale: Vec<_> = self
            .live
            .iter()
            .filter_map(|(&token, live)| (!valid(live.scope)).then_some(token))
            .collect();
        self.queries.retain(|_, query| valid(query.scope));
        stale
            .into_iter()
            .map(|token| {
                self.remove(token);
                Operation::Native(Native::Close(token))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(serial: u64, identifier: Option<&str>) -> Command {
        Command {
            serial,
            action: Action::Show(Message {
                identifier: identifier.map(str::to_owned),
                title: "Build".into(),
                body: "Done".into(),
                buttons: vec!["Open".into(), "Dismiss".into()],
                focus: true,
                report: true,
                close_report: true,
                sound: false,
            }),
        }
    }
    fn shown(operations: &[Operation]) -> u64 {
        operations
            .iter()
            .find_map(|operation| match operation {
                Operation::Native(Native::Show(token)) => Some(*token),
                _ => None,
            })
            .unwrap()
    }
    fn wire(operations: &[Operation]) -> Vec<u8> {
        operations
            .iter()
            .flat_map(|operation| match operation {
                Operation::Reply { command, reply, .. } => command.reply(reply).unwrap(),
                _ => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn replacement_is_scoped_and_anonymous_notifications_never_replace() {
        let mut state = Controller::default();
        let old = shown(&state.command(10, 1, show(1, Some("job"))));
        let other = shown(&state.command(20, 2, show(2, Some("job"))));
        let replaced = state.command(10, 3, show(3, Some("job")));
        let current = shown(&replaced);
        assert!(matches!(replaced[0], Operation::Native(Native::Close(token)) if token == old));
        assert!(wire(&replaced).is_empty());
        assert!(
            state
                .event(old, TuiNotificationEvent::Activated { button: 0 })
                .is_empty()
        );
        assert!(state.message(other).is_some());
        assert_eq!(state.live.len(), 2);
        assert!(
            state.command(10, 9, show(3, Some("job"))).is_empty(),
            "same command through another view is not posted twice"
        );
        let a = shown(&state.command(10, 1, show(4, None)));
        let b = shown(&state.command(10, 1, show(5, None)));
        assert_ne!(a, b);
        assert_eq!(state.live.len(), 4);
        let close = state.command(
            10,
            3,
            Command {
                serial: 6,
                action: Action::Close {
                    identifier: "job".into(),
                },
            },
        );
        assert!(matches!(close[0], Operation::Native(Native::Close(token)) if token == current));
        assert_eq!(wire(&close), b"\x1b]99;i=job:p=close;\x1b\\");
        assert!(matches!(
            close.last(),
            Some(Operation::Reply {
                command: Command { serial: 6, .. },
                reply: Reply::Finished,
                ..
            })
        ));
        assert!(state.message(other).is_some());
    }

    #[test]
    fn untracked_activation_close_and_failure_consume_exactly_once() {
        let mut state = Controller::default();
        let mut command = show(1, Some("job"));
        let Action::Show(message) = &mut command.action else {
            unreachable!()
        };
        message.focus = false;
        let token = shown(&state.command(10, 1, command));
        assert_eq!(
            wire(&state.event(token, TuiNotificationEvent::Untracked)),
            b"\x1b]99;i=job:p=close;untracked\x1b\\"
        );
        assert!(
            state
                .event(token, TuiNotificationEvent::Untracked)
                .is_empty()
        );
        assert!(
            state
                .event(token, TuiNotificationEvent::Activated { button: 3 })
                .is_empty()
        );
        assert!(state.message(token).is_some());
        let activated = state.event(token, TuiNotificationEvent::Activated { button: 2 });
        assert_eq!(
            wire(&activated),
            b"\x1b]99;i=job;2\x1b\\\x1b]99;i=job:p=close;\x1b\\"
        );
        assert!(
            !activated
                .iter()
                .any(|op| matches!(op, Operation::Focus { .. }))
        );
        assert!(state.event(token, TuiNotificationEvent::Closed).is_empty());
        assert!(
            state
                .event(token, TuiNotificationEvent::Activated { button: 0 })
                .is_empty()
        );
        let token = shown(&state.command(10, 1, show(2, None)));
        let activated = state.event(token, TuiNotificationEvent::Activated { button: 0 });
        assert!(
            activated
                .iter()
                .any(|op| matches!(op, Operation::Focus { scope: 10, pane: 1 }))
        );
        assert_eq!(
            wire(&activated),
            b"\x1b]99;i=0;\x1b\\\x1b]99;i=0:p=close;\x1b\\"
        );
        let token = shown(&state.command(10, 1, show(3, Some("failed"))));
        assert_eq!(
            wire(&state.event(token, TuiNotificationEvent::Failed)),
            b"\x1b]99;i=failed:p=close;\x1b\\"
        );
        assert!(state.is_empty());
    }

    #[test]
    fn alive_queries_exclude_other_scopes_anonymous_and_replaced_tokens() {
        let mut state = Controller::default();
        let old = shown(&state.command(10, 1, show(1, Some("old"))));
        let anonymous = shown(&state.command(10, 1, show(2, None)));
        let other = shown(&state.command(20, 2, show(3, Some("other"))));
        let operations = state.command(
            10,
            1,
            Command {
                serial: 4,
                action: Action::Alive {
                    identifier: "query".into(),
                },
            },
        );
        let Operation::Native(Native::Query { token, requested }) = &operations[0] else {
            panic!()
        };
        assert_eq!(requested, &[old, anonymous]);
        let current = shown(&state.command(10, 1, show(5, Some("old"))));
        let later = shown(&state.command(10, 1, show(6, Some("later"))));
        assert_eq!(
            wire(&state.alive(*token, &[old, anonymous, other, current, later])),
            b"\x1b]99;i=query:p=alive;\x1b\\"
        );
        assert!(state.alive(*token, &[current]).is_empty());
        let operations = state.command(
            10,
            1,
            Command {
                serial: 7,
                action: Action::Alive {
                    identifier: "query2".into(),
                },
            },
        );
        let Operation::Native(Native::Query { token, .. }) = operations[0] else {
            panic!()
        };
        assert_eq!(
            wire(&state.alive(token, &[later, current, other])),
            b"\x1b]99;i=query2:p=alive;old,later\x1b\\"
        );
    }

    #[test]
    fn capacity_and_scope_cleanup_bound_pending_work_and_retire_callbacks() {
        let mut state = Controller::default();
        let old = shown(&state.command(10, 1, show(1, Some("first"))));
        for serial in 2..=128 {
            state.command(10, 1, show(serial, None));
        }
        let evicted = state.command(20, 2, show(129, None));
        assert_eq!(state.live.len(), MAX_LIVE);
        assert!(matches!(evicted[0], Operation::Native(Native::Close(token)) if token == old));
        assert_eq!(wire(&evicted), b"\x1b]99;i=first:p=close;\x1b\\");
        for serial in 130..162 {
            state.command(
                10,
                1,
                Command {
                    serial,
                    action: Action::Alive {
                        identifier: format!("q{serial}"),
                    },
                },
            );
        }
        let evicted = state.command(
            10,
            1,
            Command {
                serial: 162,
                action: Action::Alive {
                    identifier: "last".into(),
                },
            },
        );
        assert_eq!(state.queries.len(), MAX_QUERIES);
        assert_eq!(wire(&evicted), b"\x1b]99;i=q130:p=alive;\x1b\\");
        let stale = state.prune(|scope| scope == 20);
        assert_eq!(stale.len(), 127);
        assert!(
            wire(&stale).is_empty(),
            "a disconnected scope must not receive replies through its replacement"
        );
        assert!(state.queries.is_empty());
        assert_eq!(state.live.len(), 1);
        assert!(state.event(old, TuiNotificationEvent::Closed).is_empty());
    }
}
