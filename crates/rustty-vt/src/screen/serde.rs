//! Stable JSON fields, resolved through each row's owning page.
use super::*;
use ::serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

#[derive(Serialize)]
struct ScreenRef<'a> {
    #[serde(flatten, with = "Screen")]
    screen: &'a Screen,
    rows: RowsRef<'a>,
    history: RowsRef<'a>,
    history_bytes: usize,
}
struct RowsRef<'a> {
    screen: &'a Screen,
    start: usize,
    len: usize,
}
impl Serialize for RowsRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(
            self.screen
                .all_rows()
                .skip(self.start)
                .take(self.len)
                .map(|row| RowRef {
                    id: row.id,
                    wrapped: row.wrapped,
                    wrap_continuation: row.wrap_continuation,
                    semantic: row.semantic,
                    dirty: row.dirty,
                    cells: CellsRef(row),
                }),
        )
    }
}
#[derive(Serialize)]
struct RowRef<'a> {
    id: u64,
    wrapped: bool,
    wrap_continuation: bool,
    semantic: SemanticContent,
    dirty: bool,
    cells: CellsRef<'a>,
}
struct CellsRef<'a>(RowView<'a>);
impl Serialize for CellsRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(
            self.0
                .cells()
                .iter()
                .enumerate()
                .map(|(col, cell)| CellRef {
                    text: self.0.text(col),
                    width: cell.width(),
                    style: self.0.style(col),
                    hyperlink: self.0.hyperlink(col),
                    protected: cell.protected(),
                    semantic: cell.semantic(),
                    spacer_head: cell.spacer_head(),
                }),
        )
    }
}
#[derive(Serialize)]
struct CellRef<'a> {
    #[serde(serialize_with = "serialize_text")]
    text: CellText<'a>,
    width: u8,
    style: Style,
    #[serde(flatten, serialize_with = "serialize_link")]
    hyperlink: Option<&'a HyperlinkData>,
    protected: bool,
    semantic: SemanticContent,
    spacer_head: bool,
}
fn serialize_text<S: Serializer>(text: &CellText<'_>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(text)
}
fn serialize_link<S: Serializer>(
    link: &Option<&HyperlinkData>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    hyperlink_serde::serialize_data(*link, serializer)
}
impl Serialize for Screen {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ScreenRef {
            screen: self,
            history_bytes: self.history_bytes(),
            rows: RowsRef {
                screen: self,
                start: self.history_len(),
                len: self.height,
            },
            history: RowsRef {
                screen: self,
                start: 0,
                len: self.history_len(),
            },
        }
        .serialize(serializer)
    }
}
#[derive(Deserialize)]
struct ScreenWire {
    #[serde(flatten, with = "Screen")]
    screen: Screen,
    rows: Vec<RowWire>,
    history: Vec<RowWire>,
}
#[derive(Deserialize)]
struct RowWire {
    id: u64,
    wrapped: bool,
    wrap_continuation: bool,
    semantic: SemanticContent,
    dirty: bool,
    cells: Vec<CellWire>,
}
#[derive(Deserialize)]
struct CellWire {
    text: String,
    width: u8,
    style: Style,
    #[serde(flatten, with = "hyperlink_serde")]
    hyperlink: Option<Arc<HyperlinkData>>,
    protected: bool,
    semantic: SemanticContent,
    spacer_head: bool,
}
fn valid_hyperlink(link: &HyperlinkData) -> bool {
    !link.uri_bytes().is_empty()
        && !matches!(&link.id, Some(HyperlinkId::Explicit(id)) if id.is_empty())
}
impl<'de> Deserialize<'de> for Screen {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ScreenWire::deserialize(deserializer)?;
        let mut screen = wire.screen;
        if screen.columns == 0
            || screen.columns > usize::from(u16::MAX)
            || wire.rows.is_empty()
            || wire.rows.len() > usize::from(u16::MAX)
        {
            return Err(D::Error::custom("invalid screen dimensions"));
        }
        screen.height = wire.rows.len();
        let total = wire
            .history
            .len()
            .checked_add(wire.rows.len())
            .ok_or_else(|| D::Error::custom("screen row count overflow"))?;
        let mut count = 0usize;
        let mut serials = HashSet::new();
        for page in &mut screen.pages.pages {
            if page.rows == 0
                || page.rows > page.capacity.rows
                || page.columns == 0
                || page.columns > page.capacity.cols
                || page.serial >= screen.pages.next_serial
                || !serials.insert(page.serial)
            {
                return Err(D::Error::custom("invalid screen page"));
            }
            page.capacity
                .layout()
                .map_err(|_| D::Error::custom("invalid screen page capacity"))?;
            count = count
                .checked_add(usize::from(page.rows))
                .ok_or_else(|| D::Error::custom("screen row count overflow"))?;
            let columns = page.columns;
            *page = Page::new(page.capacity, page.rows, page.serial);
            page.columns = columns;
        }
        if count != total {
            return Err(D::Error::custom("screen pages do not cover its rows"));
        }
        if screen.cursor.row >= screen.height
            || screen.cursor.col >= screen.columns
            || screen.cursor.col >= screen.row(screen.cursor.row).cells.len()
            || screen.viewport_offset > screen.history_len()
        {
            return Err(D::Error::custom(
                "screen cursor or viewport is outside its rows",
            ));
        }
        if screen
            .cursor
            .hyperlink
            .as_deref()
            .is_some_and(|link| !valid_hyperlink(link))
        {
            return Err(D::Error::custom("invalid screen cursor hyperlink"));
        }
        let mut row_ids = HashSet::new();
        for (absolute, row) in wire.history.into_iter().chain(wire.rows).enumerate() {
            if row.id >= screen.next_row || !row_ids.insert(row.id) {
                return Err(D::Error::custom("invalid screen row identity"));
            }
            if row.cells.len() != screen.physical_row(absolute).cells.len() {
                return Err(D::Error::custom("screen row width differs from its page"));
            }
            for (col, wire) in row.cells.into_iter().enumerate() {
                if wire.width > 2
                    || wire
                        .hyperlink
                        .as_deref()
                        .is_some_and(|link| !valid_hyperlink(link))
                {
                    return Err(D::Error::custom("invalid screen cell"));
                }
                let mut chars = wire.text.chars();
                let cp = chars.next();
                let suffix = chars.count();
                if suffix > 64 {
                    return Err(D::Error::custom(
                        "screen grapheme exceeds 64 suffix scalars",
                    ));
                }
                let mut cell = Cell::default();
                cell.set_codepoint(cp);
                cell.set_width(wire.width);
                cell.set_spacer_head(wire.spacer_head);
                cell.set_protected(wire.protected);
                cell.set_semantic(wire.semantic);
                let inline = cp.is_none()
                    && cell.width() == 1
                    && !cell.spacer_head()
                    && wire.style
                        == (Style {
                            background: wire.style.background,
                            ..Style::default()
                        });
                if inline {
                    cell.set_background(wire.style.background);
                } else if wire.style != Style::default() {
                    cell.set_style_id(1);
                }
                let copy = CellCopy {
                    cell,
                    style: wire.style,
                    link: wire.hyperlink,
                    link_id: 0,
                    text: (suffix > 0).then(|| (Arc::from(wire.text), suffix as u8)),
                };
                screen
                    .install_cell(absolute, col, copy, false)
                    .map_err(|_| D::Error::custom("screen cell resources exceed page capacity"))?;
            }
            let (index, relative) = screen.locate(absolute);
            let page = &mut screen.pages.pages[index];
            page.row_ids[relative] = row.id;
            let header = &mut page.headers[relative];
            header.set(RowHeader::WRAPPED, row.wrapped);
            header.set(RowHeader::CONTINUATION, row.wrap_continuation);
            header.set_semantic(row.semantic);
            header.set(RowHeader::DIRTY, row.dirty);
        }
        screen.sync_cursor_resources();
        Ok(screen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Terminal, snapshot};

    #[test]
    fn contextual_json_preserves_graphemes_through_live_mutation_and_native_snapshots() {
        let mut terminal = Terminal::new(8, 2, 1000);
        terminal.feed(b"\x1b[31m\x1b]8;id=shared;https://example.org/\xff\x07");
        terminal.feed("a\u{301}\r\nb\u{301}\r\nc\u{301}".as_bytes());
        let json = serde_json::to_value(terminal.screen()).unwrap();
        assert_eq!(json["history"][0]["cells"][0]["text"], "a\u{301}");
        assert_eq!(json["rows"][1]["cells"][0]["text"], "c\u{301}");
        let restored: Screen = serde_json::from_value(json.clone()).unwrap();
        let roundtrip = serde_json::to_value(&restored).unwrap();
        for field in ["rows", "history", "cursor"] {
            assert_eq!(roundtrip[field], json[field], "{field}");
        }

        *terminal.screen_mut() = restored;
        terminal.feed("\u{302}\r\nz\u{301}".as_bytes());
        terminal.resize(5, 3);
        let text = |terminal: &Terminal| {
            terminal
                .screen()
                .all_rows()
                .map(|row| terminal.screen().row_text(row))
                .collect::<Vec<_>>()
        };
        let expected = text(&terminal);
        for cluster in ["a\u{301}", "b\u{301}", "c\u{301}\u{302}", "z\u{301}"] {
            assert!(
                expected.iter().any(|line| line.contains(cluster)),
                "{cluster}"
            );
        }
        let wire = snapshot::encode_to_vec(&terminal).unwrap();
        let mut restored = snapshot::decode(wire.as_slice(), Default::default()).unwrap();
        assert_eq!(text(&restored), expected);
        restored.feed("\u{303}".as_bytes());
        assert!(
            text(&restored)
                .iter()
                .any(|line| line.contains("z\u{301}\u{303}"))
        );
    }

    #[test]
    fn contextual_json_rejects_oversized_graphemes_and_empty_cursor_links() {
        let terminal = Terminal::new(8, 2, 1000);
        let json = serde_json::to_value(terminal.screen()).unwrap();
        let mut oversized = json.clone();
        oversized["rows"][0]["cells"][0]["text"] = serde_json::json!("a".repeat(66));
        assert!(
            serde_json::from_value::<Screen>(oversized)
                .unwrap_err()
                .to_string()
                .contains("64 suffix scalars")
        );
        let mut empty_link = json;
        empty_link["cursor"]["hyperlink"] = serde_json::json!("");
        assert!(
            serde_json::from_value::<Screen>(empty_link)
                .unwrap_err()
                .to_string()
                .contains("cursor hyperlink")
        );
    }
}
