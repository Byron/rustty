use super::*;

#[test]
fn combined_global_options_leave_command_flags_and_shell_arguments_intact() {
    for globals in [
        vec!["-Ltest-a", "-f/dev/null", "-CC"],
        vec!["-L", "test-a", "-f", "/dev/null", "-C", "-C"],
    ] {
        let mut arguments = vec!["tmuxx"];
        arguments.extend(globals);
        let command = [
            "new-session",
            "-d",
            "-x",
            "1",
            "sh",
            "-c",
            "printf %s \"$1\"",
            "--",
            "-f",
            ";",
            "list-sessions",
        ];
        arguments.extend(command);
        let cli = Cli::try_parse_from(arguments).unwrap();
        assert_eq!(cli.label.as_deref(), Some("test-a"));
        assert_eq!(cli.config, [PathBuf::from("/dev/null")]);
        assert_eq!(cli.control, 2);
        assert_eq!(cli.command, command.map(OsString::from));
    }
}

#[cfg(unix)]
#[test]
fn embedded_daemon_launch_keeps_subcommand_and_native_endpoint() {
    let mut cli = Cli::try_parse_from(["rustty tmux", "-f", "/dev/null"]).unwrap();
    cli.launcher_prefix = vec!["tmux".into()];
    let endpoint = std::path::Path::new("/tmp/rustty-test.native");
    let command = server_command(&cli, endpoint, None).unwrap();
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            "tmux",
            "-D",
            "--native-endpoint",
            "/tmp/rustty-test.native",
            "--report-startup-error",
            "-f",
            "/dev/null"
        ]
        .map(std::ffi::OsStr::new)
    );
    cli.launcher_prefix.clear();
    let socket = PathBuf::from("/tmp/tmux-test");
    let command = server_command(&cli, endpoint, Some(&socket)).unwrap();
    let args = command.get_args().collect::<Vec<_>>();
    assert_eq!(args[0], "-D");
    assert!(args.windows(2).any(|args| args == ["-S", "/tmp/tmux-test"]));
}

#[cfg(windows)]
#[test]
fn windows_daemon_launch_keeps_subcommand_and_exact_pipe_endpoint() {
    let endpoint = tmuxx_runtime::windows::endpoint("owner 雪 &;!^ %").unwrap();
    let mut command = native_daemon_command(&endpoint, &["tmux"]).unwrap();
    let config = PathBuf::from(r"C:\private fixtures\config 雪 &;!^ %.conf");
    command.arg("-f").arg(&config);
    assert_eq!(command.get_program(), std::env::current_exe().unwrap());
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            std::ffi::OsStr::new("tmux"),
            std::ffi::OsStr::new("-D"),
            std::ffi::OsStr::new("--native-endpoint"),
            endpoint.as_os_str(),
            std::ffi::OsStr::new("-f"),
            config.as_os_str(),
        ]
    );
    let standalone = native_daemon_command(&endpoint, &[]).unwrap();
    assert_eq!(
        standalone.get_args().next(),
        Some(std::ffi::OsStr::new("-D"))
    );
}
