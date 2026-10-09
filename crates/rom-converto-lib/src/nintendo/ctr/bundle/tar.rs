//! Tar reading and writing for Azahar bundles: raw 100-byte names, octal or
//! GNU base-256 sizes, and the classic checksum, with no PAX, GNU longname,
//! or ustar `prefix` support, matching the parser Azahar ships.

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::{BundleEntry, BundleListing, CtrBundleError, CtrBundleResult, MAX_MEMBERS};

const BLOCK: u64 = 512;
const SIZE_FIELD: std::ops::Range<usize> = 124..136;
const CHECKSUM_FIELD: std::ops::Range<usize> = 148..156;
const TYPE_FLAG: usize = 156;
const MAX_NAME: usize = 100;

/// Lists every entry of the tar stream in `reader`. A bad checksum or a
/// short read on the first header is [`CtrBundleError::NotABundle`]; the
/// same later in the stream is [`CtrBundleError::InvalidTar`]. Every entry
/// counts toward the [`MAX_MEMBERS`] cap and the duplicate check, and each
/// entry's extent is validated against the stream length. When the cap
/// stops the listing, `truncated` reports whether more entries follow.
pub(crate) fn read_entries<R: Read + Seek>(
    reader: &mut R,
    path: &Path,
) -> CtrBundleResult<BundleListing> {
    let stream_len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
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
            // The upstream parser reports a null first record as an open
            // failure, so a file starting with zeros there is not a bundle.
            return if pos == 0 {
                Err(CtrBundleError::NotABundle(path.to_path_buf()))
            } else {
                Ok(BundleListing {
                    entries,
                    truncated: false,
                })
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
        let name = String::from_utf8(header[..name_end].to_vec()).map_err(|_| {
            CtrBundleError::InvalidTar {
                path: path.to_path_buf(),
                reason: "member name is not valid UTF-8".into(),
            }
        })?;
        if !names.insert(name.clone()) {
            return Err(CtrBundleError::DuplicateName(name));
        }
        let size = parse_size(&header[SIZE_FIELD]).ok_or_else(|| CtrBundleError::InvalidTar {
            path: path.to_path_buf(),
            reason: format!("member {name:?} declares a size that is negative or over 64 bits"),
        })?;
        let offset = pos.saturating_add(BLOCK);
        if crate::util::extent_end(offset, size, stream_len).is_none() {
            return Err(CtrBundleError::InvalidTar {
                path: path.to_path_buf(),
                reason: format!("member {name:?} extends past the end of the file"),
            });
        }
        entries.push(BundleEntry {
            regular: is_regular(header[TYPE_FLAG], &name),
            name,
            offset,
            size,
        });
        if entries.len() == MAX_MEMBERS {
            // Peek past the cap: a null record ends the archive, a short
            // read or a bad checksum is the same malformed archive it would
            // be below the cap, and anything else means members were
            // dropped from the listing.
            pos = offset.saturating_add(padded(size));
            reader.seek(SeekFrom::Start(pos))?;
            let mut next = [0u8; BLOCK as usize];
            let got = read_block(reader, &mut next)?;
            if got < next.len() {
                return Err(CtrBundleError::InvalidTar {
                    path: path.to_path_buf(),
                    reason: "no end-of-archive record".into(),
                });
            }
            if next[CHECKSUM_FIELD.start] == 0 {
                return Ok(BundleListing {
                    entries,
                    truncated: false,
                });
            }
            let stored = parse_octal(&next[CHECKSUM_FIELD]).unwrap_or(u64::MAX);
            if stored != u64::from(header_checksum(&next)) {
                return Err(CtrBundleError::InvalidTar {
                    path: path.to_path_buf(),
                    reason: "bad header checksum".into(),
                });
            }
            return Ok(BundleListing {
                entries,
                truncated: true,
            });
        }
        pos = offset.saturating_add(padded(size));
        reader.seek(SeekFrom::Start(pos))?;
    }
}

/// Writes a 512-byte ustar regular-file header for `name`/`size`, stamped
/// with `mtime` so identical inputs produce identical bundles. The caller
/// has already validated the 99-byte name limit.
pub(crate) fn write_header<W: Write>(
    w: &mut W,
    name: &str,
    size: u64,
    mtime: u64,
) -> std::io::Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_path(name)?;
    header.set_size(size);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(mtime);
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
pub(super) fn read_block(reader: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
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
/// byte has its high bit set, octal ASCII otherwise. `None` when the
/// base-256 value is negative or needs more than 64 bits.
fn parse_size(field: &[u8]) -> Option<u64> {
    if field[0] & 0x80 != 0 {
        // A leading 0xFF marks a negative value; any value bits in the
        // first byte cannot fit 64 bits.
        if field[0] & 0x7F != 0 {
            return None;
        }
        let mut size = 0u64;
        for &byte in &field[1..] {
            size = size.checked_mul(256)?.checked_add(u64::from(byte))?;
        }
        Some(size)
    } else {
        Some(parse_octal(field).unwrap_or(0))
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
/// matching the upstream `checksum()`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Seals a hand-built header: computes the classic checksum and writes
    /// it into the checksum field as octal digits plus a space.
    fn seal(header: &mut [u8; 512]) {
        let field = format!("{:o} ", header_checksum(header));
        header[CHECKSUM_FIELD.start..CHECKSUM_FIELD.start + field.len()]
            .copy_from_slice(field.as_bytes());
    }

    fn write_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        for (name, data) in entries {
            write_header(&mut buf, name, data.len() as u64, 0).unwrap();
            buf.extend_from_slice(data);
            pad_to_block(&mut buf, data.len() as u64).unwrap();
        }
        write_end(&mut buf).unwrap();
        buf
    }

    fn list(data: Vec<u8>) -> CtrBundleResult<BundleListing> {
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
        .unwrap()
        .entries;
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
        assert!(matches!(list(data), Err(CtrBundleError::InvalidTar { .. })));
    }

    #[test]
    fn empty_or_null_first_record_is_not_a_bundle() {
        assert!(matches!(
            list(Vec::new()),
            Err(CtrBundleError::NotABundle(_))
        ));
        // A file whose first block is zeros (an "empty" tar, or a CIA whose
        // cert-chain padding lands there) is an open failure for the upstream
        // parser.
        assert!(matches!(
            list(vec![0u8; 2 * BLOCK as usize]),
            Err(CtrBundleError::NotABundle(_))
        ));
        // Any truncated first header (1..511 bytes) fails the upstream open
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
        assert!(matches!(list(data), Err(CtrBundleError::InvalidTar { .. })));
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
        let entries = list(data).unwrap().entries;
        assert_eq!(entries[0].size, 0x1234);
    }

    #[test]
    fn base_256_sizes_over_64_bits_or_negative_are_invalid_tar() {
        for first in [0x81u8, 0xFF] {
            let mut header = [0u8; 512];
            header[..8].copy_from_slice(b"big.cia\0");
            header[SIZE_FIELD.start] = first;
            header[SIZE_FIELD.start + 11] = 0x12;
            seal(&mut header);
            let mut data = header.to_vec();
            data.extend_from_slice(&[0u8; 512]);
            write_end(&mut data).unwrap();
            assert!(matches!(list(data), Err(CtrBundleError::InvalidTar { .. })));
        }
    }

    #[test]
    fn non_utf8_member_names_are_rejected() {
        let mut header = [0u8; 512];
        header[..7].copy_from_slice(b"rom\xFFcia");
        header[SIZE_FIELD.start + 11] = b'1';
        seal(&mut header);
        let mut data = header.to_vec();
        data.extend_from_slice(b"x");
        pad_to_block(&mut data, 1).unwrap();
        write_end(&mut data).unwrap();
        assert!(matches!(list(data), Err(CtrBundleError::InvalidTar { .. })));
    }

    #[test]
    fn member_cap_reports_truncation_when_more_entries_follow() {
        let mut data = Vec::new();
        for i in 0..MAX_MEMBERS + 5 {
            write_header(&mut data, &format!("m{i}.cia"), 0, 0).unwrap();
        }
        // No end record on purpose: the cap must stop the reader first.
        let listing = list(data).unwrap();
        assert_eq!(listing.entries.len(), MAX_MEMBERS);
        assert_eq!(
            listing.entries[MAX_MEMBERS - 1].name,
            format!("m{}.cia", MAX_MEMBERS - 1)
        );
        assert!(listing.truncated);
    }

    #[test]
    fn member_cap_with_an_end_record_is_not_truncated() {
        let mut data = Vec::new();
        for i in 0..MAX_MEMBERS {
            write_header(&mut data, &format!("m{i}.cia"), 0, 0).unwrap();
        }
        write_end(&mut data).unwrap();
        let listing = list(data).unwrap();
        assert_eq!(listing.entries.len(), MAX_MEMBERS);
        assert!(!listing.truncated);
    }

    #[test]
    fn eof_right_after_the_capped_entry_is_invalid_tar() {
        let mut data = Vec::new();
        for i in 0..MAX_MEMBERS {
            write_header(&mut data, &format!("m{i}.cia"), 0, 0).unwrap();
        }
        // No end record and nothing after the 50th header.
        assert!(matches!(list(data), Err(CtrBundleError::InvalidTar { .. })));
    }

    #[test]
    fn corrupt_header_after_the_capped_entry_is_invalid_tar_not_truncated() {
        let mut data = Vec::new();
        for i in 0..MAX_MEMBERS + 1 {
            write_header(&mut data, &format!("m{i}.cia"), 0, 0).unwrap();
        }
        let corrupt = MAX_MEMBERS * BLOCK as usize + CHECKSUM_FIELD.start + 2;
        data[corrupt] ^= 0xFF;
        assert!(matches!(list(data), Err(CtrBundleError::InvalidTar { .. })));
    }

    #[test]
    fn capped_entry_whose_size_exceeds_the_file_is_invalid_tar() {
        let mut data = Vec::new();
        for i in 0..MAX_MEMBERS - 1 {
            write_header(&mut data, &format!("m{i}.cia"), 0, 0).unwrap();
        }
        // The last header fits the file, its declared data does not, and
        // the cap would otherwise return the listing without ever checking.
        write_header(&mut data, "big.cia", 5000, 0).unwrap();
        assert!(matches!(list(data), Err(CtrBundleError::InvalidTar { .. })));
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
        let entries = list(data).unwrap().entries;
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
