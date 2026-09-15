use rustty_parser::{BatchEvent, Event, Parser, State};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

fn parse(bytes: &[u8], chunk: usize, batched: bool) -> (u64, State, bool) {
    let mut parser = Parser::new();
    let mut hash = DefaultHasher::new();
    for bytes in bytes.chunks(chunk) {
        if batched {
            parser.advance_batched(bytes, |event| match event {
                BatchEvent::PrintAscii(bytes) => {
                    for &byte in bytes {
                        Event::Print(char::from(byte)).hash(&mut hash);
                    }
                }
                BatchEvent::PrintUtf8(text) => {
                    for cp in text.chars() {
                        Event::Print(cp).hash(&mut hash);
                    }
                }
                BatchEvent::Event(event) => event.hash(&mut hash),
            });
        } else {
            parser.advance(bytes, |event| event.hash(&mut hash));
        }
    }
    (hash.finish(), parser.state(), parser.is_ground())
}

#[test]
fn ghostty_parser_corpus_is_chunk_independent() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/fuzz-libghostty/corpus");
    let mut count = 0;
    for directory in std::fs::read_dir(corpus).unwrap() {
        let directory = directory.unwrap().path();
        if !directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("parser")
        {
            continue;
        }
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if !path.is_file() {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let whole = parse(&bytes, bytes.len().max(1), false);
            for chunk in [1, 7, 31, bytes.len().max(1)] {
                for batched in [false, true] {
                    assert_eq!(
                        whole,
                        parse(&bytes, chunk, batched),
                        "{} chunk={chunk} batched={batched}",
                        path.display()
                    );
                }
            }
            count += 1;
        }
    }
    assert!(
        count >= 666,
        "expected the complete baseline parser corpus, found {count}"
    );
}
