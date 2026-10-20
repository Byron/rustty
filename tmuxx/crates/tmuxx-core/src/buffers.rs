use super::*;

pub(super) fn creation_name(name: &[u8]) -> Result<String, String> {
    if name.is_empty() {
        return Err("empty buffer name".into());
    }
    // Keep rejected bytes out of line-framed control diagnostics.
    let invalid = || format!("invalid buffer name: {}", name.escape_ascii());
    let name = std::str::from_utf8(name).map_err(|_| invalid())?;
    if name
        .bytes()
        .any(|byte| byte.is_ascii_control() && !matches!(byte, b'\t' | b'\n'))
    {
        return Err(invalid());
    }
    Ok(clean_name(name, false))
}

impl Engine {
    pub(super) fn buffer_format(&self, key: &[u8], name: Option<&str>) -> Option<Vec<u8>> {
        let name = name.or_else(|| self.top_buffer_name())?;
        let data = self.buffers.get(name)?;
        Some(match key {
            b"buffer_name" => name.as_bytes().to_vec(),
            b"buffer_size" => data.len().to_string().into_bytes(),
            b"buffer_full" => data.split(|b| *b == 0).next().unwrap_or_default().to_vec(),
            b"buffer_sample" => sample(data),
            b"buffer_created" => self
                .buffer_created
                .get(name)
                .copied()
                .unwrap_or(0)
                .to_string()
                .into_bytes(),
            _ => return None,
        })
    }
    pub(super) fn enumerated_format(
        &self,
        key: &str,
        sid: Option<Id>,
        wid: Option<Id>,
        pid: Option<Id>,
        ctx: &ClientContext,
    ) -> Option<Vec<u8>> {
        if let Some(value) = self.buffer_format(key.as_bytes(), None) {
            return Some(value);
        }
        let session = sid.and_then(|id| self.sessions.get(&id));
        if key == "session_last_attached" {
            return Some(
                (session?.last_attached / 1_000_000)
                    .to_string()
                    .into_bytes(),
            );
        }
        if key == "config_files" {
            return Some(
                self.identity
                    .get(key)
                    .cloned()
                    .unwrap_or_default()
                    .into_bytes(),
            );
        }
        if key == "pane_search_string" {
            self.panes.get(&pid?)?;
            return Some(Vec::new());
        }
        if matches!(
            key,
            "session_group" | "session_group_size" | "session_group_list"
        ) {
            session?.group.as_ref()?;
        }
        let value = self.format_value(key, sid, wid, pid, ctx)?;
        if value.is_empty()
            && matches!(
                key,
                "pane_dead_signal"
                    | "pane_dead_status"
                    | "pane_dead_time"
                    | "pane_last_output_time"
                    | "pane_pipe_pid"
                    | "session_attached_list"
                    | "window_active_clients_list"
                    | "window_modal_pane"
                    | "client_theme"
            )
        {
            return None;
        }
        Some(value.into_bytes())
    }
}

fn sample(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let data = &bytes[..bytes.len().min(200)];
    let mut i = 0;
    while i < data.len() {
        let width = match data[i] {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 0,
        };
        if width != 0
            && let Some(value) = data
                .get(i..i + width)
                .filter(|bytes| std::str::from_utf8(bytes).is_ok())
        {
            out.extend(value);
            i += width;
            continue;
        }
        match data[i] {
            b'\\' => out.extend(b"\\\\"),
            b'\n' => out.extend(b"\\n"),
            b'\r' => out.extend(b"\\r"),
            b'\t' => out.extend(b"\\t"),
            7 => out.extend(b"\\a"),
            8 => out.extend(b"\\b"),
            11 => out.extend(b"\\v"),
            12 => out.extend(b"\\f"),
            0 => out.extend(
                if data.get(i + 1).is_some_and(|b| matches!(b, b'0'..=b'7')) {
                    b"\\000".as_slice()
                } else {
                    b"\\0"
                },
            ),
            0x20..=0x7e => out.push(data[i]),
            byte => out.extend(format!("\\{byte:03o}").as_bytes()),
        }
        i += 1;
    }
    if bytes.len() > 200 || out.len() > 200 {
        out.truncate(200);
        out.extend(b"...");
    }
    out
}

// VIS_SAFE|VIS_NOSLASH preserves valid UTF-8 and renders unsafe control bytes.
pub(super) fn paste(data: &[u8], separator: &[u8], literal: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let byte = data[i];
        let width = match byte {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 0,
        };
        if width != 0
            && let Some(value) = data
                .get(i..i + width)
                .filter(|bytes| std::str::from_utf8(bytes).is_ok())
        {
            out.extend(value);
            i += width;
            continue;
        }
        if byte == b'\n' {
            out.extend(separator);
        } else if literal || matches!(byte, 7..=9 | 13 | 0x20..=0x7e)
            // Darwin ctype treats these standalone Latin-1 bytes as graphics.
            || (cfg!(target_os = "macos") && matches!(byte, 0xa1..=0xac | 0xae..=0xff))
        {
            out.push(byte);
        } else {
            if byte == 0xa0 {
                out.extend(b"\\240");
            } else {
                if byte & 0x80 != 0 {
                    out.push(b'M');
                }
                let low = byte & 0x7f;
                if low < 0x20 || low == 0x7f {
                    out.extend([b'^', if low == 0x7f { b'?' } else { low + b'@' }]);
                } else {
                    out.extend([b'-', low]);
                }
            }
        }
        i += 1;
    }
    out
}
