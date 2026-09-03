//! `NPUMDIMG` `DATA.PSAR` to ISO: header key derivation, block table walk,
//! and streaming block decrypt plus LZRC decompression.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::sony::psp::amctrl::{BbCipher, bb_mac};
use crate::sony::psp::info::{PsarKind, read_psar_kind};
use crate::sony::psp::lzrc;
use crate::sony::psp::pbp::{DATA_PSAR, Pbp};
use crate::util::{BYTES_PER_MB, ProgressReporter};

const ISO_SECTOR_SIZE: usize = 2048;
/// Largest sectors-per-block the header at `0x0c` is known to carry.
const MAX_ISO_BLOCK: u32 = 16;
const HEADER_LEN: usize = 256;
/// Bytes of the header the BBMac covers, and the offset of the version key
/// block that follows it.
const MAC_LEN: usize = 0xc0;
const KEY_MODIFIER: usize = 0xa0;
/// Encrypted region holding the ISO geometry and the block table offset.
const BODY: std::ops::Range<usize> = 0x40..0xa0;
/// Set when a block is stored without BBCipher encryption.
const FLAG_PLAIN: u32 = 4;
const TABLE_ENTRY_LEN: u64 = 32;

/// Decrypts the `NPUMDIMG` image carried by `input` and writes the resulting
/// ISO to `output`. `input` is either an `EBOOT.PBP` or a PSN `.pkg` package
/// whose `EBOOT.PBP` item is read in place.
///
/// # Errors
/// Returns an error if `input` is neither format, its `DATA.PSAR` is not an
/// `NPUMDIMG` image, the decrypted header is inconsistent, or any read,
/// write, or block decompression fails.
pub fn to_iso(progress: &dyn ProgressReporter, input: &Path, output: &Path) -> Result<()> {
    let mut file =
        File::open(input).with_context(|| format!("psp to-iso: open {}", input.display()))?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .with_context(|| format!("psp to-iso: read {}", input.display()))?;

    match &magic {
        b"\0PBP" => convert(progress, &mut file, input, output),
        &[0x7F, b'P', b'K', b'G'] => {
            let info = crate::sony::vita::pkg::read_info(input)?;
            if info.content_type == 6 {
                bail!("psp to-iso: PS1 Classic packages cannot be converted to a PSP ISO");
            }
            let mut item = crate::sony::vita::pkg::open_item(input, "EBOOT.PBP")?;
            convert(progress, &mut item, input, output)
        }
        _ => bail!(
            "psp to-iso: {} is neither an EBOOT.PBP nor a PSN .pkg package",
            input.display()
        ),
    }
}

fn convert<R: Read + Seek>(
    progress: &dyn ProgressReporter,
    file: &mut R,
    input: &Path,
    output: &Path,
) -> Result<()> {
    let pbp = Pbp::read(file).with_context(|| format!("psp to-iso: parse {}", input.display()))?;
    let psar = pbp.segments[DATA_PSAR];

    match read_psar_kind(file, psar)? {
        Some(PsarKind::Npumdimg) => {}
        Some(PsarKind::Psisoimg) => bail!(
            "psp to-iso: DATA.PSAR holds a PSISOIMG PS1 Classic image, not an NPUMDIMG UMD image"
        ),
        Some(PsarKind::Pstitleimg) => bail!(
            "psp to-iso: DATA.PSAR holds a PSTITLEIMG PS1 Classic container, not an NPUMDIMG UMD image"
        ),
        Some(PsarKind::Unknown { magic }) => {
            bail!("psp to-iso: DATA.PSAR magic is \"{magic}\", not NPUMDIMG")
        }
        None => bail!("psp to-iso: {} carries no DATA.PSAR", input.display()),
    }
    if psar.size < HEADER_LEN as u64 {
        bail!(
            "psp to-iso: DATA.PSAR is {} bytes, shorter than the {HEADER_LEN}-byte NPUMDIMG header",
            psar.size
        );
    }

    file.seek(SeekFrom::Start(psar.offset))?;
    let mut header = [0u8; HEADER_LEN];
    file.read_exact(&mut header)?;

    let iso_block = le32(&header, 0x0c);
    if iso_block == 0 || iso_block > MAX_ISO_BLOCK {
        bail!("psp to-iso: unsupported DATA.PSAR block size of {iso_block} sectors");
    }

    let mac = bb_mac(&header[..MAC_LEN]);
    let cipher = BbCipher::new(
        &mac,
        &block_at(&header, MAC_LEN),
        &block_at(&header, KEY_MODIFIER),
    );
    cipher.apply(0, &mut header[BODY]);

    let iso_start = le32(&header, 0x54);
    let iso_end = le32(&header, 0x64);
    let iso_table = le32(&header, 0x6c) as u64;
    let Some(iso_total) = iso_end
        .checked_sub(iso_start)
        .and_then(|n| n.checked_sub(1))
    else {
        bail!(
            "psp to-iso: decrypted NPUMDIMG header is inconsistent, sector range {iso_start}..{iso_end}"
        );
    };

    let block_count = iso_total.div_ceil(iso_block);
    if iso_table + u64::from(block_count) * TABLE_ENTRY_LEN > psar.size {
        bail!("psp to-iso: block table of {block_count} entries runs past the end of DATA.PSAR");
    }

    let block_bytes = iso_block as usize * ISO_SECTOR_SIZE;
    let total = u64::from(block_count) * block_bytes as u64;
    progress.start(
        total,
        &format!(
            "Converting NPUMDIMG to ISO (~{:.2} MB)",
            total as f64 / BYTES_PER_MB
        ),
    );

    let mut out = BufWriter::new(
        File::create(output).with_context(|| format!("psp to-iso: create {}", output.display()))?,
    );
    let mut data = vec![0u8; block_bytes];
    let mut plain = vec![0u8; block_bytes];

    for index in 0..block_count {
        file.seek(SeekFrom::Start(
            psar.offset + iso_table + u64::from(index) * TABLE_ENTRY_LEN,
        ))?;
        let mut entry = [0u8; TABLE_ENTRY_LEN as usize];
        file.read_exact(&mut entry)?;
        let mut t = [0u32; 8];
        for (k, word) in t.iter_mut().enumerate() {
            *word = le32(&entry, k * 4);
        }

        let block_offset = t[4] ^ t[2] ^ t[3];
        let block_size = t[5] ^ t[1] ^ t[2];
        let block_flags = t[6] ^ t[0] ^ t[3];

        let size = block_size as usize;
        if size == 0 || size > block_bytes {
            bail!(
                "psp to-iso: block {index} is {block_size} bytes, past the {block_bytes}-byte maximum"
            );
        }
        if u64::from(block_offset) + u64::from(block_size) > psar.size {
            bail!("psp to-iso: block {index} runs past the end of DATA.PSAR");
        }

        file.seek(SeekFrom::Start(psar.offset + u64::from(block_offset)))?;
        file.read_exact(&mut data[..size])?;
        if block_flags & FLAG_PLAIN == 0 {
            cipher.apply(block_offset / 16, &mut data[..size.next_multiple_of(16)]);
        }

        if size == block_bytes {
            out.write_all(&data)?;
        } else {
            let written = lzrc::decompress(&data[..size], &mut plain)
                .with_context(|| format!("psp to-iso: block {index}"))?;
            if written != block_bytes {
                bail!(
                    "psp to-iso: block {index} decompressed to {written} bytes, expected {block_bytes}"
                );
            }
            out.write_all(&plain)?;
        }
        progress.inc(block_bytes as u64);
    }

    out.flush()?;
    progress.finish();
    Ok(())
}

fn le32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(buf[offset..offset + 4].try_into().expect("4-byte slice"))
}

fn block_at(buf: &[u8], offset: usize) -> [u8; 16] {
    buf[offset..offset + 16].try_into().expect("16-byte slice")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sony::psp::amctrl::test_fixtures::{cipher_from_iv, version_key_for};
    use crate::sony::psp::pbp::test_fixtures::build_pbp;
    use crate::util::NoProgress;

    const FIXTURE_IV: [u8; 16] = [
        0x0f, 0x1e, 0x2d, 0x3c, 0x4b, 0x5a, 0x69, 0x78, 0x87, 0x96, 0xa5, 0xb4, 0xc3, 0xd2, 0xe1,
        0xf0,
    ];
    const KEY_MODIFIER_BYTES: [u8; 16] = [
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
        0x00,
    ];

    /// Builds an `NPUMDIMG` PSAR of `sectors.len()` uncompressed one-sector
    /// blocks, encrypted with the same BBCipher the reader derives.
    fn build_npumdimg(sectors: &[Vec<u8>], plain_flags: bool) -> Vec<u8> {
        let iso_block = 1u32;
        let block_bytes = ISO_SECTOR_SIZE;
        let count = sectors.len() as u32;
        let table_offset = HEADER_LEN as u32;
        let data_offset = table_offset + count * TABLE_ENTRY_LEN as u32;

        let mut header = [0u8; HEADER_LEN];
        header[..8].copy_from_slice(b"NPUMDIMG");
        header[0x0c..0x10].copy_from_slice(&iso_block.to_le_bytes());
        header[KEY_MODIFIER..KEY_MODIFIER + 16].copy_from_slice(&KEY_MODIFIER_BYTES);

        // The encrypted body carries the sector range and the table offset.
        let mut body = [0u8; 0x60];
        body[0x14..0x18].copy_from_slice(&0u32.to_le_bytes());
        body[0x24..0x28].copy_from_slice(&(count + 1).to_le_bytes());
        body[0x2c..0x30].copy_from_slice(&table_offset.to_le_bytes());
        let cipher = cipher_from_iv(FIXTURE_IV);
        cipher.apply(0, &mut body);
        header[BODY].copy_from_slice(&body);

        // The version key sits outside the MAC range, so it can be solved for
        // once the rest of the header is final.
        let mac = bb_mac(&header[..MAC_LEN]);
        let version_key = version_key_for(&FIXTURE_IV, &mac, &KEY_MODIFIER_BYTES);
        header[MAC_LEN..MAC_LEN + 16].copy_from_slice(&version_key);

        let mut table = Vec::new();
        let mut blocks = Vec::new();
        for (i, sector) in sectors.iter().enumerate() {
            let offset = data_offset + i as u32 * block_bytes as u32;
            let flags = if plain_flags { FLAG_PLAIN } else { 0 };
            for word in [0, 0, 0, 0, offset, block_bytes as u32, flags, 0] {
                table.extend_from_slice(&word.to_le_bytes());
            }
            let mut block = sector.clone();
            if !plain_flags {
                cipher.apply(offset / 16, &mut block);
            }
            blocks.extend_from_slice(&block);
        }

        let mut psar = header.to_vec();
        psar.extend_from_slice(&table);
        psar.extend_from_slice(&blocks);
        psar
    }

    fn sector(fill: u8) -> Vec<u8> {
        (0..ISO_SECTOR_SIZE)
            .map(|i| fill ^ (i as u8).rotate_left(3))
            .collect()
    }

    fn eboot_bytes(psar: &[u8]) -> Vec<u8> {
        build_pbp(0x10000, &[b"sfo", &[], &[], &[], &[], &[], b"psp", psar])
    }

    fn write_eboot(psar: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("EBOOT.PBP");
        std::fs::write(&path, eboot_bytes(psar)).expect("write eboot");
        (dir, path)
    }

    /// Wraps `eboot` as the sole `EBOOT.PBP` item of a synthetic PSP package.
    fn build_psp_pkg(content_type: u32, eboot: Vec<u8>) -> Vec<u8> {
        crate::sony::vita::pkg::test_fixtures::build_pkg(
            2,
            1,
            content_type,
            &[crate::sony::vita::pkg::test_fixtures::Entry {
                name: "USRDIR/CONTENT/EBOOT.PBP",
                data: eboot,
                is_dir: false,
                psp_type: 0x90,
            }],
        )
    }

    #[test]
    fn writes_the_decrypted_iso() {
        let sectors = vec![sector(0x01), sector(0x02), sector(0x03)];
        let (dir, input) = write_eboot(&build_npumdimg(&sectors, false));
        let output = dir.path().join("out.iso");

        to_iso(&NoProgress, &input, &output).expect("to iso");

        let iso = std::fs::read(&output).expect("read iso");
        assert_eq!(iso, sectors.concat());
    }

    #[test]
    fn honours_the_plain_block_flag() {
        let sectors = vec![sector(0xAB), sector(0xCD)];
        let (dir, input) = write_eboot(&build_npumdimg(&sectors, true));
        let output = dir.path().join("out.iso");

        to_iso(&NoProgress, &input, &output).expect("to iso");
        assert_eq!(std::fs::read(&output).expect("read iso"), sectors.concat());
    }

    #[test]
    fn rejects_a_ps1_classic_psar() {
        let (dir, input) = write_eboot(b"PSISOIMG\0\0\0\0\0\0\0\0");
        let err = to_iso(&NoProgress, &input, &dir.path().join("out.iso")).expect_err("rejected");
        assert!(err.to_string().contains("PSISOIMG"), "{err}");
    }

    #[test]
    fn rejects_a_container_without_a_psar() {
        let bytes = build_pbp(1, &[b"sfo", &[], &[], &[], &[], &[], b"psp", &[]]);
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("EBOOT.PBP");
        std::fs::write(&input, bytes).expect("write eboot");
        assert!(to_iso(&NoProgress, &input, &dir.path().join("out.iso")).is_err());
    }

    #[test]
    fn rejects_a_corrupt_header() {
        let mut psar = build_npumdimg(&[sector(0x10)], false);
        // Flipping a byte inside the MAC range changes the derived key, so
        // the decrypted geometry stops making sense.
        psar[0x50] ^= 0xFF;
        let (dir, input) = write_eboot(&psar);
        assert!(to_iso(&NoProgress, &input, &dir.path().join("out.iso")).is_err());
    }

    #[test]
    fn converts_an_eboot_read_straight_out_of_a_pkg() {
        let sectors = vec![sector(0x11), sector(0x22), sector(0x33)];
        let eboot = eboot_bytes(&build_npumdimg(&sectors, false));
        let dir = tempfile::tempdir().expect("temp dir");

        let pbp_input = dir.path().join("EBOOT.PBP");
        std::fs::write(&pbp_input, &eboot).expect("write eboot");
        let pbp_output = dir.path().join("from-pbp.iso");
        to_iso(&NoProgress, &pbp_input, &pbp_output).expect("pbp to iso");

        let pkg_input = dir.path().join("game.pkg");
        std::fs::write(&pkg_input, build_psp_pkg(7, eboot)).expect("write pkg");
        let pkg_output = dir.path().join("from-pkg.iso");
        to_iso(&NoProgress, &pkg_input, &pkg_output).expect("pkg to iso");

        let iso = std::fs::read(&pkg_output).expect("read iso");
        assert_eq!(iso, sectors.concat());
        assert_eq!(iso, std::fs::read(&pbp_output).expect("read iso"));
    }

    #[test]
    fn rejects_a_pkg_without_an_eboot() {
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("game.pkg");
        let pkg = crate::sony::vita::pkg::test_fixtures::build_pkg(
            2,
            1,
            7,
            &[crate::sony::vita::pkg::test_fixtures::Entry {
                name: "USRDIR/CONTENT/PARAM.SFO",
                data: vec![0xAB; 64],
                is_dir: false,
                psp_type: 0x90,
            }],
        );
        std::fs::write(&input, pkg).expect("write pkg");

        let err = to_iso(&NoProgress, &input, &dir.path().join("out.iso")).expect_err("rejected");
        assert!(err.to_string().contains("EBOOT.PBP"), "{err}");
    }

    #[test]
    fn rejects_a_ps1_classic_pkg() {
        let eboot = eboot_bytes(&build_npumdimg(&[sector(0x44)], false));
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("classic.pkg");
        std::fs::write(&input, build_psp_pkg(6, eboot)).expect("write pkg");

        let err = to_iso(&NoProgress, &input, &dir.path().join("out.iso")).expect_err("rejected");
        assert!(err.to_string().contains("PS1 Classic packages"), "{err}");
    }

    #[test]
    fn rejects_an_input_that_is_neither_a_pbp_nor_a_pkg() {
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("garbage.bin");
        std::fs::write(&input, b"JUNKJUNK").expect("write garbage");

        let err = to_iso(&NoProgress, &input, &dir.path().join("out.iso")).expect_err("rejected");
        let text = err.to_string();
        assert!(
            text.contains("EBOOT.PBP") && text.contains(".pkg"),
            "{text}"
        );
    }

    #[test]
    fn rejects_a_block_table_past_the_end_of_the_psar() {
        let mut psar = build_npumdimg(&[sector(0x20)], false);
        psar.truncate(HEADER_LEN + 8);
        let (dir, input) = write_eboot(&psar);
        assert!(to_iso(&NoProgress, &input, &dir.path().join("out.iso")).is_err());
    }
}
