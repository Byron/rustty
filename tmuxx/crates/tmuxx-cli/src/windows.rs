use super::*;
use std::io::Read;
use std::os::windows::io::AsRawHandle;
use tmuxx_protocol::native::Hello;

/// A fixed SSH exec command. The first protocol record selects the instance;
/// names are never interpolated into cmd.exe or PowerShell source.
pub(super) fn native_stdio_bridge(explicit: Option<&std::path::Path>) -> io::Result<()> {
    let mut stdin = BufReader::new(io::stdin());
    let first = tmuxx_runtime::read_record(&mut stdin, tmuxx_runtime::MAX_REQUEST_BYTES)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "missing bridge hello"))?;
    let hello: Hello = serde_json::from_slice(&first)?;
    if hello.version != tmuxx_protocol::native::VERSION {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native bridge protocol version mismatch",
        ));
    }
    let endpoint = if let Some(explicit) = explicit {
        explicit.to_path_buf()
    } else if let Some(instance) = hello.instance.as_deref() {
        tmuxx_runtime::windows::endpoint(instance)?
    } else {
        let endpoints = tmuxx_runtime::windows::discover()?;
        match endpoints.as_slice() {
            [endpoint] => endpoint.clone(),
            [] => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no running tmuxx instance; start an owner explicitly",
                ));
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "multiple tmuxx instances; select an instance in the hello",
                ));
            }
        }
    };
    let mut stream = tmuxx_runtime::windows::connect(&endpoint)?;
    stream.write_all(&first)?;
    let mut output = stream.clone();
    std::thread::spawn(move || {
        let _ = io::copy(&mut stdin, &mut output);
        let _ = output.shutdown(std::net::Shutdown::Write);
    });
    io::copy(&mut stream, &mut io::stdout().lock())?;
    Ok(())
}

struct InputWorker {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for InputWorker {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            // A read can start between the worker's stop check and cancellation.
            // Keep cancelling until the worker has observed stop, rather than
            // joining after a single cancellation that could miss that race.
            while !thread.is_finished() {
                unsafe {
                    windows_sys::Win32::System::IO::CancelSynchronousIo(thread.as_raw_handle());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = thread.join();
        }
    }
}

pub(super) fn native_stdin(
    reader: &mut BufReader<NativeStream>,
    original_id: u64,
    deferred: &mut std::collections::VecDeque<ServerMessage>,
    snapshots: &mut tmuxx_protocol::native::SnapshotAssembler,
) -> io::Result<()> {
    let (ack, receive_ack) = std::sync::mpsc::sync_channel(1);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_stop = stop.clone();
    let mut output = reader.get_ref().clone();
    let thread = std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        let mut bytes = [0u8; 65536];
        let mut id = u64::MAX;
        while !worker_stop.load(std::sync::atomic::Ordering::Acquire) {
            let count = match input.read(&mut bytes) {
                Ok(n) => n,
                Err(_) => break,
            };
            if worker_stop.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            let request = Request {
                request_id: id,
                argv: vec![
                    b"tmuxx-stdin".to_vec(),
                    original_id.to_string().into_bytes(),
                ],
                stdin: (count > 0).then(|| bytes[..count].to_vec()),
                subscribe: false,
                control: false,
            };
            // A named-pipe write allocates an OVERLAPPED event and enters the
            // kernel. Serializing each JSON token directly to it makes a
            // 64 KiB stdin chunk take hundreds of thousands of tiny writes.
            // The chunk already has a fixed bound; assemble exactly one
            // newline-delimited record and keep the existing ACK barrier.
            let Ok(mut record) = serde_json::to_vec(&request) else {
                break;
            };
            record.push(b'\n');
            if output.write_all(&record).is_err() {
                break;
            }
            loop {
                match receive_ack.recv_timeout(Duration::from_millis(50)) {
                    Ok(true) => break,
                    Ok(false) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(_) if worker_stop.load(std::sync::atomic::Ordering::Acquire) => return,
                    Err(_) => {}
                }
            }
            if count == 0 {
                break;
            }
            id -= 1;
        }
    });
    let _worker = InputWorker {
        stop,
        thread: Some(thread),
    };
    let mut pending = u64::MAX;
    loop {
        let line = tmuxx_runtime::read_record(reader, 256 * 1024 * 1024)?
            .ok_or_else(|| io::Error::other("server stopped during stdin"))?;
        let Some(message) = snapshots.push(serde_json::from_slice(&line)?)? else {
            continue;
        };
        if let ServerMessage::Reply(reply) = &message
            && reply.request_id == pending
        {
            let _ = ack.try_send(reply.error.is_none());
            pending -= 1;
            if reply.error.is_some() {
                return Ok(());
            }
            continue;
        }
        let complete = matches!(&message, ServerMessage::Reply(reply) if reply.request_id == original_id)
            || matches!(
                message,
                ServerMessage::Exit { .. } | ServerMessage::Detached { .. }
            );
        deferred.push_back(message);
        if complete {
            return Ok(());
        }
        if deferred.len() > 256 {
            return Err(io::Error::other("too many pending stdin messages"));
        }
    }
}
