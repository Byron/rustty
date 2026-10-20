//! The existing exact format/parser matrices execute locally on Windows; only
//! their C oracle runs remotely. No process/desktop assertion is established.
#![cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;

struct Reference(remote_reference::RemoteReference);
impl Reference {
    fn new() -> Self {
        let reference = Self(remote_reference::RemoteReference::new());
        reference.run(["new-session", "-d", "-s", "oracle", "sleep 60"]);
        reference
    }
    fn run<I, S>(&self, arguments: I) -> Vec<u8>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        let arguments = arguments
            .into_iter()
            .map(|arg| arg.as_ref().to_vec())
            .collect::<Vec<_>>();
        let output = self.0.output(
            &arguments.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            &[],
        );
        assert!(output.status.success(), "C oracle failed: {output:?}");
        output.stdout
    }
    fn source(&self, source: &[u8]) {
        let output = self.0.source(source);
        assert!(output.status.success(), "C source failed: {output:?}");
    }
    fn option(&self, name: &[u8]) -> Vec<u8> {
        let mut output = self.run([b"show-options".as_slice(), b"-gqv", name]);
        assert_eq!(output.pop(), Some(b'\n'));
        output
    }
}

#[test]
#[ignore = "requires explicit pinned TMUXX_REMOTE_C_CONFIG; never a local C executable"]
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
#[ignore = "requires explicit pinned TMUXX_REMOTE_C_CONFIG; never a local C executable"]
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
    reference.run(argv.iter().map(|arg| arg.to_vec()));
    let parsed = tmuxx_command::parse_argv(argv).unwrap();
    assert_eq!(reference.option(b"@argv"), parsed[0].args[3]);
}

#[test]
#[ignore = "requires explicit pinned TMUXX_REMOTE_C_CONFIG; never a local C executable"]
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
