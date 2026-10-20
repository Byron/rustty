//! C's real SIXEL redraw is compared with retained image pixels and placement geometry.
#[cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;
use base64::Engine;
use serde_json::{Value, json};
use std::collections::BTreeMap;
#[cfg(unix)]
use std::{path::PathBuf, process::Command};
use tmuxx_terminal::Terminal;
fn kitty(control: &str, pixels: &[u8]) -> Vec<u8> {
    format!(
        "\x1b_G{control};{}\x1b\\",
        base64::engine::general_purpose::STANDARD.encode(pixels)
    )
    .into_bytes()
}
fn pixels(terminal: &Terminal) -> BTreeMap<(i64, i64), [u8; 3]> {
    let snapshot = terminal.snapshot();
    let (screen, graphics) = if snapshot.alternate_active {
        (
            snapshot.alternate.as_ref().unwrap(),
            snapshot.alternate_graphics.as_ref().unwrap(),
        )
    } else {
        (&snapshot.primary, &snapshot.primary_graphics)
    };
    let mut pixels = BTreeMap::new();
    let mut ordered = graphics.placements.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|p| {
        (
            p.z,
            graphics
                .images
                .iter()
                .find(|i| i.id == p.image_id)
                .unwrap()
                .tmux_order
                .unwrap_or(u64::from(p.image_id)),
            p.placement_id,
        )
    });
    for placement in ordered {
        let Some(image) = graphics
            .images
            .iter()
            .find(|image| image.id == placement.image_id)
        else {
            continue;
        };
        assert!(!placement.virtual_placement && placement.parent.is_none());
        let Some(row) = screen.all_rows().position(|row| row.id == placement.row) else {
            continue;
        };
        let [width, height] = placement.pixel_size(image, [8, 16]);
        let [sx, sy, sw, sh] = placement.source_rect(image);
        let offset = placement.cell_offset([8, 16]);
        let origin = (
            placement.col as i64 * 8 + i64::from(offset[0]),
            (row as i64 - screen.history_len() as i64) * 16 + i64::from(offset[1]),
        );
        for y in 0..height {
            for x in 0..width {
                let point = (origin.0 + i64::from(x), origin.1 + i64::from(y));
                if point.0 < 0
                    || point.1 < 0
                    || point.0 >= i64::from(snapshot.cols) * 8
                    || point.1 >= i64::from(snapshot.rows) * 16
                {
                    continue;
                }
                let at =
                    (((sy + y * sh / height) * image.width + sx + x * sw / width) * 4) as usize;
                if image.pixels[at + 3] == 0 {
                    continue;
                }
                assert_eq!(image.pixels[at + 3], 255, "fixture uses opaque pixels");
                pixels.insert(point, image.pixels[at..at + 3].try_into().unwrap());
            }
        }
    }
    pixels
}
fn compare(name: &str, cols: u16, rows: u16, steps: Vec<Value>) {
    #[cfg(windows)]
    let (observations, fixture): (Vec<Value>, String) = {
        let remote = remote_reference::RemoteReference::new();
        let output = remote.observe(
            "image_pixels",
            &serde_json::to_vec(&json!({"cols":cols,"rows":rows,"steps":steps})).unwrap(),
        );
        assert!(
            output.status.success(),
            "remote C pixel observation failed: {output:?}"
        );
        (
            serde_json::from_slice(&output.stdout).unwrap(),
            format!("remote C pixel fixture {name}"),
        )
    };
    #[cfg(unix)]
    let (observations, fixture, directory): (Vec<Value>, String, PathBuf) = {
        let directory = PathBuf::from(format!("/tmp/txpixels-{}-{name}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let fixture = directory.join("fixture.json");
        let output = directory.join("result.json");
        std::fs::write(
            &fixture,
            serde_json::to_vec(&json!({"cols":cols,"rows":rows,"steps":steps})).unwrap(),
        )
        .unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap()
            .to_owned();
        let status = Command::new("python3")
            .arg("-B")
            .arg(root.join("tmuxx/conformance/image_pixels.py"))
            .arg(std::env::var_os("TMUXX_C_TMUX").expect("set TMUXX_C_TMUX"))
            .arg(&fixture)
            .arg(&output)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "C pixel observation failed; fixture {}",
            fixture.display()
        );
        (
            serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap(),
            fixture.display().to_string(),
            directory,
        )
    };
    let mut terminal = Terminal::new(cols, rows, 2000);
    terminal.set_cell_size(8, 16);
    let mut failures = Vec::new();
    for (index, (step, expected)) in steps.iter().zip(observations).enumerate() {
        if let Some(feed) = step["feed"].as_array() {
            let bytes = feed
                .iter()
                .map(|byte| byte.as_u64().unwrap() as u8)
                .collect::<Vec<_>>();
            let chunk = step["feed_chunk"]
                .as_u64()
                .unwrap_or(bytes.len().max(1) as u64) as usize;
            for bytes in bytes.chunks(chunk) {
                terminal.feed(bytes);
            }
        }
        if let Some(limit) = step["limit"].as_u64() {
            terminal.set_image_size_limit(limit as usize);
        }
        if let Some(size) = step["resize"].as_array() {
            terminal.resize(
                size[0].as_u64().unwrap() as u16,
                size[1].as_u64().unwrap() as u16,
            );
        }
        if step["respawn"].as_bool() == Some(true) {
            let (cols, rows) = terminal.size();
            terminal = Terminal::new(cols, rows, 2000);
            terminal.set_cell_size(8, 16);
        }
        if name.starts_with("sixel") && step["respawn"].as_bool() != Some(true) {
            let cursor = expected["cursor"].as_array().unwrap();
            assert_eq!(
                terminal.cursor(),
                (
                    cursor[0].as_u64().unwrap() as usize,
                    cursor[1].as_u64().unwrap() as usize
                ),
                "{name} step{index} cursor"
            );
        }
        let actual = pixels(&terminal);
        let expected = expected["pixels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pixel| {
                let p = pixel.as_array().unwrap();
                (
                    (p[0].as_i64().unwrap(), p[1].as_i64().unwrap()),
                    [
                        p[2].as_u64().unwrap() as u8,
                        p[3].as_u64().unwrap() as u8,
                        p[4].as_u64().unwrap() as u8,
                    ],
                )
            })
            .collect::<BTreeMap<_, _>>();
        if actual.keys().ne(expected.keys()) {
            failures.push(format!(
                "{name} step{index}: positions Rust{} C{}, Rust-only {:?}, C-only {:?}",
                actual.len(),
                expected.len(),
                actual
                    .keys()
                    .filter(|p| !expected.contains_key(p))
                    .take(3)
                    .collect::<Vec<_>>(),
                expected
                    .keys()
                    .filter(|p| !actual.contains_key(p))
                    .take(3)
                    .collect::<Vec<_>>()
            ));
            continue;
        }
        for (point, color) in &expected {
            assert!(
                color
                    .iter()
                    .zip(actual[point])
                    .all(|(c, r)| c.abs_diff(r) <= 3),
                "{name} step{index} {point:?}: {color:?} {:?}",
                actual[point]
            );
        }
        let restored = terminal.snapshot().restore().unwrap();
        let restored_pixels = pixels(&restored);
        assert!(
            restored_pixels == actual,
            "snapshot image state at {name} step{index}: live{} restored{}",
            actual.len(),
            restored_pixels.len()
        );
    }
    assert!(
        failures.is_empty(),
        "{}; retained fixture {}",
        failures.join("\n"),
        fixture
    );
    #[cfg(unix)]
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
#[ignore = "requires real C graphical PTY and TMUXX_C_TMUX with images"]
fn kitty_retained_pixels_clipping_alternate_and_respawn_match_c_redraw() {
    let raw = (0..32)
        .flat_map(|_| {
            (0..32).flat_map(|x| {
                if x < 16 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                }
            })
        })
        .collect::<Vec<_>>();
    let upload = kitty("a=t,i=7,f=32,s=32,v=32", &raw);
    let place = kitty("a=p,i=7,p=9,C=1", &[]);
    let mut steps = vec![json!({"feed":([upload,place.clone()].concat())})];
    for width in [1, 40, 2, 40] {
        steps.push(json!({"resize":[width,16]}));
    }
    steps.extend([
        json!({"feed":([b"\x1b[4;5H".to_vec(),place.clone()].concat())}),
        json!({"feed":b"\x1b[?1049h".to_vec()}),
        json!({"feed":([b"\x1b[H".to_vec(),kitty("a=T,i=8,p=9,f=32,s=1,v=1,C=1,c=1,r=1",&[0,255,0,255])].concat())}),
        json!({"feed":b"\x1b[?1049l".to_vec()}),
        json!({"respawn":true}),json!({"feed":place}),
    ]);
    compare("lifetime", 40, 16, steps);
}
#[test]
#[ignore = "requires real C graphical PTY and TMUXX_C_TMUX with images"]
fn kitty_image_anchor_reflow_between_wrapped_text_matches_c_redraw() {
    let raw = [255, 0, 0, 255].repeat(32 * 32);
    let text = b"abcdefghijklmnopqrstuvwx";
    let feed = [
        text.to_vec(),
        b"\r\n".to_vec(),
        kitty("a=t,i=7,f=32,s=32,v=32", &raw),
        kitty("a=p,i=7,p=9", &[]),
        b"\r\n".to_vec(),
        text.to_vec(),
        b"\r\n".to_vec(),
    ]
    .concat();
    let mut steps = vec![json!({"feed":feed})];
    for width in [8, 16, 8, 16] {
        steps.push(json!({"resize":[width,20]}));
    }
    compare("reflow", 16, 20, steps);
}
#[test]
fn snapshot_keeps_image_row_anchors_after_reflow() {
    let mut terminal = Terminal::new(16, 20, 2000);
    terminal.set_cell_size(8, 16);
    terminal.feed(
        &[
            b"abcdefghijklmnopqrstuvwx\r\n".to_vec(),
            kitty("a=T,i=7,f=32,s=32,v=32", &[255, 0, 0, 255].repeat(32 * 32)),
        ]
        .concat(),
    );
    for cols in [8, 16, 8, 16] {
        terminal.resize(cols, 20);
        let before = terminal.snapshot();
        let restored = before.restore().unwrap();
        assert!(!pixels(&terminal).is_empty());
        assert!(pixels(&restored) == pixels(&terminal));
        assert_eq!(
            restored
                .snapshot()
                .primary
                .all_rows()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            before.primary.all_rows().map(|r| r.id).collect::<Vec<_>>()
        );
    }
}
#[test]
#[ignore = "requires real C graphical PTY and TMUXX_C_TMUX with images"]
fn kitty_cross_screen_replacement_matches_c_redraw() {
    let red = [255, 0, 0, 255].repeat(16 * 16);
    let blue = [0, 0, 255, 255].repeat(16 * 16);
    compare(
        "cross-screen",
        40,
        16,
        vec![
            json!({"feed": kitty("a=T,i=7,p=9,f=32,s=16,v=16,C=1",&red)}),
            json!({"feed": b"\x1b[?1049h".to_vec()}),
            json!({"feed": kitty("a=T,i=7,p=9,f=32,s=16,v=16,C=1",&blue)}),
            json!({"feed": b"\x1b[?1049l".to_vec()}),
            json!({"feed": ([b"\x1b[4;1H".to_vec(),kitty("a=p,i=7,p=10,C=1",&[])].concat())}),
        ],
    );
}
#[test]
fn snapshot_keeps_terminal_options_and_rejects_invalid_widths() {
    let mut terminal = Terminal::new(80, 24, 2000);
    terminal.set_screen_options(false, false);
    terminal.set_allow_set_title(false);
    terminal.set_cursor_options(6, Some([1, 2, 3]), &Terminal::default_palette());
    terminal.set_width_options(&[(0xe9, 0xe9, 2)], false);
    let mut restored = terminal.snapshot().restore().unwrap();
    restored.feed("é\x1b[?1049h\x1b]2;ignored\x07".as_bytes());
    let state = restored.snapshot();
    assert_eq!(state.primary.cursor.col, 2);
    assert!(!state.alternate_active);
    assert!(state.title.is_empty());
    assert_eq!(state.settings.cursor_style, 6);
    let mut corrupt = state;
    corrupt.settings.widths.push((5, 4, 1));
    assert!(corrupt.restore().is_err());
}

fn sixel(width: usize, height: usize, color: [u8; 3], transparent: bool, holes: bool) -> Vec<u8> {
    let mut wire = format!(
        "\x1bP9;{}q\"1;1;{width};{height}#0;2;{};{};{}#0",
        u8::from(transparent),
        color[0],
        color[1],
        color[2]
    )
    .into_bytes();
    for band in (0..height).step_by(6) {
        if band != 0 {
            wire.push(b'-');
        }
        for x in 0..width {
            let mut pattern = 0;
            for bit in 0..6 {
                let y = band + bit;
                if y < height && (!holes || (x / 8 != 1 || y >= 16) && (x + y) % 3 != 0) {
                    pattern |= 1 << bit;
                }
            }
            wire.push(b'?' + pattern);
        }
    }
    wire.extend_from_slice(b"\x1b\\");
    wire
}
#[test]
#[ignore = "requires image-enabled C graphical PTY and TMUXX_C_TMUX"]
fn sixel_decoding_palette_background_and_rejections_match_c_pixels() {
    let mut steps = Vec::new();
    let clear = b"\x1b[2J\x1b[H".as_slice();
    for (width, height) in [(8, 16), (13, 19), (1, 1)] {
        steps.push(
            json!({"feed":([clear.to_vec(),sixel(width,height,[100,0,0],true,false)].concat())}),
        );
    }
    for hue in [0, 120, 240] {
        steps.push(json!({"feed":([clear.to_vec(),format!("\x1bP9;1q\"1;1;8;16#0;1;{hue};50;100#0!8~-!8~-!8N\x1b\\").into_bytes()].concat())}));
    }
    steps.push(json!({"feed":([clear.to_vec(),b"\x1bP9;1q\"1;1;8;16#5!8~-!8~-!8N\x1b\\".to_vec()].concat())}));
    let stray = sixel(8, 16, [0, 100, 0], true, false)
        .into_iter()
        .flat_map(|b| {
            if b == b'-' {
                vec![b' ', b'\t', 127, 128, b]
            } else {
                vec![b]
            }
        })
        .collect::<Vec<_>>();
    steps.push(json!({"feed":([clear.to_vec(),stray].concat()),"feed_chunk":1}));
    steps.push(json!({"limit":4096}));
    for body in [
        b"q".as_slice(),
        b"q#0;2;100",
        b"q!0~",
        b"q!10001~",
        b"q#1025~",
        b"q#0;2;101;0;0~",
        b"q#0;1;361;50;100~",
        b"q\"1;1;10001;1",
        b"q\"1;1;1;10001",
        b"q\"1;1;32;33",
        b"q\"1;1;18446744073709551616;1",
        b"q\"1;1;32;1#0;2;100;0;0@------@",
        b"q\"1;1;1;32#0;2;100;0;0!33@",
    ] {
        steps.push(json!({"feed":([clear,b"\x1bP9;1",body,b"\x1b\\"].concat())}));
    }
    compare("sixel-decode", 40, 16, steps);
}
#[test]
#[ignore = "requires image-enabled C graphical PTY and TMUXX_C_TMUX"]
fn sixel_transparent_overlap_erase_resize_scroll_and_lifetime_match_c_pixels() {
    for background in [false, true] {
        let mut steps = vec![
            json!({"feed":([b"\x1b[3;3H".to_vec(),sixel(24,32,[100,0,0],true,false),b"\x1b[3;3H".to_vec(),sixel(24,32,[0,0,100],background,true)].concat())}),
        ];
        for width in [4, 40] {
            steps.push(json!({"resize":[width,16]}));
        }
        steps.push(json!({"feed":b"\x1b[3;3H\x1b[X".to_vec()}));
        steps.push(json!({"feed":b"\x1b[3;4HQ".to_vec()}));
        steps.push(json!({"feed":b"\x1b[?1049h".to_vec()}));
        steps.push(json!({"feed":sixel(8,16,[0,100,0],true,false)}));
        steps.push(json!({"feed":b"\x1b[?1049l".to_vec()}));
        steps.push(
            json!({"feed":([b"\x1b[16;1H".to_vec(),sixel(8,32,[0,100,0],true,false)].concat())}),
        );
        steps.push(json!({"respawn":true}));
        compare(
            if background {
                "sixel-transparent"
            } else {
                "sixel-opaque"
            },
            40,
            16,
            steps,
        );
    }
}
#[test]
fn sixel_snapshot_resumes_every_input_boundary_without_early_placement() {
    let wire = sixel(8, 16, [100, 0, 0], true, false);
    let mut full = Terminal::new(40, 16, 2000);
    full.set_cell_size(8, 16);
    full.feed(&wire);
    for cut in 0..wire.len() {
        let mut terminal = Terminal::new(40, 16, 2000);
        terminal.set_cell_size(8, 16);
        terminal.feed(&wire[..cut]);
        assert!(terminal.snapshot().primary_graphics.placements.is_empty());
        let mut restored = terminal.snapshot().restore().unwrap();
        restored.feed(&wire[cut..]);
        assert_eq!(pixels(&restored), pixels(&full), "input boundary {cut}");
        assert_eq!(restored.cursor(), full.cursor());
    }
}

#[test]
#[ignore = "requires image-enabled C graphical PTY and TMUXX_C_TMUX"]
fn kitty_source_crops_offsets_and_deletion_match_c_pixels() {
    let mut pixels = Vec::new();
    for y in 0..32 {
        for x in 0..16 {
            pixels.extend(if x < 8 {
                if y < 16 {
                    [255, 0, 0, 255]
                } else {
                    [0, 255, 0, 255]
                }
            } else if y < 16 {
                [0, 0, 255, 255]
            } else {
                [255, 255, 255, 255]
            });
        }
    }
    let upload = kitty("a=t,i=7,f=32,s=16,v=32", &pixels);
    let mut steps = Vec::new();
    for control in [
        "a=p,i=7,p=1,C=1,x=8,y=16,w=8,h=16",
        "a=p,i=7,p=1,C=1,x=8,y=16,w=80,h=80",
        "a=p,i=7,p=1,C=1,x=0,y=16,w=8,h=16,X=3,Y=5",
        "a=p,i=7,p=1,C=1,x=8,y=0,w=8,h=16,X=50,Y=50,c=2,r=2",
        "a=p,i=7,p=1,C=1,x=0,y=16,w=8,h=16,c=2",
        "a=p,i=7,p=1,C=1,x=8,y=0,w=8,h=16,r=2",
    ] {
        steps.push(json!({"feed":([b"\x1b[2J\x1b[H".to_vec(),upload.clone(),kitty(control,&[])].concat())}));
    }
    let display = [
        b"\x1b[2J\x1b[H".to_vec(),
        kitty("a=t,i=7,f=32,s=1,v=1", &[255, 0, 0, 255]),
        kitty("a=t,i=8,f=32,s=1,v=1", &[0, 255, 0, 255]),
        kitty("a=p,i=7,p=1,C=1,c=1,r=2", &[]),
        b"\x1b[1;3H".to_vec(),
        kitty("a=p,i=7,p=2,C=1,c=1,r=2", &[]),
        b"\x1b[1;5H".to_vec(),
        kitty("a=p,i=8,p=1,C=1,c=1,r=2", &[]),
        b"\x1b[1;7H".to_vec(),
        kitty("a=T,f=32,s=1,v=1,C=1,c=1,r=2", &[0, 0, 255, 255]),
        b"\x1b[1;9H".to_vec(),
        sixel(8, 32, [100, 100, 100], true, false),
    ]
    .concat();
    for control in [
        "a=d,d=i,i=7,p=1",
        "a=d,d=i,i=7",
        "a=d,d=I,i=8,p=1",
        "a=d,d=I,i=7,p=999",
        "a=d,d=a",
        "a=d",
        "a=d,d=A,i=7,p=1",
    ] {
        steps.push(json!({"feed":display.clone()}));
        steps.push(json!({"feed":kitty(control,&[])}));
    }
    steps.push(json!({"feed":([b"\x1b[2J\x1b[H".to_vec(),upload,kitty("a=p,i=7,p=1,C=1", &[]),b"\x1b[?1049h\x1b[H".to_vec(),kitty("a=p,i=7,p=2,C=1",&[])].concat())}));
    steps.push(json!({"feed":kitty("a=d,d=I,i=7",&[])}));
    steps.push(json!({"feed":b"\x1b[?1049l".to_vec()}));
    compare("kitty-crop-delete", 40, 16, steps);
}
