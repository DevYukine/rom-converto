//! Synthetic NCAs and `KeySet`s for unit tests. Lets crypto round-trip
//! tests run without real prod.keys or real game files.

use std::collections::HashMap;
use std::sync::Mutex;

use aes::Aes128;
use aes::cipher::array::Array;
use aes::cipher::{BlockCipherEncrypt, KeyInit};
use sha2::{Digest, Sha256};

use crate::nintendo::nx::constants::{
    ENC_AES_CTR, ENC_AES_CTR_EX, NCA_FS_ENTRY_OFFSET, NCA_FS_HEADER_OFFSET, NCA_HEADER_SIZE,
    NCA3_MAGIC,
};
use crate::nintendo::nx::crypto::aes_ctr::{apply_ctr, apply_ctr_at};
use crate::nintendo::nx::crypto::aes_xts::encrypt_nca_header;
use crate::nintendo::nx::crypto::derive::{KEY_AREA_KEY_COUNT, KEY_AREA_KEY_SIZE, KEY_AREA_TOTAL};
use crate::nintendo::nx::decrypt::bucket_tree::{AesCtrExEntry, IndirectEntry, build_table};
use crate::nintendo::nx::decrypt::nca::ctr_iv;
use crate::nintendo::nx::error::NxResult;
use crate::nintendo::nx::keys::{KeyAreaKind, KeySet};
use crate::nintendo::nx::merge::round_up_media_unit;
use crate::nintendo::nx::models::cnmt::CNMT_CONTENT_TYPE_PROGRAM;
use crate::nintendo::nx::models::hfs0::{
    self as hfs0_mod, DEFAULT_HASHED_REGION, Hfs0FileSpec, Hfs0LayoutHints, hash_first_chunk,
};
use crate::nintendo::nx::models::nca::{
    CONTENT_TYPE_META, FS_TYPE_PARTITION_FS, FS_TYPE_ROMFS, FsHeader,
    HASH_TYPE_HIERARCHICAL_INTEGRITY, HASH_TYPE_HIERARCHICAL_SHA256, initial_ctr_for_offset,
};
use crate::nintendo::nx::models::pfs0::{self as pfs0_mod, Pfs0LayoutHints};
use crate::nintendo::nx::models::xci::{MEDIA_UNIT, XCI_PREFIX_SIZE, build_xci_prefix};
use crate::nintendo::nx::walker::NcaInput;

/// Captures every `warn()` call; every other method is a no-op.
#[derive(Default)]
pub struct WarnRecorder {
    pub warnings: Mutex<Vec<String>>,
}

impl crate::util::ProgressReporter for WarnRecorder {
    fn start(&self, _total: u64, _msg: &str) {}
    fn inc(&self, _delta: u64) {}
    fn finish(&self) {}
    fn warn(&self, message: &str) {
        self.warnings
            .lock()
            .expect("warn lock")
            .push(message.into());
    }
}

pub const TEST_HEADER_KEY: [u8; 32] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
];

pub const TEST_KAK_APPLICATION_00: [u8; 16] = [
    0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55,
];

pub const TEST_BODY_KEY: [u8; 16] = [
    0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
];

/// Titlekek for master key index 0, wrapping the title keys of
/// [`build_common_ticket`].
pub const TEST_TITLEKEK_00: [u8; 16] = [
    0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77,
];

/// Plain title key of the NCAs built by [`build_title_key_nca`].
pub const TEST_TITLE_KEY: [u8; 16] = [
    0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF, 0xD0,
];

pub fn synthetic_keyset() -> KeySet {
    let mut kak = HashMap::new();
    kak.insert((KeyAreaKind::Application, 0), TEST_KAK_APPLICATION_00);
    KeySet {
        header_key: Some(TEST_HEADER_KEY),
        key_area_keys: kak,
        ..KeySet::default()
    }
}

/// [`synthetic_keyset`] plus [`TEST_TITLEKEK_00`], enough to unwrap the
/// title key of a common ticket from [`build_common_ticket`].
pub fn synthetic_keyset_with_titlekek() -> KeySet {
    let mut keys = synthetic_keyset();
    keys.titlekeks.insert(0, TEST_TITLEKEK_00);
    keys
}

/// Builds a common (non-personalized) ticket for `rights_id` whose title
/// key `title_key` is wrapped with [`TEST_TITLEKEK_00`]. Layout per
/// `Ticket::parse`: RSA-2048 signature type, data block at 0x140.
pub fn build_common_ticket(rights_id: [u8; 16], title_key: [u8; 16]) -> Vec<u8> {
    let mut buf = vec![0u8; 0x2C0];
    buf[0..4].copy_from_slice(&0x10004u32.to_le_bytes());
    let data_off = 0x140;
    let cipher = Aes128::new_from_slice(&TEST_TITLEKEK_00).unwrap();
    let mut block = Array::from(title_key);
    cipher.encrypt_block(&mut block);
    buf[data_off + 0x40..data_off + 0x50].copy_from_slice(block.as_slice());
    buf[data_off + 0x141] = 0; // common, not personalized
    buf[data_off + 0x145] = 0; // master key revision 0
    buf[data_off + 0x160..data_off + 0x170].copy_from_slice(&rights_id);
    buf
}

pub fn encrypt_key_area_block(plain_keys: [[u8; 16]; KEY_AREA_KEY_COUNT]) -> [u8; KEY_AREA_TOTAL] {
    let cipher = Aes128::new_from_slice(&TEST_KAK_APPLICATION_00).unwrap();
    let mut out = [0u8; KEY_AREA_TOTAL];
    for (i, plain) in plain_keys.iter().enumerate() {
        let mut block = Array::from(*plain);
        cipher.encrypt_block(&mut block);
        out[i * KEY_AREA_KEY_SIZE..(i + 1) * KEY_AREA_KEY_SIZE].copy_from_slice(block.as_slice());
    }
    out
}

/// Build a minimal encrypted NCA whose section 0 is AES-CTR encrypted
/// `plaintext_section`, readable by [`synthetic_keyset`].
pub fn build_synthetic_nca(plaintext_section: &[u8]) -> Vec<u8> {
    build_synthetic_nca_with_rights_id(plaintext_section, [0u8; 16])
}

/// Variant of [`build_synthetic_nca`] carrying `rights_id`. With a
/// nonzero id and no matching ticket bundled, key derivation fails
/// with a missing title key.
pub fn build_synthetic_nca_with_rights_id(
    plaintext_section: &[u8],
    rights_id: [u8; 16],
) -> Vec<u8> {
    build_nca_with_body_key(plaintext_section, rights_id, TEST_BODY_KEY, TEST_BODY_KEY)
}

/// Rights-protected NCA whose section 0 is AES-CTR encrypted with
/// [`TEST_TITLE_KEY`], the way a title-key NCA is on a console. The key
/// area holds unrelated keys, so only a ticket's title key reads it.
/// `plaintext_section` must be a multiple of 0x200 bytes.
pub fn build_title_key_nca(plaintext_section: &[u8], rights_id: [u8; 16]) -> Vec<u8> {
    build_nca_with_body_key(plaintext_section, rights_id, [0x99; 16], TEST_TITLE_KEY)
}

fn build_nca_with_body_key(
    plaintext_section: &[u8],
    rights_id: [u8; 16],
    key_area_body_key: [u8; 16],
    body_key: [u8; 16],
) -> Vec<u8> {
    let mut header = [0u8; NCA_HEADER_SIZE];
    header[0x200..0x204].copy_from_slice(&NCA3_MAGIC);
    header[0x207] = 0;
    header[0x220] = 1;
    header[0x230..0x240].copy_from_slice(&rights_id);

    let section_start_byte = 0x4000u64;
    let section_size = plaintext_section.len() as u64;
    let section_end_byte = section_start_byte + section_size;
    let start_sector = (section_start_byte / 0x200) as u32;
    let end_sector = (section_end_byte / 0x200) as u32;

    header[NCA_FS_ENTRY_OFFSET..NCA_FS_ENTRY_OFFSET + 4]
        .copy_from_slice(&start_sector.to_le_bytes());
    header[NCA_FS_ENTRY_OFFSET + 4..NCA_FS_ENTRY_OFFSET + 8]
        .copy_from_slice(&end_sector.to_le_bytes());

    let fs0_off = NCA_FS_HEADER_OFFSET;
    header[fs0_off + 4] = ENC_AES_CTR;
    let ctr_low: u32 = 0x12345678;
    let ctr_high: u32 = 0x9ABCDEF0;
    header[fs0_off + 0x140..fs0_off + 0x144].copy_from_slice(&ctr_low.to_le_bytes());
    header[fs0_off + 0x144..fs0_off + 0x148].copy_from_slice(&ctr_high.to_le_bytes());

    let key_area = encrypt_key_area_block([[0x11; 16], [0x22; 16], key_area_body_key, [0x44; 16]]);
    header[0x300..0x340].copy_from_slice(&key_area);

    let keys = synthetic_keyset();
    encrypt_nca_header(&mut header, keys.header_key().unwrap()).unwrap();

    let mut nca = vec![0u8; section_start_byte as usize];
    nca[..NCA_HEADER_SIZE].copy_from_slice(&header);

    let mut encrypted = plaintext_section.to_vec();
    let counter = initial_ctr_for_offset(
        &FsHeader {
            section_ctr_low: ctr_low,
            section_ctr_high: ctr_high,
            ..Default::default()
        },
        section_start_byte,
    );
    apply_ctr(&body_key, &counter, &mut encrypted).unwrap();
    nca.extend_from_slice(&encrypted);
    nca
}

/// One CNMT content record: content id, record type, id offset.
pub type CnmtRecord = ([u8; 16], u8, u8);

/// Serialize a minimal `PackagedContentMeta` (CNMT) blob matching the
/// layout `Cnmt::parse` expects; content records carry no hash or size.
fn build_cnmt_bytes(
    title_id: u64,
    version: u32,
    title_type: u8,
    extended_header: &[u8],
    records: &[CnmtRecord],
) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&title_id.to_le_bytes());
    b.extend_from_slice(&version.to_le_bytes());
    b.push(title_type);
    b.push(0); // reserved
    b.extend_from_slice(&(extended_header.len() as u16).to_le_bytes());
    b.extend_from_slice(&(records.len() as u16).to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes()); // content_meta_count
    b.push(0); // attributes
    b.push(0); // storage_id
    b.push(0); // install_type
    b.push(0); // reserved
    b.extend_from_slice(&0u64.to_le_bytes()); // required_download_system_version
    b.extend_from_slice(extended_header);
    for (cid, content_type, id_offset) in records {
        b.extend_from_slice(&[0u8; 32]); // hash
        b.extend_from_slice(cid); // content_id
        b.extend_from_slice(&[0u8; 6]); // size (48-bit)
        b.push(*content_type);
        b.push(*id_offset);
    }
    b
}

/// Build an encrypted Meta NCA (content_type = Meta) readable by
/// `read_meta_cnmt` under [`synthetic_keyset`]. Section 0 holds a PFS0
/// wrapping a single `{title_id:016x}.cnmt` file whose records are all
/// Program content with no extended header.
pub fn build_meta_nca(
    title_id: u64,
    version: u32,
    title_type: u8,
    contents: &[[u8; 16]],
) -> Vec<u8> {
    let records: Vec<CnmtRecord> = contents
        .iter()
        .map(|cid| (*cid, CNMT_CONTENT_TYPE_PROGRAM, 0))
        .collect();
    build_meta_nca_records(title_id, version, title_type, &[], &records)
}

/// [`build_meta_nca`] with an explicit extended header and typed records.
pub fn build_meta_nca_records(
    title_id: u64,
    version: u32,
    title_type: u8,
    extended_header: &[u8],
    records: &[CnmtRecord],
) -> Vec<u8> {
    let cnmt = build_cnmt_bytes(title_id, version, title_type, extended_header, records);
    let cnmt_name = format!("{title_id:016x}.cnmt");
    let hdr = pfs0_mod::build_header(
        &[(cnmt_name, cnmt.len() as u64)],
        &Pfs0LayoutHints::default(),
    )
    .unwrap();
    let mut section = hdr.bytes;
    section.extend_from_slice(&cnmt);
    while !section.len().is_multiple_of(0x200) {
        section.push(0);
    }

    let mut header = [0u8; NCA_HEADER_SIZE];
    header[0x200..0x204].copy_from_slice(&NCA3_MAGIC);
    header[0x205] = CONTENT_TYPE_META;
    header[0x207] = 0; // key_index = Application
    header[0x210..0x218].copy_from_slice(&title_id.to_le_bytes());
    header[0x220] = 1; // key_generation_new = master_key_00

    let section_start_byte = 0x4000u64;
    let section_end_byte = section_start_byte + section.len() as u64;
    let start_sector = (section_start_byte / 0x200) as u32;
    let end_sector = (section_end_byte / 0x200) as u32;
    let entry_off = NCA_FS_ENTRY_OFFSET;
    header[entry_off..entry_off + 4].copy_from_slice(&start_sector.to_le_bytes());
    header[entry_off + 4..entry_off + 8].copy_from_slice(&end_sector.to_le_bytes());

    let fs0_off = NCA_FS_HEADER_OFFSET;
    header[fs0_off + 2] = FS_TYPE_PARTITION_FS;
    header[fs0_off + 3] = HASH_TYPE_HIERARCHICAL_SHA256;
    header[fs0_off + 4] = ENC_AES_CTR;
    // One hash layer over the whole section, whose data layer (the
    // PFS0) starts at section offset 0.
    header[fs0_off + 0x2C..fs0_off + 0x30].copy_from_slice(&2u32.to_le_bytes());
    header[fs0_off + 0x48..fs0_off + 0x50].copy_from_slice(&(section.len() as u64).to_le_bytes());
    let ctr_low: u32 = 0;
    let ctr_high: u32 = 0;
    header[fs0_off + 0x140..fs0_off + 0x144].copy_from_slice(&ctr_low.to_le_bytes());
    header[fs0_off + 0x144..fs0_off + 0x148].copy_from_slice(&ctr_high.to_le_bytes());

    let key_area = encrypt_key_area_block([[0x11; 16], [0x22; 16], TEST_BODY_KEY, [0x44; 16]]);
    header[0x300..0x340].copy_from_slice(&key_area);

    let keys = synthetic_keyset();
    let header_key = keys.header_key().unwrap();
    encrypt_nca_header(&mut header, header_key).unwrap();

    let mut nca = vec![0u8; section_start_byte as usize];
    nca[..NCA_HEADER_SIZE].copy_from_slice(&header);

    let mut encrypted = section;
    let counter = initial_ctr_for_offset(
        &FsHeader {
            section_ctr_low: ctr_low,
            section_ctr_high: ctr_high,
            ..Default::default()
        },
        section_start_byte,
    );
    apply_ctr(&TEST_BODY_KEY, &counter, &mut encrypted).unwrap();
    nca.extend_from_slice(&encrypted);
    nca
}

/// Serialize a PFS0 (NSP) over the given named files.
pub fn build_test_nsp(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let specs: Vec<(String, u64)> = files
        .iter()
        .map(|(n, d)| (n.clone(), d.len() as u64))
        .collect();
    let hdr = pfs0_mod::build_header(&specs, &Pfs0LayoutHints::default()).unwrap();
    let mut out = hdr.bytes;
    for (_, d) in files {
        out.extend_from_slice(d);
    }
    out
}

/// Serialize a minimal but valid XCI whose secure partition holds the
/// given NCAs; update/normal partitions are empty stubs. Mirrors the
/// layout produced by the super-XCI writer so `list_container` relists it.
pub fn build_test_xci(ncas: &[(String, Vec<u8>)]) -> Vec<u8> {
    build_test_xci_partitions(&[("secure", ncas)])
}

/// Named partition with its member files, for [`build_test_xci_partitions`].
pub type PartitionFiles<'a> = (&'a str, &'a [(String, Vec<u8>)]);

/// Serialize a minimal but valid XCI where each named partition holds
/// its given NCAs and the remaining partitions are empty stubs. Mirrors
/// the layout produced by the super-XCI writer so `list_container`
/// relists it.
pub fn build_test_xci_partitions(files_by_partition: &[PartitionFiles<'_>]) -> Vec<u8> {
    for (name, _) in files_by_partition {
        assert!(
            matches!(*name, "update" | "normal" | "secure"),
            "unknown XCI partition {name}"
        );
    }
    let partition_blob = |name: &str| -> Vec<u8> {
        let files = files_by_partition
            .iter()
            .find(|(p, _)| *p == name)
            .map(|(_, files)| *files)
            .unwrap_or(&[]);
        let specs: Vec<Hfs0FileSpec> = files
            .iter()
            .map(|(name, data)| Hfs0FileSpec {
                name: name.clone(),
                size: data.len() as u64,
                sha256: hash_first_chunk(data, DEFAULT_HASHED_REGION),
                hashed_region_size: DEFAULT_HASHED_REGION,
            })
            .collect();
        let natural_len = hfs0_mod::build_header(&specs, &Hfs0LayoutHints::default())
            .unwrap()
            .bytes
            .len();
        let header = hfs0_mod::build_header(
            &specs,
            &Hfs0LayoutHints {
                target_total_header_size: Some(round_up_media_unit(natural_len)),
                first_file_data_offset: 0,
            },
        )
        .unwrap();

        let data_bytes: u64 = files.iter().map(|(_, d)| d.len() as u64).sum();
        let unpadded = header.bytes.len() as u64 + data_bytes;
        let total = unpadded.div_ceil(MEDIA_UNIT) * MEDIA_UNIT;

        let mut blob = header.bytes.clone();
        for (_, data) in files {
            blob.extend_from_slice(data);
        }
        blob.extend_from_slice(&vec![0u8; (total - unpadded) as usize]);
        blob
    };

    let update_blob = partition_blob("update");
    let normal_blob = partition_blob("normal");
    let secure_blob = partition_blob("secure");

    let root_spec = |name: &str, header: &[u8], size: u64| Hfs0FileSpec {
        name: name.into(),
        size,
        sha256: hash_first_chunk(header, DEFAULT_HASHED_REGION),
        hashed_region_size: DEFAULT_HASHED_REGION,
    };
    let root_specs = vec![
        root_spec("update", &update_blob, update_blob.len() as u64),
        root_spec("normal", &normal_blob, normal_blob.len() as u64),
        root_spec("secure", &secure_blob, secure_blob.len() as u64),
    ];
    let root_header = hfs0_mod::build_header(
        &root_specs,
        &Hfs0LayoutHints {
            target_total_header_size: Some(MEDIA_UNIT as usize),
            first_file_data_offset: 0,
        },
    )
    .unwrap();

    let secure_offset = XCI_PREFIX_SIZE as u64
        + root_header.bytes.len() as u64
        + update_blob.len() as u64
        + normal_blob.len() as u64;

    let mut out = vec![0u8; XCI_PREFIX_SIZE];
    out.extend_from_slice(&root_header.bytes);
    out.extend_from_slice(&update_blob);
    out.extend_from_slice(&normal_blob);
    out.extend_from_slice(&secure_blob);

    let total_size = out.len() as u64;
    let prefix = build_xci_prefix(secure_offset, total_size, &root_header.bytes);
    out[..XCI_PREFIX_SIZE].copy_from_slice(&prefix);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::models::ticket::Ticket;
    use crate::nintendo::nx::walker::NcaWalker;
    use std::sync::Arc;

    struct Mem(Vec<u8>);

    impl crate::nintendo::nx::walker::NcaInput for Mem {
        fn read_exact_at(
            &self,
            buf: &mut [u8],
            offset: u64,
        ) -> crate::nintendo::nx::error::NxResult<()> {
            let start = offset as usize;
            buf.copy_from_slice(&self.0[start..start + buf.len()]);
            Ok(())
        }
    }

    #[test]
    fn title_key_nca_reads_back_through_its_common_ticket() {
        let rights_id = [0xA5; 16];
        let plain: Vec<u8> = (0..0x400u32).map(|i| (i % 253) as u8).collect();
        let nca = build_title_key_nca(&plain, rights_id);
        let nca_len = nca.len() as u64;

        let ticket = Ticket::parse(&build_common_ticket(rights_id, TEST_TITLE_KEY)).unwrap();
        assert_eq!(ticket.rights_id, rights_id);
        assert!(!ticket.personalized);
        let mut keys = synthetic_keyset_with_titlekek();
        keys.title_keys
            .insert(ticket.rights_id, ticket.encrypted_title_key);

        let walker = NcaWalker::open(Arc::new(Mem(nca)), 0, nca_len, &keys).unwrap();
        let section = walker.sections.first().unwrap();
        let mut got = vec![0u8; plain.len()];
        walker.read_section_plain(section, 0, &mut got).unwrap();
        assert_eq!(got, plain);
    }
}

/// Section offset of the IVFC data level (the RomFS image) in
/// [`romfs_section`] and [`patch_romfs_section`].
pub const DATA_LEVEL: u64 = 0x4000;

/// FS header generation every fixture section is encrypted under.
pub const FIXTURE_GENERATION: u32 = 1;

/// Deterministic xorshift bytes, so a read from a wrong offset never
/// matches by accident.
pub fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

/// In-memory [`NcaInput`].
pub struct MemInput(pub Vec<u8>);

impl NcaInput for MemInput {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> NxResult<()> {
        let src = usize::try_from(offset)
            .ok()
            .and_then(|start| self.0.get(start..start.checked_add(buf.len())?))
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

/// One section of [`build_nca`]: plaintext body, FS header, and the CTR
/// runs `(start, end, generation)`, section-relative, it is encrypted with.
#[derive(Clone)]
pub struct FixtureSection {
    pub body: Vec<u8>,
    pub fs: [u8; 0x200],
    pub runs: Vec<(u64, u64, u32)>,
}

fn ctr_section(mut body: Vec<u8>, mut fs: [u8; 0x200]) -> FixtureSection {
    body.resize(body.len().next_multiple_of(0x200), 0);
    fs[4] = ENC_AES_CTR;
    fs[0x140..0x144].copy_from_slice(&FIXTURE_GENERATION.to_le_bytes());
    FixtureSection {
        runs: vec![(0, body.len() as u64, FIXTURE_GENERATION)],
        body,
        fs,
    }
}

/// ExeFS-style section: a PFS0 of `files` behind a 0x200-byte
/// HierarchicalSha256 hash table (left zero, but covered by the master
/// hash).
pub fn pfs0_section(files: &[(String, Vec<u8>)]) -> FixtureSection {
    let pfs0 = build_test_nsp(files);
    let mut body = vec![0u8; 0x200];
    body.extend_from_slice(&pfs0);
    let mut fs = [0u8; 0x200];
    fs[0x08..0x28].copy_from_slice(&Sha256::digest(&body[..0x20]));
    fs[0] = 2;
    fs[2] = FS_TYPE_PARTITION_FS;
    fs[3] = HASH_TYPE_HIERARCHICAL_SHA256;
    fs[0x28..0x2C].copy_from_slice(&0x1000u32.to_le_bytes());
    fs[0x2C..0x30].copy_from_slice(&2u32.to_le_bytes());
    fs[0x38..0x40].copy_from_slice(&0x20u64.to_le_bytes());
    fs[0x40..0x48].copy_from_slice(&0x200u64.to_le_bytes());
    fs[0x48..0x50].copy_from_slice(&(pfs0.len() as u64).to_le_bytes());
    ctr_section(body, fs)
}

/// RomFS image of `files` (`/`-separated paths, data laid out in list
/// order after a 0x200-byte gap, metadata tables last, every entry in one
/// hash bucket). Returns the image and each file's data offset in it.
pub fn romfs_image<S: AsRef<str>>(files: &[(S, Vec<u8>)]) -> (Vec<u8>, Vec<u64>) {
    const EMPTY: u32 = u32::MAX;
    const FILE_DATA: u64 = 0x200;
    struct Dir {
        name: String,
        parent: usize,
        dirs: Vec<usize>,
        files: Vec<usize>,
    }
    let mut dirs = vec![Dir {
        name: String::new(),
        parent: 0,
        dirs: Vec::new(),
        files: Vec::new(),
    }];
    let mut file_dirs = Vec::new();
    let mut file_names = Vec::new();
    for (path, _) in files {
        let mut parts: Vec<&str> = path.as_ref().split('/').collect();
        let name = parts.pop().unwrap();
        let mut cur = 0;
        for part in parts {
            cur = match dirs[cur]
                .dirs
                .iter()
                .copied()
                .find(|&d| dirs[d].name == part)
            {
                Some(d) => d,
                None => {
                    dirs.push(Dir {
                        name: part.into(),
                        parent: cur,
                        dirs: Vec::new(),
                        files: Vec::new(),
                    });
                    let id = dirs.len() - 1;
                    dirs[cur].dirs.push(id);
                    id
                }
            };
        }
        dirs[cur].files.push(file_names.len());
        file_dirs.push(cur);
        file_names.push(name);
    }

    let mut dir_offs = Vec::new();
    let mut at = 0;
    for d in &dirs {
        dir_offs.push(at as u32);
        at += 0x18 + d.name.len().next_multiple_of(4);
    }
    let mut file_offs = Vec::new();
    at = 0;
    for name in &file_names {
        file_offs.push(at as u32);
        at += 0x20 + name.len().next_multiple_of(4);
    }
    let next = |list: &[usize], item: usize, offs: &[u32]| {
        list.iter()
            .position(|&x| x == item)
            .and_then(|p| list.get(p + 1))
            .map_or(EMPTY, |&x| offs[x])
    };

    let mut dir_table = Vec::new();
    for (i, d) in dirs.iter().enumerate() {
        for v in [
            dir_offs[d.parent],
            next(&dirs[d.parent].dirs, i, &dir_offs),
            d.dirs.first().map_or(EMPTY, |&c| dir_offs[c]),
            d.files.first().map_or(EMPTY, |&f| file_offs[f]),
            dir_offs.get(i + 1).copied().unwrap_or(EMPTY),
            d.name.len() as u32,
        ] {
            dir_table.extend_from_slice(&v.to_le_bytes());
        }
        dir_table.extend_from_slice(d.name.as_bytes());
        dir_table.resize(dir_table.len().next_multiple_of(4), 0);
    }

    let mut data = Vec::new();
    let mut data_offs = Vec::new();
    for (_, bytes) in files {
        data.resize(data.len().next_multiple_of(0x10), 0);
        data_offs.push(data.len() as u64);
        data.extend_from_slice(bytes);
    }
    let mut file_table = Vec::new();
    for (i, name) in file_names.iter().enumerate() {
        let parent = file_dirs[i];
        file_table.extend_from_slice(&dir_offs[parent].to_le_bytes());
        file_table.extend_from_slice(&next(&dirs[parent].files, i, &file_offs).to_le_bytes());
        file_table.extend_from_slice(&data_offs[i].to_le_bytes());
        file_table.extend_from_slice(&(files[i].1.len() as u64).to_le_bytes());
        let hash_next = file_offs.get(i + 1).copied().unwrap_or(EMPTY);
        file_table.extend_from_slice(&hash_next.to_le_bytes());
        file_table.extend_from_slice(&(name.len() as u32).to_le_bytes());
        file_table.extend_from_slice(name.as_bytes());
        file_table.resize(file_table.len().next_multiple_of(4), 0);
    }

    let mut image = vec![0u8; FILE_DATA as usize];
    image.extend_from_slice(&data);
    image.resize(image.len().next_multiple_of(4), 0);
    let mut append = |bytes: &[u8]| {
        let off = image.len() as u64;
        image.extend_from_slice(bytes);
        [off, bytes.len() as u64]
    };
    let dir_hash = append(&0u32.to_le_bytes());
    let dir_meta = append(&dir_table);
    let file_hash = append(&0u32.to_le_bytes());
    let file_meta = append(&file_table);
    let header = [
        0x50,
        dir_hash[0],
        dir_hash[1],
        dir_meta[0],
        dir_meta[1],
        file_hash[0],
        file_hash[1],
        file_meta[0],
        file_meta[1],
        FILE_DATA,
    ];
    for (i, v) in header.into_iter().enumerate() {
        image[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    (image, data_offs.iter().map(|o| FILE_DATA + o).collect())
}

/// RomFS section: IVFC levels 0..4 as (zeroed) hash levels, level 5
/// holding `image` at [`DATA_LEVEL`]. The master hash covers level 0.
pub fn romfs_section(image: &[u8]) -> FixtureSection {
    let mut body = vec![0u8; DATA_LEVEL as usize];
    body.extend_from_slice(image);
    let mut fs = [0u8; 0x200];
    fs[0] = 2;
    fs[2] = FS_TYPE_ROMFS;
    fs[3] = HASH_TYPE_HIERARCHICAL_INTEGRITY;
    fs[8..12].copy_from_slice(b"IVFC");
    fs[0x0C..0x10].copy_from_slice(&0x2_0000u32.to_le_bytes());
    fs[0x10..0x14].copy_from_slice(&0x20u32.to_le_bytes());
    fs[0x14..0x18].copy_from_slice(&7u32.to_le_bytes());
    for level in 0..6 {
        let at = 0x18 + level * 0x18;
        let (offset, size) = if level == 5 {
            (DATA_LEVEL, image.len() as u64)
        } else {
            (level as u64 * 0x200, 0x20)
        };
        fs[at..at + 8].copy_from_slice(&offset.to_le_bytes());
        fs[at + 8..at + 16].copy_from_slice(&size.to_le_bytes());
        fs[at + 0x10..at + 0x14].copy_from_slice(&0xEu32.to_le_bytes());
    }
    // Level 0 is 0x20 zero bytes, hashed as one zero-padded 0x4000 block.
    fs[0xC8..0xE8].copy_from_slice(&Sha256::digest([0u8; 0x4000]));
    ctr_section(body, fs)
}

/// BKTR patch section whose patched virtual space is the
/// [`romfs_section`] of `patched_image`, except each `(virtual start,
/// length, base offset)` range of `from_base`, which reads the base
/// section instead. Layout: patch data, indirect table, AesCtrEx table.
/// The data switches generation at 0x800 and 0x1000 with an unencrypted
/// run between them; the tables use the FS header generation.
pub fn patch_romfs_section(patched_image: &[u8], from_base: &[(u64, u64, u64)]) -> FixtureSection {
    let FixtureSection {
        body: virtual_body,
        mut fs,
        ..
    } = romfs_section(patched_image);
    let mut data = Vec::new();
    let mut entries = Vec::new();
    let mut virt = 0u64;
    let patch_run = |data: &mut Vec<u8>, entries: &mut Vec<IndirectEntry>, from: u64, to: u64| {
        entries.push(IndirectEntry {
            virtual_offset: from,
            physical_offset: data.len() as u64,
            storage_index: 1,
        });
        data.extend_from_slice(&virtual_body[from as usize..to as usize]);
    };
    for &(start, len, base) in from_base {
        if start > virt {
            patch_run(&mut data, &mut entries, virt, start);
        }
        entries.push(IndirectEntry {
            virtual_offset: start,
            physical_offset: base,
            storage_index: 0,
        });
        virt = start + len;
    }
    if virt < virtual_body.len() as u64 {
        patch_run(&mut data, &mut entries, virt, virtual_body.len() as u64);
    }
    data.resize(data.len().next_multiple_of(0x200), 0);
    let indirect_offset = data.len() as u64;
    let indirect = build_table(&entries, virtual_body.len() as u64);
    data.extend_from_slice(&indirect);
    let aes_ctr_ex_offset = data.len() as u64;
    let ex_entries = [
        AesCtrExEntry {
            offset: 0,
            encrypted: true,
            generation: 7,
        },
        AesCtrExEntry {
            offset: 0x800,
            encrypted: false,
            generation: 0,
        },
        AesCtrExEntry {
            offset: 0x1000,
            encrypted: true,
            generation: 9,
        },
        AesCtrExEntry {
            offset: indirect_offset,
            encrypted: true,
            generation: FIXTURE_GENERATION,
        },
    ];
    let ex_table = build_table(&ex_entries, aes_ctr_ex_offset);
    data.extend_from_slice(&ex_table);

    fs[4] = ENC_AES_CTR_EX;
    let mut put_table = |at: usize, offset: u64, size: usize, count: usize| {
        fs[at..at + 8].copy_from_slice(&offset.to_le_bytes());
        fs[at + 8..at + 0x10].copy_from_slice(&(size as u64).to_le_bytes());
        fs[at + 0x10..at + 0x14].copy_from_slice(b"BKTR");
        fs[at + 0x14..at + 0x18].copy_from_slice(&1u32.to_le_bytes());
        fs[at + 0x18..at + 0x1C].copy_from_slice(&(count as u32).to_le_bytes());
    };
    put_table(0x100, indirect_offset, indirect.len(), entries.len());
    put_table(0x120, aes_ctr_ex_offset, ex_table.len(), ex_entries.len());
    FixtureSection {
        runs: vec![
            (0, 0x800, 7),
            (0x1000, indirect_offset, 9),
            (indirect_offset, data.len() as u64, FIXTURE_GENERATION),
        ],
        body: data,
        fs,
    }
}

/// Encrypted NCA3 of `sections` laid out from 0x4000, readable with
/// [`synthetic_keyset`] (or [`synthetic_keyset_with_titlekek`] plus
/// `build_common_ticket(rights_id, TEST_BODY_KEY)` when `rights_id` is set). Section `i` gets secure value `i + 1`.
pub fn build_nca(
    content_type: u8,
    title_id: u64,
    rights_id: [u8; 16],
    sections: &[FixtureSection],
) -> Vec<u8> {
    assemble_nca(content_type, title_id, rights_id, sections, true)
}

/// [`build_nca`] stored plaintext: header and sections in the clear, the
/// key area and the padding at 0x340 zero.
pub fn build_plaintext_nca(
    content_type: u8,
    title_id: u64,
    sections: &[FixtureSection],
) -> Vec<u8> {
    assemble_nca(content_type, title_id, [0; 16], sections, false)
}

fn assemble_nca(
    content_type: u8,
    title_id: u64,
    rights_id: [u8; 16],
    sections: &[FixtureSection],
    encrypt: bool,
) -> Vec<u8> {
    let mut header = [0u8; NCA_HEADER_SIZE];
    header[0x200..0x204].copy_from_slice(&NCA3_MAGIC);
    header[0x205] = content_type;
    header[0x210..0x218].copy_from_slice(&title_id.to_le_bytes());
    header[0x230..0x240].copy_from_slice(&rights_id);
    if encrypt {
        header[0x220] = 1;
        header[0x300..0x340].copy_from_slice(&encrypt_key_area_block([
            [0x11; 16],
            [0x22; 16],
            TEST_BODY_KEY,
            [0x44; 16],
        ]));
    }
    let mut nca = vec![0u8; 0x4000];
    for (i, section) in sections.iter().enumerate() {
        let start = nca.len() as u64;
        let secure = i as u32 + 1;
        let mut body = section.body.clone();
        body.resize(body.len().next_multiple_of(0x200), 0);
        if encrypt {
            for &(from, to, generation) in &section.runs {
                apply_ctr_at(
                    &TEST_BODY_KEY,
                    &ctr_iv(secure, generation),
                    start + from,
                    &mut body[from as usize..to as usize],
                )
                .unwrap();
            }
        }
        nca.extend_from_slice(&body);
        let entry = NCA_FS_ENTRY_OFFSET + i * 0x10;
        header[entry..entry + 4].copy_from_slice(&((start / 0x200) as u32).to_le_bytes());
        header[entry + 4..entry + 8].copy_from_slice(&((nca.len() / 0x200) as u32).to_le_bytes());
        let fs = NCA_FS_HEADER_OFFSET + i * 0x200;
        header[fs..fs + 0x200].copy_from_slice(&section.fs);
        header[fs + 0x144..fs + 0x148].copy_from_slice(&secure.to_le_bytes());
    }
    header[0x208..0x210].copy_from_slice(&(nca.len() as u64).to_le_bytes());
    if encrypt {
        encrypt_nca_header(&mut header, &TEST_HEADER_KEY).unwrap();
    }
    nca[..NCA_HEADER_SIZE].copy_from_slice(&header);
    nca
}
