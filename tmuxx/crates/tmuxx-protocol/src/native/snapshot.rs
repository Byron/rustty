use super::{
    ServerMessage, SnapshotState,
    codec::{encode_error, write_record},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

pub const MAX_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const SNAPSHOT_CHUNK_BYTES: usize = 256 * 1024;
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub fn snapshot_bytes(state: &SnapshotState, limit: u64) -> io::Result<u64> {
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
    state
        .serialize(&mut rmp_serde::Serializer::new(&mut counter).with_struct_map())
        .map_err(encode_error)?;
    Ok(counter.bytes)
}

/// Stream one immutable snapshot; bound rejection is a recoverable wire record.
pub fn write_snapshot(
    writer: &mut impl Write,
    instance_id: &str,
    sequence: u64,
    state: &SnapshotState,
    limit: u64,
) -> io::Result<()> {
    let bytes = match snapshot_bytes(state, limit) {
        Ok(bytes) => bytes,
        Err(error) => {
            return write_record(
                writer,
                &ServerMessage::SnapshotAborted {
                    instance_id: instance_id.into(),
                    sequence,
                    error: error.to_string(),
                },
            );
        }
    };
    write_record(
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
            let message = ServerMessage::SnapshotChunk {
                offset: self.offset,
                data: std::mem::take(&mut self.bytes),
            };
            write_record(self.writer, &message)?;
            if let ServerMessage::SnapshotChunk { data, .. } = message {
                self.offset += data.len() as u64;
                self.bytes = data;
                self.bytes.clear();
            }
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
    state
        .serialize(&mut rmp_serde::Serializer::new(&mut chunks).with_struct_map())
        .map_err(encode_error)?;
    chunks.flush()?;
    write_record(chunks.writer, &ServerMessage::SnapshotEnd { sequence })
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
                if offset != spool.bytes || data.len() > SNAPSHOT_CHUNK_BYTES {
                    return Err(invalid("invalid snapshot chunk offset or size"));
                }
                let total = spool
                    .bytes
                    .checked_add(data.len() as u64)
                    .filter(|total| *total <= spool.expected)
                    .ok_or_else(|| invalid("snapshot exceeds declared size"))?;
                if data.is_empty() {
                    return Err(invalid("empty snapshot chunk"));
                }
                spool.file.write_all(&data)?;
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
                let mut reader = io::BufReader::new(&mut spool.file).take(spool.expected);
                let mut decoder = rmp_serde::Deserializer::new(&mut reader);
                decoder.set_max_depth(128);
                let state = SnapshotState::deserialize(&mut decoder).map_err(io::Error::other)?;
                if reader.read(&mut [0])? != 0 {
                    return Err(invalid("trailing snapshot data"));
                }
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
    use crate::native::{
        SnapshotSession,
        codec::{decode, read_record},
    };

    fn state() -> SnapshotState {
        SnapshotState {
            sessions: vec![SnapshotSession {
                id: 1,
                name: "x".repeat(SNAPSHOT_CHUNK_BYTES * 2 + 17),
                active: 1,
                windows: Default::default(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn streaming_is_atomic_bounded_and_recoverable() {
        let state = state();
        let mut wire = Vec::new();
        write_snapshot(&mut wire, "owner", 9, &state, MAX_SNAPSHOT_BYTES).unwrap();
        let mut assembler = SnapshotAssembler::default();
        let mut result = Vec::new();
        let mut reader = wire.as_slice();
        let mut chunks = 0;
        while let Some(bytes) = read_record(&mut reader, SNAPSHOT_CHUNK_BYTES + 128).unwrap() {
            let message = decode(&bytes).unwrap();
            chunks += usize::from(matches!(message, ServerMessage::SnapshotChunk { .. }));
            if let Some(message) = assembler.push(message).unwrap() {
                result.push(message);
            }
        }
        assert_eq!(chunks, 3);
        assert_eq!(result.len(), 1);
        let ServerMessage::Snapshot {
            instance_id,
            sequence,
            state: restored,
        } = &result[0]
        else {
            panic!("missing assembled snapshot");
        };
        assert_eq!(instance_id, "owner");
        assert_eq!(*sequence, 9);
        assert_eq!(
            rmp_serde::to_vec_named(restored).unwrap(),
            rmp_serde::to_vec_named(&state).unwrap()
        );
        assembler.finish().unwrap();
        let mut rejected = Vec::new();
        write_snapshot(&mut rejected, "owner", 10, &state, 16).unwrap();
        let rejected = read_record(&mut rejected.as_slice(), 1024)
            .unwrap()
            .unwrap();
        assert!(matches!(
            assembler.push(decode(&rejected).unwrap()).unwrap(),
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
                    data: vec![0x80]
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

    #[test]
    fn malformed_interleaved_or_incomplete_snapshots_never_publish() {
        let state = SnapshotState::default();
        let bytes = rmp_serde::to_vec_named(&state).unwrap();
        for message in [
            ServerMessage::SnapshotChunk {
                offset: 0,
                data: Vec::new(),
            },
            ServerMessage::SnapshotChunk {
                offset: 0,
                data: vec![0; SNAPSHOT_CHUNK_BYTES + 1],
            },
            ServerMessage::SnapshotEnd { sequence: 1 },
            ServerMessage::Changed { sequence: 1 },
            ServerMessage::SnapshotStart {
                instance_id: "owner".into(),
                sequence: 2,
                bytes: 1,
            },
        ] {
            let mut assembler = SnapshotAssembler::default();
            assembler
                .push(ServerMessage::SnapshotStart {
                    instance_id: "owner".into(),
                    sequence: 1,
                    bytes: bytes.len() as u64,
                })
                .unwrap();
            assert!(assembler.push(message).is_err());
            assembler.finish().unwrap();
        }
        for mut bytes in [
            bytes,
            vec![
                0x81, 0xa8, b's', b'e', b's', b's', b'i', b'o', b'n', b's', 0xc6, 0xff, 0xff, 0xff,
                0xff,
            ],
        ] {
            bytes.push(0xc0);
            let mut assembler = SnapshotAssembler::default();
            assembler
                .push(ServerMessage::SnapshotStart {
                    instance_id: "owner".into(),
                    sequence: 1,
                    bytes: bytes.len() as u64,
                })
                .unwrap();
            assembler
                .push(ServerMessage::SnapshotChunk {
                    offset: 0,
                    data: bytes,
                })
                .unwrap();
            assert!(
                assembler
                    .push(ServerMessage::SnapshotEnd { sequence: 1 })
                    .is_err()
            );
            assembler.finish().unwrap();
        }
    }
}
