use serde::{Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};

pub const BLOCK_BYTES: usize = 256 * 1024;
const MAGIC: &[u8; 4] = b"TMX2";
const HEADER_BYTES: usize = 16;
const FINAL: u8 = 1;
const COMPRESSED: u8 = 2;
const COMPRESS_MIN_BYTES: usize = 1024;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn encode_error(error: rmp_serde::encode::Error) -> io::Error {
    match error {
        rmp_serde::encode::Error::InvalidValueWrite(error) => error.into(),
        error => io::Error::other(error),
    }
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    let mut remaining = bytes;
    let mut decoder = rmp_serde::Deserializer::new(&mut remaining);
    decoder.set_max_depth(128);
    let value = T::deserialize(&mut decoder).map_err(io::Error::other)?;
    if !remaining.is_empty() {
        return Err(invalid("trailing native record data"));
    }
    Ok(value)
}

/// Serialize directly into independently compressed blocks, without a record-sized buffer.
pub fn write_record(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    struct Blocks<'a, W> {
        writer: &'a mut W,
        bytes: Vec<u8>,
        compressed: Vec<u8>,
    }
    impl<W: Write> Blocks<'_, W> {
        fn emit(&mut self, last: bool) -> io::Result<()> {
            let mut header = [0; HEADER_BYTES];
            header[..4].copy_from_slice(MAGIC);
            header[4] = if last { FINAL } else { 0 };
            let length = self.bytes.len();
            let mut payload = self.bytes.as_slice();
            if length >= COMPRESS_MIN_BYTES {
                self.compressed
                    .resize(lz4_flex::block::get_maximum_output_size(length), 0);
                let compressed = lz4_flex::block::compress_into(&self.bytes, &mut self.compressed)
                    .map_err(io::Error::other)?;
                if compressed < length {
                    header[4] |= COMPRESSED;
                    payload = &self.compressed[..compressed];
                }
            }
            header[8..12].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            header[12..16].copy_from_slice(&(length as u32).to_le_bytes());
            self.writer.write_all(&header)?;
            self.writer.write_all(payload)?;
            self.bytes.clear();
            Ok(())
        }
    }
    impl<W: Write> Write for Blocks<'_, W> {
        fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
            let length = bytes.len();
            while !bytes.is_empty() {
                if self.bytes.len() == BLOCK_BYTES {
                    self.emit(false)?;
                }
                let count = bytes.len().min(BLOCK_BYTES - self.bytes.len());
                self.bytes.extend_from_slice(&bytes[..count]);
                bytes = &bytes[count..];
            }
            Ok(length)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.writer.flush()
        }
    }
    let mut blocks = Blocks {
        writer,
        bytes: Vec::new(),
        compressed: Vec::new(),
    };
    value
        .serialize(&mut rmp_serde::Serializer::new(&mut blocks).with_struct_map())
        .map_err(encode_error)?;
    blocks.emit(true)
}

/// Bound both compressed input and accumulated decoded bytes before allocating.
pub fn read_record(reader: &mut impl Read, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let mut record = Vec::new();
    let mut encoded = Vec::new();
    loop {
        let mut header = [0; HEADER_BYTES];
        loop {
            match reader.read(&mut header[..1]) {
                Ok(0) if record.is_empty() => return Ok(None),
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated native record",
                    ));
                }
                Ok(_) => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        reader.read_exact(&mut header[1..4])?;
        if &header[..4] != MAGIC {
            return Err(invalid("unsupported native protocol framing or version"));
        }
        reader.read_exact(&mut header[4..])?;
        let flags = header[4];
        let compressed = flags & COMPRESSED != 0;
        let encoded_length = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
        let length = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
        if flags & !(FINAL | COMPRESSED) != 0
            || header[5..8] != [0; 3]
            || length == 0
            || length > BLOCK_BYTES
            || encoded_length == 0
            || encoded_length > BLOCK_BYTES
            || (!compressed && encoded_length != length)
            || (compressed && encoded_length >= length)
            || (flags & FINAL == 0 && length != BLOCK_BYTES)
        {
            return Err(invalid("invalid native block flags or size"));
        }
        let total = record
            .len()
            .checked_add(length)
            .filter(|total| *total <= limit)
            .ok_or_else(|| invalid("native record exceeds limit"))?;
        let start = record.len();
        record.resize(total, 0);
        if compressed {
            encoded.resize(encoded_length, 0);
            reader.read_exact(&mut encoded)?;
            let decoded = lz4_flex::block::decompress_into(&encoded, &mut record[start..])
                .map_err(|_| invalid("invalid compressed native block"))?;
            if decoded != length {
                return Err(invalid("native block decoded size mismatch"));
            }
        } else {
            reader.read_exact(&mut record[start..])?;
        }
        if flags & FINAL != 0 {
            return Ok(Some(record));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Bytes(#[serde(with = "serde_bytes")] Vec<u8>);

    fn random_bytes(length: usize) -> Vec<u8> {
        let mut state = 7_u64;
        (0..length)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    #[test]
    fn records_roundtrip_tiny_compressed_and_incompressible_blocks() {
        for bytes in [
            b"hello".to_vec(),
            vec![42; BLOCK_BYTES * 2 + 17],
            random_bytes(BLOCK_BYTES - 5),
            random_bytes(BLOCK_BYTES * 2 - 5),
            random_bytes(BLOCK_BYTES * 2 + 17),
        ] {
            let value = Bytes(bytes);
            let mut wire = Vec::new();
            write_record(&mut wire, &value).unwrap();
            if value.0.len() > BLOCK_BYTES && value.0[0] == 42 {
                assert!(wire.len() < value.0.len() / 100);
                assert_ne!(wire[4] & COMPRESSED, 0);
            } else {
                assert_eq!(wire[4] & COMPRESSED, 0);
                assert!(wire.len() <= value.0.len() + 64);
            }
            let mut reader = wire.as_slice();
            let encoded = read_record(&mut reader, value.0.len() + 5)
                .unwrap()
                .unwrap();
            assert_eq!(decode::<Bytes>(&encoded).unwrap(), value);
            assert!(read_record(&mut reader, usize::MAX).unwrap().is_none());
        }
    }

    #[test]
    fn fragmented_records_do_not_consume_the_following_record() {
        struct Fragmented<'a>(&'a [u8]);
        impl Read for Fragmented<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                let count = bytes.len().min(3);
                self.0.read(&mut bytes[..count])
            }
        }
        let mut wire = Vec::new();
        write_record(&mut wire, &"first").unwrap();
        write_record(&mut wire, &"second").unwrap();
        let mut reader = Fragmented(&wire);
        for expected in ["first", "second"] {
            let encoded = read_record(&mut reader, 32).unwrap().unwrap();
            assert_eq!(decode::<String>(&encoded).unwrap(), expected);
        }
        assert!(read_record(&mut reader, 32).unwrap().is_none());
    }

    #[test]
    fn invalid_lengths_versions_and_compression_fail_before_allocation() {
        let mut wire = Vec::new();
        write_record(&mut wire, &Bytes(vec![42; 2048])).unwrap();
        for (offset, replacement) in [(3, b'1'), (4, 0xff), (5, 1), (11, 1), (15, 1)] {
            let mut invalid = wire.clone();
            invalid[offset] = replacement;
            assert!(read_record(&mut invalid.as_slice(), usize::MAX).is_err());
        }
        assert!(read_record(&mut wire.as_slice(), 2048).is_err());
        let mut wrong_decoded_size = wire.clone();
        wrong_decoded_size[12] += 1;
        assert!(read_record(&mut wrong_decoded_size.as_slice(), 4096).is_err());
        for length in [1, HEADER_BYTES - 1, wire.len() - 1] {
            assert!(read_record(&mut &wire[..length], 4096).is_err());
        }
        wire[HEADER_BYTES..].fill(0);
        assert!(read_record(&mut wire.as_slice(), 4096).is_err());
        assert!(decode::<String>(&[0xa1, b'a', 0xc0]).is_err());
        assert!(decode::<Bytes>(&[0xc6, 0xff, 0xff, 0xff, 0xff]).is_err());
    }

    #[test]
    fn cumulative_limit_and_missing_final_block_are_rejected() {
        let mut wire = Vec::new();
        write_record(&mut wire, &Bytes(random_bytes(BLOCK_BYTES + 8))).unwrap();
        assert!(read_record(&mut wire.as_slice(), BLOCK_BYTES).is_err());
        let first_length = u32::from_le_bytes(wire[8..12].try_into().unwrap()) as usize;
        assert!(read_record(&mut &wire[..HEADER_BYTES + first_length], usize::MAX).is_err());
    }
}
