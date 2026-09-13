use rustty_vt::dnd::{Event, Item, MoveEvent, Operation, Operations};
use rustty_vt::{Effect, EffectHandler, Terminal};

#[test]
fn synchronous_callbacks_observe_each_registration_before_it_changes() {
    #[derive(Default)]
    struct Host(Vec<(Event, Option<Vec<u8>>)>);

    impl EffectHandler for Host {
        fn effect(&mut self, effect: Effect) {
            panic!("unexpected deferred effect: {effect:?}");
        }

        fn drag_and_drop(&mut self, event: Event, state: Option<&rustty_vt::dnd::State>) {
            self.0.push((
                event,
                state.map(|state| state.drop.registered_mimes.clone()),
            ));
        }
    }

    let mut terminal = Terminal::new(80, 24, 0);
    let mut host = Host::default();
    terminal.feed_with_handler(
        b"\x1b]72;t=a;text/plain\x1b\\\x1b]72;t=a;image/png\x1b\\\x1b]72;t=A\x1b\\",
        &mut host,
    );
    assert_eq!(
        host.0,
        [
            (Event::Registration, Some(b"text/plain".to_vec())),
            (Event::Registration, Some(b"image/png".to_vec())),
            (Event::Registration, None),
        ]
    );
    assert!(terminal.kitty_dnd.is_none());
}

#[test]
fn chunked_drag_conversation_retains_binary_data_across_reset_and_concludes_once() {
    let mut terminal = Terminal::new(80, 24, 0);
    assert_eq!(
        terminal.feed(b"\x1b]72;t=q:i=3\x07"),
        [Effect::Write(b"\x1b]72;t=q:i=3\x07".to_vec())]
    );
    assert!(terminal.kitty_dnd.is_none());
    assert!(terminal.feed(b"\x1b]72;t=a:i=7:m=1;text/\x1b\\").is_empty());
    // The final chunk's type and client ID must not replace the first chunk's.
    assert_eq!(
        terminal.feed(b"\x1b]72;t=q:i=99;plain\x1b\\"),
        [Effect::DragAndDrop(Event::Registration)]
    );
    let state = terminal.kitty_dnd.as_mut().unwrap();
    assert_eq!(state.drop.client_id, 7);
    assert_eq!(
        state.registered_mimes().collect::<Vec<_>>(),
        [b"text/plain"]
    );
    let position = MoveEvent {
        cell_x: 2,
        cell_y: 1,
        pixel_x: 20,
        pixel_y: 18,
        operations: Operations {
            copy: true,
            move_: true,
        },
    };
    assert_eq!(
        state.drag_move(position, &[b"text/plain"]),
        b"\x1b]72;t=m:x=2:y=1:X=20:Y=18:o=3:i=7:m=0;text/plain \x1b\\"
    );
    assert!(terminal.feed(b"\x1b]72;t=m:o=2:m=1;text/\x1b\\").is_empty());
    assert_eq!(terminal.kitty_dnd.as_ref().unwrap().client_accepted(), None);
    assert_eq!(
        terminal.feed(b"\x1b]72;t=q:o=1;plain\x1b\\"),
        [Effect::DragAndDrop(Event::Acceptance)]
    );
    let state = terminal.kitty_dnd.as_mut().unwrap();
    assert_eq!(state.client_accepted(), Some(Operation::Move));
    assert_eq!(state.drop.accepted_mimes, b"text/plain\0");
    assert_eq!(
        state.drag_drop(
            position,
            &[
                Item {
                    mime: b"text/plain".to_vec(),
                    data: vec![0, 255, 1]
                },
                Item {
                    mime: b"empty".to_vec(),
                    data: Vec::new()
                },
            ]
        ),
        b"\x1b]72;t=M:x=2:y=1:X=20:Y=18:o=3:i=7:m=0;text/plain empty \x1b\\"
    );
    assert!(state.drag_leave().is_empty());
    assert!(terminal.feed(b"\x1b]72;t=m:o=1:m=1;text/\x1b\\").is_empty());
    terminal.reset();
    let state = terminal.kitty_dnd.as_ref().unwrap();
    assert!(!state.chunking.active);
    assert!(state.drop.accept_in_progress);
    assert!(state.drop.dropped);
    assert_eq!(state.drop.items.as_ref().unwrap().len(), 2);
    assert_eq!(
        terminal.feed(b"\x1b]72;t=q\x1b\\"),
        [Effect::Write(b"\x1b]72;t=q\x1b\\".to_vec())]
    );
    // Malformed metadata must never become a request to conclude the drop.
    assert!(terminal.feed(b"\x1b]72;t=r:x=\x1b\\").is_empty());
    assert!(terminal.kitty_dnd.as_ref().unwrap().drop.dropped);
    assert_eq!(
        terminal.feed(b"\x1b]72;t=r:x=1:i=99\x07"),
        [Effect::Write(
            b"\x1b]72;t=r:x=1:i=7:m=0;AP8B\x07\x1b]72;t=r:x=1:i=7\x07".to_vec()
        )]
    );
    // An empty representation sends only one end-of-data marker.
    assert_eq!(
        terminal.feed(b"\x1b]72;t=r:x=2\x1b\\"),
        [Effect::Write(b"\x1b]72;t=r:x=2:i=7\x1b\\".to_vec())]
    );
    assert_eq!(
        terminal.feed(b"\x1b]72;t=r:x=-2147483648\x1b\\"),
        [Effect::Write(
            b"\x1b]72;t=R:x=-2147483648:i=7:m=0;ENOENT:drop data request index out of bounds\x1b\\"
                .to_vec()
        )]
    );
    assert_eq!(
        terminal.feed(b"\x1b]72;t=r:o=2\x1b\\"),
        [Effect::DragAndDrop(Event::ConcludedMove)]
    );
    assert!(terminal.kitty_dnd.as_ref().unwrap().drop.items.is_none());
    assert!(terminal.feed(b"\x1b]72;t=r:o=2\x1b\\").is_empty());
    assert_eq!(
        terminal.feed(b"\x1b]72;t=A\x1b\\"),
        [Effect::DragAndDrop(Event::Registration)]
    );
    assert!(terminal.kitty_dnd.is_none());
}
