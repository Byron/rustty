use base64::{Engine, engine::general_purpose::STANDARD};
use rustty_vt::{
    Terminal,
    snapshot::{self, DecodeOptions, Decoder},
};

fn fixture(name: &str) -> Vec<u8> {
    let text = std::fs::read_to_string(format!(
        "{}/../../src/terminal/snapshot/testdata/{name}-v1.hex",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    text.lines()
        .flat_map(|line| line.split('#').next().unwrap().split_whitespace())
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect()
}

#[test]
fn upstream_kitty_snapshots_restore_images_placements_and_pixels() {
    for (name, images, placements) in [
        ("kitty-placed", 1, 1),
        ("kitty-source-rect", 1, 1),
        ("kitty-virtual", 1, 1),
        ("kitty-relative", 2, 3),
        ("kitty-history", 1, 4),
        ("kitty-animation", 1, 1),
        ("kitty-alternate", 1, 1),
    ] {
        let bytes = fixture(name);
        let terminal = snapshot::decode(bytes.as_slice(), DecodeOptions::default()).unwrap();
        assert_eq!(terminal.graphics().images.len(), images, "{name}");
        assert_eq!(terminal.graphics().placements.len(), placements, "{name}");
        for image in terminal.graphics().images.values() {
            assert_eq!(
                image.pixels.len(),
                image.width as usize * image.height as usize * 4,
                "{name}"
            );
            assert!(
                image
                    .frames
                    .iter()
                    .all(|frame| frame.pixels.len() == image.pixels.len())
            );
        }
    }
}

#[test]
fn kitty_snapshots_roundtrip_and_resume_interrupted_transmissions() {
    for name in [
        "kitty-placed",
        "kitty-source-rect",
        "kitty-virtual",
        "kitty-relative",
        "kitty-history",
        "kitty-animation",
        "kitty-alternate",
        "kitty-loading",
        "kitty-payload-in-history",
        "unknown-records",
    ] {
        let original =
            snapshot::decode(fixture(name).as_slice(), DecodeOptions::default()).unwrap();
        let encoded = snapshot::encode_to_vec(&original).unwrap();
        let restored = snapshot::decode(encoded.as_slice(), DecodeOptions::default()).unwrap();
        assert_eq!(
            snapshot::encode_to_vec(&restored).unwrap(),
            encoded,
            "{name}"
        );
    }
    let mut restored = snapshot::decode(
        fixture("kitty-loading").as_slice(),
        DecodeOptions::default(),
    )
    .unwrap();
    let pixels = [7, 7, 7, 255].repeat(4);
    assert!(
        restored
            .feed(format!("\x1b_Gm=0;{}\x1b\\", STANDARD.encode(&pixels[6..])).as_bytes())
            .is_empty()
    );
    assert_eq!(restored.graphics().images[&6].pixels.as_slice(), pixels);
    assert_eq!(restored.graphics().placements.len(), 1);

    let mut source = Terminal::new(10, 3, 10);
    source.feed(b"\x1b_Ga=T,i=9,p=4,s=1,v=1,f=32,C=1;/wAA/w==\x1b\\");
    source.feed(b"\x1b_Ga=f,i=9,s=1,v=1,f=32,z=20,m=1;AP8=\x1b\\");
    let encoded = snapshot::encode_to_vec(&source).unwrap();
    let mut restored = snapshot::decode(encoded.as_slice(), DecodeOptions::default()).unwrap();
    restored.feed(b"\x1b_Gm=0;AP8=\x1b\\");
    assert_eq!(
        restored.graphics().images[&9].frames[0].pixels.as_slice(),
        [0, 255, 0, 255]
    );
}

#[test]
fn indexed_snapshots_stage_sources_and_preserve_rgba_for_older_peers() {
    for format in [8, 16] {
        let raw = if format == 8 {
            vec![248, 249]
        } else {
            vec![248, 0, 249, 128]
        };
        let mut source = Terminal::new(10, 3, 100);
        source.feed(
            format!(
                "\x1b_Ga=T,i=1,f={format},s=2,v=1,C=1;{}\x1b\\",
                STANDARD.encode(&raw)
            )
            .as_bytes(),
        );
        let fallback = source.graphics().images[&1].pixels.clone();
        let encoded = snapshot::encode_to_vec(&source).unwrap();
        let mut decoder = Decoder::new(encoded.as_slice(), DecodeOptions::default());
        let mut restored = decoder.ready().unwrap();
        assert_eq!(restored.graphics().bytes_used(), 8 + raw.len());
        assert!(restored.graphics().images[&1].has_pending_pixels());
        assert!(
            restored.graphics().images[&1]
                .indexed
                .as_ref()
                .unwrap()
                .data
                .is_empty()
        );
        decoder.next(&mut restored).unwrap().unwrap();
        assert_eq!(restored.graphics().images[&1].pixels, fallback);
        assert!(restored.graphics().images[&1].has_pending_pixels());
        decoder.next(&mut restored).unwrap().unwrap();
        assert_eq!(
            restored.graphics().images[&1]
                .indexed
                .as_ref()
                .unwrap()
                .data
                .as_slice(),
            raw
        );
        assert!(!restored.graphics().images[&1].has_pending_pixels());
        while decoder.next(&mut restored).unwrap().is_some() {}
        assert_eq!(snapshot::encode_to_vec(&restored).unwrap(), encoded);
        let old = frame(
            &records(&encoded)
                .into_iter()
                .filter(|(tag, _)| !matches!(tag, 0x8005 | 0x8006))
                .collect::<Vec<_>>(),
        );
        let old = snapshot::decode(old.as_slice(), DecodeOptions::default()).unwrap();
        assert!(old.graphics().images[&1].indexed.is_none());
        assert_eq!(old.graphics().images[&1].pixels, fallback);
    }
}

#[test]
fn indexed_snapshots_resume_compressed_and_plain_protocol_chunks() {
    use std::io::Write;
    for format in [8, 16] {
        for compressed in [false, true] {
            let raw = if format == 8 {
                vec![1, 249]
            } else {
                vec![1, 128, 249, 255]
            };
            let bytes = if compressed {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(&raw).unwrap();
                encoder.finish().unwrap()
            } else {
                raw.clone()
            };
            let split = bytes.len() / 2;
            let mut source = Terminal::new(10, 3, 100);
            source.feed(
                format!(
                    "\x1b_Ga=T,i=1,f={format},s=2,v=1,C=1,m=1{};{}\x1b\\",
                    if compressed { ",o=z" } else { "" },
                    STANDARD.encode(&bytes[..split])
                )
                .as_bytes(),
            );
            let encoded = snapshot::encode_to_vec(&source).unwrap();
            let mut restored =
                snapshot::decode(encoded.as_slice(), DecodeOptions::default()).unwrap();
            restored
                .feed(format!("\x1b_Gm=0;{}\x1b\\", STANDARD.encode(&bytes[split..])).as_bytes());
            assert_eq!(
                restored.graphics().images[&1]
                    .indexed
                    .as_ref()
                    .unwrap()
                    .data
                    .as_slice(),
                raw
            );
            assert_eq!(restored.graphics().placements.len(), 1);
            let old = frame(
                &records(&encoded)
                    .into_iter()
                    .filter(|(tag, _)| *tag != 0x8005)
                    .collect::<Vec<_>>(),
            );
            let mut old = snapshot::decode(old.as_slice(), DecodeOptions::default()).unwrap();
            old.feed(format!("\x1b_Gm=0;{}\x1b\\", STANDARD.encode(&bytes[split..])).as_bytes());
            assert!(old.graphics().images.is_empty());
        }
    }
}

#[test]
fn indexed_snapshot_sources_obey_combined_limits_and_cannot_overwrite_live_images() {
    let mut source = Terminal::new(10, 3, 100);
    source.feed(b"\x1b_Ga=T,i=1,f=16,s=1,v=1,C=1;Af8=\x1b\\");
    let bytes = snapshot::encode_to_vec(&source).unwrap();
    for limit in [0, 4, 5, 6] {
        let restored = snapshot::decode(
            bytes.as_slice(),
            DecodeOptions {
                max_graphics_bytes: limit,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(restored.graphics().images.len(), usize::from(limit == 6));
        assert!(restored.graphics().bytes_used() <= limit);
        assert_eq!(
            restored.graphics().placements.len(),
            usize::from(limit == 6)
        );
    }
    let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
    let mut restored = decoder.ready().unwrap();
    decoder.next(&mut restored).unwrap();
    restored.feed(b"\x1b_Gi=1,f=32,s=1,v=1;AQID/w==\x1b\\");
    while decoder.next(&mut restored).unwrap().is_some() {}
    assert!(restored.graphics().images[&1].indexed.is_none());
    assert_eq!(
        restored.graphics().images[&1].pixels.as_slice(),
        [1, 2, 3, 255]
    );
}

#[test]
fn indexed_snapshot_admission_uses_combined_bytes_before_evicting_older_images() {
    for formats in [
        vec![(8, 1), (32, 1)],
        vec![(32, 1), (8, 1)],
        vec![(32, 1), (32, 1), (16, 2)],
    ] {
        let mut source = Terminal::new(10, 3, 100);
        for (index, &(format, width)) in formats.iter().enumerate() {
            let data = vec![1; width * format / 8];
            source.feed(
                format!(
                    "\x1b_Gi={},f={format},s={width},v=1;{}\x1b\\",
                    index + 1,
                    STANDARD.encode(data)
                )
                .as_bytes(),
            );
        }
        let encoded = snapshot::encode_to_vec(&source).unwrap();
        let restored = snapshot::decode(
            encoded.as_slice(),
            DecodeOptions {
                max_graphics_bytes: 8,
                ..Default::default()
            },
        )
        .unwrap();
        let mut ids = restored
            .graphics()
            .images
            .keys()
            .copied()
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(
            ids,
            if formats.len() == 2 {
                vec![2]
            } else {
                vec![1, 2]
            }
        );
        assert!(restored.graphics().bytes_used() <= 8);
    }
}

#[test]
fn indexed_snapshot_chunks_are_bounded_and_validate_metadata_offsets_and_crc() {
    let mut source = Terminal::new(10, 3, 100);
    source.feed(b"\x1b_Gi=1,f=8,s=1,v=1;AQ==\x1b\\");
    let image = source.screen_mut().graphics.images.get_mut(&1).unwrap();
    image.width = 1024;
    image.height = 1025;
    image.pixels = std::sync::Arc::new([1, 2, 3, 255].repeat(1024 * 1025));
    image.indexed.as_mut().unwrap().data = std::sync::Arc::new(vec![1; 1024 * 1025]);
    let bytes = snapshot::encode_to_vec(&source).unwrap();
    let pieces = records(&bytes);
    let chunks = pieces
        .iter()
        .filter(|(tag, _)| *tag == 0x8006)
        .collect::<Vec<_>>();
    assert_eq!(chunks.len(), 2);
    assert!(
        chunks
            .iter()
            .all(|(_, data)| data.len() <= 1024 * 1024 + 15)
    );
    let last_chunk = pieces.iter().rposition(|(tag, _)| *tag == 0x8006).unwrap();
    let mut incomplete = pieces.clone();
    incomplete.remove(last_chunk);
    assert!(snapshot::decode(frame(&incomplete).as_slice(), DecodeOptions::default()).is_err());
    let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
    let mut restored = decoder.ready().unwrap();
    decoder.next(&mut restored).unwrap(); // RGBA fallback.
    let generation = restored.generation;
    let image_generation = restored.graphics().images[&1].generation;
    decoder.next(&mut restored).unwrap(); // First indexed source chunk.
    assert_eq!(restored.generation, generation);
    assert_eq!(restored.graphics().images[&1].generation, image_generation);
    assert!(
        restored.graphics().images[&1]
            .indexed
            .as_ref()
            .unwrap()
            .data
            .is_empty()
    );
    decoder.next(&mut restored).unwrap(); // Final source chunk becomes visible.
    assert_ne!(restored.generation, generation);
    assert_ne!(restored.graphics().images[&1].generation, image_generation);
    while decoder.next(&mut restored).unwrap().is_some() {}
    assert_eq!(
        restored.graphics().images[&1]
            .indexed
            .as_ref()
            .unwrap()
            .data
            .len(),
        1024 * 1025
    );
    for (tag, offset, value) in [(0x8005, 11, 3), (0x8005, 12, 1), (0x8006, 7, 1)] {
        let mut malformed = pieces.clone();
        malformed
            .iter_mut()
            .find(|(found, _)| *found == tag)
            .unwrap()
            .1[offset] = value;
        assert!(snapshot::decode(frame(&malformed).as_slice(), DecodeOptions::default()).is_err());
    }
    let mut damaged = bytes;
    let mut position = 10;
    for (tag, data) in &pieces {
        if *tag == 0x8006 {
            damaged[position + 10 + 15] ^= 1;
            break;
        }
        position += 10 + data.len();
    }
    assert!(
        snapshot::decode(
            damaged.as_slice(),
            DecodeOptions {
                max_graphics_bytes: 0,
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn staged_images_reserve_memory_and_display_before_the_animation_can_play() {
    let bytes = fixture("kitty-animation");
    let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
    let mut terminal = decoder.ready().unwrap();
    assert_eq!(terminal.graphics().bytes_used(), 24);
    assert!(terminal.graphics().images[&5].has_pending_pixels());
    assert!(terminal.graphics().images[&5].display_pixels().is_empty());
    assert_eq!(terminal.tick_graphics(0), None);
    let first = decoder.next(&mut terminal).unwrap().unwrap();
    assert_eq!(
        first,
        snapshot::Progress::Image(snapshot::ImageProgress {
            screen: 0,
            image_id: 5,
            frame: 2,
            bytes: 8
        })
    );
    assert_eq!(
        terminal.graphics().images[&5].display_pixels(),
        &[0, 255, 0, 255].repeat(2)
    );
    assert!(terminal.graphics().images[&5].has_pending_pixels());
    assert_eq!(terminal.tick_graphics(1), None);
    let before = terminal.graphics().images[&5].root_gap_ms;
    let reply = terminal.feed(b"\x1b_Ga=f,i=5,r=1,s=1,v=1,f=32,z=99;/////w==\x1b\\");
    assert_eq!(
        reply,
        [rustty_vt::Effect::Write(
            b"\x1b_Gi=5,r=1;EINVAL: frame data incomplete\x1b\\".to_vec()
        )]
    );
    assert_eq!(terminal.graphics().images[&5].root_gap_ms, before);
    assert_eq!(terminal.graphics().bytes_used(), 24);
    while decoder.next(&mut terminal).unwrap().is_some() {}
    assert!(!terminal.graphics().images[&5].has_pending_pixels());
    assert_eq!(terminal.tick_graphics(100), Some(150));
}

#[test]
fn payloads_and_history_placements_cannot_attach_to_replaced_images() {
    for reset in [false, true] {
        let bytes = fixture("kitty-history");
        let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
        let mut terminal = decoder.ready().unwrap();
        if reset {
            terminal.feed(b"\x1bc");
        }
        terminal.feed(b"\x1b_Gi=4,s=1,v=1,f=32;AQID/w==\x1b\\");
        while decoder.next(&mut terminal).unwrap().is_some() {}
        assert_eq!(
            terminal.graphics().images[&4].pixels.as_slice(),
            [1, 2, 3, 255]
        );
        assert!(terminal.graphics().placements.is_empty(), "reset={reset}");
    }
    let bytes = fixture("kitty-alternate");
    let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
    let mut terminal = decoder.ready().unwrap();
    terminal.feed(b"\x1b[?1049l\x1b[?1049h\x1b_Gi=1,s=1,v=1,f=32;AQID/w==\x1b\\");
    while decoder.next(&mut terminal).unwrap().is_some() {}
    assert_eq!(
        terminal.graphics().images[&1].pixels.as_slice(),
        [1, 2, 3, 255]
    );
    assert!(terminal.graphics().placements.is_empty());
}

#[test]
fn graphics_budgets_include_pending_frames_and_drop_oldest_images() {
    let bytes = fixture("kitty-animation");
    for limit in [0, 23, 24] {
        let restored = snapshot::decode(
            bytes.as_slice(),
            DecodeOptions {
                max_graphics_bytes: limit,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(restored.graphics().images.len(), usize::from(limit == 24));
        assert!(restored.graphics().bytes_used() <= limit);
    }
    let bytes = fixture("kitty-relative");
    let restored = snapshot::decode(
        bytes.as_slice(),
        DecodeOptions {
            max_graphics_bytes: 4,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(restored.graphics().images.len(), 1);
    assert!(restored.graphics().images.contains_key(&2));
    assert!(restored.graphics().placements.is_empty());

    let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
    let mut restored = decoder.ready().unwrap();
    restored.set_graphics_limit(0);
    while decoder.next(&mut restored).unwrap().is_some() {}
    assert!(restored.graphics().images.is_empty());
}

#[test]
fn deleting_pending_frames_cannot_redirect_later_payloads() {
    let bytes = fixture("kitty-animation");
    let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
    let mut terminal = decoder.ready().unwrap();
    terminal.feed(b"\x1b_Ga=d,d=f,i=5,r=2\x1b\\");
    assert_eq!(terminal.graphics().images[&5].frames.len(), 1);
    while decoder.next(&mut terminal).unwrap().is_some() {}
    let image = &terminal.graphics().images[&5];
    assert_eq!(image.pixels.as_slice(), &[255, 0, 0, 255].repeat(2));
    assert!(image.frames[0].pixels.is_empty());
}

#[test]
fn pending_graphics_views_validate_and_preserve_the_reserved_quota() {
    let bytes = fixture("kitty-animation");
    let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
    let terminal = decoder.ready().unwrap();
    let encoded = serde_json::to_vec(&terminal.graphics().view_snapshot()).unwrap();
    let view = serde_json::from_slice(&encoded).unwrap();
    let restored = rustty_vt::graphics::Graphics::from_view_snapshot_with_limit(view, 24).unwrap();
    assert_eq!(restored.bytes_used(), 24);
    assert!(restored.images[&5].has_pending_pixels());
    let view = serde_json::from_slice(&encoded).unwrap();
    assert!(rustty_vt::graphics::Graphics::from_view_snapshot_with_limit(view, 23).is_err());
}

#[test]
fn cell_checkpoints_keep_graphics_in_the_separate_carrier() {
    let terminal = snapshot::decode(
        fixture("kitty-history").as_slice(),
        DecodeOptions::default(),
    )
    .unwrap();
    let checkpoint = snapshot::encode_without_graphics_to_vec(&terminal).unwrap();
    let restored = snapshot::decode(checkpoint.as_slice(), DecodeOptions::default()).unwrap();
    assert!(restored.graphics().images.is_empty());
    assert_eq!(snapshot::encode_to_vec(&restored).unwrap(), checkpoint);
    assert!(snapshot::encode_to_vec(&terminal).unwrap().len() > checkpoint.len());
}

fn records(bytes: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let mut rest = &bytes[10..];
    let mut result = Vec::new();
    while !rest.is_empty() {
        let tag = u16::from_le_bytes(rest[..2].try_into().unwrap());
        let len = u32::from_le_bytes(rest[2..6].try_into().unwrap()) as usize;
        result.push((tag, rest[10..10 + len].to_vec()));
        rest = &rest[10 + len..];
    }
    result
}
fn frame(records: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = b"GHOSTSNP\x01\0".to_vec();
    for (tag, data) in records {
        let mut header = tag.to_le_bytes().to_vec();
        header.extend_from_slice(&(data.len() as u32).to_le_bytes());
        let crc = crc32c::crc32c_append(crc32c::crc32c(&header), data);
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&crc.to_le_bytes());
        bytes.extend_from_slice(data);
    }
    bytes
}

#[test]
fn snapshot_pixels_validate_crc_length_and_record_position_even_when_dropped() {
    let original = records(&fixture("kitty-placed"));
    let pixel = original.iter().position(|(tag, _)| *tag == 9).unwrap();
    for delta in [-1, 1] {
        let mut stream = original.clone();
        if delta < 0 {
            stream[pixel].1.pop();
        } else {
            stream[pixel].1.push(0);
        }
        assert!(snapshot::decode(frame(&stream).as_slice(), DecodeOptions::default()).is_err());
    }
    let mut stream = original.clone();
    let payload = stream.remove(pixel);
    stream.insert(0, payload);
    assert!(snapshot::decode(frame(&stream).as_slice(), DecodeOptions::default()).is_err());
    let bytes = fixture("kitty-placed");
    let pixel_offset = 10
        + original[..pixel]
            .iter()
            .map(|(_, data)| 10 + data.len())
            .sum::<usize>();
    let mut bad_crc = bytes.clone();
    bad_crc[pixel_offset + 28] ^= 1;
    for limit in [0, 320_000_000] {
        assert!(
            snapshot::decode(
                bad_crc.as_slice(),
                DecodeOptions {
                    max_graphics_bytes: limit,
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(
            snapshot::decode(
                &bytes[..pixel_offset + 29],
                DecodeOptions {
                    max_graphics_bytes: limit,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    let mut stream = original.clone();
    stream[pixel].1[10..18].copy_from_slice(&u64::MAX.to_le_bytes());
    let restored = snapshot::decode(frame(&stream).as_slice(), DecodeOptions::default()).unwrap();
    assert!(restored.graphics().images[&1].pixels.is_empty());
    let mut stream = original.clone();
    stream.insert(pixel, stream[pixel].clone());
    let restored = snapshot::decode(frame(&stream).as_slice(), DecodeOptions::default()).unwrap();
    assert_eq!(restored.graphics().images[&1].pixels.len(), 256);
}

#[test]
fn pending_graphics_metadata_is_bounded_and_invalid_placements_are_dropped() {
    let original = records(&fixture("kitty-placed"));
    let storage = original.iter().position(|(tag, _)| *tag == 8).unwrap();
    for offset in [19 + 8, 19 + 12] {
        let mut stream = original.clone();
        stream[storage].1[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let restored =
            snapshot::decode(frame(&stream).as_slice(), DecodeOptions::default()).unwrap();
        assert!(restored.graphics().images.is_empty());
    }
    let mut stream = original.clone();
    stream[storage].1[19 + 27 + 4] = 1;
    stream[storage].1[19 + 27 + 5..19 + 27 + 9].copy_from_slice(&0u32.to_le_bytes());
    let restored = snapshot::decode(frame(&stream).as_slice(), DecodeOptions::default()).unwrap();
    assert!(restored.graphics().placements.is_empty());
    let mut stream = records(&fixture("kitty-animation"));
    let storage = stream.iter().position(|(tag, _)| *tag == 8).unwrap();
    stream[storage].1[19 + 27 + 17..19 + 27 + 21].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(snapshot::decode(frame(&stream).as_slice(), DecodeOptions::default()).is_err());
}

#[test]
fn history_pixels_follow_the_page_that_first_needs_them() {
    let mut source = snapshot::decode(
        fixture("kitty-history").as_slice(),
        DecodeOptions::default(),
    )
    .unwrap();
    source.feed(b"\x1b_Ga=d,d=i,i=4,p=3\x1b\\");
    let bytes = snapshot::encode_to_vec(&source).unwrap();
    let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
    let mut restored = decoder.ready().unwrap();
    assert!(restored.graphics().placements.is_empty());
    assert!(matches!(
        decoder.next(&mut restored).unwrap(),
        Some(snapshot::Progress::Page(_))
    ));
    assert_eq!(restored.graphics().placements.len(), 1);
    assert!(matches!(
        decoder.next(&mut restored).unwrap(),
        Some(snapshot::Progress::Image(_))
    ));
    assert_eq!(
        restored.graphics().images[&4].pixels.as_slice(),
        [9, 9, 9, 255]
    );
    assert!(matches!(
        decoder.next(&mut restored).unwrap(),
        Some(snapshot::Progress::Page(_))
    ));
    assert_eq!(restored.graphics().placements.len(), 3);
    assert!(decoder.next(&mut restored).unwrap().is_none());

    let mut decoder = Decoder::new(bytes.as_slice(), DecodeOptions::default());
    let mut restored = decoder.ready().unwrap();
    restored.resize(3, 2);
    while decoder.next(&mut restored).unwrap().is_some() {}
    assert!(restored.graphics().placements.is_empty());
    assert!(!restored.graphics().images[&4].has_pending_pixels());
}

#[test]
fn large_raw_payloads_stream_without_the_metadata_allocation_limit() {
    let mut source = Terminal::new(2, 2, 0);
    source.feed(b"\x1b_Ga=T,i=1,p=1,s=1,v=1,f=32,C=1;AQID/w==\x1b\\");
    let image = source.screen_mut().graphics.images.get_mut(&1).unwrap();
    image.width = 1500;
    image.height = 1500;
    image.pixels = std::sync::Arc::new([1, 2, 3, 255].repeat(1500 * 1500));
    let bytes = snapshot::encode_to_vec(&source).unwrap();
    let restored = snapshot::decode(bytes.as_slice(), DecodeOptions::default()).unwrap();
    assert_eq!(restored.graphics().images[&1].pixels.len(), 9_000_000);
    struct SmallReads<'a>(&'a [u8]);
    impl std::io::Read for SmallReads<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            assert!(out.len() <= 8192, "dropped pixels allocated a large buffer");
            self.0.read(out)
        }
    }
    let restored = snapshot::decode(
        SmallReads(&bytes),
        DecodeOptions {
            max_graphics_bytes: 0,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(restored.graphics().images.is_empty());
}

#[test]
fn grayscale_snapshot_payloads_expand_to_rgba() {
    for (format, pixels, expected) in [
        (3, vec![17, 128], [17, 17, 17, 128]),
        (4, vec![29], [29, 29, 29, 255]),
    ] {
        let mut stream = records(&fixture("kitty-placed"));
        let storage = stream.iter_mut().find(|(tag, _)| *tag == 8).unwrap();
        storage.1[19 + 16] = format;
        storage.1[19 + 18..19 + 26].copy_from_slice(&(pixels.len() as u64 * 64).to_le_bytes());
        let payload = stream.iter_mut().find(|(tag, _)| *tag == 9).unwrap();
        payload.1.truncate(10);
        payload
            .1
            .extend_from_slice(&(pixels.len() as u64 * 64).to_le_bytes());
        payload.1.extend_from_slice(&pixels.repeat(64));
        let restored =
            snapshot::decode(frame(&stream).as_slice(), DecodeOptions::default()).unwrap();
        assert_eq!(
            restored.graphics().images[&1].pixels.as_slice(),
            expected.repeat(64)
        );
    }
}

#[test]
fn animation_controls_before_the_first_extra_frame_survive_snapshots() {
    let mut source = Terminal::new(4, 2, 0);
    source.feed(b"\x1b_Gi=1,s=1,v=1,f=32;AQID/w==\x1b\\\x1b_Ga=a,i=1,v=3\x1b\\");
    let bytes = snapshot::encode_to_vec(&source).unwrap();
    let mut restored = snapshot::decode(bytes.as_slice(), DecodeOptions::default()).unwrap();
    assert_eq!(
        restored.graphics().images[&1].max_loops,
        source.graphics().images[&1].max_loops
    );
    restored.feed(b"\x1b_Ga=f,i=1,s=1,v=1,f=32;BAUG/w==\x1b\\");
    assert_eq!(
        restored.graphics().images[&1].max_loops,
        source.graphics().images[&1].max_loops
    );
}
