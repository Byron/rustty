//! Bounded PTY draining independent of terminal parsing.
use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;

const BUFFER_BYTES: usize = 32 * 1024;
const BUFFER_COUNT: usize = 4;

type Batch = (Vec<u8>, io::Result<usize>);

/// Each buffer belongs to exactly one of the reader, parser, or two queues.
/// A full pool applies backpressure to the child. Small reads are delivered
/// immediately; no timer, refill spinning, or extra batching delays replies.
pub(super) struct Pipeline {
    ready: Receiver<Batch>,
    spare: SyncSender<Vec<u8>>,
}

impl Pipeline {
    /// Start before spawning the child so a failed worker leaves no child behind.
    pub(super) fn spawn(reader: impl Read + Send + 'static) -> io::Result<Self> {
        let (ready_tx, ready) = mpsc::sync_channel(BUFFER_COUNT);
        let (spare, spare_rx) = mpsc::sync_channel(BUFFER_COUNT);
        for _ in 0..BUFFER_COUNT {
            // No consumer can disconnect before its worker is started.
            let _ = spare.send(vec![0; BUFFER_BYTES]);
        }
        thread::Builder::new()
            .name("rustty-pty-gather".into())
            .spawn(move || gather(reader, spare_rx, ready_tx))?;
        Ok(Self { ready, spare })
    }

    pub(super) fn recv(&self) -> Option<Batch> {
        self.ready.recv().ok()
    }

    pub(super) fn recycle(&self, buffer: Vec<u8>) {
        // EOF/error can close the reader while earlier data is still parsed.
        let _ = self.spare.send(buffer);
    }
}

fn gather(mut reader: impl Read, spare: Receiver<Vec<u8>>, ready: SyncSender<Batch>) {
    while let Ok(mut buffer) = spare.recv() {
        let result = loop {
            match reader.read(&mut buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result,
            }
        };
        let ended = !matches!(&result, Ok(length) if *length != 0);
        // Data and the final EOF/error share one FIFO. Dropping the parser also
        // drops both queues, releasing a reader waiting on a buffer or delivery.
        if ready.send((buffer, result)).is_err() || ended {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::time::Duration;

    #[test]
    fn reused_buffers_preserve_every_byte_before_eof() {
        let input: Vec<_> = (0..BUFFER_BYTES * 9 + 17)
            .map(|i| (i % 251) as u8)
            .collect();
        let pipeline = Pipeline::spawn(io::Cursor::new(input.clone())).unwrap();
        let mut output = Vec::new();
        let mut buffers = HashSet::new();
        loop {
            let (buffer, result) = pipeline.recv().expect("ordered EOF");
            buffers.insert(buffer.as_ptr() as usize);
            let length = result.unwrap();
            if length == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..length]);
            pipeline.recycle(buffer);
        }
        assert_eq!(output, input);
        assert!(buffers.len() <= BUFFER_COUNT);
        assert!(pipeline.recv().is_none());
    }

    struct InterruptedThenError {
        interrupted: bool,
        input: io::Cursor<Vec<u8>>,
    }
    impl Read for InterruptedThenError {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::ErrorKind::Interrupted.into());
            }
            match self.input.read(output)? {
                0 => Err(io::ErrorKind::PermissionDenied.into()),
                length => Ok(length),
            }
        }
    }

    #[test]
    fn interrupted_reads_retry_and_error_follows_queued_data() {
        let input = vec![b'x'; BUFFER_BYTES * 6 + 3];
        let pipeline = Pipeline::spawn(InterruptedThenError {
            interrupted: false,
            input: io::Cursor::new(input.clone()),
        })
        .unwrap();
        let mut output = Vec::new();
        loop {
            let (buffer, result) = pipeline.recv().expect("final error");
            match result {
                Ok(length) => {
                    output.extend_from_slice(&buffer[..length]);
                    pipeline.recycle(buffer);
                }
                Err(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                    break;
                }
            }
        }
        assert_eq!(output, input);
        assert!(pipeline.recv().is_none());
    }

    struct Infinite {
        reads: mpsc::Sender<()>,
        dropped: mpsc::Sender<()>,
    }
    impl Read for Infinite {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            bytes.fill(b'x');
            let _ = self.reads.send(());
            Ok(bytes.len())
        }
    }
    impl Drop for Infinite {
        fn drop(&mut self) {
            let _ = self.dropped.send(());
        }
    }

    #[test]
    fn dropping_a_backpressured_pool_releases_its_worker() {
        let (reads, read_rx) = mpsc::channel();
        let (dropped, dropped_rx) = mpsc::channel();
        let pipeline = Pipeline::spawn(Infinite { reads, dropped }).unwrap();
        for _ in 0..BUFFER_COUNT {
            read_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        assert_eq!(read_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        let (buffer, result) = pipeline.recv().unwrap();
        assert_eq!(result.unwrap(), BUFFER_BYTES);
        pipeline.recycle(buffer);
        read_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(read_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        drop(pipeline);
        dropped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn startup_cancellation_releases_reader_when_unspawned_peer_closes() {
        struct Tracked(std::os::unix::net::UnixStream, mpsc::Sender<()>);
        impl Read for Tracked {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.0.read(bytes)
            }
        }
        impl Drop for Tracked {
            fn drop(&mut self) {
                let _ = self.1.send(());
            }
        }
        let (reader, unspawned_peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let (dropped, done) = mpsc::channel();
        let pipeline = Pipeline::spawn(Tracked(reader, dropped)).unwrap();
        drop(pipeline);
        drop(unspawned_peer);
        done.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}
