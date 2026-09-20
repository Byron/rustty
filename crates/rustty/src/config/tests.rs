use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestHome {
    root: PathBuf,
    loader: ConfigLoader,
}

impl TestHome {
    fn new() -> Self {
        let root = env::temp_dir().join(format!(
            "rustty-config-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let home = root.join("home");
        let loader = ConfigLoader {
            xdg_config_home: home.join(".config"),
            resources_dir: Some(root.join("resources")),
            dark_mode: true,
            working_directory: root.clone(),
            home,
        };
        Self { root, loader }
    }

    fn write(&self, path: impl AsRef<Path>, text: &str) -> PathBuf {
        let path = self.loader.home.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    fn own(&self, text: &str) -> PathBuf {
        self.write(
            "Library/Application Support/com.rustty.app/config.rustty",
            text,
        )
    }
    fn local(&self, text: &str) -> PathBuf {
        self.write(
            "Library/Application Support/com.mitchellh.ghostty.local/config.ghostty",
            text,
        )
    }
    fn stable(&self, text: &str) -> PathBuf {
        self.write(
            "Library/Application Support/com.mitchellh.ghostty/config",
            text,
        )
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_owned()).collect()
}
fn action(config: &Config, trigger: &str) -> Option<Action> {
    config
        .binding(&KeyTrigger::parse(trigger).unwrap())
        .map(|b| b.actions[0].clone())
}

#[test]
fn search_opacity_loads_reloads_and_rejects_invalid_values() {
    let home = TestHome::new();
    assert_eq!(home.loader.load().config.search_unfocused_opacity, 0.8);
    for (value, expected) in [("0", 0.0), ("0.35", 0.35), ("1", 1.0), ("", 0.8)] {
        home.own(&format!("search-unfocused-opacity={value}\n"));
        let loaded = home.loader.load();
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert_eq!(loaded.config.search_unfocused_opacity, expected);
        assert_eq!(loaded.config.unfocused_split_opacity, 0.7);
    }
    for value in ["-0.1", "1.1", "NaN", "inf", "invalid"] {
        home.own(&format!(
            "search-unfocused-opacity=0.4\nsearch-unfocused-opacity={value}\n"
        ));
        let loaded = home.loader.load();
        assert_eq!(loaded.diagnostics.len(), 1, "{value}");
        assert_eq!(loaded.config.search_unfocused_opacity, 0.4);
    }
    let loaded = home
        .loader
        .load_with_args(&args(&["--search-unfocused-opacity=0.6"]));
    assert_eq!(loaded.config.search_unfocused_opacity, 0.6);
}

#[test]
fn grapheme_width_method_loads_ghostty_policy_and_defaults_to_unicode() {
    let home = TestHome::new();
    for (value, expected) in [
        ("legacy", GraphemeWidthMethod::Legacy),
        ("unicode", GraphemeWidthMethod::Unicode),
        ("", GraphemeWidthMethod::Unicode),
    ] {
        home.local(&format!("grapheme-width-method={value}\n"));
        let loaded = home.loader.load();
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert_eq!(loaded.config.grapheme_width_method, expected);
    }
    home.local("grapheme-width-method=invalid\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.diagnostics.len(), 1);
    assert_eq!(
        loaded.config.grapheme_width_method,
        GraphemeWidthMethod::Unicode
    );
}

#[test]
fn mouse_shift_capture_loads_ghostty_policy_and_resets_to_the_default() {
    let home = TestHome::new();
    for (value, expected) in [
        ("false", MouseShiftCapture::False),
        ("true", MouseShiftCapture::True),
        ("always", MouseShiftCapture::Always),
        ("never", MouseShiftCapture::Never),
        ("", MouseShiftCapture::False),
    ] {
        home.local(&format!("mouse-shift-capture={value}\n"));
        let loaded = home.loader.load();
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert_eq!(loaded.config.mouse_shift_capture, expected);
    }
    home.local("mouse-shift-capture=invalid\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.diagnostics.len(), 1);
    assert_eq!(loaded.config.mouse_shift_capture, MouseShiftCapture::False);
}

#[test]
fn no_config_returns_defaults_without_creating_any_settings() {
    let home = TestHome::new();
    let loaded = home.loader.load();
    assert_eq!(loaded.family, ConfigFamily::Defaults);
    assert_eq!(loaded.config, Config::default());
    assert!(loaded.sources.is_empty());
    assert!(loaded.diagnostics.is_empty());
    assert!(!home.loader.home.exists());
    assert!(!loaded.own_config_path.exists());
    assert_eq!(loaded.edit_config_path, loaded.own_config_path);
    assert_eq!(loaded.own_config_path.file_name().unwrap(), "rustty.txt");
}

#[test]
fn rustty_text_settings_take_precedence_with_legacy_paths_still_supported() {
    let home = TestHome::new();
    home.local("font-size=30\n");
    home.write(".config/rustty/config.rustty", "font-size=11\n");
    let text = home.write(".config/rustty/rustty.txt", "font-size=12\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.family, ConfigFamily::Rustty);
    assert_eq!(loaded.own_config_path, text);
    assert_eq!(loaded.edit_config_path, text);
    assert_eq!(loaded.config.font_size, 12.0);
    home.own("font-size=13\n");
    assert_eq!(home.loader.load().config.font_size, 13.0);
    let text = home.write(
        "Library/Application Support/com.rustty.app/rustty.txt",
        "font-size=14\n",
    );
    let loaded = home.loader.load();
    assert_eq!(loaded.edit_config_path, text);
    assert_eq!(loaded.config.font_size, 14.0);
}

#[test]
fn settings_edits_the_active_root_without_creating_a_fallback_override() {
    let home = TestHome::new();
    let stable = home.stable("font-size=14\n");
    assert_eq!(home.loader.load().edit_config_path, stable);
    home.write(".config/ghostty/config", "font-size=15\n");
    let local = home.local("config-file=extra.ghostty\n");
    home.write(
        "Library/Application Support/com.mitchellh.ghostty.local/extra.ghostty",
        "keybind=super+h=goto_split:left\nkeybind=super+shift+f=toggle_quadrant_zoom\n",
    );
    let loaded = home.loader.load();
    assert_eq!(loaded.edit_config_path, local);
    assert!(!loaded.own_config_path.exists());
    assert_eq!(
        action(&loaded.config, "super+h"),
        Some(Action::GotoSplit(Direction::Left))
    );
    assert_eq!(
        action(&loaded.config, "super+shift+f"),
        Some(Action::ToggleQuadrantZoom)
    );
    let own = home.own("");
    let loaded = home.loader.load();
    assert_eq!(loaded.edit_config_path, own);
    assert_eq!(loaded.family, ConfigFamily::Rustty);
}

#[test]
fn font_settings_preserve_style_specific_axes_and_ordered_ranges() {
    let home = TestHome::new();
    home.own("font-style=Book\nfont-style-bold=false\nfont-style-italic=default\nfont-style-bold-italic=Heavy Italic\nfont-variation=wght=400\nfont-variation=slnt = -1.5\nfont-variation-bold=wght=650\nfont-variation-italic=ital=1\nfont-variation-bold-italic=wght=700\nfont-codepoint-map=U+2500 - U+257F, U+E000=Symbols\nfont-codepoint-map=U+E000=Override\nfont-synthetic-style=no-bold,no-italic\nfont-thicken=true\nfont-thicken-strength=0\n");
    let loaded = home
        .loader
        .load_with_args(&args(&["--font-variation=wdth=95"]));
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    let config = loaded.config;
    assert_eq!(config.font_style, FontStyleRequest::Named("Book".into()));
    assert_eq!(config.font_style_bold, FontStyleRequest::Disabled);
    assert_eq!(config.font_style_italic, FontStyleRequest::Default);
    assert_eq!(
        config.font_style_bold_italic,
        FontStyleRequest::Named("Heavy Italic".into())
    );
    assert_eq!(
        config.font_variation,
        [
            FontVariation {
                tag: *b"wght",
                value: 400.0
            },
            FontVariation {
                tag: *b"slnt",
                value: -1.5
            },
            FontVariation {
                tag: *b"wdth",
                value: 95.0
            },
        ]
    );
    assert_eq!(
        config.font_variation_bold,
        [FontVariation {
            tag: *b"wght",
            value: 650.0
        }]
    );
    assert_eq!(
        config.font_variation_italic,
        [FontVariation {
            tag: *b"ital",
            value: 1.0
        }]
    );
    assert_eq!(
        config.font_variation_bold_italic,
        [FontVariation {
            tag: *b"wght",
            value: 700.0
        }]
    );
    assert_eq!(
        config.font_codepoint_map,
        [
            CodepointMap {
                start: 0x2500,
                end: 0x257f,
                family: "Symbols".into()
            },
            CodepointMap {
                start: 0xe000,
                end: 0xe000,
                family: "Symbols".into()
            },
            CodepointMap {
                start: 0xe000,
                end: 0xe000,
                family: "Override".into()
            },
        ]
    );
    assert_eq!(config.font_synthetic_style, [false, false, true]);
    assert!(config.font_thicken);
    assert_eq!(config.font_thicken_strength, 0);
}

#[test]
fn empty_font_settings_reset_and_flag_lists_start_from_defaults() {
    let home = TestHome::new();
    home.own("font-style-bold=false\nfont-style-bold=\nfont-variation=wght=600\nfont-variation=\nfont-variation=wdth=90\nfont-variation-bold=wght=700\nfont-codepoint-map=U+E000=Old\nfont-codepoint-map=\nfont-codepoint-map=U+0041=New\nfont-synthetic-style=false\nfont-synthetic-style=no-italic\nfont-thicken=true\nfont-thicken=\nfont-thicken-strength=0\nfont-thicken-strength=\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    let config = loaded.config;
    assert_eq!(config.font_style_bold, FontStyleRequest::Default);
    assert_eq!(
        config.font_variation,
        [FontVariation {
            tag: *b"wdth",
            value: 90.0
        }]
    );
    assert_eq!(config.font_variation_bold.len(), 1);
    assert_eq!(
        config.font_codepoint_map,
        [CodepointMap {
            start: 65,
            end: 65,
            family: "New".into()
        }]
    );
    assert_eq!(config.font_synthetic_style, [true, false, true]);
    assert!(!config.font_thicken);
    assert_eq!(config.font_thicken_strength, 255);
}

#[test]
fn invalid_font_settings_leave_previous_values_without_exposing_contents() {
    let home = TestHome::new();
    home.own("font-variation=wght=300\nfont-variation=abc=2\nfont-variation=wdth=NaN\nfont-variation=slnt=SECRET\nfont-codepoint-map=U+E000=Keep\nfont-codepoint-map=U+0041,U+0043-U+0042=Bad\nfont-codepoint-map=U+200000=Bad\nfont-codepoint-map=U++41=Bad\nfont-synthetic-style=false\nfont-synthetic-style=no-bold,unknown\nfont-thicken-strength=0xA\nfont-thicken-strength=256\nfont-style-bold=Book\nfont-style-bold=private\0value\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.diagnostics.len(), 9, "{:?}", loaded.diagnostics);
    let diagnostics = format!("{:?}", loaded.diagnostics);
    assert!(!diagnostics.contains("SECRET"));
    assert!(!diagnostics.contains("private"));
    let config = loaded.config;
    assert_eq!(
        config.font_variation,
        [FontVariation {
            tag: *b"wght",
            value: 300.0
        }]
    );
    assert_eq!(
        config.font_codepoint_map,
        [CodepointMap {
            start: 0xe000,
            end: 0xe000,
            family: "Keep".into()
        }]
    );
    assert_eq!(config.font_synthetic_style, [false; 3]);
    assert_eq!(config.font_thicken_strength, 10);
    assert_eq!(
        config.font_style_bold,
        FontStyleRequest::Named("Book".into())
    );
}

#[test]
fn ghostty_loads_xdg_before_local_and_does_not_combine_stable_settings() {
    let home = TestHome::new();
    home.write(".config/ghostty/config", "font-size=9\nfont-family=First\n");
    home.write(
        ".config/ghostty/config.ghostty",
        "font-size=10\nfont-family=Second\n",
    );
    home.stable("font-size=99\nfont-family=Wrong\n");
    home.write(
        "Library/Application Support/com.mitchellh.ghostty.local/config",
        "font-size=11\n",
    );
    home.local("font-size=12\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.family, ConfigFamily::GhosttyLocal);
    assert_eq!(loaded.config.font_size, 12.0);
    assert_eq!(loaded.config.font_family, ["First", "Second"]);
    assert_eq!(loaded.sources.len(), 4);
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert!(!loaded.own_config_path.exists());
}

#[test]
fn stable_fallback_and_own_family_precedence() {
    let home = TestHome::new();
    home.stable("font-size=14\n");
    assert_eq!(home.loader.load().family, ConfigFamily::Ghostty);
    home.local("font-size=15\n");
    home.write(".config/rustty/config", "font-size=16\n");
    home.write(".config/rustty/config.rustty", "font-size=17\n");
    home.write(
        "Library/Application Support/com.rustty.app/config",
        "font-size=18\n",
    );
    let path = home.own("font-size=19\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.family, ConfigFamily::Rustty);
    assert_eq!(loaded.config.font_size, 19.0);
    assert_eq!(loaded.own_config_path, path);
    assert_eq!(loaded.sources.len(), 4);
}

#[test]
fn empty_malformed_and_nonfile_own_config_all_suppress_ghostty_fallback() {
    let home = TestHome::new();
    home.local("font-size=42\n");
    let path = home.own("");
    let loaded = home.loader.load();
    assert_eq!(loaded.family, ConfigFamily::Rustty);
    assert_eq!(loaded.config.font_size, 13.0);
    assert!(loaded.diagnostics.is_empty());
    home.own("font-size = NaN\nfont-family = \"unfinished\ninvalid line\nunknown-setting=SECRET\nforeground=#abc\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.config.font_size, 13.0);
    assert_eq!(loaded.config.foreground, Rgb::new(170, 187, 204));
    assert_eq!(loaded.diagnostics.len(), 4);
    assert!(loaded.diagnostics.iter().any(|d| d.line == 1));
    assert!(!format!("{:?}", loaded.diagnostics).contains("SECRET"));
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let loaded = home.loader.load();
    assert_eq!(loaded.family, ConfigFamily::Rustty);
    assert_eq!(loaded.config.font_size, 13.0);
    assert_eq!(loaded.diagnostics.len(), 1);
}

#[test]
fn includes_are_relative_queued_after_roots_and_cli_and_cycles_are_reported() {
    let home = TestHome::new();
    let root = home.own("config-file=child\nfont-size=14\n");
    home.write(
        "Library/Application Support/com.rustty.app/child",
        "font-size=16\nconfig-file=grandchild\n",
    );
    home.write(
        "Library/Application Support/com.rustty.app/grandchild",
        "font-size=17\nconfig-file=./config.rustty\n",
    );
    let loaded = home.loader.load_with_args(&args(&["--font-size=15"]));
    assert_eq!(loaded.config.font_size, 17.0);
    assert_eq!(loaded.sources.len(), 3);
    assert_eq!(loaded.diagnostics.len(), 1);
    assert_eq!(loaded.diagnostics[0].path, root);
    assert!(loaded.diagnostics[0].message.contains("cycle"));
}

#[test]
fn include_order_is_breadth_first_and_empty_directive_clears_pending_includes() {
    let home = TestHome::new();
    home.own("config-file=a\nconfig-file=b\n");
    home.write(
        "Library/Application Support/com.rustty.app/a",
        "font-size=14\nconfig-file=c\n",
    );
    home.write(
        "Library/Application Support/com.rustty.app/b",
        "font-size=15\n",
    );
    home.write(
        "Library/Application Support/com.rustty.app/c",
        "font-size=16\n",
    );
    let loaded = home.loader.load();
    assert_eq!(loaded.config.font_size, 16.0);
    assert!(loaded.diagnostics.is_empty());
    home.own("config-file=missing\nconfig-file=\nfont-size=20\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.config.font_size, 20.0);
    assert!(loaded.diagnostics.is_empty());
}

#[test]
fn optional_tilde_and_quoted_question_mark_includes() {
    let home = TestHome::new();
    home.own("config-file=?missing\nconfig-file=~/shared\nconfig-file=\"?literal\"\n");
    home.write("shared", "font-size=20\n");
    home.write(
        "Library/Application Support/com.rustty.app/?literal",
        "font-size=21\n",
    );
    let loaded = home.loader.load();
    assert_eq!(loaded.config.font_size, 21.0);
    assert_eq!(loaded.sources.len(), 3);
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    home.own("config-file=required-missing\n");
    assert_eq!(home.loader.load().diagnostics.len(), 1);
}

#[test]
fn named_theme_is_lower_priority_than_every_explicit_setting() {
    let home = TestHome::new();
    home.own("foreground=#112233\ntheme=Example\nconfig-file=override\n");
    home.write(
        "Library/Application Support/com.rustty.app/override",
        "cursor-style=underline\n",
    );
    home.write(".config/rustty/themes/Example", "foreground=#abcdef\nbackground=#010203\ncursor-style=bar\nconfig-file=missing\ntheme=missing\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(loaded.config.foreground, Rgb::new(0x11, 0x22, 0x33));
    assert_eq!(loaded.config.background, Rgb::new(1, 2, 3));
    assert_eq!(loaded.config.cursor_style, CursorStyle::Underline);
    assert_eq!(loaded.config.theme.unwrap().dark, "Example");
}

#[test]
fn theme_uses_selected_family_then_bundle_and_switches_with_appearance() {
    let mut home = TestHome::new();
    home.local("theme=light:Day,dark:Night\n");
    home.write(".config/ghostty/themes/Day", "background=white\n");
    let resources = home.loader.resources_dir.as_ref().unwrap().clone();
    fs::create_dir_all(resources.join("themes")).unwrap();
    fs::write(resources.join("themes/Night"), "background=black\n").unwrap();
    let dark = home.loader.load();
    assert_eq!(dark.config.background, Rgb::new(0, 0, 0));
    assert_eq!(dark.config.window_theme, WindowTheme::System);
    home.loader.dark_mode = false;
    let light = home.loader.load();
    assert_eq!(light.config.background, Rgb::new(255, 255, 255));
    assert!(light.diagnostics.is_empty());
    home.write(".config/ghostty/themes/Night", "background=#010203\n");
    home.loader.dark_mode = true;
    assert_eq!(home.loader.load().config.background, Rgb::new(1, 2, 3));
}

#[test]
fn bad_theme_reports_diagnostic_but_keeps_explicit_settings() {
    let home = TestHome::new();
    home.own("theme=../traversal\nfont-size=18\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.config.font_size, 18.0);
    assert_eq!(loaded.diagnostics.len(), 1);
    assert_eq!(loaded.diagnostics[0].key.as_deref(), Some("theme"));
}

#[test]
fn cli_font_override_reset_defaults_flag_and_direct_initial_argv() {
    let home = TestHome::new();
    home.own("font-family=File font\nfont-size=99\nconfig-default-files=false\n");
    let loaded = home.loader.load_with_args(&args(&[
        "--font-family=CLI font",
        "--font-family=Fallback",
        "-e",
        "/bin/echo",
        "a b",
        "--font-size=3",
    ]));
    assert_eq!(loaded.config.font_family, ["CLI font", "Fallback"]);
    assert_eq!(loaded.config.font_size, 99.0);
    assert_eq!(
        loaded.config.initial_command,
        Some(Command::Direct(args(&[
            "/bin/echo",
            "a b",
            "--font-size=3"
        ])))
    );
    let loaded = home
        .loader
        .load_with_args(&args(&["--config-default-files=false", "--font-size=18"]));
    assert!(loaded.sources.is_empty());
    assert_eq!(loaded.config.font_size, 18.0);
    assert_eq!(loaded.family, ConfigFamily::Defaults);
    assert_eq!(
        home.loader.load_with_args(&args(&["-e"])).diagnostics.len(),
        1
    );
}

#[test]
fn scalar_empty_values_reset_defaults_and_repeated_lists_append_or_reset() {
    let home = TestHome::new();
    home.own("font-family=First\nfont-family=Second\nfont-family=\nfont-family=Third\nfont-size=20\nfont-size=\ncursor-color=#123456\ncursor-color=\ntitle-report=true\nkeybind=clear\nkeybind=\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty());
    assert_eq!(loaded.config.font_family, ["Third"]);
    assert_eq!(loaded.config.font_size, 13.0);
    assert_eq!(loaded.config.cursor_color, None);
    assert!(loaded.config.title_report);
    assert!(
        !home
            .loader
            .load_with_args(&args(&["--title-report="]))
            .config
            .title_report
    );
    assert_eq!(action(&loaded.config, "cmd+n"), Some(Action::NewWindow));
}

#[test]
fn bom_crlf_comments_quotes_and_hash_colors() {
    let home = TestHome::new();
    home.own("\u{feff}  # comment\r\n\r\n font-size = \"15\"\r\nbackground = #345\r\ncommand = printf \"quoted\"\r\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(loaded.config.font_size, 15.0);
    assert_eq!(loaded.config.background, Rgb::new(0x33, 0x44, 0x55));
    assert_eq!(
        loaded.config.command,
        Some(Command::Shell("printf \"quoted\"".into()))
    );
}

#[test]
fn quick_terminal_space_behavior_validates_and_resets() {
    let home = TestHome::new();
    assert_eq!(
        home.loader.load().config.quick_terminal_space_behavior,
        QuickTerminalSpaceBehavior::Move
    );
    home.own("quick-terminal-space-behavior = remain\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty());
    assert_eq!(
        loaded.config.quick_terminal_space_behavior,
        QuickTerminalSpaceBehavior::Remain
    );
    home.own("quick-terminal-space-behavior = remain\nquick-terminal-space-behavior =\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty());
    assert_eq!(
        loaded.config.quick_terminal_space_behavior,
        QuickTerminalSpaceBehavior::Move
    );
    home.own("quick-terminal-space-behavior = elsewhere\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.diagnostics.len(), 1);
    assert_eq!(
        loaded.config.quick_terminal_space_behavior,
        QuickTerminalSpaceBehavior::Move
    );
}

#[test]
fn clipboard_paste_protection_defaults_validate_and_reset() {
    let home = TestHome::new();
    let config = home.loader.load().config;
    assert!(config.clipboard_paste_protection && config.clipboard_paste_bracketed_safe);
    home.own("clipboard-paste-protection = false\nclipboard-paste-bracketed-safe = false\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty());
    assert!(!loaded.config.clipboard_paste_protection);
    assert!(!loaded.config.clipboard_paste_bracketed_safe);
    let overridden = home
        .loader
        .load_with_args(&args(&["--clipboard-paste-protection=true"]));
    assert!(overridden.diagnostics.is_empty());
    assert!(overridden.config.clipboard_paste_protection);
    assert!(!overridden.config.clipboard_paste_bracketed_safe);
    home.own("clipboard-paste-protection = false\nclipboard-paste-protection =\nclipboard-paste-bracketed-safe = false\nclipboard-paste-bracketed-safe =\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty());
    assert!(
        loaded.config.clipboard_paste_protection && loaded.config.clipboard_paste_bracketed_safe
    );
    home.own("clipboard-paste-protection = maybe\nclipboard-paste-bracketed-safe = never\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.diagnostics.len(), 2);
    assert!(
        loaded.config.clipboard_paste_protection && loaded.config.clipboard_paste_bracketed_safe
    );
}

#[test]
fn current_local_workflow_settings_and_actions_are_supported() {
    let home = TestHome::new();
    home.local(
        r#"
bell-features = no-system,no-audio,attention
progress-style = false
notify-on-command-finish-action = notify
window-save-state = always
copy-on-select = clipboard
unfocused-split-opacity = 0.85
quadrant-peek-opacity = 0.60
unfocused-split-fill = #101010
split-divider-color = #5a5a5a
keybind = global:ctrl+super+backquote=toggle_quick_terminal
quick-terminal-animation-duration = 0
macos-option-as-alt = true
cursor-color = cell-foreground
cursor-text = cell-background
keybind = alt+enter=text:\x1b\r
keybind = shift+enter=text:\x1b[13;2u
keybind = ctrl+tab=unbind
keybind = ctrl+shift+tab=unbind
keybind = super+j=unbind
keybind = super+k=unbind
keybind = super+h=goto_split:left
keybind = super+j=goto_split:down
keybind = super+k=goto_split:up
keybind = super+l=goto_split:right
keybind = super+shift+h=new_split:left
keybind = super+shift+j=new_split:down
keybind = super+shift+k=new_split:up
keybind = super+shift+l=new_split:right
keybind = super+ctrl+h=goto_split:quadrant_left
keybind = super+ctrl+j=goto_split:quadrant_down
keybind = super+ctrl+k=goto_split:quadrant_up
keybind = super+ctrl+l=goto_split:quadrant_right
keybind = cmd+shift+f=toggle_quadrant_zoom
"#,
    );
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    let config = loaded.config;
    assert_eq!(config.window_save_state, WindowSaveState::Always);
    assert_eq!(config.copy_on_select, CopyOnSelect::Clipboard);
    assert_eq!(config.macos_option_as_alt, OptionAsAlt::True);
    assert_eq!(config.cursor_color, Some(TerminalColor::CellForeground));
    assert_eq!(config.cursor_text, Some(TerminalColor::CellBackground));
    assert_eq!(config.quick_terminal_animation_duration, Duration::ZERO);
    assert_eq!(config.quadrant_peek_opacity, 0.6);
    assert!(config.bell_features.attention && config.notify_on_command_finish_action.notify);
    assert!(!config.bell_features.audio && !config.bell_features.system);
    assert!(!config.progress_style);
    assert_eq!(action(&config, "ctrl+tab"), None);
    assert_eq!(
        action(&config, "super+j"),
        Some(Action::GotoSplit(Direction::Down))
    );
    assert_eq!(
        action(&config, "cmd+ctrl+h"),
        Some(Action::GotoSplit(Direction::QuadrantLeft))
    );
    assert_eq!(
        action(&config, "cmd+shift+f"),
        Some(Action::ToggleQuadrantZoom)
    );
    assert_eq!(
        action(&config, "shift+enter"),
        Some(Action::Text(b"\x1b[13;2u".to_vec()))
    );
    let global = config
        .binding(&KeyTrigger::parse("ctrl+super+backquote").unwrap())
        .unwrap();
    assert!(global.flags.global && global.flags.all && global.flags.consumed);
}

#[test]
fn keybind_sequences_prefix_replacement_chains_and_tables() {
    let home = TestHome::new();
    home.own("keybind=clear\nkeybind=ctrl+a=new_tab\nkeybind=ctrl+a>n=new_window\nkeybind=ctrl+a>t=new_tab\nkeybind=chain=toggle_fullscreen\nkeybind=tools/ctrl+b=new_tab\nkeybind=tools/\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(loaded.config.keybinds.len(), 2);
    assert_eq!(
        loaded.config.keybinds[1].actions,
        [Action::NewTab, Action::ToggleFullscreen]
    );
    assert_eq!(action(&loaded.config, "ctrl+a"), None);
    home.own("keybind=clear\nkeybind=ctrl+a>n=new_window\nkeybind=ctrl+a>t=new_tab\nkeybind=ctrl+a=new_tab\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.config.keybinds.len(), 1);
    assert_eq!(action(&loaded.config, "ctrl+a"), Some(Action::NewTab));
}

#[test]
fn global_wildcards_are_rejected_without_replacing_local_bindings() {
    let home = TestHome::new();
    for trigger in ["catch_all", "ctrl+catch_all", "physical:ctrl+catch_all"] {
        home.own(&format!(
            "keybind=clear\nkeybind={trigger}=new_tab\nkeybind=global:{trigger}=quit\nkeybind=global:ctrl+a=new_window\n"
        ));
        let loaded = home.loader.load();
        assert_eq!(loaded.diagnostics.len(), 1, "{trigger}");
        assert!(loaded.diagnostics[0].message.contains("explicit key"));
        assert_eq!(loaded.config.keybinds.len(), 2);
        let local = loaded
            .config
            .binding(&KeyTrigger::parse(trigger).unwrap())
            .unwrap();
        assert!(!local.flags.global);
        assert_eq!(local.actions, [Action::NewTab]);
        let global = loaded
            .config
            .binding(&KeyTrigger::parse("ctrl+a").unwrap())
            .unwrap();
        assert!(global.flags.global);
        assert_eq!(global.actions, [Action::NewWindow]);
    }
}

#[test]
fn bad_keys_actions_and_values_are_diagnostics_not_silent_overrides() {
    let home = TestHome::new();
    home.own("keybind=cmd+n=not_an_action\nkeybind=ctrl+ctrl+a=new_tab\nkeybind=unknown_key=new_tab\nkeybind=global:ctrl+a>b=new_tab\nkeybind=ctrl+a=resize_split:next,10\nbackground-opacity=2\nfont-size=inf\nnotify-on-command-finish-after=5\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.diagnostics.len(), 8, "{:?}", loaded.diagnostics);
    assert_eq!(action(&loaded.config, "cmd+n"), Some(Action::NewWindow));
    assert_eq!(loaded.config.background_opacity, 1.0);
    assert_eq!(loaded.config.font_size, 13.0);
}

#[test]
fn chain_after_unbind_does_not_attach_to_an_unrelated_binding() {
    let home = TestHome::new();
    home.own("keybind=ctrl+a=new_tab\nkeybind=ctrl+b=unbind\nkeybind=chain=quit\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.diagnostics.len(), 1);
    let binding = loaded
        .config
        .binding(&KeyTrigger::parse("ctrl+a").unwrap())
        .unwrap();
    assert_eq!(binding.actions, [Action::NewTab]);
    assert!(KeyTrigger::parse("ctrl++a").is_err());
    assert!(KeyTrigger::parse("ctrl+++ ").is_err());
}

#[test]
fn text_bytes_unicode_colors_duration_and_environment() {
    assert_eq!(
        parse_escaped_bytes(r"\xe6\x97\xa5\u{1f600}\n").unwrap(),
        "日😀\n".as_bytes()
    );
    assert!(parse_escaped_bytes(r"\u{d800}").is_err());
    assert!(parse_escaped_bytes(r"\x0").is_err());
    assert_eq!(Rgb::parse("FoReStGReen"), Ok(Rgb::new(34, 139, 34)));
    assert_eq!(Rgb::parse("rgb:f/00/ffff"), Ok(Rgb::new(255, 0, 255)));
    assert_eq!(
        parse_duration("1m 30s 5ms"),
        Ok(Duration::from_millis(90_005))
    );
    let home = TestHome::new();
    home.own("env=ONE=first\nenv=TWO=a=b\nenv=ONE=last\nworking-directory=home\nscrollback-limit-lines=unlimited\nscrollback-limit-bytes=0\npalette=0xF=#abc\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty());
    assert_eq!(
        loaded.config.env.get("ONE").map(String::as_str),
        Some("last")
    );
    assert_eq!(
        loaded.config.env.get("TWO").map(String::as_str),
        Some("a=b")
    );
    assert_eq!(
        loaded.config.working_directory,
        Some(home.loader.home.clone())
    );
    assert_eq!(loaded.config.scrollback_limit_lines, None);
    assert_eq!(loaded.config.scrollback_limit_bytes, Some(0));
    assert_eq!(loaded.config.palette[15], Rgb::new(170, 187, 204));
}

#[cfg(unix)]
#[test]
fn symlink_cycles_and_invalid_utf8_are_reported_without_fallback() {
    let home = TestHome::new();
    let own = home.own("config-file=alias\n");
    std::os::unix::fs::symlink(&own, own.parent().unwrap().join("alias")).unwrap();
    let loaded = home.loader.load();
    assert_eq!(loaded.diagnostics.len(), 1);
    fs::write(&own, [0xff, 0xfe]).unwrap();
    home.local("font-size=99\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.family, ConfigFamily::Rustty);
    assert_eq!(loaded.config.font_size, 13.0);
    assert_eq!(loaded.diagnostics.len(), 1);
}

#[test]
fn command_exit_and_undo_settings_keep_ghostty_defaults_and_units() {
    let defaults = Config::default();
    assert!(!defaults.wait_after_command);
    assert_eq!(defaults.abnormal_command_exit_runtime, 250);
    assert_eq!(defaults.undo_timeout, Duration::from_secs(5));
    let home = TestHome::new();
    home.own("wait-after-command=true\nabnormal-command-exit-runtime=20\nundo-timeout=1m 5s\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert!(loaded.config.wait_after_command);
    assert_eq!(loaded.config.abnormal_command_exit_runtime, 20);
    assert_eq!(loaded.config.undo_timeout, Duration::from_secs(65));
    home.own("undo-timeout=1m\nundo-timeout=\n");
    let reset = home.loader.load();
    assert!(reset.diagnostics.is_empty());
    assert_eq!(reset.config.undo_timeout, Duration::from_secs(5));
    home.own("wait-after-command=perhaps\nabnormal-command-exit-runtime=-1\nundo-timeout=-2s\n");
    assert_eq!(home.loader.load().diagnostics.len(), 3);
}
