//! Preserved generator output, plus Python-compatible seeded random cases.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    format: u32,
    cases: usize,
    inputs: BTreeMap<String, u32>,
    directories: BTreeMap<String, Vec<String>>,
    probes: Vec<Probe>,
}

#[derive(Deserialize)]
struct Probe {
    request: Value,
    response: Value,
}

pub(super) fn run(runner: &mut Runner) -> Result<()> {
    if let Some(replay) = &runner.options.replay {
        let request = serde_json::from_reader(File::open(replay)?)?;
        return runner.fixture(Fixture {
            request,
            covers: Vec::new(),
        });
    }
    let fixtures: Vec<Fixture> = serde_json::from_reader(File::open(&runner.options.fixtures)?)?;
    for fixture in fixtures {
        runner.fixture(fixture)?;
        if runner.stopped() {
            return Ok(());
        }
    }
    let mut checksums = BTreeMap::new();
    // Match the reference runner's group ordering, including its extra
    // retained-history search cases only under the thorough coverage gate.
    for group in [
        "input",
        "parser",
        "grid",
        "page-layout",
        "pages",
        "thorough",
        "unicode",
        "snapshots",
        "snapshot-wire",
        "protocols",
        "corpus",
        "osc",
    ] {
        if runner.options.selected(group) {
            archive(runner, group, &mut checksums)?;
            if runner.stopped() {
                return Ok(());
            }
        }
    }
    let count = if runner.options.generated == 0 && runner.options.thorough {
        100
    } else {
        runner.options.generated
    };
    let mut random = Random::new(&runner.options.seed)?;
    for index in 0..count {
        let request = generated(&mut random, &runner.options.seed, index);
        runner.fixture(Fixture {
            request,
            covers: Vec::new(),
        })?;
        if runner.stopped() {
            break;
        }
    }
    Ok(())
}

fn archive(runner: &mut Runner, group: &str, checksums: &mut BTreeMap<String, u32>) -> Result<()> {
    let path = runner
        .options
        .root
        .join(format!("test/rustty/fixtures/{group}.jsonl.gz"));
    let mut reader = BufReader::new(flate2::read::MultiGzDecoder::new(File::open(&path)?));
    let mut buffer = Vec::new();
    let header: Header =
        read_json(&mut reader, &mut buffer, MAX_RESPONSE)?.ok_or("empty fixture archive")?;
    if header.format != 1 {
        return Err("unsupported fixture archive version".into());
    }
    for (path, expected) in header.inputs {
        let actual = if let Some(&crc) = checksums.get(&path) {
            crc
        } else {
            let mut crc = flate2::Crc::new();
            crc.update(&fs::read(runner.options.root.join(&path))?);
            checksums.insert(path.clone(), crc.sum());
            crc.sum()
        };
        if actual != expected {
            return Err(format!(
                "fixture input changed: {path}; run test/rustty/preserve_parity_fixtures.py"
            )
            .into());
        }
    }
    for (path, expected) in header.directories {
        let mut actual = Vec::new();
        for entry in fs::read_dir(runner.options.root.join(&path))? {
            let entry = entry?;
            if entry.path().is_file() {
                actual.push(
                    entry
                        .file_name()
                        .into_string()
                        .map_err(|_| "non-UTF-8 corpus filename")?,
                );
            }
        }
        actual.sort();
        if actual != expected {
            return Err(
                format!("fixture corpus membership changed: {path}; regenerate fixtures").into(),
            );
        }
    }
    for probe in header.probes {
        let actual = runner.peers[0].request(&probe.request)?;
        if let Some(reason) = difference(
            &probe.response,
            &actual,
            "native fixture reference",
            &["capabilities"],
        ) {
            return Err(format!(
                "{group}: {reason} ({}); regenerate fixtures",
                probe.request["id"]
            )
            .into());
        }
    }
    let mut count = 0;
    while let Some(fixture) = read_json::<Fixture>(&mut reader, &mut buffer, MAX_RESPONSE)? {
        count += 1;
        // Preserve the old selector guard around the DND snapshot generator.
        let excluded = fixture.request["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("protocol/dnd/snapshot/"))
            && runner.options.filter.as_ref().is_some_and(|filter| {
                !"protocol/dnd/snapshot/".contains(filter)
                    && !filter.contains("protocol/dnd/snapshot")
            });
        if !excluded {
            runner.fixture(fixture)?;
        }
        if runner.stopped() {
            return Ok(());
        }
    }
    if count != header.cases {
        return Err(format!("{group}: expected {} fixtures, read {count}", header.cases).into());
    }
    Ok(())
}

pub(super) fn seed(text: &str) -> Result<String> {
    let text = text.trim();
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("seed must be an integer".into());
    }
    let digits = digits.trim_start_matches('0');
    Ok(if digits.is_empty() {
        "0".into()
    } else {
        format!("{}{digits}", if text.starts_with('-') { "-" } else { "" })
    })
}

// CPython's integer-seeded MT19937 and getrandbits rejection sampling. Keeping
// this small generator live preserves --seed/--generated beyond frozen cases.
struct Random {
    words: [u32; 624],
    index: usize,
}

impl Random {
    fn new(text: &str) -> Result<Self> {
        let text = seed(text)?;
        let mut key = vec![0u32];
        for digit in text.trim_start_matches('-').bytes() {
            let mut carry = u64::from(digit - b'0');
            for word in &mut key {
                carry += u64::from(*word) * 10;
                *word = carry as u32;
                carry >>= 32;
            }
            if carry != 0 {
                key.push(carry as u32);
            }
        }
        let mut words = [0u32; 624];
        words[0] = 19650218;
        for index in 1..624 {
            words[index] = (words[index - 1] ^ (words[index - 1] >> 30))
                .wrapping_mul(1812433253)
                .wrapping_add(index as u32);
        }
        let (mut i, mut j) = (1, 0);
        for _ in 0..624.max(key.len()) {
            words[i] = (words[i] ^ (words[i - 1] ^ (words[i - 1] >> 30)).wrapping_mul(1664525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i == 624 {
                words[0] = words[623];
                i = 1;
            }
            if j == key.len() {
                j = 0;
            }
        }
        for _ in 0..623 {
            words[i] = (words[i] ^ (words[i - 1] ^ (words[i - 1] >> 30)).wrapping_mul(1566083941))
                .wrapping_sub(i as u32);
            i += 1;
            if i == 624 {
                words[0] = words[623];
                i = 1;
            }
        }
        words[0] = 0x80000000;
        Ok(Self { words, index: 624 })
    }

    fn next(&mut self) -> u32 {
        if self.index == 624 {
            for i in 0..624 {
                let y = (self.words[i] & 0x80000000) | (self.words[(i + 1) % 624] & 0x7fffffff);
                self.words[i] = self.words[(i + 397) % 624] ^ (y >> 1) ^ (0x9908b0df * (y & 1));
            }
            self.index = 0;
        }
        let mut y = self.words[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c5680;
        y ^= (y << 15) & 0xefc60000;
        y ^ (y >> 18)
    }

    fn below(&mut self, limit: u32) -> u32 {
        let shift = limit.leading_zeros();
        loop {
            let value = self.next() >> shift;
            if value < limit {
                return value;
            }
        }
    }
}

fn generated(random: &mut Random, seed: &str, index: usize) -> Value {
    let writes = [
        "hello",
        "é界👩🏽‍💻",
        "\r",
        "\n",
        "\t",
        "\x08",
        "\x1b[2J",
        "\x1b[K",
        "\x1b[1;1H",
        "\x1b[31m",
        "\x1b[0m",
        "\x1b[?1049h",
        "\x1b[?1049l",
        "\x1b[2;3r",
        "\x1b[r",
        "\x1b[4h",
        "\x1b[4l",
        "\x1b[2P",
        "\x1b[2@",
    ];
    let mut operations = Vec::new();
    for _ in 0..30 {
        operations.push(if random.below(8) == 0 {
            json!({"op":"resize","cols":3 + random.below(28),"rows":2 + random.below(7)})
        } else { json!({"op":"write","data":hex(writes[random.below(writes.len() as u32) as usize].as_bytes())}) });
        if random.below(4) == 0 {
            operations.push(json!({"op":"observe"}));
        }
    }
    json!({"id":format!("generated/{seed}/{index}"),"cols":12,"rows":4,"operations":operations})
}

#[test]
fn generated_cases_match_python() {
    // CRCs of 100 reference requests each, sorted-key compact JSON plus LF,
    // produced by parity_reference.generated_requests (CPython 3.14).
    for (seed, expected) in [
        ("0", 966954647),
        ("41", 2985979900),
        ("42", 3380766153),
        ("-42", 2931399512),
        ("1208925819614629174718521", 1564143314),
        (
            "-1606938044258990275541962092341162602522202993782792835308165",
            3217077048,
        ),
    ] {
        let mut random = Random::new(seed).unwrap();
        let mut crc = flate2::Crc::new();
        for index in 0..100 {
            crc.update(&serde_json::to_vec(&generated(&mut random, seed, index)).unwrap());
            crc.update(b"\n");
        }
        assert_eq!(crc.sum(), expected, "seed {seed}");
    }
}

#[test]
fn preserved_corpora_keep_their_original_selector_contracts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (group, prefix, offset, operation) in [
        ("corpus", "corpus/", 1, "write"),
        ("parser", "parser/corpus/", 0, "write"),
        ("osc", "osc/corpus/", 0, "osc"),
    ] {
        let mut reader = BufReader::new(flate2::read::MultiGzDecoder::new(
            File::open(root.join(format!("test/rustty/fixtures/{group}.jsonl.gz"))).unwrap(),
        ));
        let mut buffer = Vec::new();
        let _: Header = read_json(&mut reader, &mut buffer, MAX_RESPONSE)
            .unwrap()
            .unwrap();
        let mut checked = 0;
        while let Some(fixture) =
            read_json::<Fixture>(&mut reader, &mut buffer, MAX_RESPONSE).unwrap()
        {
            let Some(path) = fixture.request["id"].as_str().unwrap().strip_prefix(prefix) else {
                continue;
            };
            let bytes = fs::read(root.join("test/fuzz-libghostty/corpus").join(path)).unwrap();
            let expected = &bytes[offset.min(bytes.len())..];
            let captured: Vec<_> = fixture.request["operations"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|op| op["op"] == operation)
                .collect();
            assert_eq!(
                json!(captured),
                json!([{"op":operation,"data":hex(expected)}])
            );
            checked += 1;
        }
        assert!(checked > 0);
    }
}
