use super::{Color, FontStyle, Frame, Paint, Quad, RenderError, RenderOptions, Renderer, Screen};

/// Active IME composition. Selection is Winit's pair of UTF-8 byte offsets;
/// `None` keeps the composition visible while hiding its caret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preedit {
    pub text: String,
    pub selection: Option<(usize, usize)>,
}

impl Renderer {
    pub(super) fn preedit(
        &mut self,
        screen: &Screen,
        options: &RenderOptions,
        frame: &mut Frame,
    ) -> Result<(), RenderError> {
        let Some(preedit) = options.preedit.as_ref().filter(|p| !p.text.is_empty()) else {
            return Ok(());
        };
        if !options.focused || screen.viewport_offset != 0 {
            return Ok(());
        }
        let metrics = self.metrics();
        let (start, end) = preedit
            .selection
            .unwrap_or((preedit.text.len(), preedit.text.len()));
        let (glyphs, carets) = self.fonts.shape_with_carets(
            &preedit.text,
            FontStyle::Regular,
            &[start, end, preedit.text.len()],
        )?;
        let top = options.padding[1] + screen.cursor.row as f32 * metrics.cell_height as f32;
        let left = options.padding[0];
        let right = (options.size[0] as f32 - options.padding[0]).min(
            left + screen.rows.first().map_or(0, |r| r.cells.len()) as f32
                * metrics.cell_width as f32,
        );
        let height =
            (metrics.cell_height as f32).min(options.size[1] as f32 - options.padding[1] - top);
        if right <= left || height <= 0.0 {
            return Ok(());
        }
        let thickness = metrics
            .underline_thickness
            .ceil()
            .max(1.0)
            .min(right - left)
            .min(height);
        let width = carets[2].max(thickness).ceil();
        let anchor = left + screen.cursor.col as f32 * metrics.cell_width as f32;
        let visible_width = width.min(right - left);
        let x = anchor.min(right - visible_width).max(left);
        let scroll = (carets[1] - (right - left) + metrics.cell_width as f32)
            .max(0.0)
            .min((width - (right - left)).max(0.0));
        let text_x = x - scroll;
        let clip = [x, top, visible_width, height];
        let mut overlay = Frame::empty(frame.size);
        overlay.generation = frame.generation;
        overlay
            .quads
            .push(Quad::solid(clip, Color::rgb(options.background)));
        let foreground = Color::rgb(options.foreground);
        for glyph in &glyphs {
            let cached = self.glyph(glyph)?;
            if cached.size.contains(&0) {
                continue;
            }
            overlay.quads.push(Quad {
                rect: [
                    text_x + glyph.x + cached.bearing[0] as f32,
                    top + metrics.baseline - glyph.y - cached.bearing[1] as f32,
                    cached.size[0] as f32,
                    cached.size[1] as f32,
                ],
                uv: cached.uv,
                color: foreground,
                paint: if cached.color {
                    Paint::Color
                } else {
                    Paint::Mask
                },
                atlas: cached.atlas,
            });
        }
        overlay.quads.push(Quad::solid(
            [text_x, top + height - thickness, width, thickness],
            foreground,
        ));
        if preedit.selection.is_some() {
            let selection_left = text_x + carets[0].min(carets[1]);
            let selection_width = (carets[1] - carets[0]).abs();
            let color = Color::rgb(options.cursor_color);
            if selection_width > 0.0 {
                overlay.quads.push(Quad::solid(
                    [
                        selection_left,
                        top + height - 2.0 * thickness,
                        selection_width,
                        2.0 * thickness,
                    ],
                    color,
                ));
            }
            let caret = [
                (text_x + carets[1]).clamp(x, right - thickness),
                top,
                thickness,
                height,
            ];
            overlay.quads.push(Quad::solid(caret, color));
            overlay.ime_cursor = Some(caret);
        }
        // Clip glyph overhangs and long compositions without changing terminal cells.
        frame
            .append_clipped(&overlay, [0.0, 0.0], clip)
            .expect("preedit uses this frame's atlas generation");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustty_font::FontConfig;
    use rustty_vt::Terminal;

    #[test]
    fn multilingual_preedit_tracks_native_caret_without_mutating_terminal() {
        let mut terminal = Terminal::new(12, 3, 100);
        terminal.feed(b"unchanged\r\nabc");
        let before = terminal.screen().rows.clone();
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let metrics = renderer.metrics();
        let text = "にほんe\u{301}🙂";
        let mut options = RenderOptions {
            size: [metrics.cell_width * 12 + 16, metrics.cell_height * 3 + 16],
            preedit: Some(Preedit {
                text: text.into(),
                selection: Some((3, 9)),
            }),
            ..Default::default()
        };
        let plain = renderer
            .prepare(
                terminal.screen(),
                &RenderOptions {
                    cursor_visible: false,
                    preedit: None,
                    ..options.clone()
                },
            )
            .unwrap();
        let frame = renderer.prepare(terminal.screen(), &options).unwrap();
        assert_eq!(terminal.screen().rows, before);
        let caret = frame.ime_cursor.unwrap();
        assert_eq!(caret[1], options.padding[1] + metrics.cell_height as f32);
        assert!(
            caret[0] >= options.padding[0]
                && caret[0] + caret[2] <= options.size[0] as f32 - options.padding[0]
        );
        // All existing terminal drawing remains intact; opaque composition only covers its region.
        assert_eq!(&frame.quads[..plain.quads.len()], plain.quads.as_slice());
        let overlay = &frame.quads[plain.quads.len()..];
        assert!(overlay.iter().any(|q| q.paint == Paint::Color));
        assert!(overlay.iter().any(|q| q.paint == Paint::Mask));
        assert!(overlay.iter().filter(|q| q.paint == Paint::Solid).count() >= 4);
        assert!(overlay.iter().all(|q| q.rect[1] >= caret[1]
            && q.rect[1] + q.rect[3] <= caret[1] + metrics.cell_height as f32 + 0.001));
        options.preedit.as_mut().unwrap().selection = None;
        assert!(
            renderer
                .prepare(terminal.screen(), &options)
                .unwrap()
                .ime_cursor
                .is_none()
        );
        options.preedit = None;
        let restored = renderer.prepare(terminal.screen(), &options).unwrap();
        assert!(restored.ime_cursor.is_none());
        assert_eq!(terminal.screen().rows, before);
    }

    #[test]
    fn composition_moves_left_at_the_edge_and_keeps_long_carets_visible() {
        let mut terminal = Terminal::new(4, 1, 10);
        terminal.feed(b"abc");
        let mut renderer = Renderer::new(FontConfig::default()).unwrap();
        let metrics = renderer.metrics();
        let mut options = RenderOptions {
            size: [metrics.cell_width * 4 + 16, metrics.cell_height + 16],
            preedit: Some(Preedit {
                text: "漢字".into(),
                selection: Some((6, 6)),
            }),
            ..Default::default()
        };
        let frame = renderer.prepare(terminal.screen(), &options).unwrap();
        let caret = frame.ime_cursor.unwrap();
        assert!(caret[0] + caret[2] <= options.size[0] as f32 - options.padding[0]);
        let text = "composition にほん e\u{301} 🙂 at the edge";
        options.preedit = Some(Preedit {
            text: text.into(),
            selection: Some((usize::MAX, usize::MAX)),
        });
        let long = renderer.prepare(terminal.screen(), &options).unwrap();
        let caret = long.ime_cursor.unwrap();
        assert!(
            caret[0] >= options.padding[0]
                && caret[0] + caret[2] <= options.size[0] as f32 - options.padding[0]
        );
        options.preedit = Some(Preedit {
            text: text.into(),
            selection: Some((0, 0)),
        });
        let start = renderer
            .prepare(terminal.screen(), &options)
            .unwrap()
            .ime_cursor
            .unwrap();
        assert!(start[0] < caret[0]);
    }
}
