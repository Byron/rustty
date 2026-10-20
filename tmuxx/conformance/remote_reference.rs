//! Portable test-only client for the pinned, leased SSH C oracle.
//! No production crate or C code is linked into this test transport.
use std::{
    cell::Cell,
    io::{BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio},
    sync::{Arc, Mutex},
};

const MAX_FRAME: usize = 64 * 1024 * 1024;

struct Connection {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    identity: Vec<u8>,
}

#[derive(Clone)]
pub struct RemoteReference(Arc<Mutex<Connection>>);

#[allow(dead_code)]
pub struct RemoteControl {
    reference: RemoteReference,
    pub id: u32,
    pub pid: u32,
    pub name: Vec<u8>,
    closed: Cell<bool>,
}

impl RemoteReference {
    #[allow(dead_code)] // Startup-only fixtures use the explicit environment/configuration constructors.
    pub fn new() -> Self {
        Self::new_with_environment(&[])
    }

    pub fn new_with_environment(environment: &[(&[u8], Option<&[u8]>)]) -> Self {
        Self::start(environment, None)
    }

    #[allow(dead_code)]
    pub fn new_with_configuration(source: &[u8]) -> Self {
        Self::start(&[], Some(source))
    }

    fn start(environment: &[(&[u8], Option<&[u8]>)], configuration: Option<&[u8]>) -> Self {
        let config = std::env::var_os("TMUXX_REMOTE_C_CONFIG")
            .expect("set TMUXX_REMOTE_C_CONFIG to the explicit pinned SSH oracle configuration");
        let helper = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../conformance/remote_c_reference.py");
        let mut child = Command::new(
            std::env::var_os("TMUXX_REMOTE_PYTHON").unwrap_or_else(|| "python".into()),
        )
        .arg(helper)
        .args(["--config".as_ref(), config.as_os_str()])
        .args((!environment.is_empty()).then_some("--environment-stdin"))
        .args(configuration.is_some().then_some("--configuration-stdin"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start remote C oracle helper");
        let input = child.stdin.take();
        let output = BufReader::new(child.stdout.take().unwrap());
        let reference = Self(Arc::new(Mutex::new(Connection {
            child,
            input,
            output,
            identity: Vec::new(),
        })));
        let mut connection = reference.0.lock().unwrap();
        if !environment.is_empty() || configuration.is_some() {
            let mut payload = vec![if configuration.is_some() { 22 } else { 21 }, 0, 0, 0, 0];
            field(
                &mut payload,
                configuration.unwrap_or(&environment_packet(environment)),
            );
            let input = connection.input.as_mut().unwrap();
            input
                .write_all(&(payload.len() as u32).to_be_bytes())
                .unwrap();
            input.write_all(&payload).unwrap();
            input.flush().unwrap();
        }
        let ready = receive(&mut connection.output);
        assert!(
            ready.status.success(),
            "pinned C identity rejected: {ready:?}"
        );
        connection.identity = ready.stdout;
        drop(connection);
        reference
    }

    #[allow(dead_code)]
    pub fn identity(&self) -> Vec<u8> {
        self.0.lock().unwrap().identity.clone()
    }

    #[allow(dead_code)]
    pub fn startup_client(&self) -> RemoteControl {
        self.control_result(self.request(24, &[], &[]))
    }

    #[allow(dead_code)]
    pub fn malformed_startup_clients(&self) {
        let output = self.request(23, &[], &[]);
        assert!(
            output.status.success(),
            "malformed startup clients: {output:?}"
        );
        assert_eq!(output.stdout, b"140");
    }

    #[allow(dead_code)] // Observer-only targets do not send direct C commands.
    pub fn output(&self, arguments: &[&[u8]], input: &[u8]) -> Output {
        self.request(1, arguments, input)
    }

    #[allow(dead_code)]
    pub fn output_with_environment(
        &self,
        arguments: &[&[u8]],
        input: &[u8],
        environment: &[(&[u8], Option<&[u8]>)],
    ) -> Output {
        let mut data = environment_packet(environment);
        data.extend_from_slice(input);
        self.request(20, arguments, &data)
    }

    #[allow(dead_code)] // The Unicode target needs commands/stdin, not source-file.
    pub fn source(&self, source: &[u8]) -> Output {
        self.request(2, &[], source)
    }

    #[allow(dead_code)]
    pub fn put(&self, leaf: &[u8], contents: &[u8]) -> Vec<u8> {
        let output = self.request(5, &[leaf], contents);
        assert!(output.status.success(), "remote file upload: {output:?}");
        output.stdout
    }

    #[allow(dead_code)]
    pub fn get(&self, leaf: &[u8]) -> Vec<u8> {
        let output = self.request(6, &[leaf], &[]);
        assert!(output.status.success(), "remote file read: {output:?}");
        output.stdout
    }

    #[allow(dead_code)]
    pub fn exists(&self, leaf: &[u8]) -> bool {
        let output = self.request(7, &[leaf], &[]);
        assert!(output.status.success(), "remote file existence: {output:?}");
        assert!(
            output.stdout == b"0" || output.stdout == b"1",
            "invalid existence response"
        );
        output.stdout == b"1"
    }

    #[allow(dead_code)]
    pub fn path(&self, leaf: &[u8]) -> Vec<u8> {
        let output = self.request(8, &[leaf], &[]);
        assert!(output.status.success(), "remote file path: {output:?}");
        output.stdout
    }

    #[allow(dead_code)]
    pub fn observe(&self, name: &str, input: &[u8]) -> Output {
        self.request(9, &[name.as_bytes()], input)
    }

    #[allow(dead_code)]
    pub fn control_open(&self, session: &str) -> RemoteControl {
        self.control_open_args(&[b"attach-session", b"-t", session.as_bytes()])
    }

    #[allow(dead_code)]
    pub fn control_open_args(&self, arguments: &[&[u8]]) -> RemoteControl {
        self.process_open(arguments, true)
    }

    #[allow(dead_code)]
    pub fn control_open_with_environment(
        &self,
        arguments: &[&[u8]],
        environment: &[(&[u8], Option<&[u8]>)],
    ) -> RemoteControl {
        let mut args = vec![b"control".as_slice()];
        args.extend_from_slice(arguments);
        self.control_result(self.request(25, &args, &environment_packet(environment)))
    }

    #[allow(dead_code)]
    pub fn command_open_args(&self, arguments: &[&[u8]]) -> RemoteControl {
        self.process_open(arguments, false)
    }

    fn process_open(&self, arguments: &[&[u8]], control: bool) -> RemoteControl {
        let mut args = vec![if control {
            b"control".as_slice()
        } else {
            b"command"
        }];
        args.extend_from_slice(arguments);
        self.control_result(self.request(10, &args, &[]))
    }

    fn control_result(&self, output: Output) -> RemoteControl {
        assert!(
            output.status.success() && output.stdout.len() >= 8,
            "remote C client open: {output:?}"
        );
        RemoteControl {
            reference: self.clone(),
            id: u32::from_be_bytes(output.stdout[..4].try_into().unwrap()),
            pid: u32::from_be_bytes(output.stdout[4..8].try_into().unwrap()),
            name: output.stdout[8..].to_vec(),
            closed: Cell::new(false),
        }
    }

    #[allow(dead_code)]
    pub fn directory(&self) -> Vec<u8> {
        let output = self.request(17, &[], &[]);
        assert!(
            output.status.success(),
            "remote fixture directory: {output:?}"
        );
        output.stdout
    }

    #[allow(dead_code)]
    pub fn server_alive(&self) -> bool {
        let output = self.request(18, &[], &[]);
        assert!(
            output.status.success() && (output.stdout == b"0" || output.stdout == b"1"),
            "remote server identity: {output:?}"
        );
        output.stdout == b"1"
    }

    #[allow(dead_code)]
    pub fn watch_process(&self, pid: u32) -> u32 {
        let output = self.request(15, &[pid.to_string().as_bytes()], &[]);
        assert!(
            output.status.success(),
            "remote owned process observation: {output:?}"
        );
        String::from_utf8(output.stdout).unwrap().parse().unwrap()
    }

    #[allow(dead_code)]
    pub fn process_alive(&self, observation: u32) -> bool {
        let output = self.request(16, &[observation.to_string().as_bytes()], &[]);
        assert!(
            output.status.success() && (output.stdout == b"0" || output.stdout == b"1"),
            "remote process identity: {output:?}"
        );
        output.stdout == b"1"
    }

    fn request(&self, operation: u8, args: &[&[u8]], data: &[u8]) -> Output {
        assert!(args.len() <= 256, "oracle argv exceeds count bound");
        let mut payload = vec![operation];
        payload.extend_from_slice(&(args.len() as u32).to_be_bytes());
        for argument in args {
            assert!(!argument.contains(&0), "oracle argv contains NUL");
            field(&mut payload, argument);
        }
        field(&mut payload, data);
        let mut connection = self.0.lock().unwrap();
        let input = connection.input.as_mut().expect("closed oracle connection");
        input
            .write_all(&(payload.len() as u32).to_be_bytes())
            .unwrap();
        input.write_all(&payload).unwrap();
        input.flush().unwrap();
        receive(&mut connection.output)
    }
}

#[allow(dead_code)]
impl RemoteControl {
    pub fn write(&self, bytes: &[u8]) -> Output {
        self.reference
            .request(11, &[self.id.to_string().as_bytes()], bytes)
    }

    pub fn read(&self) -> Output {
        self.reference
            .request(12, &[self.id.to_string().as_bytes()], &[])
    }

    pub fn exited(&self) -> Option<i32> {
        let output = self
            .reference
            .request(14, &[self.id.to_string().as_bytes()], &[]);
        assert!(
            output.status.success(),
            "remote C client status: {output:?}"
        );
        (output.stdout != b"running")
            .then(|| String::from_utf8(output.stdout).unwrap().parse().unwrap())
    }

    pub fn finish_input(&self) -> Output {
        self.reference
            .request(19, &[self.id.to_string().as_bytes()], &[])
    }

    pub fn close(&self) {
        if !self.closed.get() {
            let output = self
                .reference
                .request(13, &[self.id.to_string().as_bytes()], &[]);
            assert!(
                output.status.success(),
                "remote C client cleanup: {output:?}"
            );
            self.closed.set(true);
        }
    }
}

fn environment_packet(environment: &[(&[u8], Option<&[u8]>)]) -> Vec<u8> {
    assert!(
        environment.len() <= 256,
        "oracle environment exceeds variable bound"
    );
    let mut payload = (environment.len() as u32).to_be_bytes().to_vec();
    for (key, value) in environment {
        assert!(
            !key.is_empty() && !key.contains(&0) && !key.contains(&b'='),
            "invalid oracle environment name"
        );
        field(&mut payload, key);
        if let Some(value) = value {
            assert!(!value.contains(&0), "invalid oracle environment value");
            field(&mut payload, value);
        } else {
            payload.extend_from_slice(&u32::MAX.to_be_bytes());
        }
    }
    payload
}

impl Drop for RemoteControl {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.close();
        }
    }
}

fn field(payload: &mut Vec<u8>, bytes: &[u8]) {
    assert!(
        payload.len() + 4 + bytes.len() <= MAX_FRAME,
        "oracle frame exceeds byte bound"
    );
    payload.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    payload.extend_from_slice(bytes);
}

fn receive(reader: &mut impl Read) -> Output {
    let mut length = [0; 4];
    reader
        .read_exact(&mut length)
        .expect("oracle ended; an ambiguous operation is never replayed");
    let length = u32::from_be_bytes(length) as usize;
    assert!(
        (12..=MAX_FRAME).contains(&length),
        "invalid oracle response bound"
    );
    let mut payload = vec![0; length];
    reader
        .read_exact(&mut payload)
        .expect("incomplete oracle response");
    let code = i32::from_be_bytes(payload[..4].try_into().unwrap());
    let out_length = u32::from_be_bytes(payload[4..8].try_into().unwrap()) as usize;
    let end = 8usize
        .checked_add(out_length)
        .expect("oracle length overflow");
    assert!(end + 4 <= length, "truncated oracle stdout");
    let err_length = u32::from_be_bytes(payload[end..end + 4].try_into().unwrap()) as usize;
    assert_eq!(
        end + 4 + err_length,
        length,
        "truncated oracle stderr or trailing bytes"
    );
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(if code < 0 { -code } else { code << 8 })
    };
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(code as u32)
    };
    Output {
        status,
        stdout: payload[8..end].to_vec(),
        stderr: payload[end + 4..].to_vec(),
    }
}

impl Drop for RemoteReference {
    fn drop(&mut self) {
        // During unwinding, closing stdin still asks the leased remote helper
        // to clean up. Never replace the original test failure with a Drop panic.
        if Arc::strong_count(&self.0) == 1 && !std::thread::panicking() {
            let output = self.request(3, &[], &[]);
            assert!(
                output.status.success(),
                "remote fixture cleanup failed: {output:?}"
            );
            eprintln!(
                "TMUXX_REMOTE_C_CLEANUP={}",
                String::from_utf8(output.stdout).unwrap()
            );
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.input.take();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break None;
                }
            }
        };
        if !std::thread::panicking() {
            assert!(
                status.is_some_and(|status| status.success()),
                "remote oracle helper failed or exceeded cleanup deadline"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_response_survives_every_byte_boundary() {
        struct OneByte<'a>(&'a [u8]);
        impl Read for OneByte<'_> {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                let count = output.len().min(1).min(self.0.len());
                output[..count].copy_from_slice(&self.0[..count]);
                self.0 = &self.0[count..];
                Ok(count)
            }
        }
        let bytes =
            b"\x00\x00\x00\x12\x00\x00\x00\x07\x00\x00\x00\x03\x00\xffX\x00\x00\x00\x03\xfe\r\n";
        let output = receive(&mut OneByte(bytes));
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"\0\xffX");
        assert_eq!(output.stderr, b"\xfe\r\n");
    }

    #[test]
    fn response_eof_and_declared_bounds_fail_without_replay_or_aliasing() {
        for bytes in [
            b"".as_slice(),
            b"\x00\x00\x00\x0c\x00",
            b"\x7f\xff\xff\xff",
            b"\x00\x00\x00\x0c\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00",
        ] {
            assert!(std::panic::catch_unwind(|| receive(&mut &bytes[..])).is_err());
        }
    }
}
