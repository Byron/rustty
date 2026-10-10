#![cfg(all(unix, not(target_os = "android")))]

use base64::{Engine, engine::general_purpose::STANDARD};
use rustty_vt::{Effect, Terminal};
use std::{
    ffi::CString,
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
    sync::atomic::{AtomicUsize, Ordering},
};

struct SharedMemory(CString);

impl SharedMemory {
    fn new(data: &[u8]) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let name = CString::new(format!(
            "/rustty-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .unwrap();
        // SAFETY: name is NUL terminated; File owns the newly opened descriptor.
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600,
            )
        };
        assert!(fd >= 0, "{}", std::io::Error::last_os_error());
        let shm = Self(name);
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_len(data.len() as u64).unwrap();
        if !data.is_empty() {
            // SAFETY: the mapping spans the object's initialized length.
            unsafe {
                let ptr = libc::mmap(
                    std::ptr::null_mut(),
                    data.len(),
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    file.as_raw_fd(),
                    0,
                );
                assert_ne!(ptr, libc::MAP_FAILED, "{}", std::io::Error::last_os_error());
                std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.cast(), data.len());
                assert_eq!(libc::munmap(ptr, data.len()), 0);
            }
        }
        shm
    }

    fn command(&self, options: &str) -> Vec<u8> {
        format!(
            "\x1b_Gt=s,{options};{}\x1b\\",
            STANDARD.encode(self.0.as_bytes())
        )
        .into_bytes()
    }

    fn assert_consumed(&self) {
        // SAFETY: the name remains a valid C string.
        let fd = unsafe { libc::shm_open(self.0.as_ptr(), libc::O_RDONLY, 0) };
        if fd >= 0 {
            drop(unsafe { File::from_raw_fd(fd) });
            panic!("terminal did not unlink shared memory");
        }
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ENOENT)
        );
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        // SAFETY: cleanup also covers failed assertions before the terminal consumes it.
        unsafe {
            libc::shm_unlink(self.0.as_ptr());
        }
    }
}

#[test]
fn catnip_shared_memory_upload_place_and_verify() {
    let mut terminal = Terminal::new(80, 24, 100);
    terminal.set_pixel_size(800, 480);
    terminal.feed(b"\x1b[?1049h");
    let shm = SharedMemory::new(&[10, 20, 30, 255, 40, 50, 60, 128]);
    assert!(
        terminal
            .feed(&shm.command("a=t,i=1,f=32,s=2,v=1,q=1"))
            .is_empty()
    );
    shm.assert_consumed();
    assert_eq!(
        terminal.graphics().images[&1].pixels.as_ref(),
        [10, 20, 30, 255, 40, 50, 60, 128]
    );
    let pixels = terminal.graphics().images[&1].pixels.clone();
    terminal.feed(b"\x1b[?2026h\x1b[2;3H\x1b_Ga=p,i=1,p=1,x=1,y=0,w=1,h=1,X=3,Y=4,z=2,C=1,q=2\x1b\\\x1b[?2026l");
    assert_eq!(terminal.graphics().placements.len(), 1);
    assert_eq!(terminal.graphics().placements[0].source, [1, 0, 1, 1]);
    assert_eq!(terminal.graphics().placements[0].offset, [3, 4]);
    assert!(std::sync::Arc::ptr_eq(
        &pixels,
        &terminal.graphics().images[&1].pixels
    ));
    assert_eq!(
        terminal.feed(b"\x1b_Ga=q,i=2130706433,s=1,v=1,f=24,t=d;AAAA\x1b\\"),
        [Effect::Write(b"\x1b_Gi=2130706433;OK\x1b\\".to_vec())]
    );
}

#[test]
fn shared_memory_ranges_queries_and_errors_release_objects() {
    let mut terminal = Terminal::new(10, 3, 10);
    for (options, succeeds) in [
        ("O=2", true),
        ("O=2,S=4", true),
        ("O=2,S=3", false),
        ("O=4294967295", false),
        ("S=4294967295", false),
        ("s=10001", false),
    ] {
        let shm = SharedMemory::new(&[0, 0, 10, 20, 30, 255]);
        let effects = terminal.feed(&shm.command(&format!("a=q,i=1,f=32,s=1,v=1,{options}")));
        assert_eq!(
            effects == [Effect::Write(b"\x1b_Gi=1;OK\x1b\\".to_vec())],
            succeeds,
            "{options}: {effects:?}"
        );
        shm.assert_consumed();
        assert!(terminal.graphics().images.is_empty());
    }
    let shm = SharedMemory::new(&[]);
    assert_eq!(
        terminal.feed(&shm.command("a=q,i=1,f=32,s=1,v=1")),
        [Effect::Write(
            b"\x1b_Gi=1;EINVAL: invalid data\x1b\\".to_vec()
        )]
    );
    shm.assert_consumed();
    for name in [
        b"".as_slice(),
        b"/",
        b"no-slash",
        b"/nested/name",
        b"/nul\0suffix",
        &[b'x'; 256],
    ] {
        let command = format!(
            "\x1b_Ga=q,i=1,t=s,f=32,s=1,v=1;{}\x1b\\",
            STANDARD.encode(name)
        );
        assert_eq!(
            terminal.feed(command.as_bytes()),
            [Effect::Write(
                b"\x1b_Gi=1;EINVAL: invalid data\x1b\\".to_vec()
            )]
        );
    }
}

#[test]
fn shared_memory_uses_pixel_decoders_and_ignores_the_chunk_flag() {
    use std::io::Write;
    let mut terminal = Terminal::new(10, 3, 10);
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    zlib.write_all(&[90, 80, 70]).unwrap();
    let compressed = zlib.finish().unwrap();
    let shm = SharedMemory::new(&compressed);
    assert_eq!(
        terminal.feed(&shm.command(&format!(
            "a=T,i=2,f=24,s=1,v=1,o=z,S={},C=1",
            compressed.len()
        ))),
        [Effect::Write(b"\x1b_Gi=2;OK\x1b\\".to_vec())]
    );
    shm.assert_consumed();
    assert_eq!(
        terminal.graphics().images[&2].pixels.as_ref(),
        [90, 80, 70, 255]
    );
    assert_eq!(terminal.graphics().placements.len(), 1);

    let mut png_data = Vec::new();
    {
        let mut png = png::Encoder::new(&mut png_data, 1, 1);
        png.set_color(png::ColorType::Rgba);
        png.set_depth(png::BitDepth::Eight);
        png.write_header()
            .unwrap()
            .write_image_data(&[1, 2, 3, 128])
            .unwrap();
    }
    let shm = SharedMemory::new(&png_data);
    assert_eq!(
        terminal.feed(&shm.command(&format!("i=3,f=100,S={}", png_data.len()))),
        [Effect::Write(b"\x1b_Gi=3;OK\x1b\\".to_vec())]
    );
    shm.assert_consumed();
    assert_eq!(
        terminal.graphics().images[&3].pixels.as_ref(),
        [1, 2, 3, 128]
    );

    let shm = SharedMemory::new(&[4, 5, 6]);
    assert_eq!(
        terminal.feed(&shm.command("i=4,f=24,s=1,v=1,m=1")),
        [Effect::Write(b"\x1b_Gi=4;OK\x1b\\".to_vec())]
    );
    shm.assert_consumed();
    assert_eq!(
        terminal.feed(b"\x1b_Gi=5,f=24,s=1,v=1;BwgJ\x1b\\"),
        [Effect::Write(b"\x1b_Gi=5;OK\x1b\\".to_vec())]
    );
    assert_eq!(
        terminal.graphics().images[&4].pixels.as_ref(),
        [4, 5, 6, 255]
    );
    assert_eq!(
        terminal.graphics().images[&5].pixels.as_ref(),
        [7, 8, 9, 255]
    );
    terminal.feed(b"\x1b_Gi=6,f=32,s=1,v=1,m=1;AQI=\x1b\\");
    assert_eq!(
        terminal.feed(b"\x1b_Gt=s,m=1;L3g=\x1b\\"),
        [Effect::Write(b"\x1b_Gi=6;OK\x1b\\".to_vec())]
    );
    assert_eq!(
        terminal.graphics().images[&6].pixels.as_ref(),
        [1, 2, b'/', b'x']
    );
}
