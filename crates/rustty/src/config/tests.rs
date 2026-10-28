use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[test]
fn command_finish_exclusions_match_programs_without_guessing_from_arguments() {
    let config = Config::default();
    for command in [
        "ssh host",
        "tix",
        "tix status",
        "mosh host",
        "tmux attach",
        "tmuxx",
        "screen",
        "/usr/bin/ssh host",
        "command ssh host",
        "exec ssh host",
        "env TERM=xterm ssh host",
        "TERM=xterm ssh host",
        "'/opt/my tools/ssh' host",
        "SSH.EXE host",
        "\"C:\\Windows\\ssh.exe\" host",
    ] {
        assert!(
            config.command_finish_excluded(Some(command.as_bytes())),
            "{command}"
        );
    }
    for command in [
        "",
        "cargo build",
        "echo ssh",
        "ssh-keygen",
        "my-ssh-wrapper",
        "$PROGRAM host",
        "'ssh",
        "echo; ssh host",
        "env -i ssh host",
    ] {
        assert!(
            !config.command_finish_excluded(Some(command.as_bytes())),
            "{command}"
        );
    }
    assert!(!config.command_finish_excluded(None));
    assert!(!config.command_finish_excluded(Some(b"\xff")));
}

#[test]
fn command_finish_exclusions_extend_defaults_and_can_be_replaced() {
    let home = TestHome::new();
    home.own("notify-on-command-finish-exclude=nvim\nnotify-on-command-finish-exclude=nvim\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert!(loaded.config.command_finish_excluded(Some(b"ssh host")));
    assert!(loaded.config.command_finish_excluded(Some(b"nvim file")));
    assert_eq!(
        loaded
            .config
            .notify_on_command_finish_exclude
            .iter()
            .filter(|name| *name == "nvim")
            .count(),
        1
    );

    home.own("notify-on-command-finish-exclude=\nnotify-on-command-finish-exclude=my-ssh-wrapper\nnotify-on-command-finish-exclude=bad name\nnotify-on-command-finish-exclude=/bin/ssh\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.diagnostics.len(), 2);
    assert_eq!(
        loaded.config.notify_on_command_finish_exclude,
        ["my-ssh-wrapper"]
    );
    assert!(!loaded.config.command_finish_excluded(Some(b"ssh host")));
    assert!(
        loaded
            .config
            .command_finish_excluded(Some(b"my-ssh-wrapper host"))
    );
}

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
            app_config_home: None,
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

#[test]
fn window_animations_default_to_platform_policy_and_support_overrides() {
    let home = TestHome::new();
    assert_eq!(home.loader.load().config.window_animations, None);
    home.own("window-animations=false\n");
    for (value, expected) in [("false", Some(false)), ("true", Some(true)), ("", None)] {
        let loaded = home
            .loader
            .load_with_args(&args(&[&format!("--window-animations={value}")]));
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert_eq!(loaded.config.window_animations, expected);
    }
    let invalid = home
        .loader
        .load_with_args(&args(&["--window-animations=invalid"]));
    assert_eq!(invalid.diagnostics.len(), 1);
    assert_eq!(invalid.config.window_animations, Some(false));
}

#[test]
fn animation_durations_support_independent_overrides_zero_and_reset() {
    let home = TestHome::new();
    let durations = |config: &Config| {
        [
            config.split_animation_duration,
            config.quad_zoom_animation_duration,
            config.quad_peek_animation_duration,
            config.zoom_animation_duration,
            config.zoom_from_quad_animation_duration,
        ]
    };
    let keys = [
        "split-animation-duration",
        "quad-zoom-animation-duration",
        "quad-peek-animation-duration",
        "zoom-animation-duration",
        "zoom-from-quad-animation-duration",
    ];
    let defaults = [100, 250, 100, 250, 250].map(Duration::from_millis);
    assert_eq!(durations(&home.loader.load().config), defaults);
    home.own(
        "split-animation-duration=75ms\n\
         quad-zoom-animation-duration=125ms\n\
         quad-peek-animation-duration=50ms\n\
         zoom-animation-duration=200ms\n\
         zoom-from-quad-animation-duration=1s 25ms\n",
    );
    let overrides = [75, 125, 50, 200, 1_025].map(Duration::from_millis);
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(durations(&loaded.config), overrides);
    for (value, expected, diagnostic_count) in [
        ("0", [Duration::ZERO; 5], 0),
        ("", defaults, 0),
        ("250", overrides, 5),
        ("-1ms", overrides, 5),
        ("1.5ms", overrides, 5),
    ] {
        let args = keys.map(|key| format!("--{key}={value}"));
        let loaded = home.loader.load_with_args(&args);
        assert_eq!(loaded.diagnostics.len(), diagnostic_count, "{value}");
        assert_eq!(durations(&loaded.config), expected, "{value}");
    }
}

#[test]
fn zoom_animation_background_mix_defaults_validates_and_resets() {
    let home = TestHome::new();
    assert_eq!(home.loader.load().config.zoom_animation_background_mix, 0.5);
    home.own("zoom-animation-background-mix=25%\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(loaded.config.zoom_animation_background_mix, 0.25);
    for (value, expected) in [
        ("0", 0.0),
        ("0%", 0.0),
        ("50", 0.5),
        ("50%", 0.5),
        ("100", 1.0),
        ("100%", 1.0),
        ("37.5%", 0.375),
        ("", 0.5),
    ] {
        let loaded = home.loader.load_with_args(&args(&[&format!(
            "--zoom-animation-background-mix={value}"
        )]));
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert_eq!(
            loaded.config.zoom_animation_background_mix, expected,
            "{value}"
        );
    }
    for value in ["-1%", "101", "100.1%", "NaN", "inf%", "-inf", "50%%", "%"] {
        let loaded = home.loader.load_with_args(&args(&[&format!(
            "--zoom-animation-background-mix={value}"
        )]));
        assert_eq!(loaded.diagnostics.len(), 1, "{value}");
        assert_eq!(loaded.config.zoom_animation_background_mix, 0.25, "{value}");
    }
}

#[test]
fn tab_frame_color_mix_defaults_validates_and_resets() {
    let home = TestHome::new();
    assert_eq!(home.loader.load().config.tab_frame_color_mix, 0.09);
    home.own("tab-frame-color-mix=25%\n");
    for (value, expected) in [
        ("0%", 0.0),
        ("18", 0.18),
        ("37.5%", 0.375),
        ("100%", 1.0),
        ("", 0.09),
    ] {
        let loaded = home
            .loader
            .load_with_args(&args(&[&format!("--tab-frame-color-mix={value}")]));
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert_eq!(loaded.config.tab_frame_color_mix, expected, "{value}");
    }
    for value in ["-1%", "101", "100.1%", "NaN", "inf%", "-inf", "50%%", "%"] {
        let loaded = home
            .loader
            .load_with_args(&args(&[&format!("--tab-frame-color-mix={value}")]));
        assert_eq!(loaded.diagnostics.len(), 1, "{value}");
        assert_eq!(loaded.config.tab_frame_color_mix, 0.25, "{value}");
    }
}

#[test]
fn tmuxx_config_file_resolves_paths_without_requiring_the_file() {
    let home = TestHome::new();
    assert_eq!(home.loader.load().config.tmuxx_config_file, None);
    let absolute = home.root.join("tmuxx config.conf");
    home.own(&format!("tmuxx-config-file=\"{}\"\n", absolute.display()));
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(loaded.config.tmuxx_config_file, Some(absolute));

    let config = home.own("tmuxx-config-file=\"server settings/tmuxx.conf\"\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(
        loaded.config.tmuxx_config_file,
        Some(config.parent().unwrap().join("server settings/tmuxx.conf"))
    );

    home.own("tmuxx-config-file=~/tmuxx config.conf\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(
        loaded.config.tmuxx_config_file,
        Some(home.loader.home.join("tmuxx config.conf"))
    );
    let loaded = home
        .loader
        .load_with_args(&args(&["--tmuxx-config-file=cli settings/tmuxx.conf"]));
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(
        loaded.config.tmuxx_config_file,
        Some(
            home.loader
                .working_directory
                .join("cli settings/tmuxx.conf")
        )
    );
}

#[test]
fn tmuxx_config_file_uses_the_declaring_include_directory() {
    let home = TestHome::new();
    home.own("config-file=~/shared/config.rustty\n");
    let included = home.write(
        "shared/config.rustty",
        "tmuxx-config-file=server/tmuxx.conf\n",
    );
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(
        loaded.config.tmuxx_config_file,
        Some(included.parent().unwrap().join("server/tmuxx.conf"))
    );
}

#[test]
fn tmuxx_config_file_resets_and_rejects_nul_without_replacing_the_previous_path() {
    let home = TestHome::new();
    let config = home.own("tmuxx-config-file=tmuxx.conf\ntmuxx-config-file=bad\0path\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.diagnostics.len(), 1);
    assert_eq!(
        loaded.config.tmuxx_config_file,
        Some(config.parent().unwrap().join("tmuxx.conf"))
    );
    home.own("tmuxx-config-file=tmuxx.conf\n");
    let loaded = home.loader.load_with_args(&args(&["--tmuxx-config-file="]));
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(loaded.config.tmuxx_config_file, None);
}

#[test]
fn native_ssh_command_preserves_shell_quoting_and_resets_to_rustty_helper() {
    let home = TestHome::new();
    let default = "rustty tmux --native-stdio";
    assert_eq!(home.loader.load().config.tmuxx_ssh_command, default);
    let command =
        "'/opt/Rustty App/rustty' tmux --native-endpoint /tmp/custom.native --native-stdio";
    home.own(&format!("tmuxx-ssh-command={command}\n"));
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(loaded.config.tmuxx_ssh_command, command);
    assert_eq!(
        home.loader
            .load_with_args(&args(&["--tmuxx-ssh-command="]))
            .config
            .tmuxx_ssh_command,
        default
    );
    for invalid in ["\" \"", "bad\0command"] {
        home.own(&format!(
            "tmuxx-ssh-command={command}\ntmuxx-ssh-command={invalid}\n"
        ));
        let loaded = home.loader.load();
        assert_eq!(loaded.diagnostics.len(), 1);
        assert_eq!(loaded.config.tmuxx_ssh_command, command);
    }
}

#[test]
fn native_ssh_host_commands_select_exact_targets_and_preserve_fallback() {
    let home = TestHome::new();
    let linux = "/home/byron/.cargo/bin/tmuxx -L rustty --native-stdio";
    let windows = "rustty.exe tmux --native-stdio";
    let quoted = "TMUXX_ENDPOINT=/tmp/other.native '/opt/Rustty App/rustty' tmux --native-stdio";
    home.own(&format!(
        "tmuxx-ssh-command-host=linux=old-helper\n\
         tmuxx-ssh-command-host=windows={windows}\n\
         tmuxx-ssh-command-host=linux = {linux}\n\
         tmuxx-ssh-command-host=user@linux={quoted}\n\
         tmuxx-ssh-command=global-helper\n"
    ));
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(loaded.config.tmuxx_ssh_command_for("linux"), linux);
    assert_eq!(loaded.config.tmuxx_ssh_command_for("windows"), windows);
    assert_eq!(loaded.config.tmuxx_ssh_command_for("user@linux"), quoted);
    for host in ["other", "Linux", "other@linux"] {
        assert_eq!(loaded.config.tmuxx_ssh_command_for(host), "global-helper");
    }
    let loaded = home.loader.load_with_args(&args(&[
        "--tmuxx-ssh-command=cli-fallback",
        "--tmuxx-ssh-command-host=linux=cli-helper --native-stdio",
    ]));
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(
        loaded.config.tmuxx_ssh_command_for("linux"),
        "cli-helper --native-stdio"
    );
    assert_eq!(loaded.config.tmuxx_ssh_command_for("windows"), windows);
    assert_eq!(loaded.config.tmuxx_ssh_command_for("other"), "cli-fallback");
    let loaded = home
        .loader
        .load_with_args(&args(&["--tmuxx-ssh-command-host="]));
    assert!(loaded.config.tmuxx_ssh_command_hosts.is_empty());
    assert_eq!(
        loaded.config.tmuxx_ssh_command_for("linux"),
        "global-helper"
    );
}

#[test]
fn invalid_native_ssh_host_commands_preserve_previous_entries() {
    let home = TestHome::new();
    for invalid in [
        "missing-command",
        "=helper",
        "linux=",
        "two hosts=helper",
        "host\0=helper",
        "linux=bad\0command",
    ] {
        home.own(&format!(
            "tmuxx-ssh-command-host=linux=working-helper\n\
             tmuxx-ssh-command-host={invalid}\n"
        ));
        let loaded = home.loader.load();
        assert_eq!(loaded.diagnostics.len(), 1);
        assert_eq!(loaded.config.tmuxx_ssh_command_hosts.len(), 1);
        assert_eq!(
            loaded.config.tmuxx_ssh_command_for("linux"),
            "working-helper"
        );
    }
}

#[test]
fn named_tmuxx_connections_replace_clear_and_keep_ssh_aliases() {
    let home = TestHome::new();
    assert!(home.loader.load().config.tmuxx_connections.is_empty());
    home.own(
        "tmuxx-connection = Azure Linux = ssh:old-host\n\
         tmuxx-connection = Build server = system-ssh:byron@build\n\
         tmuxx-connection = Azure Linux = ssh:20.240.208.217\n",
    );
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(loaded.config.tmuxx_connections.len(), 2);
    assert_eq!(
        loaded.config.tmuxx_connections["Azure Linux"],
        "ssh:20.240.208.217"
    );
    assert_eq!(
        loaded.config.tmuxx_connections["Build server"],
        "system-ssh:byron@build"
    );
    let loaded = home
        .loader
        .load_with_args(&args(&["--tmuxx-connection=Azure Linux=ssh:azure"]));
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(loaded.config.tmuxx_connections["Azure Linux"], "ssh:azure");
    assert_eq!(loaded.config.tmuxx_connections.len(), 2);
    let loaded = home.loader.load_with_args(&args(&["--tmuxx-connection="]));
    assert!(loaded.config.tmuxx_connections.is_empty());
}

#[test]
fn named_local_tmuxx_connections_replace_clear_and_accept_spaces_and_unicode() {
    let home = TestHome::new();
    home.own(
        "tmuxx-connection = Work = local:old\n\
         tmuxx-connection = Remote = ssh:azure\n\
         tmuxx-connection = Work = local:work\n",
    );
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(loaded.config.tmuxx_connections["Work"], "local:work");
    assert_eq!(loaded.config.tmuxx_connections["Remote"], "ssh:azure");
    assert_eq!(loaded.config.tmuxx_connections.len(), 2);

    for instance in ["build work".to_owned(), "\u{e9}".repeat(32)] {
        let loaded = home.loader.load_with_args(&args(&[&format!(
            "--tmuxx-connection=Work=local:{instance}"
        )]));
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert_eq!(
            loaded.config.tmuxx_connections["Work"],
            format!("local:{instance}")
        );
        assert_eq!(loaded.config.tmuxx_connections["Remote"], "ssh:azure");
        assert_eq!(loaded.config.tmuxx_connections.len(), 2);
    }

    let loaded = home.loader.load_with_args(&args(&["--tmuxx-connection="]));
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert!(loaded.config.tmuxx_connections.is_empty());
    home.own("tmuxx-connection=Work=local:work\ntmuxx-connection=\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert!(loaded.config.tmuxx_connections.is_empty());
}

#[test]
fn invalid_named_local_tmuxx_connections_preserve_previous_entries() {
    let home = TestHome::new();
    let too_long_ascii = "a".repeat(65);
    let too_long_unicode = "\u{e9}".repeat(33);
    for instance in [
        "",
        ".",
        "..",
        "../work",
        "work/child",
        r"work\child",
        "wo\0rk",
        "wo\trk",
        "wo\u{7f}rk",
        &too_long_ascii,
        &too_long_unicode,
    ] {
        home.own(&format!(
            "tmuxx-connection=Work=local:work\n\
             tmuxx-connection=Remote=ssh:azure\n\
             tmuxx-connection=Work=local:{instance}\n"
        ));
        let loaded = home.loader.load();
        assert_eq!(loaded.diagnostics.len(), 1, "{instance:?}");
        assert_eq!(loaded.config.tmuxx_connections.len(), 2);
        assert_eq!(loaded.config.tmuxx_connections["Work"], "local:work");
        assert_eq!(loaded.config.tmuxx_connections["Remote"], "ssh:azure");
    }
}

#[test]
fn invalid_named_tmuxx_connections_preserve_previous_entries() {
    let home = TestHome::new();
    for invalid in [
        "missing-target",
        "=ssh:host",
        "Azure Linux=",
        "Azure Linux=ssh:",
        "Azure Linux=system-ssh:",
        "Azure Linux=local",
        "Azure Linux=ssh:two hosts",
        "Azure Linux=ssh:host\0",
        "Azure Linux=ssh:host\u{7f}",
        "bad\tname=ssh:host",
    ] {
        home.own(&format!(
            "tmuxx-connection=Azure Linux=ssh:azure\n\
             tmuxx-connection={invalid}\n"
        ));
        let loaded = home.loader.load();
        assert_eq!(loaded.diagnostics.len(), 1, "{invalid:?}");
        assert_eq!(loaded.config.tmuxx_connections.len(), 1);
        assert_eq!(loaded.config.tmuxx_connections["Azure Linux"], "ssh:azure");
    }
}

#[test]
fn tmuxx_themes_replace_clear_and_accept_cli_overrides() {
    let home = TestHome::new();
    for option in ["tmuxx-theme-host", "tmuxx-theme-session"] {
        let themes = |config: Config| {
            if option == "tmuxx-theme-host" {
                config.tmuxx_theme_hosts
            } else {
                config.tmuxx_theme_sessions
            }
        };
        assert!(themes(Config::default()).is_empty());
        home.own(&format!(
            "{option} = windows = Old\n\
             {option} = linux = Other\n\
             {option} = windows = New Theme\n"
        ));
        let loaded = home.loader.load();
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert_eq!(
            themes(loaded.config),
            BTreeMap::from([
                ("linux".into(), "Other".into()),
                ("windows".into(), "New Theme".into()),
            ])
        );

        let loaded = home
            .loader
            .load_with_args(&[format!("--{option}=windows=CLI Theme")]);
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert_eq!(themes(loaded.config)["windows"], "CLI Theme");
        let loaded = home.loader.load_with_args(&[format!("--{option}=")]);
        assert!(themes(loaded.config).is_empty());
        home.own(&format!("{option}=windows=Old\n{option}=\n"));
        assert!(themes(home.loader.load().config).is_empty());
    }
}

#[test]
fn invalid_tmuxx_themes_preserve_previous_entries() {
    let home = TestHome::new();
    for option in ["tmuxx-theme-host", "tmuxx-theme-session"] {
        for invalid in [
            "missing-theme",
            "=Theme",
            "work=",
            "wo\0rk=Theme",
            "wo\u{7f}rk=Theme",
            "wo\trk=Theme",
            "work=bad\0theme",
            "work=bad\u{7f}theme",
            "work=bad\ttheme",
            "work=light:Day",
            "work=light:Day,dark:",
            "work=light:Day,light:Other,dark:Night",
            "work=dark:Night,unknown:Day",
        ] {
            home.own(&format!("{option}=work=Original\n{option}={invalid}\n"));
            let loaded = home.loader.load();
            assert_eq!(loaded.diagnostics.len(), 1, "{option}: {invalid:?}");
            let themes = if option == "tmuxx-theme-host" {
                loaded.config.tmuxx_theme_hosts
            } else {
                loaded.config.tmuxx_theme_sessions
            };
            assert_eq!(themes, BTreeMap::from([("work".into(), "Original".into())]));
        }
    }
    home.own("tmuxx-theme-host=two hosts=Theme\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.diagnostics.len(), 1);
    assert!(loaded.config.tmuxx_theme_hosts.is_empty());
}

#[test]
fn tmuxx_themes_match_exactly_with_session_precedence_and_light_dark_variants() {
    let mut home = TestHome::new();
    home.own(
        "tmuxx-theme-host = byron@windows = Host Theme\n\
         tmuxx-theme-session = build work = light:Day,dark:Night\n\
         tmuxx-theme-session = build* = Literal\n",
    );
    home.write(".config/rustty/themes/Day", "background=white\n");
    home.write(".config/rustty/themes/Night", "background=black\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    let config = loaded.config;
    assert_eq!(config.tmuxx_theme_for(None, "other"), None);
    assert_eq!(config.tmuxx_theme_for(Some("windows"), "other"), None);
    assert_eq!(config.tmuxx_theme_for(Some("byron@Windows"), "other"), None);
    assert_eq!(config.tmuxx_theme_for(None, "build123"), None);
    assert_eq!(config.tmuxx_theme_for(None, "build*"), Some("Literal"));
    assert_eq!(
        config.tmuxx_theme_for(Some("byron@windows"), "other"),
        Some("Host Theme")
    );
    let theme = config.tmuxx_theme_for(Some("byron@windows"), "build work");
    assert_eq!(theme, Some("light:Day,dark:Night"));
    assert_eq!(config.tmuxx_theme_for(None, "build work"), theme);
    for (dark, background) in [(false, Rgb::new(255, 255, 255)), (true, Rgb::new(0, 0, 0))] {
        home.loader.dark_mode = dark;
        let themed = home.loader.load_with_theme(&[], theme.unwrap());
        assert!(themed.diagnostics.is_empty(), "{:?}", themed.diagnostics);
        assert_eq!(themed.config.background, background);
        assert_eq!(themed.config.window_theme, WindowTheme::System);
    }
}

#[test]
fn windows_roaming_config_root_is_explicit_and_does_not_read_terminal_settings() {
    let mut home = TestHome::new();
    let roaming = home.root.join("roaming");
    fs::create_dir_all(roaming.join("Rustty/themes")).unwrap();
    fs::write(
        roaming.join("Rustty/rustty.txt"),
        "font-size=17\ntheme=Native\n",
    )
    .unwrap();
    fs::write(roaming.join("Rustty/themes/Native"), "background=#123456\n").unwrap();
    home.loader.app_config_home = Some(roaming.clone());
    let loaded = home.loader.load();
    assert_eq!(loaded.family, ConfigFamily::Rustty);
    assert_eq!(loaded.own_config_path, roaming.join("Rustty/rustty.txt"));
    assert_eq!(loaded.config.font_size, 17.0);
    assert_eq!(loaded.config.background, Rgb::new(0x12, 0x34, 0x56));
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
fn renderer_selection_defaults_to_auto_and_validates_file_and_cli_values() {
    let home = TestHome::new();
    assert_eq!(home.loader.load().config.renderer, Renderer::Auto);
    for (value, expected) in [
        ("auto", Renderer::Auto),
        ("gpu", Renderer::Gpu),
        ("software", Renderer::Software),
        ("", Renderer::Auto),
    ] {
        home.own(&format!("renderer=gpu\nrenderer={value}\n"));
        let loaded = home.loader.load();
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert_eq!(loaded.config.renderer, expected);
    }

    home.own("renderer=software\n");
    let loaded = home.loader.load_with_args(&args(&["--renderer=gpu"]));
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(loaded.config.renderer, Renderer::Gpu);

    let loaded = home.loader.load_with_args(&args(&["--renderer=invalid"]));
    assert_eq!(loaded.diagnostics.len(), 1);
    assert_eq!(loaded.config.renderer, Renderer::Software);

    home.own("renderer=invalid\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.diagnostics.len(), 1);
    assert_eq!(loaded.config.renderer, Renderer::Auto);
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
fn stream_deck_is_opt_in_and_reloads_without_changing_save_policy() {
    let home = TestHome::new();
    assert!(!home.loader.load().config.stream_deck);
    home.local("stream-deck=true\nstream-deck-serial=test-device\nwindow-save-state=never\n");
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty());
    assert!(loaded.config.stream_deck);
    assert_eq!(
        loaded.config.stream_deck_serial.as_deref(),
        Some("test-device")
    );
    assert_eq!(loaded.config.window_save_state, WindowSaveState::Never);
    home.local("stream-deck=false\nstream-deck-serial=\n");
    let loaded = home.loader.load();
    assert!(!loaded.config.stream_deck);
    assert!(loaded.config.stream_deck_serial.is_none());
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
fn ghostty_local_fallback_accepts_rustty_tmuxx_settings_directly() {
    let home = TestHome::new();
    let command = "/home/byron/.cargo/bin/tmuxx -L rustty --native-stdio";
    let path = home.local(&format!(
        "tmuxx-connection=Azure Linux=ssh:20.240.208.217\n\
         tmuxx-ssh-command=global-helper --native-stdio\n\
         tmuxx-ssh-command-host=20.240.208.217={command}\n"
    ));
    let loaded = home.loader.load();
    assert_eq!(loaded.family, ConfigFamily::GhosttyLocal);
    assert_eq!(loaded.edit_config_path, path);
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(
        loaded.config.tmuxx_connections["Azure Linux"],
        "ssh:20.240.208.217"
    );
    assert_eq!(
        loaded.config.tmuxx_ssh_command_for("20.240.208.217"),
        command
    );
    assert_eq!(
        loaded.config.tmuxx_ssh_command_for("other"),
        "global-helper --native-stdio"
    );
    assert!(!loaded.own_config_path.exists());

    home.own("");
    let loaded = home.loader.load();
    assert_eq!(loaded.family, ConfigFamily::Rustty);
    assert!(loaded.config.tmuxx_connections.is_empty());
    assert!(loaded.config.tmuxx_ssh_command_hosts.is_empty());
    assert_eq!(
        loaded.config.tmuxx_ssh_command,
        Config::default().tmuxx_ssh_command
    );
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
    assert_eq!(loaded.config.font_size, Config::default().font_size);
    assert!(loaded.diagnostics.is_empty());
    home.own("font-size = NaN\nfont-family = \"unfinished\ninvalid line\nunknown-setting=SECRET\nforeground=#abc\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.config.font_size, Config::default().font_size);
    assert_eq!(loaded.config.foreground, Rgb::new(170, 187, 204));
    assert_eq!(loaded.diagnostics.len(), 4);
    assert!(loaded.diagnostics.iter().any(|d| d.line == 1));
    assert!(!format!("{:?}", loaded.diagnostics).contains("SECRET"));
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let loaded = home.loader.load();
    assert_eq!(loaded.family, ConfigFamily::Rustty);
    assert_eq!(loaded.config.font_size, Config::default().font_size);
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
    home.own("config-file=?missing\nconfig-file=~/shared\n");
    home.write("shared", "font-size=20\n");
    let loaded = home.loader.load();
    assert_eq!(loaded.config.font_size, 20.0);
    assert_eq!(loaded.sources.len(), 2);
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    home.own("config-file=\"?literal\"\n");
    #[cfg(not(windows))]
    {
        home.write(
            "Library/Application Support/com.rustty.app/?literal",
            "font-size=21\n",
        );
        let loaded = home.loader.load();
        assert_eq!(loaded.config.font_size, 21.0);
        assert!(loaded.diagnostics.is_empty());
    }
    // Windows cannot create a '?' filename, but quoting must still make the
    // include required rather than suppress its invalid-filename diagnostic.
    #[cfg(windows)]
    assert_eq!(home.loader.load().diagnostics.len(), 1);
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
fn theme_catalog_lists_user_first_and_resolves_duplicate_names_by_search_order() {
    let mut home = TestHome::new();
    let native = home.root.join("roaming");
    home.loader.app_config_home = Some(native.clone());
    let resources = home.loader.resources_dir.as_ref().unwrap();
    fs::create_dir_all(resources.join("themes")).unwrap();
    for name in ["Bundled", "Shared", "zulu"] {
        fs::write(resources.join("themes").join(name), "background=#010203\n").unwrap();
    }
    let shared = home.write(".config/rustty/themes/Shared", "background=#040506\n");
    home.write(".config/rustty/themes/Zebra", "background=#040506\n");
    home.write(".config/ghostty/themes/Ghostty", "background=#070809\n");
    fs::create_dir_all(native.join("Rustty/themes")).unwrap();
    for name in ["alpha", "Shared"] {
        fs::write(
            native.join("Rustty/themes").join(name),
            "background=#ffffff\n",
        )
        .unwrap();
    }
    fs::create_dir_all(home.loader.xdg_config_home.join("rustty/themes/folder")).unwrap();
    #[cfg(unix)]
    home.write(".config/rustty/themes/bad\nname", "background=red\n");
    let choices = home.loader.themes(ConfigFamily::Rustty).unwrap();
    assert_eq!(
        choices
            .iter()
            .map(|choice| (choice.name.as_str(), choice.user))
            .collect::<Vec<_>>(),
        [
            ("alpha", true),
            ("Shared", true),
            ("Zebra", true),
            ("Bundled", false),
            ("zulu", false)
        ],
    );
    assert!(choices.iter().all(|choice| choice.path.is_absolute()));
    assert_eq!(choices[1].path, fs::canonicalize(shared).unwrap());
    assert_eq!(choices[3].config_value(), "Bundled");
    let ghostty = home.loader.themes(ConfigFamily::GhosttyLocal).unwrap();
    assert_eq!(ghostty[0].name, "Ghostty");
    assert!(ghostty[0].user);
    assert!(!ghostty.iter().any(|choice| choice.name == "Zebra"));
}

#[test]
fn theme_catalog_missing_directories_are_read_only() {
    let home = TestHome::new();
    assert!(home.loader.themes(ConfigFamily::Rustty).unwrap().is_empty());
    assert!(!home.loader.home.exists());
    assert!(!home.loader.resources_dir.as_ref().unwrap().exists());
    let loaded = home.loader.load_with_theme(&[], "missing");
    assert_eq!(loaded.diagnostics.len(), 1);
    assert_eq!(loaded.diagnostics[0].key.as_deref(), Some("theme"));
    assert!(!home.loader.home.exists());
}

#[test]
fn theme_catalog_follows_filesystem_case_and_nonfile_shadowing() {
    let home = TestHome::new();
    home.own("");
    let user = home.write(".config/rustty/themes/CaseTheme", "background=red\n");
    let user_dir = user.parent().unwrap();
    fs::create_dir(user_dir.join("Blocked")).unwrap();
    let bundled = home.loader.resources_dir.as_ref().unwrap().join("themes");
    fs::create_dir_all(&bundled).unwrap();
    for name in ["casetheme", "blocked"] {
        fs::write(bundled.join(name), "background=blue\n").unwrap();
    }
    let choices = home.loader.themes(ConfigFamily::Rustty).unwrap();
    let case_insensitive = user_dir.join("casetheme").exists();
    assert_eq!(choices.len(), if case_insensitive { 1 } else { 3 });
    for choice in choices {
        home.own(&format!("theme={}\n", choice.config_value()));
        let saved = home.loader.load();
        let preview = home
            .loader
            .load_with_theme(&[], choice.path.to_str().unwrap());
        assert!(saved.diagnostics.is_empty(), "{:?}", saved.diagnostics);
        assert!(preview.diagnostics.is_empty());
        assert_eq!(saved.config.background, preview.config.background);
    }
}

#[cfg(unix)]
#[test]
fn theme_catalog_rejects_unrepresentable_canonical_paths() {
    use std::os::unix::fs::symlink;

    let home = TestHome::new();
    let safe = home.write(".config/rustty/themes/Safe", "background=blue\n");
    let themes = safe.parent().unwrap();
    let target = home.write("target\ncommand=exit\n/theme", "background=red\n");
    symlink(target, themes.join("light:literal")).unwrap();
    #[cfg(not(target_os = "macos"))]
    {
        use std::os::unix::ffi::OsStringExt;
        let invalid = std::ffi::OsString::from_vec(b"invalid\xff".to_vec());
        let target = home.write(PathBuf::from(invalid).join("theme"), "background=red\n");
        symlink(target, themes.join("InvalidPath")).unwrap();
    }
    let choices = home.loader.themes(ConfigFamily::Rustty).unwrap();
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].name, "Safe");
}

#[test]
fn user_theme_selection_survives_creating_the_first_rustty_config() {
    let home = TestHome::new();
    home.write(".config/ghostty/themes/UserTheme", "background=blue\n");
    let initial = home.loader.load();
    assert_eq!(initial.family, ConfigFamily::Defaults);
    let choices = home.loader.themes(initial.family).unwrap();
    let choice = &choices[0];
    assert!(choice.user);
    assert_eq!(choice.config_value(), choice.path.to_str().unwrap());
    fs::create_dir_all(initial.edit_config_path.parent().unwrap()).unwrap();
    fs::write(
        initial.edit_config_path,
        format!("theme={}\n", choice.config_value()),
    )
    .unwrap();
    let saved = home.loader.load();
    assert_eq!(saved.family, ConfigFamily::Rustty);
    assert!(saved.diagnostics.is_empty(), "{:?}", saved.diagnostics);
    assert_eq!(saved.config.background, Rgb::new(0, 0, 255));
}

#[test]
fn theme_preview_overrides_includes_and_cli_but_keeps_explicit_settings() {
    let home = TestHome::new();
    let root_text = "theme=Root\nforeground=#112233\nconfig-file=override\n";
    let root = home.own(root_text);
    let include_text = "theme=Included\ncursor-style=underline\n";
    let include = home.write(
        "Library/Application Support/com.rustty.app/override",
        include_text,
    );
    for (name, background) in [
        ("Root", "#010101"),
        ("CLI", "#020202"),
        ("Included", "#030303"),
        ("Preview", "#040506"),
    ] {
        home.write(
            format!(".config/rustty/themes/{name}"),
            &format!(
                "background={background}\nforeground=#ffffff\nfont-size=99\ncursor-style=bar\n"
            ),
        );
    }
    let arguments = args(&["--theme=CLI", "--font-size=18"]);
    let original = home.loader.load_with_args(&arguments);
    assert_eq!(original.config.background, Rgb::new(3, 3, 3));
    let loaded = home.loader.load_with_theme(&arguments, "Preview");
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(loaded.config.theme.unwrap().dark, "Preview");
    assert_eq!(loaded.config.background, Rgb::new(4, 5, 6));
    assert_eq!(loaded.config.foreground, Rgb::new(0x11, 0x22, 0x33));
    assert_eq!(loaded.config.font_size, 18.0);
    assert_eq!(loaded.config.cursor_style, CursorStyle::Underline);
    assert_eq!(fs::read_to_string(root).unwrap(), root_text);
    assert_eq!(fs::read_to_string(include).unwrap(), include_text);
    assert_eq!(
        home.loader.load_with_args(&arguments).config,
        original.config
    );
}

#[test]
fn theme_preview_supports_dual_variants_reset_and_diagnostics() {
    let mut home = TestHome::new();
    home.own("theme=Day\nfont-size=18\n");
    home.write(".config/rustty/themes/Day", "background=white\n");
    home.write(".config/rustty/themes/Night", "background=black\n");
    for (dark, background) in [(false, Rgb::new(255, 255, 255)), (true, Rgb::new(0, 0, 0))] {
        home.loader.dark_mode = dark;
        let loaded = home.loader.load_with_theme(&[], "light:Day,dark:Night");
        assert!(loaded.diagnostics.is_empty());
        assert_eq!(loaded.config.background, background);
        assert_eq!(loaded.config.window_theme, WindowTheme::System);
    }
    let reset = home.loader.load_with_theme(&[], "");
    assert!(reset.config.theme.is_none());
    assert_eq!(reset.config.background, Config::default().background);
    for invalid in ["light:Day", "../bad", "missing"] {
        let loaded = home.loader.load_with_theme(&[], invalid);
        assert_eq!(loaded.config.font_size, 18.0);
        assert_eq!(
            loaded.diagnostics.len(),
            1,
            "{invalid}: {:?}",
            loaded.diagnostics
        );
        assert_eq!(loaded.diagnostics[0].key.as_deref(), Some("theme"));
        assert!(loaded.diagnostics[0].path.ends_with("<theme-preview>"));
    }
}

#[cfg(unix)]
#[test]
fn theme_catalog_and_config_values_handle_literal_filenames() {
    let home = TestHome::new();
    home.own("");
    for name in [
        "Normal Theme",
        "light:literal",
        "dark:literal",
        " spaced ",
        "\"quoted\"",
        "back\\slash",
    ] {
        home.write(
            Path::new(".config/rustty/themes").join(name),
            "background=#123456\n",
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        use std::os::unix::ffi::OsStringExt;
        let invalid = std::ffi::OsString::from_vec(b"invalid\xff".to_vec());
        fs::write(
            home.loader
                .xdg_config_home
                .join("rustty/themes")
                .join(invalid),
            "background=red\n",
        )
        .unwrap();
    }
    let choices = home.loader.themes(ConfigFamily::Rustty).unwrap();
    assert_eq!(choices.len(), 6);
    for choice in choices {
        let value = choice.config_value();
        assert_eq!(unquote(&value), choice.path.to_str().unwrap());
        home.own(&format!("theme={value}\n"));
        let loaded = home.loader.load();
        assert!(
            loaded.diagnostics.is_empty(),
            "{}: {:?}",
            choice.name,
            loaded.diagnostics
        );
        assert_eq!(loaded.config.background, Rgb::new(0x12, 0x34, 0x56));
        let preview = home
            .loader
            .load_with_theme(&[], choice.path.to_str().unwrap());
        assert!(preview.diagnostics.is_empty());
        assert_eq!(preview.config.background, loaded.config.background);
    }
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
    assert_eq!(loaded.config.font_size, Config::default().font_size);
    assert_eq!(loaded.config.cursor_color, None);
    assert!(loaded.config.title_report);
    assert!(
        !home
            .loader
            .load_with_args(&args(&["--title-report="]))
            .config
            .title_report
    );
    assert_eq!(
        action(
            &loaded.config,
            if cfg!(windows) {
                "ctrl+shift+n"
            } else {
                "cmd+n"
            }
        ),
        Some(Action::NewWindow)
    );
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
fn session_dump_on_quit_defaults_validates_and_resets() {
    let home = TestHome::new();
    assert_eq!(
        home.loader.load().config.session_dump_on_quit,
        SessionDumpOnQuit::Unchecked
    );
    for (value, expected) in [
        ("disabled", SessionDumpOnQuit::Disabled),
        ("unchecked", SessionDumpOnQuit::Unchecked),
        ("checked", SessionDumpOnQuit::Checked),
        ("", SessionDumpOnQuit::Unchecked),
    ] {
        home.own(&format!(
            "session-dump-on-quit=checked\nsession-dump-on-quit={value}\n"
        ));
        let loaded = home.loader.load();
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert_eq!(loaded.config.session_dump_on_quit, expected);
    }

    home.own("session-dump-on-quit=disabled\n");
    let overridden = home
        .loader
        .load_with_args(&args(&["--session-dump-on-quit=checked"]));
    assert!(overridden.diagnostics.is_empty());
    assert_eq!(
        overridden.config.session_dump_on_quit,
        SessionDumpOnQuit::Checked
    );

    let invalid = home
        .loader
        .load_with_args(&args(&["--session-dump-on-quit=invalid"]));
    assert_eq!(invalid.diagnostics.len(), 1);
    assert_eq!(
        invalid.config.session_dump_on_quit,
        SessionDumpOnQuit::Disabled
    );
    home.own("session-dump-on-quit=invalid\n");
    let invalid = home.loader.load();
    assert_eq!(invalid.diagnostics.len(), 1);
    assert_eq!(
        invalid.config.session_dump_on_quit,
        SessionDumpOnQuit::Unchecked
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
fn outliner_and_pin_are_bindable_without_default_shortcuts() {
    for (name, expected) in [
        ("toggle_outliner", Action::ToggleOutliner),
        ("toggle_pin", Action::TogglePin),
    ] {
        let home = TestHome::new();
        assert!(
            home.loader
                .load()
                .config
                .keybinds
                .iter()
                .all(|binding| !binding.actions.contains(&expected))
        );
        home.own(&format!("keybind=ctrl+shift+o={name}\n"));
        let loaded = home.loader.load();
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        assert_eq!(action(&loaded.config, "ctrl+shift+o"), Some(expected));
        assert!(Action::parse(&format!("{name}:extra")).is_err());
    }
}

#[test]
fn goto_pin_accepts_positive_indices_and_rejects_invalid_arguments() {
    for index in [1, 9, 10, usize::MAX] {
        let binding = KeyBinding::parse(&format!("ctrl+p=goto_pin:{index}")).unwrap();
        assert_eq!(binding.actions, [Action::GotoPin(index)]);
    }
    for invalid in [
        "goto_pin",
        "goto_pin:",
        "goto_pin:0",
        "goto_pin:-1",
        "goto_pin:one",
        "goto_pin:1:extra",
        "goto_pin:18446744073709551616",
    ] {
        assert!(Action::parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn pin_hotkeys_can_be_remapped_and_disabled_in_config() {
    let home = TestHome::new();
    home.own(
        "keybind = cmd+shift+1=unbind\n\
         keybind = ctrl+alt+1=goto_pin:1\n\
         keybind = cmd+shift+2=goto_pin:10\n",
    );
    let loaded = home.loader.load();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    assert_eq!(action(&loaded.config, "cmd+shift+1"), None);
    assert_eq!(
        action(&loaded.config, "ctrl+alt+1"),
        Some(Action::GotoPin(1))
    );
    assert_eq!(
        action(&loaded.config, "cmd+shift+2"),
        Some(Action::GotoPin(10))
    );
    assert_eq!(
        action(&loaded.config, "cmd+1"),
        action(&Config::default(), "cmd+1"),
    );
}

#[test]
#[cfg(target_os = "macos")]
fn pin_shortcuts_add_shift_to_tab_shortcuts_without_replacing_them() {
    let config = Config::default();
    for index in 1..=9 {
        assert_eq!(
            action(&config, &format!("cmd+shift+{index}")),
            Some(Action::GotoPin(index))
        );
        assert_eq!(
            action(&config, &format!("cmd+{index}")),
            Some(if index == 9 {
                Action::LastTab
            } else {
                Action::GotoTab(index)
            })
        );
    }
}

#[test]
fn stream_deck_commands_are_bindable_and_reject_unknown_arguments() {
    for (name, command) in [
        ("needs_input", StreamDeckAction::NeedsInput),
        ("working", StreamDeckAction::Working),
        ("done", StreamDeckAction::Done),
        ("idle", StreamDeckAction::Idle),
        ("next_page", StreamDeckAction::NextPage),
        ("brightness", StreamDeckAction::Brightness),
        ("pull_to_first", StreamDeckAction::PullToFirst),
    ] {
        let binding =
            KeyBinding::parse(&format!("global:ctrl+shift+p=stream_deck:{name}")).unwrap();
        assert!(binding.flags.global);
        assert_eq!(binding.actions, [Action::StreamDeck(command)]);
    }
    for invalid in [
        "stream_deck",
        "stream_deck:",
        "stream_deck:unknown",
        "stream_deck:idle:extra",
    ] {
        assert!(Action::parse(invalid).is_err(), "{invalid}");
    }
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
    let new_window = if cfg!(windows) {
        "ctrl+shift+n"
    } else {
        "cmd+n"
    };
    home.own(&format!("keybind={new_window}=not_an_action\nkeybind=ctrl+ctrl+a=new_tab\nkeybind=unknown_key=new_tab\nkeybind=global:ctrl+a>b=new_tab\nkeybind=ctrl+a=resize_split:next,10\nbackground-opacity=2\nfont-size=inf\nnotify-on-command-finish-after=5\n"));
    let loaded = home.loader.load();
    assert_eq!(loaded.diagnostics.len(), 8, "{:?}", loaded.diagnostics);
    assert_eq!(action(&loaded.config, new_window), Some(Action::NewWindow));
    assert_eq!(loaded.config.background_opacity, 1.0);
    assert_eq!(loaded.config.font_size, Config::default().font_size);
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
    for escape in [r"\u{aaaaaa}", r"\u{AAAAAA}", r"\u{110000}", r"\u{FFFFFFFF}"] {
        assert!(parse_escaped_bytes(escape).is_err());
    }
    assert_eq!(parse_escaped_bytes(r"\u{aA}").unwrap(), "ª".as_bytes());
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
    assert_eq!(loaded.config.font_size, Config::default().font_size);
    assert_eq!(loaded.diagnostics.len(), 1);
}

#[test]
fn truncated_utf8_configuration_is_rejected_without_partial_application() {
    let home = TestHome::new();
    let own = home.own("");
    for tail in [
        b"\xc2".as_slice(),
        b"\xe2",
        b"\xe2\x94",
        b"\xf0",
        b"\xf0\x9f",
        b"\xf0\x9f\x98",
    ] {
        let mut bytes = b"font-size=99\nwindow-title=".to_vec();
        bytes.extend_from_slice(tail);
        fs::write(&own, bytes).unwrap();
        let loaded = home.loader.load();
        assert_eq!(loaded.config.font_size, Config::default().font_size);
        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(loaded.diagnostics[0].message.contains("UTF-8"));
    }
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
