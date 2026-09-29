//! Minimal XEX2 reader: just enough of the fixed header and the optional
//! header table to reach the execution id, which is where a GoD
//! container's title id, media id and disc numbering come from.

use super::error::{GodError, GodResult};
use crate::util::extent_end;

/// Fixed XEX2 header: magic, module flags, code offset, reserved,
/// certificate offset, optional header count. The optional header table
/// of `(key, value)` pairs starts right after it.
const FIXED_HEADER_SIZE: usize = 0x18;

/// Optional header key whose value is a file offset to the execution id
/// record.
const EXECUTION_ID_KEY: u32 = 0x0004_0006;

/// Bytes of the execution id record actually consumed here.
const EXECUTION_ID_SIZE: usize = 20;

/// Identity record carried in a `default.xex`: title id, media id, and
/// disc numbering that the GoD header is stamped with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionId {
    pub media_id: u32,
    pub title_id: u32,
    pub platform: u8,
    pub executable_type: u8,
    pub disc_number: u8,
    pub disc_count: u8,
}

fn be_u32(buf: &[u8], offset: usize) -> GodResult<u32> {
    let bytes = buf.get(offset..offset + 4).ok_or(GodError::InvalidXex {
        reason: "truncated",
    })?;
    Ok(u32::from_be_bytes(
        bytes.try_into().expect("bytes is always 4 bytes"),
    ))
}

/// Parses the execution id out of a `default.xex` image.
/// Reads only the XEX fixed header, optional-header table, and execution
/// record from a seekable executable range.
pub fn parse_execution_id_at<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
    base: u64,
    length: u64,
) -> GodResult<ExecutionId> {
    if FIXED_HEADER_SIZE as u64 > length {
        return Err(GodError::InvalidXex {
            reason: "truncated",
        });
    }
    let mut fixed = [0u8; FIXED_HEADER_SIZE];
    reader.seek(std::io::SeekFrom::Start(base))?;
    reader.read_exact(&mut fixed)?;
    if fixed.get(0..4) != Some(b"XEX2".as_slice()) {
        return Err(GodError::InvalidXex {
            reason: "bad XEX2 magic",
        });
    }
    let count = be_u32(&fixed, 0x14)? as u64;
    let mut table_pos = 0u64;
    let mut pair = [0u8; 8];
    let mut record_offset = None;
    while table_pos < count {
        let relative = FIXED_HEADER_SIZE as u64 + table_pos * 8;
        if extent_end(relative, 8, length).is_none() {
            return Err(GodError::InvalidXex {
                reason: "truncated",
            });
        }
        let at = relative.checked_add(base).ok_or(GodError::InvalidXex {
            reason: "truncated",
        })?;
        reader.seek(std::io::SeekFrom::Start(at))?;
        reader.read_exact(&mut pair)?;
        if u32::from_be_bytes(pair[0..4].try_into().expect("four bytes")) == EXECUTION_ID_KEY {
            record_offset =
                Some(u32::from_be_bytes(pair[4..8].try_into().expect("four bytes")) as u64);
            break;
        }
        table_pos += 1;
    }
    let offset = record_offset.ok_or(GodError::InvalidXex {
        reason: "no execution id optional header",
    })?;
    if extent_end(offset, EXECUTION_ID_SIZE as u64, length).is_none() {
        return Err(GodError::InvalidXex {
            reason: "truncated",
        });
    }
    let mut record = [0u8; EXECUTION_ID_SIZE];
    let at = base.checked_add(offset).ok_or(GodError::InvalidXex {
        reason: "truncated",
    })?;
    reader.seek(std::io::SeekFrom::Start(at))?;
    reader.read_exact(&mut record)?;
    Ok(ExecutionId {
        media_id: u32::from_be_bytes(record[0..4].try_into().expect("four bytes")),
        title_id: u32::from_be_bytes(record[12..16].try_into().expect("four bytes")),
        platform: record[16],
        executable_type: record[17],
        disc_number: record[18],
        disc_count: record[19],
    })
}

/// Encodes `exec` as a minimal XEX2 image: the fixed header, a
/// single-entry optional header table, and the execution id record it
/// points at.
#[cfg(test)]
pub(super) fn synthetic_xex(exec: &ExecutionId) -> Vec<u8> {
    let record_offset = FIXED_HEADER_SIZE + 8;
    let mut buf = vec![0u8; record_offset];
    buf[0..4].copy_from_slice(b"XEX2");
    buf[0x14..0x18].copy_from_slice(&1u32.to_be_bytes());
    buf[0x18..0x1C].copy_from_slice(&EXECUTION_ID_KEY.to_be_bytes());
    buf[0x1C..0x20].copy_from_slice(&(record_offset as u32).to_be_bytes());
    buf.extend_from_slice(&exec.media_id.to_be_bytes());
    // version and base_version, unread here but part of the record.
    buf.extend_from_slice(&[0u8; 8]);
    buf.extend_from_slice(&exec.title_id.to_be_bytes());
    buf.push(exec.platform);
    buf.push(exec.executable_type);
    buf.push(exec.disc_number);
    buf.push(exec.disc_count);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ExecutionId {
        ExecutionId {
            media_id: 0xDEAD_BEEF,
            title_id: 0x4541_08A7,
            platform: 2,
            executable_type: 1,
            disc_number: 1,
            disc_count: 2,
        }
    }

    fn parse_at(bytes: &[u8]) -> GodResult<ExecutionId> {
        let mut cursor = std::io::Cursor::new(bytes);
        parse_execution_id_at(&mut cursor, 0, bytes.len() as u64)
    }

    #[test]
    fn round_trips_every_execution_id_field() {
        let exec = sample();
        assert_eq!(parse_at(&synthetic_xex(&exec)).unwrap(), exec);
    }

    #[test]
    fn seekable_execution_id_parser_reads_a_subrange() {
        let expected = sample();
        let mut source = synthetic_xex(&expected);
        source.resize(8 * 1024 * 1024, 0);
        let mut cursor = std::io::Cursor::new(source);
        let length = cursor.get_ref().len() as u64;
        assert_eq!(
            parse_execution_id_at(&mut cursor, 0, length).unwrap(),
            expected
        );
        assert!(cursor.position() < 1024);
    }

    #[test]
    fn rejects_a_buffer_without_the_magic() {
        let mut buf = synthetic_xex(&sample());
        buf[0..4].copy_from_slice(b"XEX1");
        assert!(matches!(parse_at(&buf), Err(GodError::InvalidXex { .. })));
    }

    #[test]
    fn rejects_a_buffer_truncated_before_the_execution_id() {
        let mut buf = synthetic_xex(&sample());
        buf.truncate(FIXED_HEADER_SIZE + 8 + 4);
        assert!(matches!(parse_at(&buf), Err(GodError::InvalidXex { .. })));
    }

    #[test]
    fn rejects_a_buffer_without_the_execution_id_header() {
        let mut buf = synthetic_xex(&sample());
        buf[0x18..0x1C].copy_from_slice(&0x0004_0007u32.to_be_bytes());
        assert!(matches!(parse_at(&buf), Err(GodError::InvalidXex { .. })));
    }
}
