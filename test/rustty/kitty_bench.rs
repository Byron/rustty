//! Retained Kitty sprites, Tix-style cell updates, and whole-image eviction.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    lanes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<usize>,
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
                i % 4 * 32, i / 4 % 4 * 32, -400 - i as i32 % 5)
                .unwrap();
            }
            frame.push_str("\x1b[?2026l");
            frame
        });
    Input {
        sprites,
        setup,
        frames,
        lanes: None,
        images: None,
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

const CELL_ROWS: usize = 80;

fn upload_cell(output: &mut String, number: usize) {
    let color = [number as u8, (number >> 8) as u8, (number * 17) as u8, 255];
    let encoded = base64::engine::general_purpose::STANDARD.encode(color.repeat(10 * 20));
    write!(
        output,
        "\x1b_Ga=t,I={number},f=32,s=10,v=20,q=2;{encoded}\x1b\\"
    )
    .unwrap();
}

fn cell_input(lanes: usize, images: usize) -> Input {
    let mut setup = String::from("\x1b[?25l");
    for number in 1..=images {
        upload_cell(&mut setup, number);
    }
    let sprites = CELL_ROWS * lanes;
    let frames = std::array::from_fn(|phase| {
        let mut frame = String::from("\x1b[?2026h");
        for i in 0..sprites {
            let old = (i + (1 - phase) * images / 2) % images + 1;
            let new = (i + phase * images / 2) % images + 1;
            let id = ((i / lanes) << 16 | (i % lanes)) + 1;
            write!(frame,
                "\x1b_Ga=d,d=n,I={old},p={id},q=2\x1b\\\x1b[{};{}H\x1b_Ga=p,I={new},p={id},c=1,r=1,C=1,q=2\x1b\\",
                i / lanes + 1, i % lanes + 1).unwrap();
        }
        frame.push_str("\x1b[?2026l");
        frame
    });
    Input {
        sprites,
        setup,
        frames,
        lanes: Some(lanes),
        images: Some(images),
    }
}

fn check_cells(terminal: &Terminal, lanes: usize, images: usize, phase: usize) -> u64 {
    check_remaining_cells(terminal, lanes, images, phase, 0)
}

fn check_remaining_cells(
    terminal: &Terminal,
    lanes: usize,
    images: usize,
    phase: usize,
    deleted: usize,
) -> u64 {
    let graphics = terminal.graphics();
    assert_eq!(graphics.images.len(), images - deleted);
    let mut numbers = vec![false; images];
    for image in graphics.images.values() {
        let number = image.number as usize;
        assert!((deleted + 1..=images).contains(&number) && !numbers[number - 1]);
        numbers[number - 1] = true;
        assert_eq!([image.width, image.height], [10, 20]);
        assert_eq!(image.pixels.len(), 10 * 20 * 4);
        let color = [number as u8, (number >> 8) as u8, (number * 17) as u8, 255];
        assert!(image.pixels.chunks_exact(4).all(|pixel| pixel == color));
    }
    assert_eq!(
        graphics.placements.len(),
        (0..CELL_ROWS * lanes)
            .filter(|i| (i + phase * images / 2) % images + 1 > deleted)
            .count()
    );
    let mut ids = vec![false; CELL_ROWS * lanes];
    let mut checksum = 0;
    for placement in &graphics.placements {
        let PlacementId::External(id) = placement.placement_id else {
            panic!("internal ID")
        };
        let row = (id as usize - 1) >> 16;
        let col = (id as usize - 1) & 0xffff;
        assert!(row < CELL_ROWS && col < lanes);
        let i = row * lanes + col;
        assert!(!ids[i]);
        ids[i] = true;
        let image = &graphics.images[&placement.image_id];
        assert_eq!(image.number as usize, (i + phase * images / 2) % images + 1);
        assert_eq!(placement.row, terminal.screen().row(i / lanes).id);
        assert_eq!(placement.col, i % lanes);
        assert_eq!([placement.columns, placement.rows], [1, 1]);
        assert_eq!(placement.source, [0; 4]);
        assert_eq!(placement.offset, [0; 2]);
        assert_eq!(placement.z, 0);
        assert!(!placement.virtual_placement && placement.parent.is_none());
        checksum += u64::from(id) * u64::from(image.number);
    }
    assert_eq!(terminal.screen().cursor.row, CELL_ROWS - 1);
    assert_eq!(terminal.screen().cursor.col, lanes - 1);
    assert!(!terminal.screen().cursor.visible);
    assert!(!terminal.modes.dec(2026));
    assert_eq!(terminal.screen().history_len(), 0);
    assert!(
        terminal
            .screen()
            .rows()
            .all(|row| row.cells.iter().all(|cell| cell.codepoint().is_none()))
    );
    checksum
}

pub(super) fn cells(c: &mut Criterion) {
    let ghostty = std::env::var_os("GHOSTTY_PRIMITIVES_BIN");
    for (lanes, images) in [(32, 256), (80, 1024)] {
        let input = cell_input(lanes, images);
        let mut terminal = Terminal::new(128, CELL_ROWS as u16, 0);
        terminal.width_px = 1280;
        terminal.height_px = CELL_ROWS as u32 * 20;
        assert!(terminal.feed(input.setup.as_bytes()).is_empty());
        for (phase, frame) in input.frames.iter().enumerate() {
            assert!(terminal.feed(frame.as_bytes()).is_empty());
            check_cells(&terminal, lanes, images, phase);
        }
        let checksum = check_cells(&terminal, lanes, images, 1);
        let name = format!("{lanes}_lanes_80_rows_{images}_images");
        let units = 2 * input.sprites as u64;
        for after_image_delete in [false, true] {
            let group_name = if after_image_delete {
                assert!(terminal.feed(b"\x1b_Ga=d,d=n,I=1,q=2\x1b\\").is_empty());
                assert!(terminal.feed(input.frames[1].as_bytes()).is_empty());
                assert_eq!(check_cells(&terminal, lanes, images, 1), checksum);
                "rustty/kitty_cells_after_image_delete"
            } else {
                "rustty/kitty_cells"
            };
            let mut group = c.benchmark_group(group_name);
            group.throughput(Throughput::Elements(units));
            group.bench_function(&name, |b| {
                b.iter(|| {
                    for frame in &input.frames {
                        black_box(black_box(&mut terminal).feed(black_box(frame.as_bytes())));
                    }
                });
            });
            assert_eq!(check_cells(&terminal, lanes, images, 1), checksum);
            group.finish();
        }
        assert!(terminal.feed(b"\x1b_Ga=d,d=A,q=2\x1b\\").is_empty());
        assert!(terminal.graphics().images.is_empty());
        assert!(terminal.graphics().placements.is_empty());
        if let Some(binary) = &ghostty {
            let data = std::env::temp_dir().join(format!(
                "rustty-kitty-cells-{}-{lanes}.json",
                std::process::id()
            ));
            std::fs::write(&data, serde_json::to_vec(&input).unwrap()).unwrap();
            let mut group = c.benchmark_group("ghostty/kitty_cells");
            group.throughput(Throughput::Elements(units));
            group.bench_function(&name, |b| {
                b.iter_custom(|iterations| {
                    native(
                        Path::new(binary),
                        "kitty_cells",
                        &data,
                        iterations,
                        units,
                        checksum,
                    )
                });
            });
            group.finish();
            std::fs::remove_file(data).unwrap();
        }
    }
}

/// Time deletion only; cloning, replacement uploads, and teardown are excluded.
pub(super) fn image_deletions(c: &mut Criterion) {
    let (lanes, images) = (80, 1024);
    let input = cell_input(lanes, images);
    let mut populated = Terminal::new(128, CELL_ROWS as u16, 0);
    populated.width_px = 1280;
    populated.height_px = CELL_ROWS as u32 * 20;
    assert!(populated.feed(input.setup.as_bytes()).is_empty());
    assert!(populated.feed(input.frames[1].as_bytes()).is_empty());
    let checksum = check_cells(&populated, lanes, images, 1);
    for (name, deleted, all_visible) in [
        ("64_numbered_images", 64, false),
        ("1024_numbered_images", images, false),
        ("all_visible", images, true),
    ] {
        let mut commands = String::from("\x1b[?2026h");
        if all_visible {
            commands.push_str("\x1b_Ga=d,d=A,q=2\x1b\\");
        } else {
            for number in 1..=deleted {
                write!(commands, "\x1b_Ga=d,d=N,I={number},q=2\x1b\\").unwrap();
            }
        }
        commands.push_str("\x1b[?2026l");
        let mut terminal = populated.clone();
        assert!(terminal.feed(commands.as_bytes()).is_empty());
        check_remaining_cells(&terminal, lanes, images, 1, deleted);
        let mut restore = String::new();
        for number in 1..=deleted {
            upload_cell(&mut restore, number);
        }
        restore.push_str(&input.frames[1]);
        assert!(terminal.feed(restore.as_bytes()).is_empty());
        assert_eq!(check_cells(&terminal, lanes, images, 1), checksum);

        let mut group = c.benchmark_group("rustty/kitty_image_delete");
        group.throughput(Throughput::Elements(deleted as u64));
        group.bench_function(format!("{name}_6400_placements"), |b| {
            b.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let mut terminal = populated.clone();
                    let start = std::time::Instant::now();
                    black_box(terminal.feed(black_box(commands.as_bytes())));
                    elapsed += start.elapsed();
                }
                elapsed
            });
        });
        group.finish();
    }
}

/// Keep the changed images fixed while increasing unrelated placements.
pub(super) fn image_deletion_scaling(c: &mut Criterion) {
    const CHANGED: u32 = 64;
    for unrelated in [0, 1600, 6400, 25600] {
        let mut restore = String::new();
        let mut delete = String::from("\x1b[?2026h");
        for id in 1..=CHANGED {
            write!(
                restore,
                "\x1b_Ga=T,i={id},p=1,C=1,f=32,s=1,v=1,q=2;AQID/w==\x1b\\"
            )
            .unwrap();
            write!(delete, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\").unwrap();
        }
        delete.push_str("\x1b[?2026l");
        let mut terminal = Terminal::new(10, 1, 0);
        assert!(terminal.feed(restore.as_bytes()).is_empty());
        assert!(
            terminal
                .feed(b"\x1b_Gi=65,f=32,s=1,v=1,q=2;AQID/w==\x1b\\")
                .is_empty()
        );
        for id in 1..=unrelated {
            assert!(
                terminal
                    .feed(format!("\x1b_Ga=p,i=65,p={id},C=1,q=2\x1b\\").as_bytes())
                    .is_empty()
            );
        }
        let check = |terminal: &Terminal, restored: bool| {
            let graphics = terminal.graphics();
            assert_eq!(
                graphics.images.len(),
                1 + usize::from(restored) * CHANGED as usize
            );
            assert_eq!(
                graphics.placements.len(),
                unrelated as usize + usize::from(restored) * CHANGED as usize
            );
            for image in graphics.images.values() {
                assert_eq!(image.pixels.as_slice(), [1, 2, 3, 255]);
                assert_eq!([image.width, image.height], [1, 1]);
            }
            for placement in &graphics.placements {
                if placement.image_id == 65 {
                    assert!(
                        matches!(placement.placement_id, PlacementId::External(id) if (1..=unrelated).contains(&id))
                    );
                } else {
                    assert!(restored && (1..=CHANGED).contains(&placement.image_id));
                    assert_eq!(placement.placement_id, PlacementId::External(1));
                }
                assert_eq!(placement.row, terminal.screen().row(0).id);
                assert_eq!(placement.col, 0);
                assert!(placement.parent.is_none());
            }
            assert!(!terminal.modes.dec(2026));
            assert_eq!(terminal.screen().cursor.col, 0);
        };
        check(&terminal, true);
        assert!(terminal.feed(delete.as_bytes()).is_empty());
        check(&terminal, false);
        assert!(terminal.feed(restore.as_bytes()).is_empty());
        check(&terminal, true);
        let mut group = c.benchmark_group("rustty/kitty_image_delete_scaling");
        group.throughput(Throughput::Elements(u64::from(CHANGED)));
        group.bench_function(format!("64_images_{unrelated}_unrelated_placements"), |b| {
            b.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let start = std::time::Instant::now();
                    black_box(terminal.feed(black_box(delete.as_bytes())));
                    elapsed += start.elapsed();
                    // Restore outside timing and retain the live lookup indexes.
                    black_box(terminal.feed(restore.as_bytes()));
                }
                elapsed
            });
        });
        check(&terminal, true);
        group.finish();
    }
}
