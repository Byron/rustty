//! TUI toasts share COM routing, but have separate tokens, tags, and lifecycle state.

use super::{APP_USER_MODEL_ID, EventSink, PlatformEvent, err, xml_text};
use crate::platform::TuiNotificationEvent;
use rustty::vt::notification::Message;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};
use windows::{
    Data::Xml::Dom::XmlDocument,
    Foundation::TypedEventHandler,
    UI::Notifications::{
        NotificationSetting, ToastDismissalReason, ToastDismissedEventArgs, ToastFailedEventArgs,
        ToastNotification, ToastNotificationManager, ToastNotifier,
    },
    Win32::{Foundation::HWND, UI::WindowsAndMessaging::RegisterWindowMessageW},
    core::{HSTRING, PCWSTR},
};

const LIMIT: usize = 128;

pub(super) fn activation_message(button: u8) -> u32 {
    static IDS: OnceLock<[u32; 4]> = OnceLock::new();
    let ids = IDS.get_or_init(|| {
        std::array::from_fn(|button| {
            let name: Vec<u16> = format!("app.rustty.tui-toast-activated.v1.{button}")
                .encode_utf16()
                .chain([0])
                .collect();
            unsafe { RegisterWindowMessageW(PCWSTR(name.as_ptr())) }
        })
    });
    ids.get(usize::from(button)).copied().unwrap_or(0)
}

pub(super) fn message_button(message: u32) -> Option<u8> {
    (message != 0)
        .then(|| (0..=3).find(|button| activation_message(*button) == message))
        .flatten()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub process: u32,
    pub window: u64,
    pub token: u64,
    pub nonce: u64,
    pub button: u8,
}

impl Target {
    fn arguments(self) -> String {
        format!(
            "rustty-tui:{}:{:x}:{:x}:{:x}:{}",
            self.process, self.window, self.token, self.nonce, self.button
        )
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        let mut fields = value.strip_prefix("rustty-tui:")?.split(':');
        let process = fields.next()?.parse::<u32>().ok()?;
        let mut number = || {
            let value = fields.next()?;
            if value.is_empty() || value.len() > 16 {
                return None;
            }
            u64::from_str_radix(value, 16).ok()
        };
        let window = number()?;
        let token = number()?;
        let nonce = number()?;
        let button = fields.next()?.parse::<u8>().ok()?;
        if process == 0 || button > 3 || fields.next().is_some() {
            return None;
        }
        Some(Self {
            process,
            window,
            token,
            nonce,
            button,
        })
    }
}

struct Watch {
    notifier: ToastNotifier,
    toast: ToastNotification,
    dismissed: i64,
    failed: i64,
}
impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.toast.RemoveDismissed(self.dismissed);
        let _ = self.toast.RemoveFailed(self.failed);
        // History removal alone does not dismiss a visible toast banner. Stop
        // callbacks first so our own Hide cannot be mistaken for user dismissal.
        let _ = self.notifier.Hide(&self.toast);
    }
}
struct Entry {
    buttons: usize,
    pending: bool,
    untracked: bool,
    _watch: Option<Watch>,
}

pub(super) struct State {
    entries: Mutex<HashMap<u64, Entry>>,
    callback: EventSink,
    group: HSTRING,
}

impl State {
    pub(super) fn new(callback: EventSink, nonce: u64) -> Self {
        Self {
            entries: Mutex::default(),
            callback,
            // Windows limits toast groups and tags to sixteen characters.
            group: format!("{nonce:016x}").into(),
        }
    }

    pub(super) fn tokens(&self) -> Vec<u64> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keys()
            .copied()
            .collect()
    }

    pub(super) fn notify(
        self: &Arc<Self>,
        notifier: &ToastNotifier,
        window: HWND,
        nonce: u64,
        token: u64,
        message: &Message,
    ) -> Result<(), String> {
        if message.buttons.len() > 3 {
            return Err("notifications support at most three buttons".into());
        }
        if notifier.Setting().map_err(err)? != NotificationSetting::Enabled {
            return Err("Windows notifications are disabled".into());
        }
        if (0..=3).any(|button| activation_message(button) == 0) {
            return Err("Could not register TUI notification activation message".into());
        }
        let target = Target {
            process: std::process::id(),
            window: window.0 as usize as u64,
            token,
            nonce,
            button: 0,
        };
        let document = XmlDocument::new().map_err(err)?;
        document
            .LoadXml(&HSTRING::from(xml(target, message)))
            .map_err(err)?;
        let toast = ToastNotification::CreateToastNotification(&document).map_err(err)?;
        toast
            .SetTag(&HSTRING::from(format!("{token:x}")))
            .map_err(err)?;
        toast.SetGroup(&self.group).map_err(err)?;
        toast.SetExpiresOnReboot(true).map_err(err)?;
        let weak = Arc::downgrade(self);
        let dismissed = toast
            .Dismissed(&TypedEventHandler::<
                ToastNotification,
                ToastDismissedEventArgs,
            >::new(move |_, args| {
                if let Some(state) = weak.upgrade() {
                    if let Some(args) = args.as_ref() {
                        state.dismissed(token, args.Reason()?);
                    } else {
                        state.event(token, TuiNotificationEvent::Untracked);
                    }
                }
                Ok(())
            }))
            .map_err(err)?;
        let weak = Arc::downgrade(self);
        let failed = match toast.Failed(
            &TypedEventHandler::<ToastNotification, ToastFailedEventArgs>::new(move |_, _| {
                if let Some(state) = weak.upgrade() {
                    state.event(token, TuiNotificationEvent::Failed);
                }
                Ok(())
            }),
        ) {
            Ok(failed) => failed,
            Err(error) => {
                let _ = toast.RemoveDismissed(dismissed);
                return Err(err(error));
            }
        };
        let watch = Watch {
            notifier: notifier.clone(),
            toast: toast.clone(),
            dismissed,
            failed,
        };
        {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if entries.len() >= LIMIT || entries.contains_key(&token) {
                return Err("notification token limit or duplicate token".into());
            }
            entries.insert(
                token,
                Entry {
                    buttons: message.buttons.len(),
                    pending: true,
                    untracked: false,
                    _watch: Some(watch),
                },
            );
        }
        if let Err(error) = notifier.Show(&toast) {
            let removed = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&token);
            drop(removed);
            return Err(format!("Could not display Windows notification: {error}"));
        }
        Ok(())
    }

    pub(super) fn activated(&self, token: u64, button: u8) {
        self.event(token, TuiNotificationEvent::Activated { button });
    }

    fn dismissed(&self, token: u64, reason: ToastDismissalReason) {
        if reason == ToastDismissalReason::TimedOut {
            if let Some(entry) = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get_mut(&token)
            {
                entry.pending = false;
            }
        } else {
            self.event(token, TuiNotificationEvent::Closed);
        }
    }

    fn event(&self, token: u64, event: TuiNotificationEvent) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(entry) = entries.get_mut(&token) else {
            return;
        };
        if let TuiNotificationEvent::Activated { button } = event
            && usize::from(button) > entry.buttons
        {
            return;
        }
        if event == TuiNotificationEvent::Untracked {
            if entry.untracked {
                return;
            }
            entry.untracked = true;
        } else {
            // Dropping the Watch outside the mutex avoids COM callback reentrancy.
            let removed = entries.remove(&token);
            drop(entries);
            drop(removed);
            (self.callback)(PlatformEvent::TuiNotification { token, event });
            return;
        }
        drop(entries);
        (self.callback)(PlatformEvent::TuiNotification { token, event });
    }

    pub(super) fn close(&self, token: u64) {
        let removed = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&token);
        drop(removed);
        if let Ok(history) = ToastNotificationManager::History() {
            let _ = history.RemoveGroupedTagWithId(
                &HSTRING::from(format!("{token:x}")),
                &self.group,
                &HSTRING::from(APP_USER_MODEL_ID),
            );
        }
    }

    pub(super) fn query(&self, token: u64, tokens: &[u64]) -> Result<(), String> {
        if tokens.len() > LIMIT {
            return Err("too many notification tokens".into());
        }
        let history = ToastNotificationManager::History()
            .map_err(err)?
            .GetHistoryWithId(&HSTRING::from(APP_USER_MODEL_ID))
            .map_err(err)?;
        let mut observed = Vec::new();
        for toast in history {
            if toast.Group().map_err(err)? == self.group
                && let Ok(token) = u64::from_str_radix(&toast.Tag().map_err(err)?.to_string(), 16)
            {
                observed.push(token);
            }
        }
        let alive = self.alive(tokens, &observed);
        (self.callback)(PlatformEvent::TuiNotificationsAlive { token, alive });
        Ok(())
    }

    fn alive(&self, requested: &[u64], observed: &[u64]) -> Vec<u64> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut alive = Vec::new();
        for token in requested {
            if let Some(entry) = entries.get_mut(token) {
                let present = observed.contains(token);
                if present {
                    entry.pending = false;
                }
                if !alive.contains(token) && (entry.pending || present) {
                    alive.push(*token);
                }
            }
        }
        alive
    }
}

fn xml(target: Target, message: &Message) -> String {
    let mut xml = format!(
        "<toast launch=\"{}\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>",
        target.arguments(),
        xml_text(&message.title),
        xml_text(&message.body)
    );
    if !message.buttons.is_empty() {
        xml.push_str("<actions>");
        for (index, label) in message.buttons.iter().enumerate() {
            xml.push_str(&format!(
                "<action content=\"{}\" arguments=\"{}\" activationType=\"foreground\"/>",
                xml_text(label),
                Target {
                    button: index as u8 + 1,
                    ..target
                }
                .arguments()
            ));
        }
        xml.push_str("</actions>");
    }
    xml.push_str(if message.sound {
        "<audio src=\"ms-winsoundevent:Notification.Default\"/>"
    } else {
        "<audio silent=\"true\"/>"
    });
    xml.push_str("</toast>");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> Message {
        Message {
            identifier: None,
            title: "Build <done> 🦀".into(),
            body: "A & B".into(),
            buttons: vec!["Open \"file\"".into(), "Later".into()],
            focus: true,
            report: true,
            close_report: true,
            sound: false,
        }
    }

    type Events = Arc<Mutex<Vec<PlatformEvent>>>;
    fn state() -> (Arc<State>, Events) {
        let events = Events::default();
        let capture = events.clone();
        (
            Arc::new(State::new(
                Arc::new(move |event| capture.lock().unwrap().push(event)),
                123,
            )),
            events,
        )
    }

    fn insert(state: &State, token: u64) {
        state.entries.lock().unwrap().insert(
            token,
            Entry {
                buttons: 2,
                pending: true,
                untracked: false,
                _watch: None,
            },
        );
    }

    #[test]
    fn tui_launch_and_buttons_require_complete_instance_scoped_targets() {
        let target = Target {
            process: 123,
            window: 0xabc,
            token: 42,
            nonce: 0x1234,
            button: 2,
        };
        assert_eq!(Target::parse(&target.arguments()), Some(target));
        for invalid in [
            "rustty:123:abc:2a:1234",
            "rustty-tui:0:abc:2a:1234:2",
            "rustty-tui:123:abc:2a:1234:4",
            "rustty-tui:123:abc:2a:1234:2:extra",
            "rustty-tui:123:abc:2a:12345678901234567:2",
        ] {
            assert_eq!(Target::parse(invalid), None);
        }
        let xml = xml(
            Target {
                button: 0,
                ..target
            },
            &message(),
        );
        assert!(xml.contains("Build &lt;done&gt; 🦀"));
        assert!(xml.contains("A &amp; B"));
        assert!(xml.contains("content=\"Open &quot;file&quot;\""));
        assert!(xml.contains("arguments=\"rustty-tui:123:abc:2a:1234:1\""));
        assert!(xml.contains("arguments=\"rustty-tui:123:abc:2a:1234:2\""));
        assert!(xml.contains("<audio silent=\"true\"/>"));
        assert_ne!(
            State::new(Arc::new(|_| {}), 123).group,
            State::new(Arc::new(|_| {}), 124).group
        );
    }

    #[test]
    fn tui_lifecycle_preserves_pending_and_timed_out_action_center_entries() {
        let (state, events) = state();
        for token in 1..=5 {
            insert(&state, token);
        }
        assert_eq!(state.alive(&[1, 2, 1, 99], &[]), [1, 2]);
        assert_eq!(state.alive(&[1, 2], &[1]), [1, 2]);
        assert_eq!(state.alive(&[1, 2], &[]), [2]);
        state.dismissed(2, ToastDismissalReason::TimedOut);
        assert!(events.lock().unwrap().is_empty());
        assert_eq!(state.alive(&[2], &[2]), [2]);
        state.event(2, TuiNotificationEvent::Untracked);
        state.event(2, TuiNotificationEvent::Untracked);
        state.activated(2, 2);
        state.dismissed(2, ToastDismissalReason::UserCanceled);
        state.dismissed(3, ToastDismissalReason::UserCanceled);
        state.event(4, TuiNotificationEvent::Failed);
        // Explicit close invalidates the token before native cleanup can
        // reenter through dismissal or activation, and remains idempotent.
        state.close(5);
        state.close(5);
        state.dismissed(5, ToastDismissalReason::ApplicationHidden);
        state.activated(5, 0);
        assert!(state.alive(&[5], &[5]).is_empty());
        assert!(matches!(
            &events.lock().unwrap()[..],
            [
                PlatformEvent::TuiNotification {
                    token: 2,
                    event: TuiNotificationEvent::Untracked
                },
                PlatformEvent::TuiNotification {
                    token: 2,
                    event: TuiNotificationEvent::Activated { button: 2 }
                },
                PlatformEvent::TuiNotification {
                    token: 3,
                    event: TuiNotificationEvent::Closed
                },
                PlatformEvent::TuiNotification {
                    token: 4,
                    event: TuiNotificationEvent::Failed
                }
            ]
        ));
    }

    #[test]
    fn tui_native_com_and_forwarded_messages_validate_nonce_tokens_and_button_bounds() {
        use super::super::{Activation, BRIDGE, Bridge, Panes, message_hook};
        use windows::{
            Win32::{
                Foundation::{LPARAM, WPARAM},
                UI::{Notifications::INotificationActivationCallback, WindowsAndMessaging::MSG},
            },
            core::PCWSTR,
        };
        let (state, events) = state();
        insert(&state, 42);
        let callback: INotificationActivationCallback = Activation {
            callback: Arc::new(|_| {}),
            panes: Panes::default(),
            token: 123,
            tui: state.clone(),
            completed: None,
        }
        .into();
        let app: Vec<_> = APP_USER_MODEL_ID.encode_utf16().chain([0]).collect();
        let target = Target {
            process: std::process::id(),
            window: 0,
            token: 42,
            nonce: 124,
            button: 1,
        };
        let activate = |target: Target| {
            let arguments: Vec<_> = target.arguments().encode_utf16().chain([0]).collect();
            unsafe { callback.Activate(PCWSTR(app.as_ptr()), PCWSTR(arguments.as_ptr()), &[]) }
                .unwrap();
        };
        activate(target);
        activate(Target {
            nonce: 123,
            button: 3,
            ..target
        });
        assert!(events.lock().unwrap().is_empty());
        activate(Target {
            nonce: 123,
            button: 2,
            ..target
        });
        activate(Target {
            nonce: 123,
            button: 0,
            ..target
        });
        assert!(matches!(
            &events.lock().unwrap()[..],
            [PlatformEvent::TuiNotification {
                token: 42,
                event: TuiNotificationEvent::Activated { button: 2 }
            }]
        ));
        insert(&state, 43);
        BRIDGE.with(|slot| {
            *slot.borrow_mut() = Some(Bridge {
                callback: Arc::new(|_| {}),
                panes: Panes::default(),
                token: 123,
                tui: state.clone(),
            })
        });
        let mut message = MSG {
            message: activation_message(0),
            wParam: WPARAM(43),
            lParam: LPARAM(124),
            ..Default::default()
        };
        assert!(message_hook(&message));
        assert_eq!(events.lock().unwrap().len(), 1);
        message.lParam = LPARAM(123);
        assert!(message_hook(&message));
        assert!(message_hook(&message));
        assert!(matches!(
            &events.lock().unwrap()[1..],
            [PlatformEvent::TuiNotification {
                token: 43,
                event: TuiNotificationEvent::Activated { button: 0 }
            }]
        ));
        BRIDGE.with(|slot| slot.borrow_mut().take());
    }
}
