//! The JSON text field needs its owning page; metadata uses derived adapters.
use super::{
    Arc, Cell, CellText, GraphemeAdmission, HashSet, Hyperlink, HyperlinkAdmission, HyperlinkData,
    HyperlinkId, Row, Screen, StyleAdmission,
};
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
                    row,
                    cells: CellsRef {
                        screen: self.screen,
                        row,
                    },
                }),
        )
    }
}

#[derive(Serialize)]
struct RowRef<'a> {
    #[serde(flatten, with = "Row")]
    row: &'a Row,
    cells: CellsRef<'a>,
}

struct CellsRef<'a> {
    screen: &'a Screen,
    row: &'a Row,
}

impl Serialize for CellsRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(
            self.row
                .cells
                .iter()
                .enumerate()
                .map(|(col, cell)| CellRef {
                    cell,
                    text: self.screen.cell_text(self.row, col),
                }),
        )
    }
}

#[derive(Serialize)]
struct CellRef<'a> {
    #[serde(flatten, with = "Cell")]
    cell: &'a Cell,
    #[serde(serialize_with = "serialize_text")]
    text: CellText<'a>,
}

fn serialize_text<S: Serializer>(text: &CellText<'_>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(text)
}

impl Serialize for Screen {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ScreenRef {
            screen: self,
            history_bytes: self.history_bytes(),
            rows: RowsRef {
                screen: self,
                start: self.history.len(),
                len: self.rows.len(),
            },
            history: RowsRef {
                screen: self,
                start: 0,
                len: self.history.len(),
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
    #[serde(flatten, with = "Row")]
    row: Row,
    cells: Vec<CellWire>,
}

#[derive(Deserialize)]
struct CellWire {
    #[serde(flatten, with = "Cell")]
    cell: Cell,
    text: String,
}

fn valid_hyperlink(link: &HyperlinkData) -> bool {
    !link.uri_bytes().is_empty()
        && !matches!(&link.id, Some(HyperlinkId::Explicit(id)) if id.is_empty())
}

impl<'de> Deserialize<'de> for Screen {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ScreenWire::deserialize(deserializer)?;
        let mut screen = wire.screen;
        if screen.columns == 0 || screen.columns > usize::from(u16::MAX) || wire.rows.is_empty() {
            return Err(D::Error::custom("invalid screen dimensions"));
        }
        let history = wire.history.len();
        let total = history
            .checked_add(wire.rows.len())
            .ok_or_else(|| D::Error::custom("screen row count overflow"))?;
        let mut count = 0usize;
        let mut serials = HashSet::new();
        for page in &mut screen.pages.pages {
            if page.rows == 0
                || page.rows > page.capacity.rows
                || page.columns == 0
                || page.columns > page.capacity.cols
                || !serials.insert(page.serial)
            {
                return Err(D::Error::custom("invalid screen page"));
            }
            let layout = page
                .capacity
                .layout()
                .map_err(|_| D::Error::custom("invalid screen page capacity"))?;
            count = count
                .checked_add(usize::from(page.rows))
                .ok_or_else(|| D::Error::custom("screen page row count overflow"))?;
            page.styles = StyleAdmission::new(layout.styles_layout);
            page.links = HyperlinkAdmission::new(
                layout.hyperlink_set_layout,
                layout.string_alloc_layout,
                layout.hyperlink_map_layout.capacity as usize * 80 / 100,
            );
            page.graphemes = GraphemeAdmission::new(
                layout.grapheme_alloc_layout,
                layout.grapheme_map_layout.capacity as usize,
            );
        }
        if count != total {
            return Err(D::Error::custom("screen pages do not cover its rows"));
        }

        let mut contents = Vec::with_capacity(total);
        let mut pending = Vec::with_capacity(total);
        for (absolute, wire) in wire.history.into_iter().chain(wire.rows).enumerate() {
            let page = screen.pages.page_at(absolute).0;
            if wire.cells.len() != usize::from(page.columns) {
                return Err(D::Error::custom("screen row width differs from its page"));
            }
            let mut row = wire.row;
            row.resource_page = Some(page.serial);
            row.cells.resize(wire.cells.len(), Cell::default());
            pending.push(wire.cells);
            contents.push(row);
        }
        screen.rows = contents.split_off(history);
        screen.history = contents.into();
        if screen.cursor.row >= screen.rows.len()
            || screen.cursor.col >= screen.columns
            || screen.cursor.col >= screen.rows[screen.cursor.row].cells.len()
            || screen.viewport_offset > screen.history.len()
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

        for (absolute, cells) in pending.into_iter().enumerate() {
            for (col, wire) in cells.into_iter().enumerate() {
                let mut cell = wire.cell;
                let mut chars = wire.text.chars();
                cell.codepoint = chars.next();
                let len = chars.count();
                if len > 64 {
                    return Err(D::Error::custom(
                        "screen grapheme exceeds 64 suffix scalars",
                    ));
                }
                let style = std::mem::take(&mut cell.style);
                let hyperlink = cell.hyperlink.take();
                if hyperlink
                    .as_deref()
                    .is_some_and(|link| !valid_hyperlink(link))
                {
                    return Err(D::Error::custom("invalid screen hyperlink"));
                }
                // Admission may split and rehome a page. Expose only resources
                // whose references have already been acquired.
                screen.physical_row_mut(absolute).cells[col] = cell;
                let id = screen
                    .acquire_style(absolute, style, None)
                    .map_err(|_| D::Error::custom("screen cell style cannot fit its page"))?;
                let cell = &mut screen.physical_row_mut(absolute).cells[col];
                cell.style = style;
                cell.style_id = id;
                if let Some(hyperlink) = hyperlink {
                    let link = Hyperlink {
                        id: hyperlink.id.clone().unwrap_or(HyperlinkId::Implicit(0)),
                        uri: hyperlink.uri_bytes().to_vec(),
                    };
                    let id = screen.acquire_link_cell(absolute, &link, 0).map_err(|_| {
                        D::Error::custom("screen cell hyperlink cannot fit its page")
                    })?;
                    let cell = &mut screen.physical_row_mut(absolute).cells[col];
                    cell.hyperlink = Some(hyperlink);
                    cell.link_id = id;
                }
                if len > 0 {
                    let allocation = screen
                        .acquire_grapheme(absolute, len as u8)
                        .map_err(|_| D::Error::custom("screen grapheme cannot fit its page"))?;
                    let index = screen.pages.page_index(absolute);
                    screen.pages.pages[index]
                        .graphemes
                        .set_text(allocation, Arc::from(wire.text));
                    screen.physical_row_mut(absolute).cells[col].grapheme = Some(allocation);
                }
            }
        }
        screen.sync_cursor_resources();
        screen.history_bytes = screen.history.iter().map(Row::storage_bytes).sum();
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
