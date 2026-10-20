use super::*;

impl Host {
    pub(super) fn control_color_report(&mut self, value: &[u8]) -> Result<Outcome, String> {
        let Some((pane, channel, color)) = report(value) else {
            return Ok(Outcome::default());
        };
        if !self.engine.panes.contains_key(&pane) {
            return Ok(Outcome::default());
        }
        let colors = self.control.colors.entry(pane).or_insert([None; 2]);
        let changed = colors[channel] != Some(color);
        colors[channel] = Some(color);
        self.sync_control_colors();
        Ok(Outcome {
            changed,
            ..Outcome::default()
        })
    }

    pub(super) fn sync_control_colors(&mut self) {
        self.control
            .colors
            .retain(|pane, _| self.engine.panes.contains_key(pane));
        let active = self
            .clients
            .values()
            .any(|client| client.control || client.subscribed);
        let changes = self
            .engine
            .panes
            .iter()
            .filter_map(|(&pane, state)| {
                let colors = if active {
                    self.control.colors.get(&pane).copied().unwrap_or([None; 2])
                } else {
                    [None; 2]
                };
                (state.terminal.control_colors() != colors).then_some((pane, colors))
            })
            .collect::<Vec<_>>();
        if changes.is_empty() {
            return;
        }
        for (pane, colors) in changes {
            let effects = self.engine.set_control_colors(pane, colors);
            if let Err(error) = self.effects(effects) {
                eprintln!("tmuxx: reported colors: {error}");
            }
        }
        self.engine.sequence += 1;
        self.broadcast(ServerMessage::Changed {
            sequence: self.engine.sequence,
        });
    }
}

fn report(value: &[u8]) -> Option<(Id, usize, [u8; 3])> {
    let value = value.strip_prefix(b"%")?;
    let colon = value.iter().position(|byte| *byte == b':')?;
    let pane = unsigned(std::str::from_utf8(&value[..colon]).ok()?, usize::MAX, 10)?.0;
    let value = &value[colon + 1..];
    let channel = match value.get(..5)? {
        b"\x1b]10;" => 0,
        b"\x1b]11;" => 1,
        _ => return None,
    };
    for i in 0..127 {
        let byte = *value.get(5 + i)?;
        if byte == 7 || (byte == b'\\' && value[4 + i] == 27) {
            let end = 5 + i - usize::from(value[4 + i] == 27);
            return Some((
                pane.into(),
                channel,
                x11(std::str::from_utf8(&value[5..end]).ok()?)?,
            ));
        }
    }
    None
}

fn unsigned(value: &str, width: usize, radix: u32) -> Option<(u32, &str)> {
    let value = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let input = &value.as_bytes()[..width.min(value.len())];
    let negative = input.first() == Some(&b'-');
    let sign = usize::from(matches!(input.first(), Some(b'+' | b'-')));
    let input = &input[sign..];
    let prefix = if radix == 16 && (input.starts_with(b"0x") || input.starts_with(b"0X")) {
        2
    } else {
        0
    };
    let mut end = prefix;
    let mut number = 0u32;
    for &byte in &input[prefix..] {
        let Some(digit) = char::from(byte).to_digit(radix) else {
            break;
        };
        number = number.wrapping_mul(radix).wrapping_add(digit);
        end += 1;
    }
    if end == 0 {
        return None;
    }
    Some((
        if negative {
            number.wrapping_neg()
        } else {
            number
        },
        &value[sign + end..],
    ))
}

fn rgb(value: &str, width: usize, delimiter: &str) -> Option<[u8; 3]> {
    let (red, rest) = unsigned(value, width, 16)?;
    let (green, rest) = unsigned(rest.strip_prefix(delimiter)?, width, 16)?;
    let (blue, _) = unsigned(rest.strip_prefix(delimiter)?, width, 16)?;
    let shift = if width == 4 { 8 } else { 0 };
    Some([
        (red >> shift) as u8,
        (green >> shift) as u8,
        (blue >> shift) as u8,
    ])
}

fn float(value: &str) -> Option<(f64, &str)> {
    let value = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let end = value
        .bytes()
        .take_while(|byte| byte.is_ascii_digit() || b"+-.eE".contains(byte))
        .count();
    (1..=end).rev().find_map(|end| {
        value[..end]
            .parse()
            .ok()
            .map(|number| (number, &value[end..]))
    })
}

fn x11(value: &str) -> Option<[u8; 3]> {
    if matches!(value.len(), 12 | 18)
        && let Some(rgb_text) = value.strip_prefix("rgb:")
        && let Some(color) = rgb(rgb_text, if value.len() == 12 { 2 } else { 4 }, "/")
    {
        return Some(color);
    }
    if matches!(value.len(), 7 | 13)
        && let Some(rgb_text) = value.strip_prefix('#')
        && let Some(color) = rgb(rgb_text, if value.len() == 7 { 2 } else { 4 }, "")
    {
        return Some(color);
    }
    if let Some((red, rest)) = unsigned(value, usize::MAX, 10)
        && let Some((green, rest)) = rest
            .strip_prefix(',')
            .and_then(|rest| unsigned(rest, usize::MAX, 10))
        && let Some((blue, _)) = rest
            .strip_prefix(',')
            .and_then(|rest| unsigned(rest, usize::MAX, 10))
    {
        return Some([red as u8, green as u8, blue as u8]);
    }
    if let Some((rest, has_black)) = value
        .strip_prefix("cmyk:")
        .map(|rest| (rest, true))
        .or_else(|| value.strip_prefix("cmy:").map(|rest| (rest, false)))
    {
        let (cyan, rest) = float(rest)?;
        let (magenta, rest) = float(rest.strip_prefix('/')?)?;
        let (yellow, rest) = float(rest.strip_prefix('/')?)?;
        let black = if has_black {
            float(rest.strip_prefix('/')?)?.0
        } else {
            0.0
        };
        if [cyan, magenta, yellow, black]
            .iter()
            .all(|channel| (0.0..=1.0).contains(channel))
        {
            return Some(
                [cyan, magenta, yellow]
                    .map(|channel| ((1.0 - channel) * (1.0 - black) * 255.0) as u8),
            );
        }
        return None;
    }
    let value = value.trim_matches(' ');
    let lower = value.to_ascii_lowercase();
    if let Some(gray) = lower
        .strip_prefix("gray")
        .or_else(|| lower.strip_prefix("grey"))
    {
        if gray.is_empty() {
            return Some([190; 3]);
        }
        let gray = gray
            .trim_ascii_start()
            .parse::<u8>()
            .ok()
            .filter(|gray| *gray <= 100)?;
        return Some([(2.55 * f64::from(gray)).round() as u8; 3]);
    }
    if !value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic())
        || value.bytes().all(|byte| byte.is_ascii_hexdigit())
        || value.contains(['#', ':', '/', '\t'])
    {
        return None;
    }
    tmuxx_terminal::parse_color(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_follow_c_osc_framing_and_x11_channel_conversion() {
        for (value, color) in [
            ("rgb:80/ab/ff", [128, 171, 255]),
            ("rgb:8000/ab00/ffff", [128, 171, 255]),
            ("#8000ab00ffff", [128, 171, 255]),
            ("1,-2,259junk", [1, 254, 3]),
            ("cmy:0.5/0/1", [127, 255, 0]),
            ("cmyk:0/0/0/0.5", [127; 3]),
            ("gray50", [127; 3]),
            (" red ", [255, 0, 0]),
        ] {
            assert_eq!(x11(value), Some(color), "{value}");
        }
        for value in [
            "#123",
            "rgb:1/2/3",
            "123456",
            "abcdef",
            "rgbi:1/0/0",
            "cmy:-1/0/0",
            "red\t",
        ] {
            assert_eq!(x11(value), None, "{value}");
        }
        assert_eq!(
            report(b"%+2junk:\x1b]11;#123456\x1b\\extra"),
            Some((2, 1, [18, 52, 86]))
        );
        assert_eq!(report(b"%2:\x1b]10;red\x07"), Some((2, 0, [255, 0, 0])));
        for value in [
            &b"%2:\x1b]10;red"[..],
            b"%2:\x1b]12;red\x07",
            b"%2:\x1b]11;\x07",
            b"%2:\x1b]11;?\x07",
        ] {
            assert_eq!(report(value), None);
        }
    }
}
