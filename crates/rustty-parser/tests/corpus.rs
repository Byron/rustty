use rustty_parser::Parser;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

fn parse(bytes: &[u8], chunk: usize) -> (u64, bool) {
    let mut parser = Parser::new();
    let mut hash = DefaultHasher::new();
    for bytes in bytes.chunks(chunk) {
        parser.advance(bytes, |event| event.hash(&mut hash));
    }
    (hash.finish(), parser.is_ground())
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
            let whole = parse(&bytes, bytes.len().max(1));
            for chunk in [1, 7, 31] {
                assert_eq!(
                    whole,
                    parse(&bytes, chunk),
                    "{} chunk={chunk}",
                    path.display()
                );
            }
            count += 1;
        }
    }
    assert!(
        count >= 666,
        "expected the complete baseline parser corpus, found {count}"
    );
}
