//! Microtar-faithful tar reading and writing for Azahar bundles: raw
//! 100-byte names, octal or GNU base-256 sizes, and the classic checksum,
//! with no PAX, GNU longname, or ustar `prefix` support; matching the
//! parser Azahar ships.

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{BundleEntry, CtrBundleError, CtrBundleResult, MAX_MEMBERS};

const BLOCK: u64 = 512;
const SIZE_FIELD: std::ops::Range<usize> = 124..136;
const CHECKSUM_FIELD: std::ops::Range<usize> = 148..156;
const TYPE_FLAG: usize = 156;
const MAX_NAME: usize = 100;

/// Lists every entry of the tar stream in `reader`, mirroring microtar: a
/// bad checksum on the first header rejects the file as not a bundle, on
/// any later header it is a malformed archive; a short read where the
/// first header should be is also not a bundle, while one where the end
/// record should be is an error, not an end; a header whose checksum field
/// starts with NUL ends the archive. Every entry counts toward the
/// [`MAX_MEMBERS`] cap (reading stops there without error) and the
/// duplicate check, and each result records whether it is a regular-file
/// entry.
pub(crate) fn read_entries<R: Read + Seek>(
    reader: &mut R,
    path: &Path,
) -> CtrBundleResult<Vec<BundleEntry>> {
    let mut entries = Vec::with_capacity(MAX_MEMBERS);
    let mut names = HashSet::new();
    let mut pos: u64 = 0;
    loop {
        let mut header = [0u8; BLOCK as usize];
        let got = read_block(reader, &mut header)?;
        if got < header.len() {
            return if pos == 0 {
                Err(CtrBundleError::NotABundle(path.to_path_buf()))
            } else {
                Err(CtrBundleError::InvalidTar {
                    path: path.to_path_buf(),
                    reason: "no end-of-archive record".into(),
                })
            };
        }
        if header[CHECKSUM_FIELD.start] == 0 {
            // microtar reports a null first record as an open failure, so an
            // empty archive (or any file starting with zeros there) is not a bundle.
            return if pos == 0 {
                Err(CtrBundleError::NotABundle(path.to_path_buf()))
            } else {
                Ok(entries)
            };
        }
        let stored = parse_octal(&header[CHECKSUM_FIELD]).unwrap_or(u64::MAX);
        if stored != u64::from(header_checksum(&header)) {
            return Err(if pos == 0 {
                CtrBundleError::NotABundle(path.to_path_buf())
            } else {
                CtrBundleError::InvalidTar {
                    path: path.to_path_buf(),
                    reason: "bad header checksum".into(),
                }
            });
        }
        let name_end = header[..MAX_NAME]
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(MAX_NAME);
        // Dedupe on the raw NUL-trimmed bytes so distinct names never
        // collide through lossy UTF-8 replacement.
        if !names.insert(header[..name_end].to_vec()) {
            return Err(CtrBundleError::DuplicateName(
                String::from_utf8_lossy(&header[..name_end]).into_owned(),
            ));
        }
        let name = String::from_utf8_lossy(&header[..name_end]).into_owned();
        let size = parse_size(&header[SIZE_FIELD]);
        entries.push(BundleEntry {
            regular: is_regular(header[TYPE_FLAG], &name),
            name,
            offset: pos + BLOCK,
            size,
        });
        if names.len() == MAX_MEMBERS {
            return Ok(entries);
        }
        pos = pos.saturating_add(BLOCK).saturating_add(padded(size));
        reader.seek(SeekFrom::Start(pos))?;
    }
}

/// Writes a 512-byte ustar regular-file header for `name`/`size`. The
/// caller has already validated the 100-byte name limit.
pub(crate) fn write_header<W: Write>(w: &mut W, name: &str, size: u64) -> std::io::Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_path(name)?;
    header.set_size(size);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(now_unix());
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    w.write_all(header.as_bytes())
}

/// Zero-pads a member's data so the stream lands on the next 512-byte
/// boundary.
pub(crate) fn pad_to_block<W: Write>(w: &mut W, size: u64) -> std::io::Result<()> {
    let pad = match size % BLOCK {
        0 => 0,
        rem => BLOCK - rem,
    } as usize;
    if pad > 0 {
        w.write_all(&[0u8; BLOCK as usize][..pad])?;
    }
    Ok(())
}

/// Writes the two 512-byte zero blocks that end every tar archive.
pub(crate) fn write_end<W: Write>(w: &mut W) -> std::io::Result<()> {
    w.write_all(&[0u8; 2 * BLOCK as usize])
}

/// Fills `buf` as far as the reader allows, tolerating spurious
/// Interrupted errors; returns how many bytes were actually read.
fn read_block(reader: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        match reader.read(&mut buf[done..]) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(done)
}

/// A name ending in `/` is a V7-style directory even under a regular-file
/// type flag (V7 tar has no dedicated directory type), so it never counts
/// as regular.
fn is_regular(type_flag: u8, name: &str) -> bool {
    matches!(type_flag, b'0' | 0 | b'7') && !name.ends_with('/')
}

/// Size in bytes of `size` rounded up to the next 512-byte boundary.
fn padded(size: u64) -> u64 {
    match size % BLOCK {
        0 => size,
        rem => size.saturating_add(BLOCK - rem),
    }
}

/// Parses the 12-byte size field: GNU base-256 big-endian when the first
/// byte has its high bit set, octal ASCII otherwise, like microtar.
fn parse_size(field: &[u8]) -> u64 {
    if field[0] & 0x80 != 0 {
        let mut size = u64::from(field[0] & 0x7F);
        for &byte in &field[1..] {
            size = (size << 8) | u64::from(byte);
        }
        size
    } else {
        parse_octal(field).unwrap_or(0)
    }
}

/// `sscanf("%llo")`-style octal: leading spaces skipped, digits up to the
/// first other byte. `None` when no digits were found.
fn parse_octal(field: &[u8]) -> Option<u64> {
    let mut any = false;
    let mut value = 0u64;
    for &byte in field.iter().skip_while(|&&b| b == b' ') {
        if let digit @ b'0'..=b'7' = byte {
            any = true;
            value = value.wrapping_mul(8).wrapping_add(u64::from(digit - b'0'));
        } else {
            break;
        }
    }
    any.then_some(value)
}

/// Sum of all header bytes with the checksum field counted as spaces,
/// matching microtar's `checksum()`.
fn header_checksum(header: &[u8; 512]) -> u32 {
    let mut sum = 0u32;
    for (i, &byte) in header.iter().enumerate() {
        sum += if CHECKSUM_FIELD.contains(&i) {
            0x20
        } else {
            u32::from(byte)
        };
    }
    sum
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Seals a hand-built header: computes the microtar checksum and writes
    /// it into the checksum field as octal digits plus a space.
    fn seal(header: &mut [u8; 512]) {
        let field = format!("{:o} ", header_checksum(header));
        header[CHECKSUM_FIELD.start..CHECKSUM_FIELD.start + field.len()]
            .copy_from_slice(field.as_bytes());
    }

    fn write_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        for (name, data) in entries {
            write_header(&mut buf, name, data.len() as u64).unwrap();
            buf.extend_from_slice(data);
            pad_to_block(&mut buf, data.len() as u64).unwrap();
        }
        write_end(&mut buf).unwrap();
        buf
    }

    fn list(data: Vec<u8>) -> CtrBundleResult<Vec<BundleEntry>> {
        read_entries(&mut Cursor::new(data), Path::new("out.bcci"))
    }

    #[test]
    fn round_trip_lists_offsets_and_sizes() {
        let entries = read_entries(
            &mut Cursor::new(write_archive(&[
                ("game.cci", b"CCI-BYTES"),
                ("dlc.cia", b""),
            ])),
            Path::new("out.bcci"),
        )
        .unwrap();
        assert_eq!(
            entries,
            [
                BundleEntry {
                    name: "game.cci".into(),
                    offset: 512,
                    size: 9,
                    regular: true,
                },
                BundleEntry {
                    name: "dlc.cia".into(),
                    offset: 1536,
                    size: 0,
                    regular: true,
                },
            ]
        );
    }

    #[test]
    fn first_header_bad_checksum_is_not_a_bundle() {
        let mut data = write_archive(&[("game.cci", b"x")]);
        data[CHECKSUM_FIELD.start + 2] ^= 0xFF;
        assert!(matches!(list(data), Err(CtrBundleError::NotABundle(_))));
    }

    #[test]
    fn later_bad_checksum_is_invalid_tar() {
        let mut data = write_archive(&[("game.cci", b"x"), ("dlc.cia", b"y")]);
        data[1024 + CHECKSUM_FIELD.start + 2] ^= 0xFF;
        match list(data) {
            Err(CtrBundleError::InvalidTar { reason, .. }) => {
                assert_eq!(reason, "bad header checksum");
            }
            other => panic!("expected InvalidTar, got {other:?}"),
        }
    }

    #[test]
    fn empty_or_null_first_record_is_not_a_bundle() {
        assert!(matches!(
            list(Vec::new()),
            Err(CtrBundleError::NotABundle(_))
        ));
        // A file whose first block is zeros (an "empty" tar, or a CIA whose
        // cert-chain padding lands there) is an open failure for microtar.
        assert!(matches!(
            list(vec![0u8; 2 * BLOCK as usize]),
            Err(CtrBundleError::NotABundle(_))
        ));
        // Any truncated first header (1..511 bytes) fails microtar's Open
        // as well, so it is not a bundle rather than a malformed archive.
        assert!(matches!(
            list(vec![b'x'; 100]),
            Err(CtrBundleError::NotABundle(_))
        ));
    }

    #[test]
    fn missing_end_record_is_invalid_tar() {
        let mut data = write_archive(&[("game.cci", b"x")]);
        data.truncate(data.len() - 2 * BLOCK as usize);
        match list(data) {
            Err(CtrBundleError::InvalidTar { reason, .. }) => {
                assert_eq!(reason, "no end-of-archive record");
            }
            other => panic!("expected InvalidTar, got {other:?}"),
        }
    }

    #[test]
    fn base_256_size_is_parsed() {
        let mut header = [0u8; 512];
        header[..8].copy_from_slice(b"big.cia\0");
        header[SIZE_FIELD.start] = 0x80;
        header[SIZE_FIELD.start + 10] = 0x12;
        header[SIZE_FIELD.start + 11] = 0x34;
        seal(&mut header);
        let mut data = header.to_vec();
        data.extend_from_slice(&vec![0u8; padded(0x1234) as usize]);
        write_end(&mut data).unwrap();
        let entries = list(data).unwrap();
        assert_eq!(entries[0].size, 0x1234);
    }

    #[test]
    fn reading_stops_at_max_members_without_error() {
        let mut data = Vec::new();
        for i in 0..MAX_MEMBERS + 5 {
            write_header(&mut data, &format!("m{i}.cia"), 0).unwrap();
        }
        // No end record on purpose: the cap must stop the reader first.
        let entries = list(data).unwrap();
        assert_eq!(entries.len(), MAX_MEMBERS);
        assert_eq!(
            entries[MAX_MEMBERS - 1].name,
            format!("m{}.cia", MAX_MEMBERS - 1)
        );
    }

    #[test]
    fn duplicate_names_reject_the_bundle() {
        let data = write_archive(&[("game.cia", b"a"), ("game.cia", b"b")]);
        match list(data) {
            Err(CtrBundleError::DuplicateName(name)) => assert_eq!(name, "game.cia"),
            other => panic!("expected DuplicateName, got {other:?}"),
        }
    }

    #[test]
    fn non_regular_entries_are_listed_with_regular_false() {
        let mut header = [0u8; 512];
        header[..6].copy_from_slice(b"folder");
        header[TYPE_FLAG] = b'5';
        seal(&mut header);
        let mut data = header.to_vec();
        data.extend_from_slice(&write_archive(&[("game.cci", b"x")]));
        let entries = list(data).unwrap();
        assert_eq!(
            entries,
            [
                BundleEntry {
                    name: "folder".into(),
                    offset: 512,
                    size: 0,
                    regular: false,
                },
                BundleEntry {
                    name: "game.cci".into(),
                    offset: 1024,
                    size: 1,
                    regular: true,
                },
            ]
        );
    }
}
