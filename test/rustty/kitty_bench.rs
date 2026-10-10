//! Catnip's steady-state traffic: crop and move uploaded sprites by placement ID.
use super::{Criterion, Duration, Path, Terminal, Throughput, black_box, native};
use base64::Engine;
use rustty_vt::graphics::PlacementId;
use serde::Serialize;
use std::fmt::Write;

#[derive(Serialize)]
struct Input {
    sprites: usize,
    setup: String,
    frames: [String; 2],
}

fn input(sprites: usize) -> Input {
    let rgba = [24, 48, 96, 255].repeat(128 * 128);
    let encoded = base64::engine::general_purpose::STANDARD.encode(rgba);
    let mut setup = String::from("\x1b[?25l");
    for (chunk, data) in encoded.as_bytes().chunks(4096).enumerate() {
        let more = usize::from((chunk + 1) * 4096 < encoded.len());
        if chunk == 0 {
            write!(setup, "\x1b_Ga=t,i=100,f=32,s=128,v=128,q=2,m={more};").unwrap();
        } else {
            write!(setup, "\x1b_Gq=2,m={more};").unwrap();
        }
        setup.push_str(std::str::from_utf8(data).unwrap());
        setup.push_str("\x1b\\");
    }
    let frames =
        std::array::from_fn(|phase| {
            let mut frame = String::from("\x1b[?2026h");
            for i in 0..sprites {
                write!(frame,
                "\x1b[{};{}H\x1b_Ga=p,i=100,p={},x={},y={},w=32,h=32,X=3,Y=5,z={},C=1,q=2\x1b\\",
                (i * 3 + phase) % 32 + 1, (i * 7 + phase) % 128 + 1, i + 1,
                i % 4 * 32, i / 4 % 4 * 32, -400 - i as i32 % 5).unwrap();
            }
            frame.push_str("\x1b[?2026l");
            frame
        });
    Input {
        sprites,
        setup,
        frames,
    }
}

fn check(terminal: &Terminal, sprites: usize, phase: usize) -> u64 {
    let graphics = terminal.graphics();
    assert_eq!(graphics.images.len(), 1);
    let image = &graphics.images[&100];
    assert_eq!([image.width, image.height], [128, 128]);
    assert_eq!(image.pixels.len(), 128 * 128 * 4);
    assert!(image.pixels.chunks_exact(4).all(|p| p == [24, 48, 96, 255]));
    assert_eq!(graphics.placements.len(), sprites);
    let mut ids = vec![false; sprites];
    let mut checksum = 0;
    for placement in &graphics.placements {
        let PlacementId::External(id) = placement.placement_id else {
            panic!("internal ID")
        };
        let i = id as usize - 1;
        assert!(i < sprites && !ids[i]);
        ids[i] = true;
        assert_eq!(placement.image_id, 100);
        assert_eq!(
            placement.row,
            terminal.screen().row((i * 3 + phase) % 32).id
        );
        assert_eq!(placement.col, (i * 7 + phase) % 128);
        assert_eq!(
            placement.source,
            [(i % 4 * 32) as u32, (i / 4 % 4 * 32) as u32, 32, 32]
        );
        assert_eq!(placement.offset, [3, 5]);
        assert_eq!(placement.z, -400 - i as i32 % 5);
        assert_eq!([placement.columns, placement.rows], [0, 0]);
        assert!(!placement.virtual_placement && placement.parent.is_none());
        checksum += u64::from(id);
    }
    assert_eq!(
        terminal.screen().cursor.row,
        ((sprites - 1) * 3 + phase) % 32
    );
    assert_eq!(
        terminal.screen().cursor.col,
        ((sprites - 1) * 7 + phase) % 128
    );
    checksum
}

pub(super) fn placements(c: &mut Criterion) {
    let ghostty = std::env::var_os("GHOSTTY_PRIMITIVES_BIN");
    for sprites in [128, 1024] {
        let input = input(sprites);
        let mut terminal = Terminal::new(128, 32, 0);
        terminal.width_px = 1280;
        terminal.height_px = 640;
        assert!(terminal.feed(input.setup.as_bytes()).is_empty());
        for (phase, frame) in input.frames.iter().enumerate() {
            assert!(terminal.feed(frame.as_bytes()).is_empty());
            check(&terminal, sprites, phase);
        }
        let checksum = check(&terminal, sprites, 1);
        let mut group = c.benchmark_group("rustty/kitty_place");
        group.throughput(Throughput::Elements(2 * sprites as u64));
        group.bench_function(format!("{sprites}_sprites"), |b| {
            b.iter(|| {
                for frame in &input.frames {
                    black_box(black_box(&mut terminal).feed(black_box(frame.as_bytes())));
                }
            });
        });
        check(&terminal, sprites, 1);
        group.finish();
        if let Some(binary) = &ghostty {
            let data = std::env::temp_dir().join(format!(
                "rustty-kitty-{}-{sprites}.json",
                std::process::id()
            ));
            std::fs::write(&data, serde_json::to_vec(&input).unwrap()).unwrap();
            let mut group = c.benchmark_group("ghostty/kitty_place");
            group.throughput(Throughput::Elements(2 * sprites as u64));
            group.bench_function(format!("{sprites}_sprites"), |b| {
                b.iter_custom(|iterations| -> Duration {
                    native(
                        Path::new(binary),
                        "kitty_place",
                        &data,
                        iterations,
                        2 * sprites as u64,
                        checksum,
                    )
                });
            });
            group.finish();
            std::fs::remove_file(data).unwrap();
        }
    }
}
