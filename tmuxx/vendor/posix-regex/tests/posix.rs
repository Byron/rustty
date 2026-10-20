use tmuxx_posix_regex::{PosixRegexBuilder, matcher::MatchError};

fn captures(pattern: &[u8], text: &[u8], ignore: bool) -> Vec<Option<(usize, usize)>> {
    PosixRegexBuilder::new(pattern)
        .with_default_classes()
        .extended(true)
        .compile()
        .expect("valid POSIX ERE")
        .case_insensitive(ignore)
        .matches(text, Some(1))
        .expect("fixture within work bound")
        .into_iter()
        .next()
        .expect("fixture must match")
        .into_vec()
}

#[test]
fn earliest_start_wins_before_longest_match_and_earlier_groups_win_ties() {
    assert_eq!(captures(b"a|bb+", b"abbbb", false), [Some((0, 1))]);
    assert_eq!(
        captures(b"(a|ab)(b?)", b"ab", false),
        [Some((0, 2)), Some((0, 2)), Some((2, 2))]
    );
    assert_eq!(
        captures(b"(a|aa)*", b"aa", false),
        [Some((0, 2)), Some((0, 2))]
    );
    assert_eq!(
        captures(b"((a|ab)*)", b"abab", false),
        [Some((0, 4)), Some((0, 4)), Some((2, 4))]
    );
    let exact = PosixRegexBuilder::new(b"a|abz")
        .extended(true)
        .compile()
        .expect("valid ERE");
    assert_eq!(
        exact
            .matches_exact(b"abx")
            .expect("bounded match")
            .expect("a matches")[0],
        Some((0, 1))
    );
    let all = PosixRegexBuilder::new(b"a|abz|b")
        .extended(true)
        .compile()
        .expect("valid ERE");
    let all = all.matches(b"abx", None).expect("bounded matches");
    assert_eq!(
        all.iter().map(|groups| groups[0]).collect::<Vec<_>>(),
        [Some((0, 1)), Some((1, 2))]
    );
}

#[test]
fn unicode_scalar_matching_projects_captures_back_to_exact_bytes() {
    assert_eq!(
        captures("(界|界面)(😀?)".as_bytes(), "x界面😀y".as_bytes(), false),
        [Some((1, 11)), Some((1, 7)), Some((7, 11))]
    );
    assert_eq!(
        captures("(é)\\1".as_bytes(), "Éé".as_bytes(), true),
        [Some((0, 4)), Some((0, 2))]
    );
    assert_eq!(
        captures(b"(.)", "😀".as_bytes(), false),
        [Some((0, 4)), Some((0, 4))]
    );
    assert_eq!(
        captures(b"(\xff)", b"\xff", false),
        [Some((0, 1)), Some((0, 1))]
    );
    let raw = PosixRegexBuilder::new(b"\xff")
        .extended(true)
        .compile()
        .expect("distinct byte token");
    assert!(
        raw.matches("�".as_bytes(), None)
            .expect("bounded search")
            .is_empty()
    );
}

#[test]
fn windows_unicode_locale_classes_ranges_and_equivalence_are_executable() {
    assert_eq!(
        captures("[[:alpha:]]+".as_bytes(), "é界".as_bytes(), false)[0],
        Some((0, 5))
    );
    assert_eq!(
        captures("[é-ê]+".as_bytes(), "éê".as_bytes(), false)[0],
        Some((0, 4))
    );
    assert_eq!(
        captures(b"[[=e=]]+", "éÉe".as_bytes(), false)[0],
        Some((0, 5))
    );
    assert_eq!(
        captures("[[.界.]]".as_bytes(), "界".as_bytes(), false)[0],
        Some((0, 3))
    );
    assert_eq!(captures(b"[[:upper:]]", b"a", true)[0], Some((0, 1)));
    let glob_class = PosixRegexBuilder::new(b"[[:upper:]]")
        .with_default_classes()
        .extended(true)
        .compile()
        .expect("class")
        .case_insensitive(true)
        .fold_classes(false);
    assert!(
        glob_class
            .matches(b"a", None)
            .expect("bounded class")
            .is_empty()
    );
    let combining = PosixRegexBuilder::new("[[=\u{301}=]]".as_bytes())
        .extended(true)
        .compile()
        .expect("combining equivalent");
    assert!(
        combining
            .matches("\u{300}".as_bytes(), None)
            .expect("bounded equivalence")
            .is_empty()
    );
}

#[test]
fn malformed_patterns_and_bounded_work_never_become_a_successful_negative_match() {
    for pattern in [
        b"(".as_slice(),
        b")",
        b"[",
        b"[z-a]",
        b"*a",
        b"a{3,2}",
        b"\\9",
        b"(a\\1)",
        b"\\0",
    ] {
        assert!(
            PosixRegexBuilder::new(pattern)
                .extended(true)
                .compile()
                .is_err(),
            "{pattern:?}"
        );
    }
    let pattern = b"(".repeat(129);
    assert!(
        PosixRegexBuilder::new(&pattern)
            .extended(true)
            .compile()
            .is_err()
    );
    let regex = PosixRegexBuilder::new(b"(a?)*b")
        .extended(true)
        .compile()
        .expect("valid bounded regex")
        .work_limit(8);
    assert_eq!(regex.matches(b"aaaa", None), Err(MatchError::WorkLimit));
    assert_eq!(captures(b"", b"", false), [Some((0, 0))]);
    assert_eq!(captures(b"()", b"", false), [Some((0, 0)), Some((0, 0))]);
    assert_eq!(captures(b"a|", b"b", false), [Some((0, 0))]);
}

#[test]
fn empty_optional_and_nested_captures_preserve_posix_preferences() {
    assert_eq!(
        captures(b"(a*)(a*)", b"aa", false),
        [Some((0, 2)), Some((0, 2)), Some((2, 2))]
    );
    assert_eq!(
        captures(b"(a?)(a?)", b"a", false),
        [Some((0, 1)), Some((0, 1)), Some((1, 1))]
    );
    assert_eq!(
        captures(b"(a)?(b)", b"b", false),
        [Some((0, 1)), None, Some((0, 1))]
    );
    assert_eq!(
        captures(b"((a|ab)(b?))", b"ab", false),
        [Some((0, 2)), Some((0, 2)), Some((0, 2)), Some((2, 2))]
    );
    assert_eq!(
        captures(br"()\1a", b"a", false),
        [Some((0, 1)), Some((0, 0))]
    );
    assert_eq!(
        captures(br"(a*)\1", b"", false),
        [Some((0, 0)), Some((0, 0))]
    );
    assert_eq!(captures(b"(a?)*", b"", false), [Some((0, 0)), Some((0, 0))]);
    assert_eq!(captures(b"(a?)*", b"aa", false)[0], Some((0, 2)));
    assert_eq!(captures(b"((a?)*)*", b"a", false)[0], Some((0, 1)));
    assert_eq!(captures(b"(){2}", b"", false), [Some((0, 0)), Some((0, 0))]);
    assert_eq!(captures(b"(a?){2}", b"a", false)[0], Some((0, 1)));
}

#[test]
fn short_malformed_and_nullable_patterns_terminate_without_panics() {
    const ALPHABET: &[u8] = b"()|*?[]a\\";
    for mut value in 0..ALPHABET.len().pow(4) {
        let mut pattern = [0u8; 4];
        for byte in &mut pattern {
            *byte = ALPHABET[value % ALPHABET.len()];
            value /= ALPHABET.len();
        }
        if let Ok(regex) = PosixRegexBuilder::new(&pattern)
            .with_default_classes()
            .extended(true)
            .compile()
        {
            for text in [b"".as_slice(), b"aaa", b"\xffaa", "界a".as_bytes()] {
                let _ = regex.clone().work_limit(2000).matches(text, Some(2));
            }
        }
    }
}
