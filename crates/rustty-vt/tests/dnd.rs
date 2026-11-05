use rustty_vt::dnd::{
    Action, Command, Event, HostError, Item, MAX_DROP_BYTES, MAX_ITEMS, MAX_MIME_LIST_BYTES,
    MoveEvent, Operation, Operations,
};
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
        ), Effect::DragAndDrop(Event::ConcludedNone)]
    );
    assert!(terminal.kitty_dnd.as_ref().unwrap().drop.items.is_none());
    terminal
        .kitty_dnd
        .as_mut()
        .unwrap()
        .drag_drop(position, &[]);
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

fn registered() -> Terminal {
    let mut terminal = Terminal::new(80, 24, 0);
    terminal.feed(b"\x1b]72;t=a:i=7\x1b\\");
    terminal
}

fn position() -> MoveEvent {
    MoveEvent {
        cell_x: 2,
        cell_y: 1,
        pixel_x: 20,
        pixel_y: 18,
        operations: Operations {
            copy: true,
            move_: false,
        },
    }
}

fn command(terminal: &Terminal, offer: u64, action: Action) -> Command {
    Command {
        registration: terminal.kitty_dnd.as_ref().unwrap().view().registration,
        offer,
        action,
    }
}

fn move_offer(terminal: &mut Terminal, offer: u64, mimes: &[&[u8]]) -> Vec<u8> {
    let command = command(
        terminal,
        offer,
        Action::Move {
            event: position(),
            mimes: mimes.iter().map(|mime| mime.to_vec()).collect(),
        },
    );
    terminal
        .kitty_dnd
        .as_mut()
        .unwrap()
        .apply_host(command)
        .unwrap()
}

fn item(mime: &[u8], data: &[u8]) -> Item {
    Item {
        mime: mime.to_vec(),
        data: data.to_vec(),
    }
}

fn drop_offer(terminal: &mut Terminal, offer: u64, items: Vec<Item>) -> Result<Vec<u8>, HostError> {
    let command = command(
        terminal,
        offer,
        Action::Drop {
            event: position(),
            items,
        },
    );
    terminal.kitty_dnd.as_mut().unwrap().apply_host(command)
}

#[test]
fn registration_replaces_old_drop_data_and_invalidates_the_host_on_its_first_chunk() {
    let mut terminal = registered();
    let old = terminal.kitty_dnd.as_ref().unwrap().view();
    assert!(old.ready);
    assert!(old.registered_mimes.is_empty());
    move_offer(&mut terminal, 1, &[b"text/plain"]);
    terminal.feed(b"\x1b]72;t=m:o=1\x1b\\");
    drop_offer(
        &mut terminal,
        1,
        vec![item(b"text/plain", b"old private data")],
    )
    .unwrap();
    terminal.feed(b"\x1b]72;t=a:i=8:m=1;image/\x1b\\");
    let state = terminal.kitty_dnd.as_ref().unwrap();
    let partial = state.view();
    assert!(!partial.ready);
    assert_ne!(partial.registration, old.registration);
    assert_ne!(partial.revision, old.revision);
    assert_eq!(partial.client_id, 8);
    assert_eq!(partial.offer, None);
    assert_eq!(partial.accepted, None);
    assert!(state.drop.items.is_none());
    let pending = command(
        &terminal,
        2,
        Action::Move {
            event: position(),
            mimes: vec![b"image/png".to_vec()],
        },
    );
    assert_eq!(
        terminal.kitty_dnd.as_mut().unwrap().apply_host(pending),
        Err(HostError::StaleRegistration)
    );
    terminal.feed(b"\x1b]72;m=0;png\x1b\\");
    assert!(terminal.kitty_dnd.as_ref().unwrap().view().ready);
    let stale = Command {
        registration: old.registration,
        offer: 1,
        action: Action::Leave,
    };
    assert_eq!(
        terminal.kitty_dnd.as_mut().unwrap().apply_host(stale),
        Err(HostError::StaleRegistration)
    );
    let complete = terminal.kitty_dnd.as_ref().unwrap().view();
    terminal.feed(b"\x1b]72;t=a:i=8;image/png\x1b\\");
    assert_ne!(
        terminal.kitty_dnd.as_ref().unwrap().view().registration,
        complete.registration
    );
    terminal.feed(b"\x1b]72;t=A\x1b\\");
    assert!(terminal.kitty_dnd.is_none());
}

#[test]
fn host_drop_requires_current_offer_and_completed_copy_acceptance() {
    let mut terminal = registered();
    move_offer(&mut terminal, 10, &[b"text/plain", b"image/png"]);
    let data = || vec![item(b"text/plain", b"hello")];
    assert_eq!(
        drop_offer(&mut terminal, 10, data()),
        Err(HostError::NotAccepted)
    );
    for operation in [0, 2] {
        terminal.feed(format!("\x1b]72;t=m:o={operation}\x1b\\").as_bytes());
        assert_eq!(
            drop_offer(&mut terminal, 10, data()),
            Err(HostError::NotAccepted)
        );
    }
    let previous = terminal.kitty_dnd.as_ref().unwrap().revision();
    terminal.feed(b"\x1b]72;t=m:o=1:m=1;text/\x1b\\");
    assert_ne!(terminal.kitty_dnd.as_ref().unwrap().revision(), previous);
    assert_eq!(terminal.kitty_dnd.as_ref().unwrap().view().accepted, None);
    assert_eq!(
        drop_offer(&mut terminal, 10, data()),
        Err(HostError::NotAccepted)
    );
    terminal.feed(b"\x1b]72;m=0;plain\x1b\\");
    let view = terminal.kitty_dnd.as_ref().unwrap().view();
    assert!(view.accepts_mime(b"text/plain"));
    assert!(!view.accepts_mime(b"text/plai"));
    assert!(!view.accepts_mime(b"image/png"));
    assert_eq!(
        drop_offer(&mut terminal, 9, data()),
        Err(HostError::StaleOffer)
    );
    assert_eq!(
        drop_offer(&mut terminal, 10, vec![item(b"image/png", b"x")]),
        Err(HostError::NotAccepted)
    );
    let mut event = position();
    event.operations.copy = false;
    let invalid = command(
        &terminal,
        10,
        Action::Drop {
            event,
            items: data(),
        },
    );
    assert_eq!(
        terminal.kitty_dnd.as_mut().unwrap().apply_host(invalid),
        Err(HostError::NotAccepted)
    );
    drop_offer(&mut terminal, 10, data()).unwrap();
    assert_eq!(
        drop_offer(&mut terminal, 10, data()),
        Err(HostError::StaleOffer)
    );
    assert!(terminal.kitty_dnd.as_ref().unwrap().drop.dropped);
}

#[test]
fn unchanged_moves_keep_acceptance_but_offer_mime_and_operation_changes_invalidate_it() {
    let mut terminal = registered();
    let first = move_offer(&mut terminal, 1, &[b"text/plain"]);
    assert!(first.windows(11).any(|bytes| bytes == b"text/plain "));
    terminal.feed(b"\x1b]72;t=m:o=1\x1b\\");
    let before = terminal.kitty_dnd.as_ref().unwrap().view();
    let second = move_offer(&mut terminal, 1, &[b"text/plain"]);
    assert_eq!(second, b"\x1b]72;t=m:x=2:y=1:X=20:Y=18:o=1:i=7\x1b\\");
    assert_eq!(terminal.kitty_dnd.as_ref().unwrap().view(), before);
    terminal.feed(b"\x1b]72;t=m:o=1:m=1;text/\x1b\\");
    move_offer(&mut terminal, 1, &[b"image/png"]);
    assert!(!terminal.kitty_dnd.as_ref().unwrap().chunking.active);
    terminal.feed(b"\x1b]72;m=0;plain\x1b\\");
    assert_eq!(terminal.kitty_dnd.as_ref().unwrap().view().accepted, None);
    terminal.feed(b"\x1b]72;t=m:o=1\x1b\\");
    let mut event = position();
    event.operations.copy = false;
    let update = command(
        &terminal,
        1,
        Action::Move {
            event,
            mimes: vec![b"image/png".to_vec()],
        },
    );
    terminal
        .kitty_dnd
        .as_mut()
        .unwrap()
        .apply_host(update)
        .unwrap();
    assert_eq!(terminal.kitty_dnd.as_ref().unwrap().view().accepted, None);
    move_offer(&mut terminal, 2, &[b"image/png"]);
    assert_eq!(terminal.kitty_dnd.as_ref().unwrap().view().offer, Some(2));
    assert_eq!(terminal.kitty_dnd.as_ref().unwrap().view().accepted, None);
    let stale = command(&terminal, 1, Action::Leave);
    assert_eq!(
        terminal.kitty_dnd.as_mut().unwrap().apply_host(stale),
        Err(HostError::StaleOffer)
    );
}

#[test]
fn leave_frees_hover_state_and_disconnect_preserves_committed_owner_data() {
    let mut terminal = registered();
    move_offer(&mut terminal, 1, &[b"text/plain"]);
    terminal.feed(b"\x1b]72;t=m:o=1\x1b\\");
    let state = terminal.kitty_dnd.as_mut().unwrap();
    let revision = state.revision();
    assert!(state.cancel_offer(2).is_empty());
    assert_eq!(state.revision(), revision);
    assert_eq!(state.cancel_offer(1), b"\x1b]72;t=m:x=-1:y=-1:i=7\x1b\\");
    assert_eq!(state.view().offer, None);
    assert_eq!(state.client_accepted(), None);
    assert!(state.drop.offered.is_none());
    move_offer(&mut terminal, 2, &[b"text/plain"]);
    terminal.feed(b"\x1b]72;t=m:o=1\x1b\\");
    drop_offer(&mut terminal, 2, vec![item(b"text/plain", b"private")]).unwrap();
    let state = terminal.kitty_dnd.as_mut().unwrap();
    assert!(state.cancel_offer(2).is_empty());
    assert!(state.drag_leave().is_empty());
    let serialized = serde_json::to_string(&state.view()).unwrap();
    assert!(!serialized.contains("private"));
    assert!(!serialized.contains("items"));
    assert!(
        matches!(&terminal.feed(b"\x1b]72;t=r:x=1\x1b\\")[..], [Effect::Write(bytes)] if bytes.windows(8).any(|bytes| bytes == b"cHJpdmF0"))
    );
    terminal.feed(b"\x1b]72;t=r:o=1\x1b\\");
    assert!(terminal.kitty_dnd.as_ref().unwrap().drop.items.is_none());
    assert_eq!(terminal.kitty_dnd.as_ref().unwrap().view().offer, None);
}

#[test]
fn host_data_and_mime_limits_reject_atomically_and_keep_exact_indices() {
    let mut terminal = registered();
    move_offer(&mut terminal, 1, &[b"text/plain", b"image/png"]);
    terminal.feed(b"\x1b]72;t=m:o=1\x1b\\");
    assert!(
        terminal
            .kitty_dnd
            .as_ref()
            .unwrap()
            .view()
            .accepts_mime(b"image/png")
    );
    let before = terminal.kitty_dnd.as_ref().unwrap().view();
    let too_large = vec![Item {
        mime: b"text/plain".to_vec(),
        data: vec![0; MAX_DROP_BYTES + 1],
    }];
    assert_eq!(
        drop_offer(&mut terminal, 1, too_large),
        Err(HostError::TooLarge)
    );
    let too_many = (0..=MAX_ITEMS).map(|_| item(b"text/plain", b"x")).collect();
    assert_eq!(
        drop_offer(&mut terminal, 1, too_many),
        Err(HostError::TooLarge)
    );
    for mimes in [
        vec![b"text/plain".to_vec(), b"text/plain".to_vec()],
        vec![b"text/plain\x1b\\".to_vec()],
        vec![b"has space".to_vec()],
        vec![vec![b'a'; 256]],
        vec![Vec::new()],
    ] {
        let action = command(
            &terminal,
            2,
            Action::Move {
                event: position(),
                mimes,
            },
        );
        assert_eq!(
            terminal.kitty_dnd.as_mut().unwrap().apply_host(action),
            Err(HostError::InvalidMime)
        );
        assert_eq!(terminal.kitty_dnd.as_ref().unwrap().view(), before);
    }
    assert_eq!(
        drop_offer(&mut terminal, 1, vec![item(b"application/unknown", b"x")]),
        Err(HostError::NotAccepted)
    );
    let reply = drop_offer(
        &mut terminal,
        1,
        vec![item(b"image/png", &[0, 255]), item(b"text/plain", b"text")],
    )
    .unwrap();
    assert!(
        reply
            .windows(21)
            .any(|bytes| bytes == b"image/png text/plain ")
    );
    let items = terminal
        .kitty_dnd
        .as_ref()
        .unwrap()
        .drop
        .items
        .as_ref()
        .unwrap();
    assert_eq!(items[0].mime, b"image/png");
    assert_eq!(items[0].data, [0, 255]);
    assert_eq!(items[1].mime, b"text/plain");
}

#[test]
fn data_before_drop_is_forbidden_without_ending_hover_and_real_errors_end_drops() {
    let mut terminal = registered();
    move_offer(&mut terminal, 1, &[b"text/plain"]);
    terminal.feed(b"\x1b]72;t=m:o=1\x1b\\");
    let before = terminal.kitty_dnd.as_ref().unwrap().view();
    for request in ["x=1", "x=1:y=1", "Y=2:x=1"] {
        let effects = terminal.feed(format!("\x1b]72;t=r:{request}\x1b\\").as_bytes());
        assert!(
            matches!(&effects[..], [Effect::Write(bytes)] if bytes.windows(6).any(|bytes| bytes == b"EPERM:"))
        );
        assert_eq!(terminal.kitty_dnd.as_ref().unwrap().view(), before);
    }
    drop_offer(&mut terminal, 1, vec![item(b"text/plain", b"x")]).unwrap();
    let effects = terminal.feed(b"\x1b]72;t=r:x=2\x1b\\");
    assert!(
        matches!(&effects[..], [Effect::Write(bytes), Effect::DragAndDrop(Event::ConcludedNone)] if bytes.windows(7).any(|bytes| bytes == b"ENOENT:"))
    );
    assert!(terminal.kitty_dnd.as_ref().unwrap().drop.items.is_none());
    assert_eq!(terminal.kitty_dnd.as_ref().unwrap().view().offer, None);
    let again = terminal.feed(b"\x1b]72;t=r:x=1\x1b\\");
    assert!(
        matches!(&again[..], [Effect::Write(bytes)] if bytes.windows(6).any(|bytes| bytes == b"EPERM:"))
    );
}

#[test]
fn guarded_positions_cannot_encode_spurious_leave_events() {
    let mut terminal = registered();
    let before = terminal.kitty_dnd.as_ref().unwrap().view();
    for event in [
        MoveEvent {
            pixel_x: -1,
            ..position()
        },
        MoveEvent {
            pixel_y: -1,
            ..position()
        },
        MoveEvent {
            cell_x: u32::MAX,
            ..position()
        },
        MoveEvent {
            cell_y: u32::MAX,
            ..position()
        },
    ] {
        let action = command(
            &terminal,
            1,
            Action::Move {
                event,
                mimes: vec![b"text/plain".to_vec()],
            },
        );
        assert_eq!(
            terminal.kitty_dnd.as_mut().unwrap().apply_host(action),
            Err(HostError::InvalidPosition)
        );
        assert_eq!(terminal.kitty_dnd.as_ref().unwrap().view(), before);
    }
}

#[test]
fn oversized_chunk_chains_never_become_ready_with_truncated_mime_lists() {
    let mut terminal = registered();
    for kind in ["a", "m:o=1"] {
        terminal.feed(format!("\x1b]72;t={kind}:m=1;\x1b\\").as_bytes());
        let mut chunk = b"\x1b]72;m=1;".to_vec();
        chunk.extend_from_slice(&[b'a'; 4096]);
        chunk.extend_from_slice(b"\x1b\\");
        for _ in 0..MAX_MIME_LIST_BYTES / 4096 {
            terminal.feed(&chunk);
        }
        terminal.feed(b"\x1b]72;m=1;x\x1b\\\x1b]72;m=0\x1b\\");
        let state = terminal.kitty_dnd.as_ref().unwrap();
        if kind == "a" {
            assert!(!state.view().ready);
        } else {
            assert_eq!(state.client_accepted(), None);
        }
        terminal.feed(b"\x1b]72;t=a\x1b\\");
        assert!(terminal.kitty_dnd.as_ref().unwrap().view().ready);
    }
}

#[test]
fn command_deserialization_bounds_lists_and_keeps_byte_data_exact() {
    let terminal = registered();
    let original = command(
        &terminal,
        1,
        Action::Drop {
            event: position(),
            items: vec![item(b"image/png", &[0, 255, 1])],
        },
    );
    let bytes = serde_json::to_vec(&original).unwrap();
    assert_eq!(serde_json::from_slice::<Command>(&bytes).unwrap(), original);
    let original = command(
        &terminal,
        1,
        Action::Move {
            event: position(),
            mimes: vec![b"text/plain".to_vec(), b"image/png".to_vec()],
        },
    );
    let bytes = serde_json::to_vec(&original).unwrap();
    assert_eq!(serde_json::from_slice::<Command>(&bytes).unwrap(), original);
    let too_many = command(
        &terminal,
        1,
        Action::Drop {
            event: position(),
            items: (0..=MAX_ITEMS).map(|_| item(b"x", b"y")).collect(),
        },
    );
    assert!(serde_json::from_slice::<Command>(&serde_json::to_vec(&too_many).unwrap()).is_err());
    let too_many = command(
        &terminal,
        1,
        Action::Move {
            event: position(),
            mimes: vec![b"x".to_vec(); MAX_ITEMS + 1],
        },
    );
    assert!(serde_json::from_slice::<Command>(&serde_json::to_vec(&too_many).unwrap()).is_err());
    let too_long = command(
        &terminal,
        1,
        Action::Move {
            event: position(),
            mimes: vec![vec![b'x'; 256]],
        },
    );
    assert!(serde_json::from_slice::<Command>(&serde_json::to_vec(&too_long).unwrap()).is_err());
}

#[test]
fn repeated_data_requests_have_a_cumulative_reply_budget_and_reset_for_new_drops() {
    use rustty_vt::dnd::{MAX_DROP_REQUESTS, MAX_DROP_SERVED_BYTES};
    let mut terminal = registered();
    move_offer(&mut terminal, 1, &[b"text/plain"]);
    terminal.feed(b"\x1b]72;t=m:o=1\x1b\\");
    drop_offer(
        &mut terminal,
        1,
        vec![Item {
            mime: b"text/plain".to_vec(),
            data: vec![0; MAX_DROP_BYTES],
        }],
    )
    .unwrap();
    for _ in 0..MAX_DROP_SERVED_BYTES / MAX_DROP_BYTES {
        let effects = terminal.feed(b"\x1b]72;t=r:x=1\x1b\\");
        assert!(
            matches!(&effects[..], [Effect::Write(bytes)] if bytes.len() < 16 * 1024 * 1024 && bytes.len() > MAX_DROP_BYTES)
        );
    }
    let effects = terminal.feed(b"\x1b]72;t=r:x=1\x1b\\");
    assert!(
        matches!(&effects[..], [Effect::Write(bytes), Effect::DragAndDrop(Event::ConcludedNone)] if bytes.windows(7).any(|bytes| bytes == b"EMFILE:"))
    );
    assert!(terminal.kitty_dnd.as_ref().unwrap().drop.items.is_none());
    move_offer(&mut terminal, 2, &[b"text/plain"]);
    terminal.feed(b"\x1b]72;t=m:o=1\x1b\\");
    drop_offer(&mut terminal, 2, vec![item(b"text/plain", b"")]).unwrap();
    let requests = b"\x1b]72;t=r:x=1\x1b\\".repeat(MAX_DROP_REQUESTS + 1);
    let effects = terminal.feed(&requests);
    assert_eq!(effects.len(), MAX_DROP_REQUESTS + 2);
    assert!(matches!(
        effects.last(),
        Some(Effect::DragAndDrop(Event::ConcludedNone))
    ));
    assert!(
        matches!(&effects[MAX_DROP_REQUESTS], Effect::Write(bytes) if bytes.windows(7).any(|bytes| bytes == b"EMFILE:"))
    );
    assert!(terminal.kitty_dnd.as_ref().unwrap().drop.items.is_none());
}
