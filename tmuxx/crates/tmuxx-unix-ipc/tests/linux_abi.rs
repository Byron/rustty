//! Exercise the Linux kernel's ancillary-data ABI, including rejection cleanup.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::mem;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;
use tmuxx_unix_ipc::{Connection, Schema};

fn header(schema: Schema, marked: bool) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[..4].copy_from_slice(&200_u32.to_ne_bytes());
    match schema {
        Schema::Legacy { .. } => {
            bytes[4..6].copy_from_slice(&16_u16.to_ne_bytes());
            bytes[6..8].copy_from_slice(&u16::from(marked).to_ne_bytes());
        }
        _ => bytes[4..8]
            .copy_from_slice(&(16_u32 | if marked { 0x8000_0000 } else { 0 }).to_ne_bytes()),
    }
    bytes[8..12].copy_from_slice(&8_u32.to_ne_bytes());
    bytes
}

fn rights(stream: &UnixStream, bytes: &[u8], descriptors: &[i32]) {
    // usize storage has cmsghdr alignment on the actual target ABI.
    let space = unsafe { libc::CMSG_SPACE(mem::size_of_val(descriptors) as _) } as usize;
    let mut ancillary = vec![0_usize; space.div_ceil(mem::size_of::<usize>())];
    let mut vector = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &mut vector;
    message.msg_iovlen = 1;
    message.msg_control = ancillary.as_mut_ptr().cast();
    message.msg_controllen = space;
    // SAFETY: both buffers remain live, and CMSG_SPACE reserves the complete array.
    unsafe {
        let control = libc::CMSG_FIRSTHDR(&message);
        (*control).cmsg_level = libc::SOL_SOCKET;
        (*control).cmsg_type = libc::SCM_RIGHTS;
        (*control).cmsg_len = libc::CMSG_LEN(mem::size_of_val(descriptors) as _) as _;
        std::ptr::copy_nonoverlapping(
            descriptors.as_ptr().cast::<u8>(),
            libc::CMSG_DATA(control),
            mem::size_of_val(descriptors),
        );
        assert_eq!(
            libc::sendmsg(stream.as_raw_fd(), &message, libc::MSG_NOSIGNAL),
            bytes.len() as isize,
            "{}",
            std::io::Error::last_os_error()
        );
    }
}

fn assert_closed(peer: &mut UnixStream) {
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
}

#[test]
fn queued_rights_follow_marked_frames_and_close_on_eof() {
    for schema in [Schema::Modern { version: 8 }, Schema::Legacy { version: 8 }] {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let mut receiver = Connection::new(receiver, schema).unwrap();
        let (first, mut first_peer) = UnixStream::pair().unwrap();
        let (second, mut second_peer) = UnixStream::pair().unwrap();
        rights(
            &sender,
            &header(schema, false),
            &[first.as_raw_fd(), second.as_raw_fd()],
        );
        drop((first, second));
        assert!(receiver.recv().unwrap().unwrap().fd.is_none());
        sender.write_all(&header(schema, true)).unwrap();
        let received = receiver.recv().unwrap().unwrap().fd.unwrap();
        assert_ne!(
            unsafe { libc::fcntl(received.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        UnixStream::from(received).write_all(b"first").unwrap();
        let mut bytes = [0; 5];
        first_peer.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"first");
        assert_closed(&mut first_peer);
        drop(sender);
        assert!(
            receiver
                .recv()
                .unwrap_err()
                .to_string()
                .contains("unclaimed")
        );
        assert_closed(&mut second_peer);
        assert!(receiver.recv().is_err());
    }
}

#[test]
fn excess_and_truncated_rights_close_every_descriptor() {
    for schema in [Schema::Modern { version: 8 }, Schema::Legacy { version: 8 }] {
        // 17 exceeds the queue cap; 253 also exceeds recvmsg's control buffer.
        for count in [17, 253] {
            let (sender, receiver) = UnixStream::pair().unwrap();
            let mut receiver = Connection::new(receiver, schema).unwrap();
            let (passed, mut peer) = UnixStream::pair().unwrap();
            rights(
                &sender,
                &header(schema, false),
                &vec![passed.as_raw_fd(); count],
            );
            drop(passed);
            let error = receiver.recv().unwrap_err().to_string();
            assert!(
                error.contains(if count == 17 { "too many" } else { "truncated" }),
                "{error}"
            );
            assert_closed(&mut peer);
            assert!(receiver.recv().is_err());
        }
    }
}

#[test]
fn small_socket_buffers_preserve_frames_and_one_descriptor() {
    for schema in [Schema::Modern { version: 8 }, Schema::Legacy { version: 8 }] {
        // With no reader and a nonblocking small socket, a maximum frame first
        // makes partial progress and then hits EAGAIN. The connection must fail
        // permanently, and the receiver must close the transferred descriptor
        // when the incomplete frame reaches EOF.
        let (sender, receiver) = UnixStream::pair().unwrap();
        let size = 1024_i32;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    sender.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&size as *const i32).cast(),
                    mem::size_of_val(&size) as _,
                )
            },
            0
        );
        sender.set_nonblocking(true).unwrap();
        let mut sender = Connection::new(sender, schema).unwrap();
        let (passed, mut peer) = UnixStream::pair().unwrap();
        assert_eq!(
            sender
                .send(200, &[0x5a; 16_368], Some(passed.as_fd()))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        let mut pending = [0; 16_384];
        let received = unsafe {
            libc::recv(
                receiver.as_raw_fd(),
                pending.as_mut_ptr().cast(),
                pending.len(),
                libc::MSG_PEEK,
            )
        };
        assert!(
            received > 16 && received < pending.len() as isize,
            "partial send: {received}"
        );
        assert!(sender.send(201, b"must not append", None).is_err());
        drop((sender, passed));
        let mut receiver = Connection::new(receiver, schema).unwrap();
        assert_eq!(
            receiver.recv().unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        assert_closed(&mut peer);

        let (sender, receiver) = UnixStream::pair().unwrap();
        let size = 1024_i32;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    sender.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&size as *const i32).cast(),
                    mem::size_of_val(&size) as _,
                )
            },
            0
        );
        sender
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (passed, mut peer) = UnixStream::pair().unwrap();
        let task = std::thread::spawn(move || {
            let mut sender = Connection::new(sender, schema).unwrap();
            for index in 0..32_u8 {
                let payload = vec![index; 16_368];
                sender
                    .send(200, &payload, (index == 7).then(|| passed.as_fd()))
                    .unwrap();
            }
        });
        let mut receiver = Connection::new(receiver, schema).unwrap();
        for index in 0..32_u8 {
            let frame = receiver.recv().unwrap().unwrap();
            assert_eq!(frame.payload, vec![index; 16_368]);
            assert_eq!(frame.fd.is_some(), index == 7);
            if let Some(fd) = frame.fd {
                UnixStream::from(fd).write_all(b"once").unwrap();
            }
        }
        task.join().unwrap();
        assert!(receiver.recv().unwrap().is_none());
        let mut bytes = [0; 4];
        peer.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"once");
        assert_closed(&mut peer);
    }
}

#[test]
fn broken_peer_send_does_not_raise_sigpipe() {
    const CHILD: &str = "TMUXX_TEST_DEFAULT_SIGPIPE";
    if std::env::var_os(CHILD).is_some() {
        // A separate test process avoids changing the parallel parent's signal policy.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
        let (stream, peer) = UnixStream::pair().unwrap();
        drop(peer);
        let mut connection = Connection::new(stream, Schema::Modern { version: 8 }).unwrap();
        assert_eq!(
            connection.send(1, b"closed", None).unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
        return;
    }
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "broken_peer_send_does_not_raise_sigpipe"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
}
