//! Explicitly enabled executable oracle: TMUXX_C_ORACLE=/path/to/tmux cargo test
//! -p tmuxx-config --test c_reference -- --ignored. No C code is linked into Rust.

#[cfg(unix)]
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;
use std::process::Output;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
use tmuxx_config::{Environment, prepare};
#[cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;

#[cfg(unix)]
static SERIAL: AtomicU64 = AtomicU64::new(0);

struct Reference {
    #[cfg(unix)]
    executable: OsString,
    #[cfg(unix)]
    label: String,
    #[cfg(unix)]
    source: PathBuf,
    #[cfg(windows)]
    remote: remote_reference::RemoteReference,
}

impl Reference {
    fn new() -> Self {
        #[cfg(unix)]
        let executable = std::env::var_os("TMUXX_C_ORACLE")
            .expect("set TMUXX_C_ORACLE to the C tmux executable");
        #[cfg(unix)]
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        #[cfg(unix)]
        let label = format!("tmuxx-parser-{}-{serial}", std::process::id());
        #[cfg(unix)]
        let source = std::env::temp_dir().join(format!("{label}.conf"));
        let reference = Self {
            #[cfg(unix)]
            executable,
            #[cfg(unix)]
            label,
            #[cfg(unix)]
            source,
            #[cfg(windows)]
            remote: remote_reference::RemoteReference::new(),
        };
        reference.run(["new-session", "-d", "-s", "oracle", "sleep 60"]);
        reference
    }

    fn output<I, S>(&self, args: I) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        #[cfg(unix)]
        {
            Command::new(&self.executable)
                .args(["-L", &self.label, "-f", "/dev/null"])
                .args(
                    args.into_iter()
                        .map(|arg| OsStr::from_bytes(arg.as_ref()).to_owned()),
                )
                .env_remove("TMUX")
                .env("LC_ALL", "C.UTF-8")
                .output()
                .expect("launch C oracle")
        }
        #[cfg(windows)]
        {
            let args = args
                .into_iter()
                .map(|arg| arg.as_ref().to_vec())
                .collect::<Vec<_>>();
            self.remote
                .output(&args.iter().map(Vec::as_slice).collect::<Vec<_>>(), &[])
        }
    }

    fn run<I, S>(&self, args: I) -> Vec<u8>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        let output = self.output(args);
        assert!(
            output.status.success(),
            "C oracle failed ({}): stdout={:?}, stderr={:?}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn source(&self, source: &[u8]) {
        let output = self.source_output(source);
        assert!(output.status.success(), "C source failed: {output:?}");
    }

    fn source_output(&self, source: &[u8]) -> Output {
        #[cfg(unix)]
        {
            std::fs::write(&self.source, source).unwrap();
            self.output([
                b"source-file".as_slice(),
                self.source.as_os_str().as_bytes(),
            ])
        }
        #[cfg(windows)]
        self.remote.source(source)
    }

    fn option(&self, name: &[u8]) -> Vec<u8> {
        let mut value = self.run([b"show-options".as_slice(), b"-gqv", name]);
        assert_eq!(value.pop(), Some(b'\n'));
        value
    }
}

#[cfg(unix)]
impl Drop for Reference {
    fn drop(&mut self) {
        let _ = self.output(["kill-server"]);
        let _ = std::fs::remove_file(&self.source);
    }
}

#[test]
#[ignore = "requires explicitly selected C tmux executable"]
fn styled_width_and_trimming_match_c_bytes() {
    let reference = Reference::new();
    for value in [
        "abcde",
        "#[fg=red]abc#[default]de",
        "a#[bold]界e\u{301}xy#[default]",
        "##[fg=red]abc",
        "###[fg=red]abc",
        "####[fg=red]abc",
        "#####abc",
        "a#[broken",
        "a##[broken",
        "#",
        "##",
        "###",
        "####",
        "ab\tcd\nef",
        "#[a#{b:]}]abc",
        "#[fg=red]ab#[default]",
        "界x界y",
    ] {
        reference.run(["set-option", "-g", "@styled", value]);
        for modifier in [
            "w", "p8", "p-8", "=1", "=-1", "=2", "=-2", "=4", "=-4", "=99", "=-99", "=/3/...",
            "=/-3/...",
        ] {
            let format = format!("#{{{modifier}:@styled}}");
            let expected = reference.run(["display-message", "-p", &format]);
            let mut actual = tmuxx_format::expand(format.as_bytes(), |key| {
                (key == b"@styled").then(|| value.as_bytes().to_vec())
            })
            .unwrap();
            actual.push(b'\n');
            assert_eq!(actual, expected, "{format} on {value:?}");
        }
    }
    for format in [
        "##[red]abc",
        "###[red]abc",
        "####[red]abc",
        "#[fg=#S]#S",
        "#[fg=#{l:red}]abc",
    ] {
        let expected = reference.run(["display-message", "-p", format]);
        let mut actual = tmuxx_format::expand(format.as_bytes(), |key| {
            (key == b"session_name").then(|| b"oracle".to_vec())
        })
        .unwrap();
        actual.push(b'\n');
        assert_eq!(actual, expected, "style expansion {format}");
    }
}

#[test]
#[ignore = "requires explicitly selected C tmux executable"]
fn text_parser_and_argv_match_c_execution() {
    let reference = Reference::new();
    let source = b"set-option -g @one 'a b'\nset-option -g @two \"x\\ny\\t\\033\"; set-option -g @three c\\;d # comment\nset-option -g @unicode \\u754c\nset-option -g @joined 'one\\\ntwo'";
    let commands = tmuxx_command::parse(source).unwrap();
    reference.source(source);
    for command in commands {
        assert_eq!(command.args.len(), 4);
        assert_eq!(reference.option(&command.args[2]), command.args[3]);
    }

    let argv = [b"set-option".as_slice(), b"-g", b"@argv", b"literal\\;"];
    reference.run(argv);
    let parsed = tmuxx_command::parse_argv(argv).unwrap();
    assert_eq!(reference.option(b"@argv"), parsed[0].args[3]);
}

#[test]
#[ignore = "requires explicitly selected C tmux executable"]
fn high_byte_preservation_is_an_explicit_reference_difference() {
    let reference = Reference::new();
    let source = b"set-option -g @raw \xff\n";
    let parsed = tmuxx_command::parse(source).unwrap();
    assert_eq!(parsed[0].args[3], b"\xff");
    let c = reference.source_output(source);
    // C's lexer promotes a plain char to int. Signed-char ABIs interpret 0xff
    // as EOF; unsigned-char ABIs (including aarch64 GNU/Linux) retain it.
    // Assert each target's actual contract without normalizing either output.
    #[cfg(unix)]
    let signed_char = (0xff_u8 as std::ffi::c_char as i32) < 0;
    #[cfg(windows)]
    let signed_char = {
        let identity: serde_json::Value =
            serde_json::from_slice(&reference.remote.identity()).unwrap();
        assert!(
            identity["host"][0] == "Linux" && identity["host"][4] == "x86_64",
            "the Windows high-byte C oracle requires the pinned Linux/x86_64 build with recorded default C flags; another reference ABI needs measured char signedness: {identity}"
        );
        true
    };
    if signed_char {
        assert!(
            !c.status.success(),
            "reference changed; review signed-char high-byte difference"
        );
    } else {
        assert!(c.status.success(), "unsigned-char C source rejected: {c:?}");
        assert!(c.stderr.is_empty(), "{c:?}");
        assert_eq!(reference.option(b"@raw"), b"\xff");
    }
}

#[test]
#[ignore = "requires explicitly selected C tmux executable"]
fn preparation_matches_c_without_executing_queued_commands() {
    let reference = Reference::new();
    reference.run(["set-option", "-g", "@flag", "0"]);
    let source = b"TMUXX_PARSE_VALUE=early\nset-environment -g TMUXX_PARSE_VALUE late\nset-option -g @flag 1\n%if #{@flag}\nset-option -g @branch wrong\n%else\nset-option -g @branch prepared\n%endif\n%if 0\nTMUXX_PARSE_VALUE=wrong\n%elif 1\n%hidden TMUXX_HIDDEN=secret\n%endif\nset-option -g @value \"$TMUXX_PARSE_VALUE\"\nset-option -g @hidden \"$TMUXX_HIDDEN\"";
    let mut environment = Environment::default();
    let commands = prepare(source, &mut environment, |key| {
        (key == b"@flag").then(|| b"0".to_vec())
    })
    .unwrap();
    reference.source(source);
    for command in commands.iter().filter(|c| c.args[0] == b"set-option") {
        assert_eq!(reference.option(&command.args[2]), command.args[3]);
    }
    assert_eq!(
        environment.get(b"TMUXX_PARSE_VALUE"),
        Some(b"early".as_slice())
    );
    assert!(environment.hidden.contains(b"TMUXX_HIDDEN".as_slice()));
}

#[test]
#[ignore = "requires explicitly selected C tmux executable"]
fn format_observations_match_c() {
    let reference = Reference::new();
    let options: &[(&[u8], &[u8])] = &[
        (b"@path", b"/tmp/work"),
        (b"@s", b"abcdefghij"),
        (b"@greek", "αβγ".as_bytes()),
        (b"@cjk", "中文".as_bytes()),
        (b"@quote", b"!~]=%\n\t|&;<>(){}$`\\\"'*?[# "),
    ];
    for (name, value) in options {
        reference.run([b"set-option".as_slice(), b"-g".as_slice(), name, value]);
    }
    for format in [
        b"#{session_name}/#{pane_id}/##/#,/#}".as_slice(),
        b"#{?#{==:#{session_name},oracle},yes#,please,no}",
        b"#{&&:1,0}",
        b"#{||:0,yes}",
        b"#{m:*cle,oracle}",
        b"#{b:@path}",
        b"#{d:@path}",
        b"#{l:#{session_name}}",
        b"#{q:@quote}",
        b"#{b:@missing}",
        b"#{q/s:@missing}",
        b"#{b:#{l:/a/b}}",
        b"#{q:#{l:a b}}",
        b"#{q/s:@quote}",
        b"#{q/h:@quote}",
        b"#{n:@greek}",
        b"#{w:@greek}",
        b"#{n:@cjk}",
        b"#{w:@cjk}",
        b"#{=5:@s}",
        b"#{=-5:@s}",
        b"#{=/5/...:@s}",
        b"#{=2:@greek}",
        b"#{=2:@cjk}",
        b"#{=1:@cjk}",
        b"#{=/1/x:@cjk}",
        b"#{p6:@cjk}",
        b"#{p-6:@cjk}",
        b"#{m/i:*CLE,oracle}",
        "#{m:?,é}".as_bytes(),
        "#{m:??,é}".as_bytes(),
        "#{m:?,界}".as_bytes(),
        "#{m/i:é,É}".as_bytes(),
        "#{m:[[:alpha:]],é}".as_bytes(),
        b"#{m:[[:digit:]],5}",
        b"#{e|+|:2,3}",
        b"#{e|-|:2,5}",
        b"#{e|*|:6,7}",
        b"#{e|/|:5,2}",
        b"#{e|m|:7,3}",
        b"#{e|/|f|3:1,3}",
        b"#{e|/|f|2:10,3}",
        b"#{e|*|f:2.5,2}",
        b"#{e|/|:5,0}",
        b"#{e|m|:7,0}",
        b"#{e|+|:nan,0}",
        b"#{e|-|:1e30,1e30}",
        b"#{e|+|:9223372036854775807,0}",
        b"#{e|+|:9223372036854774784,0}",
        b"#{e|-|:-9223372036854775808,0}",
        b"#{e|/|f:5,0}",
        b"#{e|/|f:0,0}",
        b"#{e|+|f:nan,0}",
        b"#{e|+|f:-nan,0}",
        b"#{e|+|f:0x1.8p+1,2}",
        b"#{e|+|:0x10,2}",
        b"#{e|+|f:inf,0}",
        b"#{e|/|f|-1:1,3}",
        b"#{e|+||-100:1,2}",
        b"#{e|+|f| 3:1,2}",
        b"#{a:98}",
        b"#{a:200}",
        b"#{R:ab,3}",
        b"#{R:x,10001}",
        b"#{n:#{R:abcdefgh,8192}}",
        b"#{R:abcdefgh,8193}",
    ] {
        let mut c = reference.run([b"display-message".as_slice(), b"-p".as_slice(), format]);
        assert_eq!(c.pop(), Some(b'\n'));
        let rust = tmuxx_format::expand(format, |key| {
            Some(match key {
                b"session_name" => b"oracle".to_vec(),
                b"pane_id" => b"%0".to_vec(),
                _ => {
                    return options
                        .iter()
                        .find(|(name, _)| *name == key)
                        .map(|(_, value)| value.to_vec());
                }
            })
        })
        .unwrap();
        #[cfg(windows)]
        if format == "#{m:??,é}".as_bytes() {
            // The glibc oracle also accepts the UTF-8 byte count after its
            // wide-character match fails. Native Windows has an explicit
            // scalar glob contract; assert both real results, never normalize.
            assert_eq!(rust, b"0", "Windows glob must preserve scalar boundaries");
            assert_eq!(c, b"1", "review the pinned glibc wildcard difference");
            continue;
        }
        assert_eq!(rust, c, "format {}", String::from_utf8_lossy(format));
    }
}

#[cfg(windows)]
#[test]
#[ignore = "requires the explicit pinned glibc C oracle"]
fn native_globs_preserve_scalar_boundaries_against_glibc_byte_fallback() {
    let reference = Reference::new();
    for (pattern, text, native, c) in [
        ("?", "é", b"1".as_slice(), b"1".as_slice()),
        ("??", "é", b"0", b"1"),
        ("???", "é", b"0", b"0"),
        ("?", "界", b"1", b"1"),
        ("??", "界", b"0", b"0"),
        ("???", "界", b"0", b"1"),
        ("??", "ab", b"1", b"1"),
        ("???", "abc", b"1", b"1"),
    ] {
        let format = format!("#{{m:{pattern},{text}}}");
        assert_eq!(
            tmuxx_format::expand(format.as_bytes(), |_| None).unwrap(),
            native,
            "{format}"
        );
        assert_eq!(
            reference.run(["display-message", "-p", &format]),
            [c, b"\n"].concat(),
            "review pinned glibc {format}"
        );
    }
}

#[test]
#[ignore = "requires explicitly selected C tmux executable"]
fn preparation_suppresses_jobs_and_boolean_evaluation_short_circuits() {
    let reference = Reference::new();
    let source = b"%if '#(echo unsafe)'\nset-option -g @jobs wrong\n%else\nset-option -g @jobs disabled\n%endif\n";
    let commands = prepare(source, &mut Environment::default(), |_| None).unwrap();
    reference.source(source);
    assert_eq!(commands[0].args[3], reference.option(b"@jobs"));
    for expression in [
        b"#{&&:0,#(echo unused)}".as_slice(),
        b"#{||:1,#(echo unused)}",
    ] {
        let c = reference.run([b"display-message".as_slice(), b"-p".as_slice(), expression]);
        let mut rust = tmuxx_format::expand(expression, |_| None).unwrap();
        rust.push(b'\n');
        assert_eq!(rust, c);
    }
}

#[test]
#[ignore = "requires explicitly selected C tmux executable"]
fn malformed_source_rejection_is_an_explicit_reference_difference() {
    let reference = Reference::new();
    for (source, accepted) in [
        (
            b"set-option -g @bad \"unfinished".as_slice(),
            b"unfinished".as_slice(),
        ),
        (b"set-option -g @bad 'unfinished", b"unfinished"),
        (b"set-option -g @bad a\0b\n", b"a"),
    ] {
        assert!(tmuxx_command::parse(source).is_err());
        reference.source(source);
        assert_eq!(reference.option(b"@bad"), accepted);
    }
}

#[test]
#[ignore = "requires explicitly selected C tmux executable"]
fn regex_and_substitution_observations_match_c() {
    let reference = Reference::new();
    let values: &[(&[u8], &[u8])] = &[
        (b"@value", b"aba a 112 aa"),
        (b"@pattern", b"a+"),
        (b"@replace", b"X"),
    ];
    for (key, value) in values {
        reference.run([b"set".as_slice(), b"-g".as_slice(), key, value]);
    }
    for format in [
        b"#{m/r:a+,aba}".as_slice(),
        b"#{m/r:^a+$,aba}",
        b"#{m/ri:^AB,abC}",
        b"#{m/r:[,abc}",
        b"#{m/r:(a|ab)b?,ab}",
        "#{m/r:^.$,é}".as_bytes(),
        "#{m/r:^[[:alpha:]]$,é}".as_bytes(),
        "#{m/ri:^é$,É}".as_bytes(),
        "#{s/(é)/<\\1>/:#{l:été}}".as_bytes(),
        "#{s/é/X/i:#{l:Été}}".as_bytes(),
        "#{s/x*/Z/:#{l:é界}}".as_bytes(),
        b"#{s/a/X/:@value}",
        b"#{s/(ab|a)/<\\1>/:@value}",
        b"#{s/(a)(b)?/\\2-\\1-\\9/:@value}",
        b"#{s/a+/&/:@value}",
        b"#{s/^a/X/:@value}",
        b"#{s/a$/X/:@value}",
        b"#{s/[/X/:@value}",
        b"#{s//X/:@value}",
        b"#{s/x*/Z/:@value}",
        b"#{s/ /_/g:@value}",
        b"#{s/A/X/i:@value}",
        b"#{s/#{@pattern}/#{@replace}/:@value}",
        b"#{s/a/X/:#{l:banana}}",
        b"#{s/a/X/:@missing}",
        b"#{s/a/X/;s/X/Z/:@value}",
        b"#{n;s/a/X/:@value}",
        b"#{p20;=/5/...;s/a/Z/:@value}",
        b"#{q;b:@value}",
        b"#{d;b:@value}",
        b"#{b;q/s:#{l:/a b/c}}",
        b"#{q/s;b:@missing}",
        b"#{w;n;s/a/long/:@value}",
        b"#{n;m/r:a+,aaaa}",
    ] {
        let mut c = reference.run([b"display-message".as_slice(), b"-p".as_slice(), format]);
        assert_eq!(c.pop(), Some(b'\n'));
        let rust = tmuxx_format::expand(format, |key| {
            values
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_vec())
        })
        .unwrap();
        assert_eq!(rust, c, "format {}", String::from_utf8_lossy(format));
    }
}

#[test]
#[ignore = "requires explicitly selected C tmux executable"]
fn timestamp_and_time_expansion_match_c() {
    let reference = Reference::new();
    let values: &[(&[u8], &[u8])] = &[
        (b"@stamp", b"1700000000"),
        (b"@zero", b"0"),
        (b"@negative", b"-1"),
        (b"@invalid", b"invalid"),
        (b"@future", b"2200000000"),
        (b"@template", b"%Y/%%/#{@label}"),
        (b"@label", b"literal"),
    ];
    for (key, value) in values {
        reference.run([b"set".as_slice(), b"-g".as_slice(), key, value]);
    }
    for format in [
        b"#{t:@stamp}".as_slice(),
        b"#{t/f/%Y-%m-%d %H#:%M#:%S:@stamp}",
        b"#{t/p:@stamp}",
        b"#{t/r:@future}",
        b"#{t/f/%Y:@future}",
        b"#{t:@zero}",
        b"#{t:@negative}",
        b"#{t:@invalid}",
        b"#{t:#{l:1700000000}}",
        b"#{T:@template}",
        b"#{t/f/%Y:@missing}",
    ] {
        reference.run([
            b"set-option".as_slice(),
            b"-gF".as_slice(),
            b"@result".as_slice(),
            format,
        ]);
        let c = reference.option(b"@result");
        let rust = tmuxx_format::expand(format, |key| {
            values
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_vec())
        })
        .unwrap();
        assert_eq!(rust, c, "format {}", String::from_utf8_lossy(format));
    }
}

#[test]
#[ignore = "requires current C tmux relative and difference time modifiers"]
fn relative_difference_and_pretty_time_boundaries_match_c() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let reference = Reference::new();
    let now = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    };
    for age in [
        -3600,
        -1,
        0,
        1,
        59,
        60,
        65,
        3599,
        3600,
        3661,
        86399,
        86400,
        90061,
        27 * 86400,
        28 * 86400,
        60 * 86400,
        366 * 86400,
    ] {
        let mut compared = false;
        for _ in 0..5 {
            let start = now();
            let timestamp = (start - age).to_string();
            reference.run(["set-option", "-g", "@time", &timestamp]);
            let format = "#{t/r:@time}|#{t/d:@time}|#{t/p:@time}|#{t/prd:@time}|#{t/r;t/d:@time}";
            let expected = reference.run(["display-message", "-p", format]);
            let mut actual = tmuxx_format::expand(format.as_bytes(), |key| {
                (key == b"@time").then(|| timestamp.as_bytes().to_vec())
            })
            .unwrap();
            actual.push(b'\n');
            // Both implementations must observe the same second; no output normalization.
            if now() != start {
                continue;
            }
            assert_eq!(actual, expected, "timestamp age {age}");
            compared = true;
            break;
        }
        assert!(
            compared,
            "could not compare age {age} within one clock second"
        );
    }
}

#[test]
#[ignore = "requires explicitly selected C tmux executable"]
fn format_parser_resource_bounds_are_explicit_reference_differences() {
    let reference = Reference::new();
    reference.run(["set", "-g", "@value", "text"]);
    for count in [128, 129] {
        let format = format!("#{{{}:@value}}", vec!["n"; count].join(";"));
        assert_eq!(reference.run(["display", "-p", &format]), b"4\n");
        let actual = tmuxx_format::expand(format.as_bytes(), |key| {
            (key == b"@value").then(|| b"text".to_vec())
        });
        if count == 128 {
            assert_eq!(actual.unwrap(), b"4");
        } else {
            assert_eq!(actual.unwrap_err().message, "too many format modifiers");
        }
    }
    reference.run(["set", "-g", "@recursive", "#{E:@recursive}"]);
    assert_eq!(reference.run(["display", "-p", "#{E:@recursive}"]), b"\n");
    assert_eq!(
        tmuxx_format::expand(b"#{E:@recursive}", |key| (key == b"@recursive")
            .then(|| b"#{E:@recursive}".to_vec()))
        .unwrap_err()
        .message,
        "format nesting limit exceeded"
    );
}

#[test]
#[ignore = "requires current C tmux format modifiers"]
fn fuzzy_color_argument_quotes_and_contextless_modifiers_match_c() {
    let reference = Reference::new();
    let check = |format: &str, values: &[(&[u8], &[u8])]| {
        for (name, value) in values {
            reference.run([
                b"set-option".as_slice(),
                b"-g".as_slice(),
                name,
                if *value == b";" {
                    b"\\;".as_slice()
                } else {
                    value
                },
            ]);
        }
        let mut expected = reference.run(["display-message", "-p", format]);
        assert_eq!(expected.pop(), Some(b'\n'));
        let actual = tmuxx_format::expand(format.as_bytes(), |key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.to_vec())
        })
        .unwrap();
        assert_eq!(actual, expected, "{format} with {values:?}");
    };
    for (pattern, text) in [
        ("", "abc"),
        (" | ", "abc"),
        ("abc", "a_b_c"),
        ("abc", "acb"),
        ("dev bash", "dev:1 bash"),
        ("dev bash", "dev:1 sh"),
        ("abc", "ABC"),
        ("ABC", "abc"),
        ("'bash", "dev bash"),
        ("'bash", "b-a-s-h"),
        ("^dev", "dev bash"),
        ("^dev", "prod dev"),
        ("bash$", "dev bash"),
        ("bash$", "bash dev"),
        ("!ssh", "dev bash"),
        ("!long", "x"),
        ("!x", ""),
        ("!ssh", "dev ssh"),
        ("!ssh", "s_s_h"),
        ("dev !ssh", "dev bash"),
        ("dev !ssh", "dev ssh"),
        ("prod | dev", "dev bash"),
        ("prod | dev", "prod bash"),
        ("prod | dev", "test bash"),
        ("dev", "#[bold]dev#[default]"),
        ("dev", "#[fg=red,bg=blue]dev#[default]"),
        ("dev", "#[bold]d#[default]e#[underscore]v#[default]"),
        ("bash", "#[bold]dev#[default] bash"),
        ("é", "café"),
        ("é", "É"),
        ("éx", "éx"),
        ("界", "a界b"),
        ("a", "aba"),
        ("'a", "aba"),
        ("a | 'a", "abca"),
        ("ab", "aab"),
        ("^ab$", "ab"),
        ("^ab$", "abc"),
        ("'", "abc"),
        ("abc !x", "ABC x"),
        ("a", "#[align=right]abc#[align=left]left"),
        ("a", "#[align=centre]abc#[align=left]left#[align=right]end"),
        ("a", "#[align=absolute-centre]abc#[align=left]left"),
        ("a", "#[align=right]a#[default]b"),
        ("a", "#[align=right]a#[noalign]b"),
        ("a", "#[align=center]abc"),
        ("a", "#[ALIGN=RIGHT]a#[noalign]bb"),
        ("a", "#[align=right,invalid]a#[align=left]bbb"),
        ("a", "#[invalid,align=right]a#[align=left]bbb"),
        ("a", "#[align=right,fg=invalid]a#[align=left]bbb"),
        ("a", "#[align=right,bold|underscore]a#[align=left]bbb"),
        ("a", "#[align=right,width=101%]a#[align=left]bbb"),
        ("a", "#[align=right,range=pane|%1]a#[align=left]bbb"),
        ("#", "###hi"),
        ("[", "##[bold]x"),
        ("x", "#[bold"),
        ("a", "a\tb\nc"),
    ] {
        for flag in ["z", "p", "pzi"] {
            check(
                &format!("#{{m/{flag}:#{{@pattern}},#{{@text}}}}"),
                &[
                    (b"@pattern", pattern.as_bytes()),
                    (b"@text", text.as_bytes()),
                ],
            );
        }
    }
    for value in [
        "red",
        "4",
        "brightblue",
        "94",
        "colour4",
        "color255",
        "colour+4",
        "colour 4",
        "colour256",
        "#7f7f7f",
        "#fff",
        "AntiqueWhite",
        "light blue",
        "default",
        "terminal",
        "themegreen",
        "themedarkgrey",
        "none",
        "invalid",
    ] {
        for flag in ["", "/f", "/b", "/fb"] {
            check(
                &format!("#{{c{flag}:#{{@value}}}}"),
                &[(b"@value", value.as_bytes())],
            );
        }
    }
    for value in [
        b"".as_slice(),
        b"a b$c",
        b"a'b",
        b"\"",
        b"~",
        b"~home",
        b"a\nb\t",
        b"\\$x\"#",
        b";",
        b"a#b",
    ] {
        for format in [
            "#{q/a:@value}",
            "#{q/a:#{@value}}",
            "#{q/a;n:@value}",
            "#{q/a;q/s:@value}",
        ] {
            check(format, &[(b"@value", value)]);
        }
    }
    for format in [
        "#{A:a,b}",
        "#{A/3:a,b}",
        "#{A/0:a,b}",
        "#{A:}",
        "#{I/c:RGB}",
        "#{I/f:overline}",
        "#{I/e:HOME}",
        "#{I:x}",
        "#{a:0}",
        "#{a:31}",
        "#{a:32}",
        "#{a:126}",
        "#{a:127}",
        "#{==:a}",
        "#{<:a}",
        "#{s|foo/|bar/|:@value}",
    ] {
        check(format, &[(b"@value", b"foo/bar foo/")]);
    }
}

#[test]
#[ignore = "requires current C tmux modifier parser"]
fn modifier_delimiters_escaping_precedence_and_failures_match_c() {
    let reference = Reference::new();
    let values: &[(&[u8], &[u8])] = &[
        (b"@text", b"a/b:a;b#c"),
        (b"@pattern", b"a/b"),
        (b"@replace", b"#{l:x}"),
        (b"@flags", b"i"),
        (b"@path", b"/some/path with space/file"),
        (b"@time", b"1700000000"),
    ];
    for (name, value) in values {
        reference.run([b"set-option".as_slice(), b"-g".as_slice(), name, value]);
    }
    let check = |format: &str| {
        reference.run(["set-option", "-gF", "@result", format]);
        let expected = reference.option(b"@result");
        let actual = tmuxx_format::expand(format.as_bytes(), |key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.to_vec())
        })
        .unwrap();
        assert_eq!(actual, expected, "{format}");
    };
    for delimiter in ["/", "|", "~", "!", "^", "_", "+", "=", "#"] {
        for format in [
            format!("#{{m{delimiter}i{delimiter}:ABC,abc}}"),
            format!("#{{m{delimiter}#{{@flags}}{delimiter}:ABC,abc}}"),
            format!("#{{q{delimiter}a{delimiter}:@path}}"),
            format!("#{{c{delimiter}f{delimiter}:red}}"),
            format!("#{{e{delimiter}+{delimiter}f{delimiter}3{delimiter}:1.5,2}}"),
            format!("#{{={delimiter}3{delimiter}..{delimiter}:@text}}"),
            format!("#{{p{delimiter}-12{delimiter}:@text}}"),
            format!("#{{t{delimiter}f{delimiter}%Y-%m{delimiter}:@time}}"),
        ] {
            check(&format);
        }
    }
    for format in [
        "#{mi:ABC,abc}",
        "#{qsa:@path}",
        "#{qas:@path}",
        "#{q/hs:@path}",
        "#{q/x:@path}",
        "#{cf:red}",
        "#{s|a/b|x|:@text}",
        "#{s|a#:b#;|x|:@text}",
        "#{s|#{@pattern}|#{@replace}|:@text}",
        "#{s|a|x|;s|x|z|:@text}",
        "#{s/a/x:@text}",
        "#{s/a/:@text}",
        "#{s/a:@text}",
        "#{=/2/##{/@text}",
        "#{=/3/#,/;n:@text}",
        "#{=/3/#:/;n:@text}",
        "#{l:a#,b##c#:d#}e}",
        "#{l:#{@text},##}",
        "#{l:##{@text}}",
        "#{l:#{l:##}##}",
        "#{q;:@path}",
        "#{;q:@path}",
        "#{q;;a:@path}",
        "#{bogus:@path}",
        "before#{==:a}after",
        "before#{R:a}after",
        "before#{e|+:a}after",
        "#{==:a,b,c}",
        "#{l;==:a,b}",
        "#{==;l:a,b}",
        "#{a;l:65}",
        "#{l;a:65}",
        "#{!;!!:1}",
        "#{!!;!:1}",
        "#{==;!=:a,a}",
        "#{!=;==:a,a}",
        "#{&&;||:0,1}",
        "#{||;&&:0,1}",
        "#{l;E:#{@text}}",
        "#{q;n:?@text,yes,no}",
        "#{=3;=/2/..;=4:@text}",
        "#{e|+/ignored/ignored/ignored:@text}",
        "#{cf;cb:red}",
        "#{cb;cf:red}",
        "#{t/f/%Y;t:@time}",
        "#{t;t/f/%Y:@time}",
        "before#{unclosed",
        "before#(unclosed",
        "before#",
        "before#:#,##",
        "#{l:#:}",
        "#{a: 65}",
        "#{a:+65}",
        "#{a:65 }",
        "#{R:x,+3}",
    ] {
        check(format);
    }
}
