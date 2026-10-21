use rustty::config::{Config, ThemeChoice};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    Global,
    Host(String),
    Session(String),
}

impl Scope {
    pub fn for_connection(config: &Config, host: Option<&str>, session: &str) -> Self {
        if !config.tmuxx_theme_sessions.contains_key(session)
            && let Some(host) = host.filter(|host| config.tmuxx_theme_hosts.contains_key(*host))
        {
            return Self::Host(host.to_owned());
        }
        Self::Session(session.to_owned())
    }

    pub fn select(&self, config: &mut Config, theme: &str) {
        match self {
            Self::Host(host) => {
                config
                    .tmuxx_theme_hosts
                    .insert(host.clone(), theme.to_owned());
            }
            Self::Session(session) => {
                config
                    .tmuxx_theme_sessions
                    .insert(session.clone(), theme.to_owned());
            }
            Self::Global => {}
        }
    }

    pub fn config_line(&self, theme: &ThemeChoice) -> String {
        let value = theme.config_value();
        let (key, name) = match self {
            Self::Global => return format!("theme = {value}"),
            Self::Host(host) => ("tmuxx-theme-host", host),
            Self::Session(session) => ("tmuxx-theme-session", session),
        };
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(&value);
        let value = format!("{name} = {value}");
        if value.starts_with('"') {
            format!("{key} = \"{value}\"")
        } else {
            format!("{key} = {value}")
        }
    }
}

pub(super) fn appearance(original: &Config, theme: Config) -> Config {
    // Theme files can contain behavioral settings; previews must stay reversible.
    let mut config = original.clone();
    config.font_family = theme.font_family;
    config.font_family_bold = theme.font_family_bold;
    config.font_family_italic = theme.font_family_italic;
    config.font_family_bold_italic = theme.font_family_bold_italic;
    config.font_feature = theme.font_feature;
    config.font_size = theme.font_size;
    config.font_style = theme.font_style;
    config.font_style_bold = theme.font_style_bold;
    config.font_style_italic = theme.font_style_italic;
    config.font_style_bold_italic = theme.font_style_bold_italic;
    config.font_variation = theme.font_variation;
    config.font_variation_bold = theme.font_variation_bold;
    config.font_variation_italic = theme.font_variation_italic;
    config.font_variation_bold_italic = theme.font_variation_bold_italic;
    config.font_codepoint_map = theme.font_codepoint_map;
    config.font_synthetic_style = theme.font_synthetic_style;
    config.font_thicken = theme.font_thicken;
    config.font_thicken_strength = theme.font_thicken_strength;
    config.background = theme.background;
    config.foreground = theme.foreground;
    config.palette = theme.palette;
    config.cursor_color = theme.cursor_color;
    config.cursor_text = theme.cursor_text;
    config.cursor_style = theme.cursor_style;
    config.cursor_style_blink = theme.cursor_style_blink;
    config.selection_foreground = theme.selection_foreground;
    config.selection_background = theme.selection_background;
    config.background_opacity = theme.background_opacity;
    config.search_unfocused_opacity = theme.search_unfocused_opacity;
    config.unfocused_split_opacity = theme.unfocused_split_opacity;
    config.unfocused_split_fill = theme.unfocused_split_fill;
    config.split_divider_color = theme.split_divider_color;
    config.quadrant_peek_opacity = theme.quadrant_peek_opacity;
    config.window_padding_x = theme.window_padding_x;
    config.window_padding_y = theme.window_padding_y;
    config.window_theme = theme.window_theme;
    config.theme = theme.theme;
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustty::config::{
        ClipboardAccess, Command, FontStyleRequest, GraphemeWidthMethod, KeyBinding, Padding,
        Renderer, Rgb, TerminalColor, Theme, WindowTheme,
    };

    #[test]
    fn scoped_selection_preserves_other_rules_and_can_restore_original_config() {
        let mut original = Config::default();
        original
            .tmuxx_theme_hosts
            .insert("windows".into(), "Host".into());
        original
            .tmuxx_theme_hosts
            .insert("linux".into(), "Host".into());
        original
            .tmuxx_theme_sessions
            .insert("build work".into(), "Session".into());
        assert_eq!(
            Scope::for_connection(&original, None, "other"),
            Scope::Session("other".into())
        );
        assert_eq!(
            Scope::for_connection(&original, Some("unconfigured"), "other"),
            Scope::Session("other".into())
        );
        assert_eq!(
            Scope::for_connection(&original, Some("windows"), "other"),
            Scope::Host("windows".into())
        );
        let scope = Scope::for_connection(&original, Some("windows"), "build work");
        assert_eq!(scope, Scope::Session("build work".into()));
        let mut preview = original.clone();
        scope.select(&mut preview, "/themes/Preview");
        assert_eq!(
            preview.tmuxx_theme_sessions["build work"],
            "/themes/Preview"
        );
        assert_eq!(preview.tmuxx_theme_hosts, original.tmuxx_theme_hosts);
        assert_eq!(preview.background, original.background);
        assert_eq!(preview.theme, original.theme);
        scope.select(&mut preview, &original.tmuxx_theme_sessions["build work"]);
        assert_eq!(preview, original);

        Scope::Host("windows".into()).select(&mut preview, "/themes/Preview");
        assert_eq!(preview.tmuxx_theme_hosts["windows"], "/themes/Preview");
        assert_eq!(preview.tmuxx_theme_hosts["linux"], "Host");
        assert_eq!(preview.tmuxx_theme_sessions, original.tmuxx_theme_sessions);
    }

    #[test]
    fn scoped_config_lines_keep_the_selector_and_theme_value() {
        let mut theme = ThemeChoice {
            name: "Chosen Theme".into(),
            path: "/themes/Chosen Theme".into(),
            user: false,
        };
        assert_eq!(Scope::Global.config_line(&theme), "theme = Chosen Theme");
        assert_eq!(
            Scope::Host("windows".into()).config_line(&theme),
            "tmuxx-theme-host = windows = Chosen Theme"
        );
        assert_eq!(
            Scope::Session("build work".into()).config_line(&theme),
            "tmuxx-theme-session = build work = Chosen Theme"
        );
        theme.user = true;
        assert_eq!(
            Scope::Session("build work".into()).config_line(&theme),
            "tmuxx-theme-session = build work = /themes/Chosen Theme"
        );
        theme.user = false;
        theme.name = "\"Quoted\"".into();
        assert_eq!(
            Scope::Session("\"quoted session".into()).config_line(&theme),
            "tmuxx-theme-session = \"\"quoted session = \"Quoted\"\""
        );
    }

    #[test]
    fn previews_only_change_appearance_and_restore_exactly() {
        let original = Config::default();
        let mut expected = original.clone();
        expected.font_family = vec!["Preview Font".into()];
        expected.font_size = 18.0;
        expected.font_style_bold = FontStyleRequest::Disabled;
        expected.background = Rgb::new(1, 2, 3);
        expected.foreground = Rgb::new(4, 5, 6);
        expected.palette[1] = Rgb::new(7, 8, 9);
        expected.cursor_color = Some(TerminalColor::CellForeground);
        expected.selection_background = Some(TerminalColor::CellBackground);
        expected.background_opacity = 0.8;
        expected.split_divider_color = Some(Rgb::new(10, 11, 12));
        expected.window_padding_x = Padding {
            start: 8.0,
            end: 12.0,
        };
        expected.window_theme = WindowTheme::Light;
        expected.theme = Some(Theme {
            light: "Preview".into(),
            dark: "Preview".into(),
        });
        let mut theme = expected.clone();
        theme.renderer = Renderer::Software;
        theme.grapheme_width_method = GraphemeWidthMethod::Legacy;
        theme.window_width = 200;
        theme.window_height = 60;
        theme.scrollback_limit_bytes = Some(0);
        theme.scrollback_limit_lines = Some(0);
        theme.scrollback_compression = false;
        theme.command = Some(Command::Shell("exit".into()));
        theme.initial_command = Some(Command::Shell("exit".into()));
        theme.working_directory = Some("/tmp".into());
        theme.env.insert("PREVIEW".into(), "changed".into());
        theme.keybinds = vec![KeyBinding::parse("ctrl+a=ignore").unwrap()];
        theme.clipboard_read = ClipboardAccess::Allow;
        theme.clipboard_write = ClipboardAccess::Deny;
        theme.clipboard_write_limit_bytes = Some(0);
        theme.clipboard_paste_protection = false;
        theme.clipboard_paste_bracketed_safe = false;
        theme.title_report = true;
        theme.progress_style = false;
        theme.stream_deck = true;

        let preview = appearance(&original, theme);
        assert_eq!(preview, expected);
        assert_eq!(appearance(&preview, original.clone()), original);
    }
}
