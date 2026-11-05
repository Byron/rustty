//! Real pane routing with a fake native sink: no alerts or permission prompts.
use super::*;
use rustty_app::platform::TuiNotificationEvent;
use vt::notification::{Capabilities, Command, Message};

type Captures = Vec<(tui_notifications::Native, Option<Message>)>;

fn apply(
    app: &mut App,
    operations: Vec<tui_notifications::Operation>,
    captures: &mut Captures,
) -> Vec<u8> {
    let bytes = operations
        .iter()
        .flat_map(|operation| match operation {
            tui_notifications::Operation::Reply { command, reply, .. } => {
                command.reply(reply).unwrap()
            }
            _ => Vec::new(),
        })
        .collect();
    app.notification_operations_with(operations, |_, native, message| {
        captures.push((native.clone(), message.cloned()));
        Ok(())
    });
    bytes
}

fn command(app: &mut App, pane: Id, osc: &[u8], captures: &mut Captures) -> Result<Vec<u8>> {
    let command: Command = {
        let mut terminal = app.panes[&pane].session.terminal()?;
        terminal.notification_capabilities = Some(Capabilities {
            buttons: true,
            close_events: true,
        });
        terminal
            .feed(osc)
            .into_iter()
            .find_map(|effect| match effect {
                vt::Effect::DesktopNotification(command) => Some(command),
                _ => None,
            })
            .ok_or("missing desktop notification command")?
    };
    let scope = app.panes[&pane].session.notification_identity()?;
    let operations = app.tui_notifications.command(scope, pane, command);
    Ok(apply(app, operations, captures))
}

fn shown(captures: &Captures) -> u64 {
    captures
        .iter()
        .rev()
        .find_map(|(native, _)| match native {
            tui_notifications::Native::Show(token) => Some(*token),
            _ => None,
        })
        .unwrap()
}

pub(super) fn check(app: &mut App, event_loop: &ActiveEventLoop, directory: &Path) -> Result<()> {
    let window = *app
        .windows
        .keys()
        .next()
        .ok_or("no notification smoke window")?;
    let mut host = app.windows.remove(&window).unwrap();
    let first = app.focused(host.id).ok_or("no notification smoke pane")?;
    app.action(
        event_loop,
        &mut host,
        Action::NewSplit(Direction::Right),
        true,
    );
    let second = app
        .focused(host.id)
        .ok_or("notification smoke split failed")?;
    let host_id = host.id;
    app.draw(event_loop, &mut host)?;
    app.windows.insert(window, host);
    let saved = [first, second].map(|id| {
        let mut terminal = app.panes[&id].session.terminal().unwrap();
        let fixture = vt::Terminal::new(terminal.cols, terminal.rows, 0);
        (id, std::mem::replace(&mut *terminal, fixture))
    });
    let result = (|| -> Result<()> {
        let mut captures = Vec::new();
        let before_errors = app.errors.len();
        command(
            app,
            first,
            b"\x1b]99;i=job:a=report:c=1;First job\x1b\\",
            &mut captures,
        )?;
        let old = shown(&captures);
        command(
            app,
            second,
            b"\x1b]99;i=job:a=report:c=1;Second job\x1b\\",
            &mut captures,
        )?;
        let other = shown(&captures);
        assert!(command(app, first, b"\x1b]99;i=job:a=report:c=1:d=0;Updated\x1b\\\x1b]99;i=job:p=body:d=0;Body text\x1b\\\x1b]99;i=job:p=buttons;Open\xe2\x80\xa8Dismiss\x1b\\", &mut captures)?.is_empty());
        let current = shown(&captures);
        assert_ne!(current, old);
        let message = app.tui_notifications.message(current).unwrap();
        assert_eq!(
            (&message.title, &message.body),
            (&"Updated".to_owned(), &"Body text".to_owned())
        );
        assert_eq!(message.buttons, ["Open", "Dismiss"]);
        assert!(app.tui_notifications.message(other).is_some());
        assert!(
            app.tui_notifications
                .event(old, TuiNotificationEvent::Activated { button: 0 })
                .is_empty()
        );
        let operations = app
            .tui_notifications
            .event(current, TuiNotificationEvent::Untracked);
        assert_eq!(
            apply(app, operations, &mut captures),
            b"\x1b]99;i=job:p=close;untracked\x1b\\"
        );
        assert!(
            app.tui_notifications
                .event(current, TuiNotificationEvent::Untracked)
                .is_empty()
        );
        let operations = app
            .tui_notifications
            .event(current, TuiNotificationEvent::Activated { button: 2 });
        assert_eq!(
            apply(app, operations, &mut captures),
            b"\x1b]99;i=job;2\x1b\\\x1b]99;i=job:p=close;\x1b\\"
        );
        assert_eq!(app.focused(host_id), Some(first));
        assert!(
            app.tui_notifications
                .event(current, TuiNotificationEvent::Closed)
                .is_empty()
        );

        command(
            app,
            first,
            b"\x1b]99;a=-focus,report:c=1;Anonymous A\x1b\\",
            &mut captures,
        )?;
        let anonymous_a = shown(&captures);
        command(
            app,
            first,
            b"\x1b]99;a=-focus,report:c=1;Anonymous B\x1b\\",
            &mut captures,
        )?;
        let anonymous_b = shown(&captures);
        assert_ne!(anonymous_a, anonymous_b);
        command(
            app,
            first,
            b"\x1b]99;i=kept:a=-focus;Kept\x1b\\",
            &mut captures,
        )?;
        let kept = shown(&captures);
        command(app, first, b"\x1b]99;i=query:p=alive;\x1b\\", &mut captures)?;
        let (tui_notifications::Native::Query { token, requested }, _) = captures.last().unwrap()
        else {
            panic!()
        };
        assert!(!requested.contains(&other));
        let operations = app
            .tui_notifications
            .alive(*token, &[kept, anonymous_a, anonymous_b, other]);
        assert_eq!(
            apply(app, operations, &mut captures),
            b"\x1b]99;i=query:p=alive;kept\x1b\\"
        );
        assert_eq!(
            command(app, second, b"\x1b]99;i=job:p=close;\x1b\\", &mut captures)?,
            b"\x1b]99;i=job:p=close;\x1b\\"
        );
        app.focus_pane(host_id, second);
        let operations = app
            .tui_notifications
            .event(anonymous_a, TuiNotificationEvent::Activated { button: 0 });
        assert_eq!(
            apply(app, operations, &mut captures),
            b"\x1b]99;i=0;\x1b\\\x1b]99;i=0:p=close;\x1b\\"
        );
        assert_eq!(
            app.focused(host_id),
            Some(second),
            "a=-focus must preserve native pane focus"
        );
        let operations = app
            .tui_notifications
            .event(anonymous_b, TuiNotificationEvent::Failed);
        assert_eq!(
            apply(app, operations, &mut captures),
            b"\x1b]99;i=0:p=close;\x1b\\"
        );
        let operations = app.tui_notifications.prune(|_| false);
        assert!(apply(app, operations, &mut captures).is_empty());
        assert!(app.tui_notifications.is_empty());
        assert!(
            app.tui_notifications
                .event(kept, TuiNotificationEvent::Activated { button: 0 })
                .is_empty()
        );
        assert_eq!(app.errors.len(), before_errors);
        fs::write(
            directory.join("notifications-result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true, "panes": 2, "native_notifications_posted": 0,
                "checks": ["multipart-body-and-buttons", "same-ID-scoped-to-owner", "replacement-suppresses-old-close", "stale-callback-reject", "untracked-once", "button-report-and-close", "focus-routing", "anonymous-notifications-distinct", "alive-scoped-and-excludes-anonymous", "explicit-close", "no-focus-action", "failure-close", "scope-cleanup"]
            }))?,
        )?;
        eprintln!(
            "Native notification smoke: scoped lifecycle, replies, buttons and focus passed without posting notifications"
        );
        Ok(())
    })();
    for (id, terminal) in saved {
        *app.panes[&id].session.terminal()? = terminal;
    }
    result
}
