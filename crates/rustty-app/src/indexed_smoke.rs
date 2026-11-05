//! Exercise shared indexed sources through real native panes and retained frames.
use super::*;
use base64::Engine;

const INDICES: [u8; 10] = [1, 2, 248, 249, 250, 251, 252, 253, 254, 255];
const ALPHA: [u8; 10] = [0, 128, 255, 128, 255, 128, 255, 128, 255, 128];

fn fixture(cols: u16, rows: u16, pane: usize) -> vt::Terminal {
    let mut terminal = vt::Terminal::new(cols, rows, 100);
    terminal.feed(format!(
        "\x1b[?25l\x1b[1;1HPane {pane}: shared indexed source\x1b[3;1Hf8: opaque palette / special colors\x1b[4;1H 1  2 248249250251252253254255\x1b[9;1Hf16: index with alpha 0 / 128 / 255\x1b[10;1H 1  2 248249250251252253254255\x1b[15;1HPalette and theme update in place"
    ).as_bytes());
    terminal
}

fn colors(
    terminal: &mut vt::Terminal,
    foreground: [u8; 3],
    background: [u8; 3],
    palette: [[u8; 3]; 2],
) {
    let rgb = |[r, g, b]: [u8; 3]| format!("#{r:02x}{g:02x}{b:02x}");
    terminal.feed(
        format!(
            "\x1b]10;{}\x07\x1b]11;{}\x07\x1b]4;1;{};2;{}\x07",
            rgb(foreground),
            rgb(background),
            rgb(palette[0]),
            rgb(palette[1])
        )
        .as_bytes(),
    );
}

fn transmit(terminal: &mut vt::Terminal, id: u32, format: u8, row: u8, bytes: &[u8]) {
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    terminal.feed(
        format!("\x1b[{row};1H\x1b_Ga=T,i={id},C=1,f={format},s=10,v=1,c=30,r=3,q=2;{data}\x1b\\")
            .as_bytes(),
    );
}

fn pixel(frame: &Frame, quad: &rustty_render::Quad, column: usize) -> [u8; 4] {
    let u = quad.uv[0] + (column as f32 + 0.5) / 10.0 * (quad.uv[2] - quad.uv[0]);
    let v = (quad.uv[1] + quad.uv[3]) / 2.0;
    for upload in frame.atlas_uploads.iter().rev() {
        if upload.page != quad.atlas {
            continue;
        }
        let x = (u * upload.page_size as f32).floor() as u32;
        let y = (v * upload.page_size as f32).floor() as u32;
        if x < upload.origin[0]
            || y < upload.origin[1]
            || x >= upload.origin[0] + upload.size[0]
            || y >= upload.origin[1] + upload.size[1]
        {
            continue;
        }
        let offset = ((y - upload.origin[1]) * upload.size[0] + x - upload.origin[0]) as usize * 4;
        return upload.pixels[offset..offset + 4].try_into().unwrap();
    }
    panic!("indexed image has no retained atlas pixel");
}

fn check_pixels(host: &Host, id: Id, expected: [[u8; 3]; 10]) {
    let frame = &host.prepared[&id].frame;
    let images: Vec<_> = frame
        .quads
        .iter()
        .filter(|quad| quad.paint == rustty_render::Paint::Color)
        .collect();
    assert_eq!(
        images.len(),
        2,
        "each pane must display both indexed formats"
    );
    for (format, quad) in images.into_iter().enumerate() {
        for (index, [r, g, b]) in expected.into_iter().enumerate() {
            assert_eq!(
                pixel(frame, quad, index),
                [r, g, b, if format == 0 { 255 } else { ALPHA[index] }],
                "pane {id} f{} index {}",
                if format == 0 { 8 } else { 16 },
                INDICES[index]
            );
        }
    }
}

pub(super) fn check(app: &mut App, event_loop: &ActiveEventLoop, directory: &Path) -> Result<()> {
    let window = *app.windows.keys().next().ok_or("no indexed smoke window")?;
    let mut host = app.windows.remove(&window).unwrap();
    let focused = host.focused;
    host.focused = true;
    let saved_config = app.loaded.config.clone();
    app.loaded.config.selection_background = Some(config::TerminalColor::CellBackground);
    app.loaded.config.selection_foreground = Some(config::TerminalColor::CellForeground);
    app.loaded.config.cursor_text = Some(config::TerminalColor::CellForeground);
    app.loaded.config.cursor_color = Some(config::TerminalColor::CellBackground);
    let first = app.focused(host.id).ok_or("no indexed smoke pane")?;
    app.action(
        event_loop,
        &mut host,
        Action::NewSplit(Direction::Right),
        true,
    );
    let second = app
        .focused(host.id)
        .ok_or("indexed split did not create a pane")?;
    assert_ne!(first, second);
    app.draw(event_loop, &mut host)?;
    let panes = [first, second];
    let fg = [[230, 230, 235], [135, 190, 245]];
    let bg = [[20, 28, 38], [42, 25, 58]];
    let palette = [
        [[230, 80, 75], [90, 210, 140]],
        [[80, 130, 240], [245, 195, 65]],
    ];
    let mut saved = Vec::new();
    let mut source: Option<vt::graphics::Graphics> = None;
    for (index, id) in panes.into_iter().enumerate() {
        let mut terminal = app.panes[&id].session.terminal()?;
        let mut fixture = fixture(terminal.cols, terminal.rows, index + 1);
        colors(&mut fixture, fg[index], bg[index], palette[index]);
        if let Some(graphics) = &source {
            fixture.screen_mut().graphics = graphics.clone();
        } else {
            transmit(&mut fixture, 1, 8, 5, &INDICES);
            let data: Vec<_> = INDICES
                .into_iter()
                .zip(ALPHA)
                .flat_map(|(index, alpha)| [index, alpha])
                .collect();
            transmit(&mut fixture, 2, 16, 11, &data);
            source = Some(fixture.screen().graphics.clone());
        }
        saved.push((id, std::mem::replace(&mut *terminal, fixture)));
    }
    let source = source.unwrap();
    let result = (|| -> Result<()> {
        app.draw(event_loop, &mut host)?;
        for (index, id) in panes.into_iter().enumerate() {
            let [a, b] = palette[index];
            let fg = fg[index];
            let bg = bg[index];
            check_pixels(&host, id, [a, b, bg, fg, fg, bg, fg, bg, fg, fg]);
        }
        capture_window(
            app,
            event_loop,
            &mut host,
            &directory.join("indexed-graphics.png"),
        )?;
        let unchanged = Arc::clone(&host.prepared[&second].frame);
        let new_fg = [240, 165, 85];
        let new_bg = [15, 45, 42];
        let new_palette = [[200, 70, 180], [45, 200, 215]];
        let cursor = [245, 210, 55];
        {
            let mut terminal = app.panes[&first].session.terminal()?;
            colors(&mut terminal, new_fg, new_bg, new_palette);
            terminal.feed(b"\x1b]12;#f5d237\x07");
        }
        app.draw(event_loop, &mut host)?;
        assert!(
            Arc::ptr_eq(&unchanged, &host.prepared[&second].frame),
            "another pane's palette change must retain this frame"
        );
        let [a, b] = new_palette;
        check_pixels(
            &host,
            first,
            [
                a, b, new_bg, new_fg, new_fg, new_bg, cursor, new_bg, new_fg, new_fg,
            ],
        );
        let [a, b] = palette[1];
        check_pixels(
            &host,
            second,
            [a, b, bg[1], fg[1], fg[1], bg[1], fg[1], bg[1], fg[1], fg[1]],
        );

        // A real theme application changes terminal defaults. OSC overrides in
        // the other pane and its already-transmitted sources remain intact.
        let select_bg = [90, 45, 120];
        let select_fg = [250, 210, 225];
        let cursor_text = [10, 15, 20];
        app.loaded.config.selection_background =
            Some(config::TerminalColor::Rgb(config::Rgb::new(90, 45, 120)));
        app.loaded.config.selection_foreground =
            Some(config::TerminalColor::Rgb(config::Rgb::new(250, 210, 225)));
        app.loaded.config.cursor_text =
            Some(config::TerminalColor::Rgb(config::Rgb::new(10, 15, 20)));
        let mut theme = app.loaded.config.clone();
        theme.foreground = config::Rgb::new(245, 225, 185);
        theme.background = config::Rgb::new(45, 32, 25);
        theme.palette[1] = config::Rgb::new(235, 120, 65);
        theme.palette[2] = config::Rgb::new(160, 195, 95);
        theme.cursor_color = Some(config::TerminalColor::Rgb(config::Rgb::new(210, 145, 245)));
        app.panes[&second].session.apply_appearance(&theme)?;
        app.panes[&second]
            .session
            .terminal()?
            .feed(b"\x1b]110\x07\x1b]111\x07\x1b]112\x07\x1b]104;1;2\x07");
        app.draw(event_loop, &mut host)?;
        let [a, b] = new_palette;
        check_pixels(
            &host,
            first,
            [
                a,
                b,
                new_bg,
                new_fg,
                select_bg,
                select_fg,
                cursor,
                cursor_text,
                new_fg,
                new_fg,
            ],
        );
        let tf = theme.foreground.to_array();
        let tb = theme.background.to_array();
        check_pixels(
            &host,
            second,
            [
                theme.palette[1].to_array(),
                theme.palette[2].to_array(),
                tb,
                tf,
                select_bg,
                select_fg,
                [210, 145, 245],
                cursor_text,
                tf,
                tf,
            ],
        );
        for id in panes {
            let terminal = app.panes[&id].session.terminal()?;
            for number in [1, 2] {
                let original = source.images.get(&number).unwrap();
                let live = terminal.screen().graphics.images.get(&number).unwrap();
                let displayed = host.prepared[&id]
                    .screen
                    .graphics
                    .images
                    .get(&number)
                    .unwrap();
                for image in [live, displayed] {
                    assert!(Arc::ptr_eq(
                        &original.indexed.as_ref().unwrap().data,
                        &image.indexed.as_ref().unwrap().data
                    ));
                    assert!(
                        Arc::ptr_eq(&original.pixels, &image.pixels),
                        "static old-peer fallback must remain unchanged"
                    );
                }
            }
        }
        capture_window(
            app,
            event_loop,
            &mut host,
            &directory.join("indexed-graphics-updated.png"),
        )?;
        fs::write(
            directory.join("indexed-graphics-result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true,
                "renderer": app.painter.description(),
                "panes": 2,
                "checks": ["shared-indexed-source", "per-pane-palette", "all-special-colors-248-255", "f8-opaque", "f16-alpha", "cell-derived-colors", "fixed-colors", "osc-palette-defaults-and-cursor", "unchanged-sibling-frame", "theme-application", "no-retransmission", "unchanged-old-peer-fallback", "retained-atlas-pixels"],
                "captures": ["indexed-graphics.png", "indexed-graphics-updated.png"]
            }))?,
        )?;
        eprintln!(
            "Native indexed graphics smoke: shared sources, all special colors, f16 alpha, OSC and theme updates passed"
        );
        Ok(())
    })();
    for (id, terminal) in saved {
        *app.panes[&id].session.terminal()? = terminal;
    }
    app.loaded.config = saved_config;
    host.focused = focused;
    app.windows.insert(window, host);
    result
}
