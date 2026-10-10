//! Image upload cost and retained source allocations across four live terminals.
use base64::Engine;
use criterion::{Criterion, Throughput};
use rustty_vt::Terminal;
use std::{
    collections::HashSet,
    fmt::Write,
    hint::black_box,
    sync::Arc,
    time::{Duration, Instant},
};

const TERMINALS: usize = 4;

struct Input {
    side: u32,
    pixels: Vec<Vec<u8>>,
    commands: Vec<Vec<u8>>,
}

impl Input {
    fn new(side: u32, duplicates: bool) -> Self {
        let pixels: Vec<_> = (0..TERMINALS)
            .map(|index| {
                let seed = if duplicates { 0 } else { index as u8 };
                (0..side as usize * side as usize)
                    .flat_map(|pixel| [pixel as u8, (pixel >> 8) as u8, seed, 255])
                    .collect::<Vec<_>>()
            })
            .collect();
        let commands = pixels
            .iter()
            .map(|pixels| {
                let encoded = base64::engine::general_purpose::STANDARD.encode(pixels);
                let mut command = String::new();
                for (index, chunk) in encoded.as_bytes().chunks(4096).enumerate() {
                    let more = usize::from((index + 1) * 4096 < encoded.len());
                    if index == 0 {
                        write!(
                            command,
                            "\x1b_Ga=t,i=1,f=32,s={side},v={side},q=2,m={more};"
                        )
                        .unwrap();
                    } else {
                        write!(command, "\x1b_Gq=2,m={more};").unwrap();
                    }
                    command.push_str(std::str::from_utf8(chunk).unwrap());
                    command.push_str("\x1b\\");
                }
                command.into_bytes()
            })
            .collect();
        Self {
            side,
            pixels,
            commands,
        }
    }

    fn terminals(&self) -> [Terminal; TERMINALS] {
        std::array::from_fn(|_| Terminal::new(80, 24, 0))
    }

    fn feed(&self, terminals: &mut [Terminal; TERMINALS]) {
        for (terminal, command) in terminals.iter_mut().zip(&self.commands) {
            black_box(terminal.feed(black_box(command)));
        }
    }

    fn check(&self, terminals: &[Terminal; TERMINALS]) -> usize {
        let mut sources = HashSet::new();
        for (terminal, pixels) in terminals.iter().zip(&self.pixels) {
            assert_eq!(terminal.graphics().images.len(), 1);
            let image = &terminal.graphics().images[&1];
            assert_eq!([image.width, image.height], [self.side; 2]);
            assert_eq!(&image.pixels[..], pixels.as_slice());
            sources.insert(image.pixels.as_ptr() as usize);
        }
        sources.len()
    }

    fn check_release(&self, terminals: [Terminal; TERMINALS]) {
        let weak: Vec<_> = terminals
            .iter()
            .map(|terminal| Arc::downgrade(&terminal.graphics().images[&1].pixels))
            .collect();
        drop(terminals);
        assert!(weak.iter().all(|pixels| pixels.strong_count() == 0));
        // Drop these probes too: the baseline uses Arc<[u8]> backing allocations.
        drop(weak);
    }
}

pub(super) fn uploads(c: &mut Criterion) {
    for side in [256, 1024] {
        for duplicates in [false, true] {
            let input = Input::new(side, duplicates);
            let name = format!(
                "{}_{side}px_4_terminals",
                if duplicates { "duplicate" } else { "unique" }
            );
            let mut terminals = input.terminals();
            input.feed(&mut terminals);
            let sources = input.check(&terminals);
            if !duplicates {
                assert_eq!(sources, TERMINALS);
            }
            eprintln!(
                "kitty_upload/{name}: sources={sources}, source_bytes={}",
                sources * side as usize * side as usize * 4
            );
            input.check_release(terminals);
            let mut group = c.benchmark_group("rustty/kitty_upload");
            group.throughput(Throughput::Bytes(
                TERMINALS as u64 * u64::from(side).pow(2) * 4,
            ));
            group.bench_function(&name, |b| {
                b.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        let mut terminals = input.terminals();
                        let start = Instant::now();
                        input.feed(&mut terminals);
                        elapsed += start.elapsed();
                        input.check_release(terminals);
                    }
                    elapsed
                });
            });
            group.finish();
            let mut terminals = input.terminals();
            input.feed(&mut terminals);
            assert_eq!(input.check(&terminals), sources);
            input.check_release(terminals);
        }
    }
}
