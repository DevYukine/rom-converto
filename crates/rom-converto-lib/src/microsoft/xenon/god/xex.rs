//! Minimal XEX2 reader: just enough of the fixed header and the optional
//! header table to reach the execution id, which is where a GoD
//! container's title id, media id and disc numbering come from.

use super::error::{GodError, GodResult};

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
pub fn parse_execution_id(buf: &[u8]) -> GodResult<ExecutionId> {
    if buf.get(0..4) != Some(b"XEX2".as_slice()) {
        return Err(GodError::InvalidXex {
            reason: "bad XEX2 magic",
        });
    }

    let header_count = be_u32(buf, 0x14)?;
    let mut value = None;
    for index in 0..header_count as usize {
        let pair = FIXED_HEADER_SIZE + index * 8;
        if be_u32(buf, pair)? == EXECUTION_ID_KEY {
            value = Some(be_u32(buf, pair + 4)? as usize);
            break;
        }
    }
    let offset = value.ok_or(GodError::InvalidXex {
        reason: "no execution id optional header",
    })?;

    let record = buf
        .get(offset..offset + EXECUTION_ID_SIZE)
        .ok_or(GodError::InvalidXex {
            reason: "truncated",
        })?;
    Ok(ExecutionId {
        media_id: u32::from_be_bytes(
            record[0..4]
                .try_into()
                .expect("record[0..4] is always 4 bytes"),
        ),
        title_id: u32::from_be_bytes(
            record[12..16]
                .try_into()
                .expect("record[12..16] is always 4 bytes"),
        ),
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

    #[test]
    fn round_trips_every_execution_id_field() {
        let exec = sample();
        assert_eq!(parse_execution_id(&synthetic_xex(&exec)).unwrap(), exec);
    }

    #[test]
    fn rejects_a_buffer_without_the_magic() {
        let mut buf = synthetic_xex(&sample());
        buf[0..4].copy_from_slice(b"XEX1");
        assert!(matches!(
            parse_execution_id(&buf),
            Err(GodError::InvalidXex { .. })
        ));
    }

    #[test]
    fn rejects_a_buffer_truncated_before_the_execution_id() {
        let mut buf = synthetic_xex(&sample());
        buf.truncate(FIXED_HEADER_SIZE + 8 + 4);
        assert!(matches!(
            parse_execution_id(&buf),
            Err(GodError::InvalidXex { .. })
        ));
    }

    #[test]
    fn rejects_a_buffer_without_the_execution_id_header() {
        let mut buf = synthetic_xex(&sample());
        buf[0x18..0x1C].copy_from_slice(&0x0004_0007u32.to_be_bytes());
        assert!(matches!(
            parse_execution_id(&buf),
            Err(GodError::InvalidXex { .. })
        ));
    }
}
