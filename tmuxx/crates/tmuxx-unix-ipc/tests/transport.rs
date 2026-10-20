#![cfg(unix)]

use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use tmuxx_unix_ipc::{Connection, Schema, pack_argv, peer_uid, unpack_argv};

#[test]
fn messages_and_descriptors_work_in_both_schemas() {
    for schema in [Schema::Modern { version: 8 }, Schema::Legacy { version: 8 }] {
        let (sender, receiver) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(&sender).unwrap(), unsafe { libc::geteuid() });
        let mut sender = Connection::new(sender, schema).unwrap();
        let mut receiver = Connection::new(receiver, Schema::Auto { version: 8 }).unwrap();
        let (passed, mut peer) = UnixStream::pair().unwrap();
        sender.send(100, b"before", None).unwrap();
        sender.send(101, &[0, 255], Some(passed.as_fd())).unwrap();
        sender.send(102, b"after", None).unwrap();
        assert_eq!(receiver.recv().unwrap().unwrap().payload, b"before");
        let message = receiver.recv().unwrap().unwrap();
        assert_eq!(receiver.schema(), schema);
        assert_eq!(message.kind, 101);
        assert_eq!(message.peer_id, 8);
        assert_eq!(message.payload, [0, 255]);
        let fd = message.fd.unwrap();
        assert_ne!(
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        let mut received = UnixStream::from(fd);
        received.write_all(b"works").unwrap();
        let mut answer = [0; 5];
        peer.read_exact(&mut answer).unwrap();
        assert_eq!(&answer, b"works");
        assert_eq!(receiver.recv().unwrap().unwrap().payload, b"after");
        drop(sender);
        assert!(receiver.recv().unwrap().is_none());
    }
}

#[test]
fn short_header_and_invalid_lengths_fail() {
    for header in [vec![0; 16], vec![0; 7], {
        let mut bytes = vec![0; 16];
        bytes[4..8].copy_from_slice(&0x8000_0010_u32.to_ne_bytes());
        bytes
    }] {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        let mut reader = Connection::new(reader, Schema::Modern { version: 8 }).unwrap();
        writer.write_all(&header).unwrap();
        drop(writer);
        assert!(reader.recv().is_err());
        assert!(reader.recv().is_err());
    }
}

#[test]
fn fragmented_message_reassembles() {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    let mut bytes = Vec::new();
    for field in [200_u32, 21, 8, u32::MAX] {
        bytes.extend_from_slice(&field.to_ne_bytes());
    }
    bytes.extend_from_slice(b"hello");
    let task = std::thread::spawn(move || {
        for byte in bytes {
            writer.write_all(&[byte]).unwrap();
        }
    });
    let mut reader = Connection::new(reader, Schema::Modern { version: 8 }).unwrap();
    assert_eq!(reader.recv().unwrap().unwrap().payload, b"hello");
    task.join().unwrap();
}

#[test]
fn argv_is_byte_preserving_and_validated() {
    let argv = vec![b"display-message".to_vec(), vec![255, 1, b'\n'], vec![]];
    assert_eq!(unpack_argv(&pack_argv(&argv).unwrap()).unwrap(), argv);
    assert!(pack_argv(&[vec![0]]).is_err());
    assert!(pack_argv(&[vec![b'a'; 16_384]]).is_err());
    for payload in [
        vec![],
        vec![0; 3],
        (-1_i32).to_ne_bytes().to_vec(),
        [1_i32.to_ne_bytes().as_slice(), b"missing-nul"].concat(),
        [0_i32.to_ne_bytes().as_slice(), b"extra\0"].concat(),
    ] {
        assert!(unpack_argv(&payload).is_err());
    }
}

#[test]
fn oversize_and_unresolved_fd_send_are_rejected_without_writing() {
    let (stream, _peer) = UnixStream::pair().unwrap();
    let (fd, _other) = UnixStream::pair().unwrap();
    let mut connection = Connection::new(stream, Schema::Auto { version: 8 }).unwrap();
    assert!(connection.send(1, &[], Some(fd.as_fd())).is_err());
    assert!(connection.send(1, &vec![0; 16_384], None).is_err());
    connection.send(1, b"still usable", None).unwrap();
}

#[test]
fn file_paths_require_explicit_client_policy() {
    use tmuxx_unix_ipc::{FileAccess, FileClient, Message, int_pair, msg, read_i32};
    let (client, server) = UnixStream::pair().unwrap();
    let mut client = Connection::new(client, Schema::Modern { version: 8 }).unwrap();
    let mut server = Connection::new(server, Schema::Modern { version: 8 }).unwrap();
    let mut files = FileClient::command(FileAccess::StdioOnly, 1024);
    let message = Message {
        kind: msg::READ_OPEN,
        peer_id: 8,
        pid: 1,
        payload: [int_pair(4, -1), b"/definitely-not-a-file\0".to_vec()].concat(),
        fd: None,
    };
    assert!(
        files
            .handle(&mut client, &message, &mut std::io::empty())
            .unwrap()
    );
    let response = server.recv().unwrap().unwrap();
    assert_eq!(response.kind, msg::READ_DONE);
    assert_eq!(read_i32(&response.payload, 4).unwrap(), libc::EACCES);
}

#[test]
fn identification_preserves_terminal_features_and_registry_order() {
    use tmuxx_unix_ipc::{Identity, accept_client, identify, msg};
    let source = include_str!("../../../test-support/fixtures/tty-features.c");
    let table = source
        .split("tty_features[] = {")
        .nth(1)
        .unwrap()
        .split("};")
        .next()
        .unwrap();
    for (bit, name) in table
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix('&')
                .map(|line| line.trim_end_matches(','))
        })
        .enumerate()
    {
        let marker = format!("{name} = {{");
        let name = source
            .split(&marker)
            .nth(1)
            .unwrap()
            .split('"')
            .nth(1)
            .unwrap();
        assert_eq!(tmuxx_unix_ipc::client::feature_names(1 << bit), name);
    }
    for schema in [Schema::Modern { version: 8 }, Schema::Legacy { version: 8 }] {
        let (a, b) = UnixStream::pair().unwrap();
        let mut sender = Connection::new(a, schema).unwrap();
        let mut receiver = Connection::new(b, Schema::Auto { version: 8 }).unwrap();
        let mut identity = Identity::current(true).unwrap();
        identity.features = (1 << 4) | (1 << 18);
        identity.environment.clear();
        identify(&mut sender, &identity, None, None).unwrap();
        sender
            .send(
                msg::COMMAND,
                &pack_argv(&[b"list-sessions".to_vec()]).unwrap(),
                None,
            )
            .unwrap();
        let accepted = accept_client(&mut receiver).unwrap();
        assert_eq!(accepted.identity.features, identity.features);
        assert_eq!(
            tmuxx_unix_ipc::client::feature_names(accepted.identity.features),
            "clipboard,RGB"
        );
    }
}

#[test]
fn identification_rejects_malformed_order_lengths_and_versions() {
    use tmuxx_unix_ipc::{accept_client, msg};
    for schema in [Schema::Modern { version: 8 }, Schema::Legacy { version: 8 }] {
        for (kind, payload) in [
            (msg::COMMAND, pack_argv(&[b"new-session".to_vec()]).unwrap()),
            (msg::IDENTIFY_FLAGS, vec![0; 3]),
            (msg::IDENTIFY_LONGFLAGS, vec![0; 7]),
            (msg::IDENTIFY_FEATURES, vec![0; 3]),
            (msg::IDENTIFY_CLIENTPID, vec![0; 3]),
            (msg::IDENTIFY_CWD, b"missing terminator".to_vec()),
            (msg::IDENTIFY_ENVIRON, b"a\0b\0".to_vec()),
            (msg::IDENTIFY_DONE, vec![0]),
        ] {
            let (a, b) = UnixStream::pair().unwrap();
            let mut sender = Connection::new(a, schema).unwrap();
            let mut receiver = Connection::new(b, Schema::Auto { version: 8 }).unwrap();
            sender.send(kind, &payload, None).unwrap();
            assert!(accept_client(&mut receiver).is_err(), "kind {kind}");
        }
        let (a, b) = UnixStream::pair().unwrap();
        let mut sender = Connection::new(a, schema).unwrap();
        let mut receiver = Connection::new(b, Schema::Auto { version: 8 }).unwrap();
        sender.send(msg::IDENTIFY_DONE, &[], None).unwrap();
        sender.send(msg::IDENTIFY_TERM, b"screen\0", None).unwrap();
        assert!(accept_client(&mut receiver).is_err());
    }
    for schema in [Schema::Modern { version: 7 }, Schema::Legacy { version: 7 }] {
        let (a, b) = UnixStream::pair().unwrap();
        let mut sender = Connection::new(a, schema).unwrap();
        let mut receiver = Connection::new(b, Schema::Auto { version: 8 }).unwrap();
        sender.send(msg::IDENTIFY_DONE, &[], None).unwrap();
        let error = accept_client(&mut receiver).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        let reply = sender.recv().unwrap().unwrap();
        assert_eq!(reply.kind, msg::VERSION);
        assert_eq!(reply.peer_id, 8);
        assert!(reply.payload.is_empty());
    }
}

#[test]
fn rejected_identification_closes_previously_accepted_and_rejected_descriptors() {
    use tmuxx_unix_ipc::{accept_client, msg};
    for schema in [Schema::Modern { version: 8 }, Schema::Legacy { version: 8 }] {
        for bad_kind in [msg::IDENTIFY_TERM, msg::IDENTIFY_STDIN] {
            let (a, b) = UnixStream::pair().unwrap();
            let mut sender = Connection::new(a, schema).unwrap();
            let mut receiver = Connection::new(b, Schema::Auto { version: 8 }).unwrap();
            let (input, mut input_peer) = UnixStream::pair().unwrap();
            let (invalid, mut invalid_peer) = UnixStream::pair().unwrap();
            sender
                .send(msg::IDENTIFY_STDIN, &[], Some(input.as_fd()))
                .unwrap();
            sender.send(bad_kind, &[], Some(invalid.as_fd())).unwrap();
            drop(input);
            drop(invalid);
            assert!(accept_client(&mut receiver).is_err());
            drop(receiver);
            for peer in [&mut input_peer, &mut invalid_peer] {
                peer.set_nonblocking(true).unwrap();
                assert_eq!(peer.read(&mut [0]).unwrap(), 0);
            }
        }
    }
}
