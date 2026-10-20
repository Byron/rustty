use super::*;

const THEMES: [(&str, &str, u8); 10] = [
    ("themeblack", "black", 0),
    ("themewhite", "white", 7),
    ("themelightgrey", "light-grey", 7),
    ("themedarkgrey", "dark-grey", 0),
    ("themegreen", "green", 2),
    ("themeyellow", "yellow", 3),
    ("themered", "red", 1),
    ("themeblue", "blue", 4),
    ("themecyan", "cyan", 6),
    ("thememagenta", "magenta", 5),
];

fn components(value: &str) -> Result<[Option<&str>; 2], String> {
    let mut colors = [None; 2];
    for token in value
        .split([' ', ',', '\n'])
        .filter(|token| !token.is_empty())
    {
        if token.eq_ignore_ascii_case("default") {
            colors = [None; 2];
            continue;
        }
        let Some((key, value)) = token.split_once('=') else {
            return Err(format!(
                "unsupported pane style token in native-client profile: {token}"
            ));
        };
        let index = if key.eq_ignore_ascii_case("fg") {
            0
        } else if key.eq_ignore_ascii_case("bg") {
            1
        } else {
            return Err(format!(
                "unsupported pane style token in native-client profile: {token}"
            ));
        };
        colors[index] = if value.eq_ignore_ascii_case("default") {
            None
        } else {
            Some(value)
        };
    }
    Ok(colors)
}

pub(super) fn rgb_name([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}
pub(super) fn valid_color(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "default" | "terminal")
        || THEMES
            .iter()
            .any(|(name, _, _)| value.eq_ignore_ascii_case(name))
        || terminal_options::colour(value).is_some()
}
pub(super) fn color_name(value: &str) -> String {
    let value = value.to_ascii_lowercase();
    if !valid_color(&value) {
        return "default".into();
    }
    let ansi = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    if let Ok(index) = value.parse::<usize>() {
        if index < 8 {
            return ansi[index].into();
        }
        if (90..=97).contains(&index) {
            return format!("bright{}", ansi[index - 90]);
        }
    }
    if let Some(index) = value
        .strip_prefix("colour")
        .or_else(|| value.strip_prefix("color"))
        .and_then(|n| n.parse::<u8>().ok())
    {
        return format!("colour{index}");
    }
    if value == "default"
        || value == "terminal"
        || THEMES.iter().any(|(name, _, _)| *name == value)
        || ansi
            .iter()
            .any(|name| value == *name || value == format!("bright{name}"))
    {
        return value;
    }
    terminal_options::colour(&value)
        .map(rgb_name)
        .unwrap_or_else(|| "default".into())
}

pub(super) fn validate(value: &str) -> Result<(), String> {
    if value.contains("#{") || value.contains("#(") {
        return Err("dynamic pane styles are unsupported in native-client profile".into());
    }
    for color in components(value)?.into_iter().flatten() {
        if color.eq_ignore_ascii_case("terminal") {
            return Err(
                "terminal pane style colour is unsupported in native-client profile".into(),
            );
        }
        if !THEMES
            .iter()
            .any(|(name, _, _)| color.eq_ignore_ascii_case(name))
            && terminal_options::colour(color).is_none()
        {
            return Err(format!("invalid style: {value}"));
        }
    }
    Ok(())
}

impl Engine {
    pub(super) fn cursor_default_color(&self, id: Id) -> String {
        let value = self.pane_option(id, "cursor-colour").unwrap_or("");
        let context = ClientContext {
            no_format_jobs: true,
            ..ClientContext::default()
        };
        let value = self
            .expand(value.as_bytes(), None, None, None, &context)
            .unwrap_or_default();
        if value.is_empty() {
            "none".into()
        } else {
            color_name(&text(&value))
        }
    }
    pub(super) fn pane_color_name(&self, id: Id, index: usize) -> String {
        let pane = &self.panes[&id];
        let application = pane.terminal.application_color(index).map(rgb_name);
        let colors = |name| {
            components(self.pane_option(id, name).unwrap_or("default")).unwrap_or_default()[index]
                .map(color_name)
        };
        (if self.windows[&pane.window].active == id {
            colors("window-active-style")
                .or(application.clone())
                .or_else(|| colors("window-style"))
        } else {
            colors("window-style").or(application)
        })
        .unwrap_or_else(|| "default".into())
    }
    pub(super) fn pane_style(&self, id: Id) -> tmuxx_terminal::TmuxPaneStyle {
        let pane = &self.panes[&id];
        let policy = self.global_option("theme").unwrap_or("detect");
        let dark = match policy {
            "light" => false,
            "dark" => true,
            _ => pane
                .terminal
                .host_dark()
                .or_else(|| {
                    pane.terminal.control_colors()[1]
                        .map(|rgb| rgb.into_iter().map(u16::from).sum::<u16>() <= 382)
                })
                .unwrap_or(true),
        };
        let color = |value: &str| {
            if let Some((_, suffix, index)) = THEMES
                .iter()
                .find(|(name, _, _)| value.eq_ignore_ascii_case(name))
            {
                if policy == "terminal" {
                    return terminal_options::colour(&format!("colour{index}"));
                }
                let option = format!("{}-theme-{suffix}", if dark { "dark" } else { "light" });
                let value = self.global_option(&option)?;
                let context = ClientContext {
                    current_pane: Some(id),
                    formats: BTreeMap::from([(b"client_colours".to_vec(), b"256".to_vec())]),
                    ..ClientContext::default()
                };
                let value = self
                    .expand(
                        value.as_bytes(),
                        None,
                        Some(pane.window),
                        Some(id),
                        &context,
                    )
                    .ok()?;
                terminal_options::colour(&text(&value))
            } else {
                terminal_options::colour(value)
            }
        };
        let colors = |name| {
            components(self.pane_option(id, name).unwrap_or("default"))
                .unwrap_or_default()
                .map(|value| value.and_then(color))
        };
        tmuxx_terminal::TmuxPaneStyle {
            normal: colors("window-style"),
            active: colors("window-active-style"),
            is_active: self.windows[&pane.window].active == id,
        }
    }

    /// A native host change can alter both theme tokens and application reports.
    pub fn set_pane_host_state(
        &mut self,
        pane: Id,
        focused: bool,
        visible: bool,
        dark: Option<bool>,
        focus_events: bool,
    ) -> Vec<Effect> {
        let Some(terminal) = self.panes.get_mut(&pane).map(|pane| &mut pane.terminal) else {
            return Vec::new();
        };
        let mut effects = terminal.set_host_state(focused, visible, dark, focus_events);
        let style = self.pane_style(pane);
        effects.extend(
            self.panes
                .get_mut(&pane)
                .unwrap()
                .terminal
                .set_pane_style(style),
        );
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                tmuxx_terminal::Effect::Write(data) => Some(Effect::Write { pane, data }),
                _ => None,
            })
            .collect()
    }

    pub fn set_control_colors(&mut self, pane: Id, colors: [Option<[u8; 3]>; 2]) -> Vec<Effect> {
        let Some(terminal) = self.panes.get_mut(&pane).map(|pane| &mut pane.terminal) else {
            return Vec::new();
        };
        let mut effects = terminal.set_control_colors(colors);
        let style = self.pane_style(pane);
        effects.extend(
            self.panes
                .get_mut(&pane)
                .unwrap()
                .terminal
                .set_pane_style(style),
        );
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                tmuxx_terminal::Effect::Write(data) => Some(Effect::Write { pane, data }),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(
        engine: &mut Engine,
        context: &mut ClientContext,
        source: &str,
    ) -> Result<Outcome, String> {
        let command = tmuxx_command::parse(source.as_bytes()).unwrap();
        engine.execute(&command[0], context)
    }
    #[test]
    fn active_styles_preserve_application_colors_and_reconnect_settings() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        for source in [
            "new -d ''",
            "splitw -d ''",
            "set -g window-style 'fg=#123456,bg=#abcdef'",
        ] {
            run(&mut engine, &mut context, source).unwrap();
        }
        let bytes = b"\x1b]10;#010203\x07\x1b]11;#040506\x07";
        engine.feed(0, bytes);
        engine.feed(1, bytes);
        assert_eq!(engine.panes[&0].terminal.snapshot().background, [4, 5, 6]);
        assert_eq!(
            engine.panes[&1].terminal.snapshot().background,
            [0xab, 0xcd, 0xef]
        );
        run(
            &mut engine,
            &mut context,
            "set -g window-active-style 'bg=#fedcba'",
        )
        .unwrap();
        assert_eq!(
            engine.panes[&0].terminal.snapshot().background,
            [0xfe, 0xdc, 0xba]
        );
        run(&mut engine, &mut context, "selectp -t %1").unwrap();
        assert_eq!(
            engine.panes[&1].terminal.snapshot().background,
            [0xfe, 0xdc, 0xba]
        );
        assert_eq!(
            engine.panes[&0].terminal.snapshot().foreground,
            [0x12, 0x34, 0x56]
        );
        let snapshot = engine.panes[&1].terminal.snapshot();
        let mut restored = snapshot.restore().unwrap();
        assert_eq!(restored.snapshot().background, snapshot.background);
        restored.set_pane_style(tmuxx_terminal::TmuxPaneStyle::default());
        assert_eq!(restored.snapshot().background, [4, 5, 6]);
        let before = engine.panes[&1].terminal.snapshot().background;
        for value in [
            "dim=50%",
            "bold",
            "bg=terminal",
            "#{?pane_active,bg=red,bg=blue}",
        ] {
            assert!(
                run(
                    &mut engine,
                    &mut context,
                    &format!("set -g window-style '{value}'")
                )
                .unwrap_err()
                .contains("unsupported")
            );
            assert_eq!(engine.panes[&1].terminal.snapshot().background, before);
        }
        engine.exited(1, 0);
        assert_eq!(
            engine.panes[&0].terminal.snapshot().background,
            [0xfe, 0xdc, 0xba]
        );
        run(&mut engine, &mut context, "splitw ''").unwrap();
        engine.close_pane(2);
        assert_eq!(
            engine.panes[&0].terminal.snapshot().background,
            [0xfe, 0xdc, 0xba]
        );
    }
    #[test]
    fn forced_theme_maps_native_colors_without_overriding_reported_host_theme() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        for source in [
            "new -d ''",
            "set -g window-style 'fg=themewhite,bg=themeblack'",
            "set -g theme light",
        ] {
            run(&mut engine, &mut context, source).unwrap();
        }
        assert_eq!(engine.panes[&0].terminal.snapshot().background, [242; 3]);
        engine.set_pane_host_state(0, true, true, Some(true), false);
        let reply = engine.feed(0, b"\x1b[?996n");
        assert!(
            reply
                .iter()
                .any(|effect| matches!(effect,Effect::Write {data,..} if data == b"\x1b[?997;1n"))
        );
        run(&mut engine, &mut context, "set -g theme detect").unwrap();
        assert_eq!(engine.panes[&0].terminal.snapshot().background, [13; 3]);
        let restored = engine.panes[&0].terminal.snapshot().restore().unwrap();
        assert_eq!(restored.host_dark(), Some(true));
    }
}
