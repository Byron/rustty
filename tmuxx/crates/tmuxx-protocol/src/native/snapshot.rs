use super::ServerMessage;
use base64::Engine;
use std::{
    fs::{File, OpenOptions},
    io::{self, Seek, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

pub const MAX_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const SNAPSHOT_CHUNK_BYTES: usize = 256 * 1024;
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub fn snapshot_bytes(state: &serde_json::Value, limit: u64) -> io::Result<u64> {
    struct Counter {
        bytes: u64,
        limit: u64,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(bytes.len() as u64)
                .filter(|bytes| *bytes <= self.limit)
                .ok_or_else(|| invalid("snapshot exceeds byte limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, state).map_err(io::Error::from)?;
    Ok(counter.bytes)
}
fn record(writer: &mut impl Write, message: &ServerMessage) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, message)?;
    writer.write_all(b"\n")
}

/// Stream one immutable snapshot; bound rejection is a recoverable wire record.
pub fn write_snapshot(
    writer: &mut impl Write,
    instance_id: &str,
    sequence: u64,
    state: &serde_json::Value,
    limit: u64,
) -> io::Result<()> {
    let bytes = match snapshot_bytes(state, limit) {
        Ok(bytes) => bytes,
        Err(error) => {
            return record(
                writer,
                &ServerMessage::SnapshotAborted {
                    instance_id: instance_id.into(),
                    sequence,
                    error: error.to_string(),
                },
            );
        }
    };
    record(
        writer,
        &ServerMessage::SnapshotStart {
            instance_id: instance_id.into(),
            sequence,
            bytes,
        },
    )?;
    struct Chunks<'a, W> {
        writer: &'a mut W,
        bytes: Vec<u8>,
        offset: u64,
    }
    impl<W: Write> Chunks<'_, W> {
        fn emit(&mut self) -> io::Result<()> {
            if self.bytes.is_empty() {
                return Ok(());
            }
            record(
                self.writer,
                &ServerMessage::SnapshotChunk {
                    offset: self.offset,
                    data: base64::engine::general_purpose::STANDARD.encode(&self.bytes),
                },
            )?;
            self.offset += self.bytes.len() as u64;
            self.bytes.clear();
            Ok(())
        }
    }
    impl<W: Write> Write for Chunks<'_, W> {
        fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
            let length = bytes.len();
            while !bytes.is_empty() {
                let count = bytes.len().min(SNAPSHOT_CHUNK_BYTES - self.bytes.len());
                self.bytes.extend_from_slice(&bytes[..count]);
                bytes = &bytes[count..];
                if self.bytes.len() == SNAPSHOT_CHUNK_BYTES {
                    self.emit()?;
                }
            }
            Ok(length)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.emit()
        }
    }
    let mut chunks = Chunks {
        writer,
        bytes: Vec::with_capacity(SNAPSHOT_CHUNK_BYTES),
        offset: 0,
    };
    serde_json::to_writer(&mut chunks, state)?;
    chunks.flush()?;
    record(chunks.writer, &ServerMessage::SnapshotEnd { sequence })
}

struct Spool {
    file: File,
    path: PathBuf,
    instance_id: String,
    sequence: u64,
    expected: u64,
    bytes: u64,
}
impl Spool {
    fn new(instance_id: String, sequence: u64, expected: u64) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..32 {
            let path = std::env::temp_dir().join(format!(
                "tmuxx-snapshot-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    // Unix keeps the open file alive without leaving crash debris.
                    #[cfg(unix)]
                    std::fs::remove_file(&path)?;
                    return Ok(Self {
                        file,
                        path,
                        instance_id,
                        sequence,
                        expected,
                        bytes: 0,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot create snapshot spool",
        ))
    }
}
impl Drop for Spool {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Atomic receiver: fragments spool to a private file and never replace live state.
pub struct SnapshotAssembler {
    limit: u64,
    pending: Option<Spool>,
}
impl Default for SnapshotAssembler {
    fn default() -> Self {
        Self::new(MAX_SNAPSHOT_BYTES)
    }
}
impl SnapshotAssembler {
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            pending: None,
        }
    }
    pub fn finish(&mut self) -> io::Result<()> {
        if self.pending.take().is_some() {
            Err(invalid("truncated snapshot stream"))
        } else {
            Ok(())
        }
    }
    pub fn push(&mut self, message: ServerMessage) -> io::Result<Option<ServerMessage>> {
        let result = self.push_inner(message);
        if result.is_err() {
            self.pending = None;
        }
        result
    }
    fn push_inner(&mut self, message: ServerMessage) -> io::Result<Option<ServerMessage>> {
        match message {
            ServerMessage::SnapshotStart {
                instance_id,
                sequence,
                bytes,
            } => {
                if self.pending.is_some() {
                    return Err(invalid("overlapping snapshot streams"));
                }
                if bytes > self.limit {
                    return Err(invalid("snapshot exceeds byte limit"));
                }
                self.pending = Some(Spool::new(instance_id, sequence, bytes)?);
                Ok(None)
            }
            ServerMessage::SnapshotChunk { offset, data } => {
                let spool = self
                    .pending
                    .as_mut()
                    .ok_or_else(|| invalid("snapshot chunk without start"))?;
                if offset != spool.bytes || data.len() > SNAPSHOT_CHUNK_BYTES.div_ceil(3) * 4 {
                    return Err(invalid("invalid snapshot chunk offset or size"));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|_| invalid("invalid snapshot chunk data"))?;
                let total = spool
                    .bytes
                    .checked_add(bytes.len() as u64)
                    .filter(|total| *total <= spool.expected)
                    .ok_or_else(|| invalid("snapshot exceeds declared size"))?;
                if bytes.is_empty() {
                    return Err(invalid("empty snapshot chunk"));
                }
                spool.file.write_all(&bytes)?;
                spool.bytes = total;
                Ok(None)
            }
            ServerMessage::SnapshotEnd { sequence } => {
                let mut spool = self
                    .pending
                    .take()
                    .ok_or_else(|| invalid("snapshot end without start"))?;
                if sequence != spool.sequence || spool.bytes != spool.expected {
                    return Err(invalid("incomplete or mismatched snapshot"));
                }
                spool.file.rewind()?;
                let state = serde_json::from_reader(io::BufReader::new(&mut spool.file))?;
                Ok(Some(ServerMessage::Snapshot {
                    instance_id: spool.instance_id.clone(),
                    sequence,
                    state,
                }))
            }
            message @ ServerMessage::SnapshotAborted { .. } => {
                self.pending = None;
                Ok(Some(message))
            }
            message => {
                if self.pending.is_some() {
                    return Err(invalid("interleaved snapshot stream"));
                }
                if let ServerMessage::Snapshot { state, .. } = &message {
                    snapshot_bytes(state, self.limit)?;
                }
                Ok(Some(message))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_is_atomic_bounded_and_recoverable() {
        let state = serde_json::json!({"pixels": "x".repeat(SNAPSHOT_CHUNK_BYTES * 2 + 17)});
        let mut wire = Vec::new();
        write_snapshot(&mut wire, "owner", 9, &state, MAX_SNAPSHOT_BYTES).unwrap();
        let mut assembler = SnapshotAssembler::default();
        let mut result = Vec::new();
        for line in wire
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            assert!(line.len() < 512 * 1024);
            if let Some(message) = assembler
                .push(serde_json::from_slice(line).unwrap())
                .unwrap()
            {
                result.push(message);
            }
        }
        assert_eq!(
            result,
            [ServerMessage::Snapshot {
                instance_id: "owner".into(),
                sequence: 9,
                state: state.clone()
            }]
        );
        assembler.finish().unwrap();
        let mut rejected = Vec::new();
        write_snapshot(&mut rejected, "owner", 10, &state, 16).unwrap();
        assert!(matches!(
            assembler
                .push(serde_json::from_slice(&rejected).unwrap())
                .unwrap(),
            Some(ServerMessage::SnapshotAborted { sequence: 10, .. })
        ));
        assert!(
            assembler
                .push(ServerMessage::SnapshotStart {
                    instance_id: "owner".into(),
                    sequence: 11,
                    bytes: 10
                })
                .unwrap()
                .is_none()
        );
        assert!(
            assembler
                .push(ServerMessage::SnapshotChunk {
                    offset: 1,
                    data: "e30=".into()
                })
                .is_err()
        );
        assembler.finish().unwrap();
        assert!(
            assembler
                .push(ServerMessage::SnapshotStart {
                    instance_id: "owner".into(),
                    sequence: 12,
                    bytes: 10
                })
                .unwrap()
                .is_none()
        );
        assert!(assembler.finish().is_err());
        let mut bounded = SnapshotAssembler::new(3);
        assert!(
            bounded
                .push(ServerMessage::SnapshotStart {
                    instance_id: "owner".into(),
                    sequence: 13,
                    bytes: 4
                })
                .is_err()
        );
    }
}
