use egui::{ColorImage, TextureHandle};
use rustty::vt::clipboard::{Content, ReadResult};

#[derive(Default)]
pub struct Preview {
    selected: usize,
    part: Option<(usize, Part)>,
    texture: Option<TextureHandle>,
}

enum Part {
    Text(String, bool),
    Image(ColorImage),
    Unavailable,
}

impl Part {
    fn decode(content: &Content) -> Self {
        if content.mime.starts_with(b"image/") {
            let reader =
                image::ImageReader::new(std::io::Cursor::new(&content.data)).with_guessed_format();
            if let Ok(mut reader) = reader {
                let mut limits = image::Limits::default();
                limits.max_image_width = Some(16384);
                limits.max_image_height = Some(16384);
                limits.max_alloc = Some(16 * 1024 * 1024);
                reader.limits(limits);
                if let Ok(image) = reader.decode() {
                    let rgba = if image.width() > 400 || image.height() > 200 {
                        image.thumbnail(400, 200).to_rgba8()
                    } else {
                        image.to_rgba8()
                    };
                    return Self::Image(ColorImage::from_rgba_unmultiplied(
                        [rgba.width() as usize, rgba.height() as usize],
                        rgba.as_raw(),
                    ));
                }
            }
        }
        if let Ok(text) = std::str::from_utf8(&content.data) {
            let end = text.floor_char_boundary(64 * 1024);
            Self::Text(text[..end].to_owned(), end < text.len())
        } else {
            Self::Unavailable
        }
    }
}

impl Preview {
    pub fn show(&mut self, ui: &mut egui::Ui, contents: &[Content]) {
        if contents.is_empty() {
            ui.label("No representations.");
            return;
        }
        self.selected = self.selected.min(contents.len() - 1);
        let mime = |content: &Content| String::from_utf8_lossy(&content.mime).into_owned();
        if contents.len() > 1 {
            egui::ComboBox::from_id_salt("clipboard representation")
                .selected_text(mime(&contents[self.selected]))
                .show_ui(ui, |ui| {
                    for (index, content) in contents.iter().enumerate() {
                        ui.selectable_value(&mut self.selected, index, mime(content));
                    }
                });
        } else {
            ui.label(mime(&contents[0]));
        }
        let content = &contents[self.selected];
        ui.label(format!("{} bytes", content.data.len()));
        if self
            .part
            .as_ref()
            .is_none_or(|(index, _)| *index != self.selected)
        {
            self.texture = None;
            self.part = Some((self.selected, Part::decode(content)));
        }
        egui::ScrollArea::both()
            .max_width(400.0)
            .max_height(200.0)
            .show(ui, |ui| match &self.part.as_ref().unwrap().1 {
                Part::Text(text, truncated) => {
                    ui.monospace(text);
                    if *truncated {
                        ui.label("Preview truncated; the full content will be sent.");
                    }
                }
                Part::Image(image) => {
                    let texture = self.texture.get_or_insert_with(|| {
                        ui.ctx().load_texture(
                            "clipboard preview",
                            image.clone(),
                            Default::default(),
                        )
                    });
                    ui.add(egui::Image::from_texture(&*texture).max_size(egui::vec2(400.0, 200.0)));
                }
                Part::Unavailable => {
                    ui.label("This representation cannot be previewed.");
                }
            });
    }
}

pub struct Pending {
    pub pane: u64,
    pub request: rustty::vt::Effect,
    pub read_result: Option<ReadResult>,
    pub preview: Preview,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(mime: &[u8], data: &[u8]) -> Content {
        Content {
            mime: mime.to_vec(),
            data: data.into(),
        }
    }

    #[test]
    fn previews_unknown_text_empty_and_binary_representations() {
        assert!(
            matches!(Part::decode(&content(b"application/custom", b"hello")), Part::Text(text, false) if text == "hello")
        );
        assert!(
            matches!(Part::decode(&content(b"text/plain", b"")), Part::Text(text, false) if text.is_empty())
        );
        assert!(matches!(
            Part::decode(&content(b"application/octet-stream", b"\xff\0")),
            Part::Unavailable
        ));
    }

    #[test]
    fn text_preview_is_bounded_at_a_character_boundary() {
        let text = "a".repeat(65535) + "🦀tail";
        assert!(
            matches!(Part::decode(&content(b"text/plain", text.as_bytes())), Part::Text(preview, true) if preview.len() == 65535)
        );
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&vec![0; (width * height * 4) as usize])
                .unwrap();
        }
        bytes
    }

    #[test]
    fn png_preview_preserves_alpha_and_rejects_oversized_or_invalid_images() {
        assert!(
            matches!(Part::decode(&content(b"image/png", &png(1, 1))), Part::Image(image) if image.size == [1, 1] && image.pixels[0] == egui::Color32::TRANSPARENT)
        );
        assert!(matches!(
            Part::decode(&content(b"image/png", &png(1, 16385))),
            Part::Unavailable
        ));
        assert!(matches!(
            Part::decode(&content(b"image/png", b"invalid")),
            Part::Text(_, false)
        ));
    }

    #[test]
    fn switching_representations_releases_the_image_and_preserves_the_payload() {
        let contents = [
            content(b"image/png", &png(1, 1)),
            content(b"application/binary", b"\xff"),
        ];
        let original = contents.clone();
        let context = egui::Context::default();
        let mut preview = Preview::default();
        let draw = |preview: &mut Preview| {
            let mut output = context.run_ui(Default::default(), |ui| preview.show(ui, &contents));
            output.textures_delta.clear();
        };
        draw(&mut preview);
        let texture = preview.texture.as_ref().unwrap().id();
        draw(&mut preview);
        assert_eq!(preview.texture.as_ref().unwrap().id(), texture);
        preview.selected = 1;
        draw(&mut preview);
        assert!(preview.texture.is_none());
        assert!(matches!(preview.part, Some((1, Part::Unavailable))));
        assert_eq!(contents, original);
    }
}
