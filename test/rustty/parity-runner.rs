//! Run the differential suite without linking either terminal implementation.
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread::JoinHandle,
    time::Duration,
};

#[path = "parity-cases.rs"]
mod cases;
#[cfg(test)]
#[path = "parity-tests.rs"]
mod tests;

type Error = Box<dyn std::error::Error + Send + Sync>;
type Result<T> = std::result::Result<T, Error>;
const MAX_REQUEST: usize = 32 * 1024 * 1024;
const MAX_RESPONSE: usize = 128 * 1024 * 1024;
const GROUPS: &[&str] = &[
    "input",
    "parser",
    "grid",
    "page-layout",
    "pages",
    "unicode",
    "snapshots",
    "snapshot-wire",
    "protocols",
    "corpus",
    "osc",
];

struct Options {
    root: PathBuf,
    artifacts: PathBuf,
    fixtures: PathBuf,
    zig: PathBuf,
    rust: PathBuf,
    groups: BTreeSet<String>,
    filter: Option<String>,
    replay: Option<PathBuf>,
    minimize: bool,
    thorough: bool,
    no_build: bool,
    timeout: Duration,
    seed: String,
    generated: usize,
    max_failures: usize,
}

impl Options {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Option<Self>> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?;
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map_or_else(|| root.join("target"), |path| root.join(path));
        let mut result = Self {
            artifacts: root.join("target/parity"),
            fixtures: root.join("test/rustty/smoke.json"),
            zig: root.join("zig-out/bin/vt-oracle"),
            rust: target.join("release/examples/parity"),
            root,
            groups: BTreeSet::new(),
            filter: None,
            replay: None,
            minimize: false,
            thorough: false,
            no_build: false,
            timeout: Duration::from_secs(15),
            seed: "0".into(),
            generated: 0,
            max_failures: 20,
        };
        while let Some(arg) = args.next() {
            let (flag, inline) = arg
                .split_once('=')
                .map_or((arg.as_str(), None), |(k, v)| (k, Some(v)));
            let mut value = || -> Result<String> {
                inline
                    .map(str::to_owned)
                    .or_else(|| args.next())
                    .ok_or_else(|| format!("{flag} requires a value").into())
            };
            match flag {
                "--help" | "-h" => {
                    println!(
                        "Rustty differential runner\n\nOptions:\n  --no-build\n  --case TEXT\n  --replay PATH [--minimize]\n  --timeout SECONDS\n  --seed INTEGER --generated COUNT\n  --thorough\n  --max-failures COUNT\n  --artifacts PATH\n  --zig-bin PATH --rust-bin PATH\n  --fixtures PATH\n  --{}",
                        GROUPS.join("\n  --")
                    );
                    return Ok(None);
                }
                "--no-build" => result.no_build = true,
                "--thorough" => result.thorough = true,
                "--minimize" => result.minimize = true,
                "--case" => result.filter = Some(value()?),
                "--replay" => result.replay = Some(value()?.into()),
                "--artifacts" => result.artifacts = value()?.into(),
                "--fixtures" => result.fixtures = value()?.into(),
                "--zig-bin" => result.zig = value()?.into(),
                "--rust-bin" => result.rust = value()?.into(),
                "--seed" => result.seed = cases::seed(&value()?)?,
                "--generated" => result.generated = value()?.parse()?,
                "--max-failures" => result.max_failures = value()?.parse()?,
                "--timeout" => result.timeout = Duration::try_from_secs_f64(value()?.parse()?)?,
                _ if flag.starts_with("--") && GROUPS.contains(&&flag[2..]) => {
                    result.groups.insert(flag[2..].to_owned());
                }
                _ => return Err(format!("unknown option: {arg}").into()),
            }
        }
        if result.minimize && result.replay.is_none() {
            return Err("--minimize requires --replay".into());
        }
        if result.timeout.is_zero() || result.max_failures == 0 {
            return Err("timeout and max-failures must be positive".into());
        }
        result.artifacts = std::path::absolute(result.artifacts)?;
        Ok(Some(result))
    }

    fn selected(&self, group: &str) -> bool {
        self.thorough || self.groups.contains(group)
    }
}

fn read_json<T: DeserializeOwned>(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
    limit: usize,
) -> Result<Option<T>> {
    buffer.clear();
    let count = reader.take(limit as u64 + 1).read_until(b'\n', buffer)?;
    if count == 0 {
        return Ok(None);
    }
    if count > limit || !buffer.ends_with(b"\n") {
        return Err("response exceeded limit or was truncated".into());
    }
    Ok(Some(serde_json::from_slice(buffer)?))
}

struct Peer {
    name: &'static str,
    process: Child,
    requests: Option<SyncSender<Vec<u8>>>,
    responses: Receiver<(Vec<u8>, Result<Value>)>,
    worker: Option<JoinHandle<()>>,
    timeout: Duration,
    buffer: Vec<u8>,
}

impl Peer {
    fn spawn(
        name: &'static str,
        command: &mut Command,
        artifacts: &Path,
        timeout: Duration,
    ) -> Result<Self> {
        let mut process = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(File::create(artifacts.join(format!("{name}.log")))?)
            .spawn()?;
        let mut input = process.stdin.take().expect("piped stdin");
        let output = process.stdout.take().expect("piped stdout");
        let (requests, receiver) = mpsc::sync_channel::<Vec<u8>>(1);
        let (sender, responses) = mpsc::sync_channel(1);
        // The deadline covers writes as well as reads: a stopped oracle can
        // fill its input pipe before it ever produces a response.
        let worker = std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            let mut buffer = Vec::new();
            for request in receiver {
                let result = (|| {
                    input.write_all(&request)?;
                    input.flush()?;
                    read_json(&mut reader, &mut buffer, MAX_RESPONSE)?
                        .ok_or_else(|| format!("oracle exited; inspect {name}.log").into())
                })();
                let failed = result.is_err();
                if sender.send((request, result)).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            name,
            process,
            requests: Some(requests),
            responses,
            worker: Some(worker),
            timeout,
            buffer: Vec::new(),
        })
    }

    fn request(&mut self, request: &Value) -> Result<Value> {
        self.buffer.clear();
        serde_json::to_writer(&mut self.buffer, request)?;
        if self.buffer.len() > MAX_REQUEST {
            return Err(format!("{}: request exceeded {MAX_REQUEST} bytes", self.name).into());
        }
        self.buffer.push(b'\n');
        self.requests
            .as_ref()
            .ok_or("oracle is closed")?
            .send(std::mem::take(&mut self.buffer))?;
        let result = match self.responses.recv_timeout(self.timeout) {
            Ok((buffer, result)) => {
                self.buffer = buffer;
                result.map_err(|error| format!("{}: {error}", self.name).into())
            }
            Err(error) => Err(format!(
                "{}: response deadline/error after {:?}: {error}",
                self.name, self.timeout
            )
            .into()),
        };
        if result.is_err() {
            self.close();
        }
        result
    }

    fn close(&mut self) {
        self.requests.take();
        let _ = self.process.kill();
        let _ = self.process.wait();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.close();
    }
}

// Metadata exclusions apply only to the response root. Nested state is always
// compared, including booleans versus numbers and integer versus float values.
fn difference(left: &Value, right: &Value, path: &str, ignored: &[&str]) -> Option<String> {
    if left == right {
        return None;
    }
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => {
            let keep = |key: &&String| !ignored.contains(&key.as_str());
            if a.keys().filter(keep).ne(b.keys().filter(keep)) {
                return Some(format!("{path}: different fields"));
            }
            for (key, value) in a.iter().filter(|(key, _)| !ignored.contains(&key.as_str())) {
                if value != &b[key]
                    && let Some(reason) = difference(value, &b[key], &format!("{path}.{key}"), &[])
                {
                    return Some(reason);
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Some(format!(
                    "{path}: Zig length {}, Rust length {}",
                    a.len(),
                    b.len()
                ));
            }
            for (index, (a, b)) in a.iter().zip(b).enumerate() {
                if a != b
                    && let Some(reason) = difference(a, b, &format!("{path}[{index}]"), &[])
                {
                    return Some(reason);
                }
            }
        }
        _ => return Some(format!("{path}: Zig {left}, Rust {right}")),
    }
    None
}

fn ok(response: &Value) -> Result<bool> {
    response
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or_else(|| "oracle response lacks boolean ok field".into())
}

fn without(mut value: Value, keys: &[&str]) -> Result<Value> {
    let object = value
        .as_object_mut()
        .ok_or("oracle response is not an object")?;
    object.retain(|key, _| !keys.contains(&key.as_str()));
    Ok(value)
}

struct Comparison {
    values: [Value; 2],
    reason: Option<String>,
}

fn compare(
    request: &Value,
    mut send: impl FnMut(usize, &Value) -> Result<Value>,
) -> Result<Comparison> {
    if request.get("kind").and_then(Value::as_str) == Some("snapshot") {
        return compare_snapshot(request, send);
    }
    let mut sent = Cow::Borrowed(request);
    if request.get("expected_error").is_some() {
        sent.to_mut()
            .as_object_mut()
            .ok_or("request is not an object")?
            .remove("expected_error");
    }
    let values = [send(0, &sent)?, send(1, &sent)?];
    let mut reason = difference(&values[0], &values[1], "response", &["capabilities"]);
    let success = [ok(&values[0])?, ok(&values[1])?];
    if let Some(expected) = request
        .get("expected_error")
        .filter(|value| value.as_str().is_some_and(|s| !s.is_empty()))
    {
        if success.iter().any(|value| *value)
            || values
                .iter()
                .any(|value| value.get("err") != Some(expected))
        {
            reason.get_or_insert_with(|| format!("expected decoder rejection: {expected}"));
        }
    } else if success.iter().any(|value| !value) {
        reason.get_or_insert_with(|| {
            format!(
                "request failed: {} / {}",
                values[0]["err"], values[1]["err"]
            )
        });
    }
    Ok(Comparison { values, reason })
}

fn compare_snapshot(
    request: &Value,
    mut send: impl FnMut(usize, &Value) -> Result<Value>,
) -> Result<Comparison> {
    let mut args = request.clone();
    let object = args
        .as_object_mut()
        .ok_or("snapshot request is not an object")?;
    object.remove("after");
    object.insert("kind".into(), json!("terminal"));
    let prefix = request
        .get("operations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut suffix = vec![json!({"op":"observe"})];
    suffix.extend(
        request
            .get("after")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .cloned(),
    );
    let semantic = |value| without(value, &["id", "capabilities", "snapshots"]);
    args["operations"] = json!([prefix.clone(), vec![json!({"op":"snapshot"})]].concat());
    let encoded = [send(0, &args)?, send(1, &args)?];
    let mut values = [Value::Null, Value::Null];
    for (result, value) in values.iter_mut().zip(&encoded) {
        *result = json!({"id":request["id"],"ok":ok(value)?,"err":value["err"],"source":semantic(value.clone())?});
    }
    if encoded
        .iter()
        .map(ok)
        .collect::<Result<Vec<_>>>()?
        .iter()
        .any(|value| !value)
    {
        let reason = difference(&values[0], &values[1], "response", &[])
            .or_else(|| Some("snapshot encoding failed".into()));
        return Ok(Comparison { values, reason });
    }
    let mut payloads = serde_json::Map::new();
    for (name, value) in ["zig", "rust"].into_iter().zip(&encoded) {
        let snapshots = value["snapshots"]
            .as_array()
            .ok_or("snapshot encoder did not return payloads")?;
        if snapshots.len() != 1 {
            return Err("snapshot encoder did not return exactly one payload".into());
        }
        payloads.insert(name.into(), snapshots[0].clone());
    }
    let mut own_difference = None;
    for (index, result) in values.iter_mut().enumerate() {
        result["encodings"] = json!(payloads);
        args["operations"] = json!(
            [
                prefix.clone(),
                vec![json!({"op":"checkpoint"})],
                suffix.clone()
            ]
            .concat()
        );
        let live = send(index, &args)?;
        let live_ok = ok(&live)?;
        result["live"] = semantic(live)?;
        result["restored"] = json!({});
        // Retain native-first restore ordering even though JSON object keys sort.
        for source in ["zig", "rust"] {
            args["operations"] = json!(
                [
                    vec![json!({"op":"restore","data":payloads[source]})],
                    suffix.clone()
                ]
                .concat()
            );
            let restored = send(index, &args)?;
            result["ok"] = json!(ok(result)? && live_ok && ok(&restored)?);
            let restored = semantic(restored)?;
            if own_difference.is_none() {
                own_difference = difference(
                    &result["live"],
                    &restored,
                    &format!("{}.restore-{source}", ["zig", "rust"][index]),
                    &[],
                );
            }
            result["restored"][source] = restored;
        }
    }
    let reason = difference(&values[0], &values[1], "response", &["encodings"])
        .or(own_difference)
        .or_else(|| {
            values
                .iter()
                .any(|value| value["ok"] == false)
                .then(|| "snapshot phase failed".into())
        });
    Ok(Comparison { values, reason })
}

fn unhex(text: &str) -> Result<Vec<u8>> {
    let mut digits = text.bytes();
    let mut result = Vec::with_capacity(text.len() / 2);
    while let Some(high) = digits.find(|byte| !byte.is_ascii_whitespace()) {
        let low = digits.next().ok_or("odd hex payload length")?;
        let a = (high as char).to_digit(16).ok_or("invalid hex payload")?;
        let b = (low as char).to_digit(16).ok_or("invalid hex payload")?;
        result.push((a * 16 + b) as u8);
    }
    Ok(result)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(DIGITS[usize::from(byte >> 4)] as char);
        result.push(DIGITS[usize::from(byte & 15)] as char);
    }
    result
}

// Generate each variant only when it is needed; exhaustive splits must not
// multiply the resident size of large capture-limit fixtures.
fn each_variant(
    request: &Value,
    exhaustive: bool,
    replay: bool,
    mut check: impl FnMut(&Value) -> Result<bool>,
) -> Result<()> {
    if !check(request)?
        || replay
        || request
            .get("expected_error")
            .is_some_and(|value| value.as_str().is_some_and(|s| !s.is_empty()))
    {
        return Ok(());
    }
    let kind = request
        .get("kind")
        .map_or("terminal", |value| value.as_str().unwrap_or(""));
    if !["terminal", "input", "parser", "snapshot"].contains(&kind) {
        return Ok(());
    }
    let id = request["id"].as_str().ok_or("request lacks string id")?;
    for name in ["scalar", "chunks"] {
        let mut variant = request.clone();
        variant["id"] = json!(format!("{id}/{name}"));
        variant["scalar"] = json!(name == "scalar");
        if name == "chunks" {
            for field in ["operations", "after"] {
                let Some(operations) = request.get(field).and_then(Value::as_array) else {
                    continue;
                };
                let mut split = Vec::new();
                for operation in operations {
                    if operation["op"] != "write" {
                        split.push(operation.clone());
                        continue;
                    }
                    let data = unhex(operation["data"].as_str().ok_or("write lacks hex data")?)?;
                    let sizes = if data.len() > 4096 {
                        [2, 4096, 1, 16384, 7]
                    } else {
                        [2, 7, 1, 13, 4]
                    };
                    let mut pos = 0;
                    for size in sizes.into_iter().cycle() {
                        if pos >= data.len() {
                            break;
                        }
                        let end = (pos + size).min(data.len());
                        split.push(json!({"op":"write","data":hex(&data[pos..end])}));
                        pos = end;
                    }
                }
                variant[field] = json!(split);
            }
        }
        if !check(&variant)? {
            return Ok(());
        }
    }
    if exhaustive {
        for field in ["operations", "after"] {
            let Some(operations) = request.get(field).and_then(Value::as_array) else {
                continue;
            };
            for (index, operation) in operations.iter().enumerate() {
                if operation["op"] != "write" {
                    continue;
                }
                let data = unhex(operation["data"].as_str().ok_or("write lacks hex data")?)?;
                if data.len() > 64 {
                    continue;
                }
                for split in 1..data.len() {
                    let mut variant = request.clone();
                    variant["id"] = json!(format!("{id}/split-{field}-{index}-{split}"));
                    variant[field] = json!(
                        [
                            operations[..index].to_vec(),
                            vec![
                                json!({"op":"write","data":hex(&data[..split])}),
                                json!({"op":"write","data":hex(&data[split..])})
                            ],
                            operations[index + 1..].to_vec(),
                        ]
                        .concat()
                    );
                    if !check(&variant)? {
                        return Ok(());
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct Fixture {
    request: Value,
    covers: Vec<String>,
}

struct Runner {
    options: Options,
    peers: [Peer; 2],
    capabilities: [BTreeSet<String>; 2],
    covered: BTreeSet<String>,
    checked: usize,
    failures: usize,
    aborted: bool,
}

impl Runner {
    fn stopped(&self) -> bool {
        self.aborted || self.failures >= self.options.max_failures
    }

    fn fixture(&mut self, fixture: Fixture) -> Result<()> {
        if self.stopped() {
            return Ok(());
        }
        let id = fixture.request["id"]
            .as_str()
            .ok_or("fixture lacks string id")?;
        if self.options.replay.is_none()
            && self
                .options
                .filter
                .as_ref()
                .is_some_and(|filter| !id.contains(filter))
        {
            return Ok(());
        }
        let mut success = true;
        let mut baseline: Option<[Value; 2]> = None;
        each_variant(
            &fixture.request,
            self.options.thorough,
            self.options.replay.is_some(),
            |request| {
                let mut comparison =
                    match compare(request, |index, request| self.peers[index].request(request)) {
                        Ok(result) => result,
                        Err(error) => {
                            self.aborted = true;
                            Comparison {
                                values: [Value::Null, Value::Null],
                                reason: Some(error.to_string()),
                            }
                        }
                    };
                if comparison.reason.is_none()
                    && let Some(baseline) = &baseline
                {
                    for (index, (original, value)) in
                        baseline.iter().zip(&comparison.values).enumerate()
                    {
                        if let Some(reason) = difference(
                            original,
                            value,
                            &format!("{}.delivery", ["zig", "rust"][index]),
                            &["id", "capabilities", "encodings"],
                        ) {
                            comparison.reason = Some(reason);
                            break;
                        }
                    }
                }
                self.checked += 1;
                if self.checked % 100 == 0 {
                    println!(
                        "Checked {} comparisons ({})",
                        self.checked,
                        request["id"].as_str().unwrap_or("")
                    );
                }
                if let Some(reason) = &comparison.reason {
                    success = false;
                    self.failures += 1;
                    let path = self.save_failure(request, &comparison, reason)?;
                    println!(
                        "FAIL {}: {reason}\n  {}",
                        request["id"].as_str().unwrap_or(""),
                        path.display()
                    );
                    if self.options.minimize
                        && comparison.values.iter().all(|value| value["ok"] == true)
                    {
                        let (reduced, result, attempts) =
                            minimize(request, &mut |index, request| {
                                self.peers[index].request(request)
                            })?;
                        let path = self.save_failure(
                            &reduced,
                            &result,
                            result.reason.as_deref().unwrap_or("difference disappeared"),
                        )?;
                        println!("  Minimized in {attempts} comparisons: {}", path.display());
                    }
                }
                if baseline.is_none() && !self.aborted {
                    let [left, right] = comparison.values;
                    baseline = Some([
                        without(left, &["id", "capabilities", "encodings"])?,
                        without(right, &["id", "capabilities", "encodings"])?,
                    ]);
                }
                io::stdout().flush()?;
                Ok(!self.stopped())
            },
        )?;
        if success {
            self.covered.extend(fixture.covers);
        }
        Ok(())
    }

    fn save_failure(
        &self,
        request: &Value,
        comparison: &Comparison,
        reason: &str,
    ) -> Result<PathBuf> {
        let root = self.options.artifacts.join("failures");
        fs::create_dir_all(&root)?;
        // Keep earlier failures, including repeated runs with the same case ID.
        let mut index = self.checked;
        let path = loop {
            let path = root.join(format!("{index:06}"));
            match fs::create_dir(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => index += 1,
                Err(error) => return Err(error.into()),
            }
        };
        for (name, value) in [
            ("request", request),
            ("zig", &comparison.values[0]),
            ("rust", &comparison.values[1]),
        ] {
            let mut file = File::create(path.join(format!("{name}.json")))?;
            serde_json::to_writer_pretty(&mut file, value)?;
            writeln!(file)?;
        }
        fs::write(path.join("difference.txt"), format!("{reason}\n"))?;
        Ok(path
            .strip_prefix(&self.options.root)
            .unwrap_or(&path)
            .to_owned())
    }
}

fn reduce<T: Clone>(
    mut sequence: Vec<T>,
    attempts: &mut usize,
    mut fails: impl FnMut(&[T]) -> Result<bool>,
) -> Result<Vec<T>> {
    let mut partitions = 2;
    while !sequence.is_empty() && *attempts < 2000 {
        let size = sequence.len().div_ceil(partitions).max(1);
        let mut changed = false;
        for start in (0..sequence.len()).step_by(size) {
            if *attempts >= 2000 {
                break;
            }
            let reduced = [
                sequence[..start].to_vec(),
                sequence[(start + size).min(sequence.len())..].to_vec(),
            ]
            .concat();
            *attempts += 1;
            if fails(&reduced)? {
                sequence = reduced;
                partitions = partitions.saturating_sub(1).max(2);
                changed = true;
                break;
            }
        }
        if !changed {
            if partitions >= sequence.len() {
                break;
            }
            partitions = (partitions * 2).min(sequence.len());
        }
    }
    Ok(sequence)
}

fn minimize(
    request: &Value,
    send: &mut impl FnMut(usize, &Value) -> Result<Value>,
) -> Result<(Value, Comparison, usize)> {
    let mut request = request.clone();
    request["id"] = json!(format!(
        "{}/minimized",
        request["id"].as_str().ok_or("request lacks string id")?
    ));
    let numbers = regex::Regex::new(r"\[\d+\]")?;
    let signature = |reason: &Option<String>| {
        reason.as_ref().map(|reason| {
            numbers
                .replace_all(reason.split(':').next().unwrap_or(""), "[]")
                .into_owned()
        })
    };
    let original = compare(&request, &mut *send)?;
    if original.reason.is_none() || original.values.iter().any(|value| value["ok"] != true) {
        return Err("minimization requires a reproducible successful state mismatch".into());
    }
    let original = signature(&original.reason);
    let mut fails = |candidate: &Value| -> Result<bool> {
        let result = compare(candidate, &mut *send)?;
        Ok(result.values.iter().all(|value| value["ok"] == true)
            && signature(&result.reason) == original)
    };
    let mut attempts = 0;
    let operations = request
        .get("operations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let operations = reduce(operations, &mut attempts, |operations| {
        let mut candidate = request.clone();
        candidate["operations"] = json!(operations);
        fails(&candidate)
    })?;
    request["operations"] = json!(operations);
    for index in 0..operations.len() {
        if request["operations"][index]["op"] != "write" {
            continue;
        }
        let bytes = unhex(
            request["operations"][index]["data"]
                .as_str()
                .ok_or("write lacks hex data")?,
        )?;
        let bytes = reduce(bytes, &mut attempts, |bytes| {
            let mut candidate = request.clone();
            candidate["operations"][index] = json!({"op":"write","data":hex(bytes)});
            fails(&candidate)
        })?;
        request["operations"][index] = json!({"op":"write","data":hex(&bytes)});
    }
    let result = compare(&request, send)?;
    Ok((request, result, attempts))
}

fn coverage_gaps(
    manifest: &Value,
    capabilities: &[BTreeSet<String>; 2],
    covered: &BTreeSet<String>,
) -> Result<Vec<String>> {
    let mut missing = Vec::new();
    for entry in manifest["requirements"]
        .as_array()
        .ok_or("coverage manifest lacks requirements")?
    {
        let id = entry["id"]
            .as_str()
            .ok_or("coverage requirement lacks id")?;
        if entry["status"] != "complete" {
            missing.push(format!(
                "{id}: {} — {}",
                entry["status"], entry["remaining"]
            ));
        } else if !capabilities.iter().all(|peer| peer.contains(id)) {
            missing.push(format!("{id}: an oracle lacks this capability"));
        } else if !covered.contains(id) {
            missing.push(format!("{id}: no passing coverage case in this run"));
        }
    }
    Ok(missing)
}

fn run(options: Options) -> Result<bool> {
    fs::create_dir_all(&options.artifacts)?;
    // A setup failure must not leave a previous run's passing summary behind.
    fs::write(
        options.artifacts.join("summary.json"),
        b"{\"aborted\":true,\"full_parity\":false}\n",
    )?;
    if !options.no_build {
        let mut zig = Command::new("zig");
        zig.current_dir(&options.root).args([
            "build",
            "vt-oracle",
            "-Demit-lib-vt=true",
            "-Demit-macos-app=false",
        ]);
        if options.selected("pages") || options.selected("corpus") || options.selected("osc") {
            zig.arg("-Doptimize=ReleaseSafe");
        }
        if !zig.status()?.success()
            || !Command::new("cargo")
                .current_dir(&options.root)
                .args([
                    "+1.95.0",
                    "build",
                    "--release",
                    "--offline",
                    "-p",
                    "rustty-vt",
                    "--example",
                    "parity",
                ])
                .status()?
                .success()
        {
            return Err("oracle build failed".into());
        }
    }
    let peers = [
        Peer::spawn(
            "zig",
            Command::new(options.zig.canonicalize()?).current_dir(&options.root),
            &options.artifacts,
            options.timeout,
        )?,
        Peer::spawn(
            "rust",
            Command::new(options.rust.canonicalize()?).current_dir(&options.root),
            &options.artifacts,
            options.timeout,
        )?,
    ];
    let mut runner = Runner {
        options,
        peers,
        capabilities: Default::default(),
        covered: BTreeSet::new(),
        checked: 0,
        failures: 0,
        aborted: false,
    };
    let execution = (|| -> Result<()> {
        for (peer, capabilities) in runner.peers.iter_mut().zip(&mut runner.capabilities) {
            let response = peer.request(&json!({"kind":"capabilities"}))?;
            if !ok(&response)? {
                return Err("capability query failed".into());
            }
            *capabilities = serde_json::from_value(
                response
                    .get("capabilities")
                    .ok_or("oracle lacks capabilities")?
                    .clone(),
            )?;
        }
        cases::run(&mut runner)
    })();
    let error = execution.err().map(|error| error.to_string());
    if let Some(error) = &error {
        runner.aborted = true;
        eprintln!("Parity runner: {error}");
    }
    let gaps = if runner.options.thorough {
        let manifest: Value = serde_json::from_reader(File::open(
            runner.options.root.join("test/rustty/coverage.json"),
        )?)?;
        coverage_gaps(&manifest, &runner.capabilities, &runner.covered)?
    } else {
        Vec::new()
    };
    for gap in &gaps {
        println!("COVERAGE GAP {gap}");
    }
    let passed = !runner.aborted && runner.failures == 0 && gaps.is_empty() && runner.checked != 0;
    let full = runner.options.thorough && passed;
    let summary = json!({"mode":if runner.options.thorough {"thorough"} else {"smoke"},"checked":runner.checked,
        "failures":runner.failures,"coverage_gaps":gaps,"covered":runner.covered,"full_parity":full,
        "aborted":runner.aborted,"error":error});
    let mut file = File::create(runner.options.artifacts.join("summary.json"))?;
    serde_json::to_writer_pretty(&mut file, &summary)?;
    writeln!(file)?;
    println!(
        "{} comparisons, {} failures, {} coverage gaps; {}",
        runner.checked,
        runner.failures,
        gaps.len(),
        if full {
            "full parity verified"
        } else {
            "full parity not established"
        }
    );
    Ok(passed)
}

fn main() {
    match Options::parse(std::env::args().skip(1)).and_then(|options| options.map_or(Ok(true), run))
    {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(error) => {
            eprintln!("Parity runner: {error}");
            std::process::exit(1);
        }
    }
}
