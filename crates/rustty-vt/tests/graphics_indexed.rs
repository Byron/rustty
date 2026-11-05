use base64::{Engine, engine::general_purpose::STANDARD};
use rustty_vt::{
    Effect, Terminal,
    graphics::{Graphics, quota::Quota},
};
use std::io::Write;

fn command(options: &str, bytes: &[u8]) -> Vec<u8> {
    format!("\x1b_G{options};{}\x1b\\", STANDARD.encode(bytes)).into_bytes()
}

fn ok(id: u32) -> Vec<Effect> {
    vec![Effect::Write(format!("\x1b_Gi={id};OK\x1b\\").into_bytes())]
}

#[test]
fn indexed_formats_preserve_indices_alpha_and_static_special_color_fallbacks() {
    for bpp in [1u8, 2] {
        let mut terminal = Terminal::new(10, 3, 100);
        terminal.foreground = [1, 2, 3];
        terminal.background = [4, 5, 6];
        terminal.cursor_color = Some([7, 8, 9]);
        let bytes = (0..=255u8)
            .flat_map(|index| {
                if bpp == 1 {
                    vec![index]
                } else {
                    vec![index, index]
                }
            })
            .collect::<Vec<_>>();
        let query = command(&format!("a=q,i=9,f={},s=256,v=1", bpp * 8), &bytes);
        assert_eq!(terminal.feed(&query), ok(9));
        assert!(terminal.graphics().images.is_empty());
        assert_eq!(
            terminal.feed(&command(
                &format!("a=T,i=1,f={},s=256,v=1,C=1", bpp * 8),
                &bytes
            )),
            ok(1)
        );
        let image = &terminal.graphics().images[&1];
        let source = image.indexed.as_ref().unwrap();
        assert_eq!(source.bytes_per_pixel, bpp);
        assert_eq!(source.data.as_slice(), bytes);
        let specials = [
            [4, 5, 6],
            [1, 2, 3],
            [1, 2, 3],
            [4, 5, 6],
            [7, 8, 9],
            [4, 5, 6],
            [1, 2, 3],
            [1, 2, 3],
        ];
        for (index, pixel) in image.pixels.chunks_exact(4).enumerate() {
            assert_eq!(
                pixel[..3],
                if index < 248 {
                    terminal.palette[index]
                } else {
                    specials[index - 248]
                }
            );
            assert_eq!(pixel[3], if bpp == 1 { 255 } else { index as u8 });
        }
        assert_eq!(
            terminal.graphics().bytes_used(),
            256 * (4 + usize::from(bpp))
        );
        assert_eq!(terminal.graphics().placements.len(), 1);
        let fallback = image.pixels.clone();
        terminal.feed(b"\x1b]4;0;#abcdef\x07\x1b]10;#234567\x07");
        assert!(std::sync::Arc::ptr_eq(
            &fallback,
            &terminal.graphics().images[&1].pixels
        ));
    }
}

#[test]
fn indexed_chunked_and_compressed_uploads_keep_first_chunk_format() {
    for bpp in [1u8, 2] {
        for compressed in [false, true] {
            let raw = [0, 64, 128, 255].repeat(usize::from(bpp));
            let data = if compressed {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(&raw).unwrap();
                encoder.finish().unwrap()
            } else {
                raw.clone()
            };
            let split = data.len() / 2;
            let mut terminal = Terminal::new(10, 3, 100);
            let options = format!(
                "i=1,f={},s=4,v=1,m=1{}",
                bpp * 8,
                if compressed { ",o=z" } else { "" }
            );
            assert!(terminal.feed(&command(&options, &data[..split])).is_empty());
            assert_eq!(terminal.feed(&command("m=0,f=32", &data[split..])), ok(1));
            assert_eq!(
                terminal.graphics().images[&1]
                    .indexed
                    .as_ref()
                    .unwrap()
                    .data
                    .as_slice(),
                raw
            );
        }
    }
}

#[test]
fn indexed_compressed_uploads_require_a_complete_zlib_stream() {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&[1, 128]).unwrap();
    let compressed = encoder.finish().unwrap();
    for length in 0..compressed.len() {
        let mut terminal = Terminal::new(10, 3, 100);
        terminal.feed(&command("i=1,f=16,s=1,v=1,o=z", &compressed[..length]));
        assert!(
            terminal.graphics().images.is_empty(),
            "accepted {length}/{} zlib bytes",
            compressed.len()
        );
    }
}

#[test]
fn indexed_uploads_reject_invalid_lengths_dimensions_compression_and_animation() {
    for (options, data) in [
        ("f=8,s=0,v=1", &[1][..]),
        ("f=8,s=10001,v=1", &[1][..]),
        ("f=8,s=2,v=1", &[1][..]),
        ("f=8,s=1,v=1", &[1, 2][..]),
        ("f=16,s=1,v=1", &[1][..]),
        ("f=16,s=1,v=1,o=x", &[1, 255][..]),
        ("f=16,s=1,v=1,o=z", &[1, 255][..]),
    ] {
        let mut terminal = Terminal::new(10, 3, 100);
        let effects = terminal.feed(&command(&format!("i=1,{options}"), data));
        assert!(
            matches!(&effects[..], [Effect::Write(reply)] if reply.windows(6).any(|text| text == b"EINVAL")),
            "{options}: {effects:?}"
        );
        assert!(terminal.graphics().images.is_empty());
    }
    let mut terminal = Terminal::new(10, 3, 100);
    terminal.feed(&command("i=1,f=8,s=1,v=1", &[1]));
    let original = terminal.graphics().images[&1].pixels.clone();
    for options in ["a=f,i=1,f=32,s=1,v=1", "a=a,i=1,s=3", "a=c,i=1,r=1,c=1"] {
        let effects = terminal.feed(&command(options, &[1, 2, 3, 255]));
        assert!(
            matches!(&effects[..], [Effect::Write(reply)] if reply.windows(6).any(|text| text == b"EINVAL"))
        );
        assert!(std::sync::Arc::ptr_eq(
            &original,
            &terminal.graphics().images[&1].pixels
        ));
        assert!(terminal.graphics().images[&1].frames.is_empty());
    }
    terminal.feed(&command("i=1,f=32,s=1,v=1", &[1, 2, 3, 255]));
    assert!(terminal.graphics().images[&1].indexed.is_none());
    for format in [8, 16] {
        let effects = terminal.feed(&command(&format!("a=f,i=1,f={format},s=1,v=1,m=1"), &[1]));
        assert!(
            matches!(&effects[..], [Effect::Write(reply)] if reply.windows(6).any(|text| text == b"EINVAL"))
        );
    }
}

#[test]
fn indexed_storage_and_native_tmux_quotas_charge_source_and_rgba() {
    for bpp in [1u8, 2] {
        let required = 4 + usize::from(bpp);
        for limit in [required - 1, required] {
            for tmux in [false, true] {
                let mut terminal = Terminal::new(10, 3, 100);
                if tmux {
                    terminal.tmux_graphics = true;
                    let quota = Quota::default();
                    quota.set_limits(100, limit);
                    terminal.set_tmux_image_quota(quota);
                } else {
                    terminal.set_graphics_limit(limit);
                }
                terminal.feed(&command(
                    &format!("i=1,f={},s=1,v=1", bpp * 8),
                    &vec![255; usize::from(bpp)],
                ));
                assert_eq!(
                    terminal.graphics().images.len(),
                    usize::from(limit == required),
                    "bpp{bpp} limit{limit} tmux{tmux}"
                );
                assert_eq!(
                    terminal.graphics().bytes_used(),
                    if limit == required { required } else { 0 }
                );
            }
        }
    }
}

#[test]
fn indexed_views_validate_sources_and_preserve_old_peer_fallback() {
    let mut terminal = Terminal::new(10, 3, 100);
    terminal.feed(&command("i=1,f=16,s=2,v=1", &[1, 0, 249, 255]));
    let view = terminal.graphics().view_snapshot();
    let mut legacy = serde_json::to_value(&view).unwrap();
    legacy["images"][0]
        .as_object_mut()
        .unwrap()
        .remove("indexed");
    let legacy = Graphics::from_view_snapshot(serde_json::from_value(legacy).unwrap()).unwrap();
    assert!(legacy.images[&1].indexed.is_none());
    assert_eq!(legacy.images[&1].pixels, view.images[0].pixels);
    assert!(Graphics::from_view_snapshot_with_limit(view.clone(), 11).is_err());
    assert_eq!(
        Graphics::from_view_snapshot_with_limit(view.clone(), 12)
            .unwrap()
            .bytes_used(),
        12
    );
    for (bpp, data) in [(0, vec![1, 2]), (3, vec![1, 2]), (2, vec![1, 2, 3])] {
        let mut invalid = view.clone();
        let source = invalid.images[0].indexed.as_mut().unwrap();
        source.bytes_per_pixel = bpp;
        source.data = std::sync::Arc::new(data);
        assert!(Graphics::from_view_snapshot(invalid).is_err());
    }
    let mut animated = view.clone();
    animated.images[0].animation_state = 3;
    assert!(Graphics::from_view_snapshot(animated).is_err());
    let mut pending = view;
    pending.images[0].indexed.as_mut().unwrap().data = std::sync::Arc::new(Vec::new());
    let pending = Graphics::from_view_snapshot_with_limit(pending, 12).unwrap();
    assert_eq!(pending.bytes_used(), 12);
    assert!(pending.images[&1].has_pending_pixels());
}
