//! Synthetic NCAs and `KeySet`s for unit tests. Lets crypto round-trip
//! tests run without real prod.keys or real game files.

use std::collections::HashMap;

use aes::Aes128;
use aes::cipher::array::Array;
use aes::cipher::{BlockCipherEncrypt, KeyInit};

use crate::nintendo::nx::constants::{
    ENC_AES_CTR, NCA_FS_ENTRY_OFFSET, NCA_FS_HEADER_OFFSET, NCA_HEADER_SIZE, NCA3_MAGIC,
};
use crate::nintendo::nx::crypto::aes_ctr::apply_ctr;
use crate::nintendo::nx::crypto::aes_xts::encrypt_nca_header;
use crate::nintendo::nx::crypto::derive::{KEY_AREA_KEY_COUNT, KEY_AREA_KEY_SIZE, KEY_AREA_TOTAL};
use crate::nintendo::nx::keys::{KeyAreaKind, KeySet};
use crate::nintendo::nx::merge::round_up_media_unit;
use crate::nintendo::nx::models::cnmt::CNMT_CONTENT_TYPE_PROGRAM;
use crate::nintendo::nx::models::hfs0::{
    self as hfs0_mod, DEFAULT_HASHED_REGION, Hfs0FileSpec, Hfs0LayoutHints, hash_first_chunk,
};
use crate::nintendo::nx::models::nca::{CONTENT_TYPE_META, FsHeader, initial_ctr_for_offset};
use crate::nintendo::nx::models::pfs0::{self as pfs0_mod, Pfs0LayoutHints};
use crate::nintendo::nx::models::xci::{MEDIA_UNIT, XCI_PREFIX_SIZE, build_xci_prefix};

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

pub fn synthetic_keyset() -> KeySet {
    let mut kak = HashMap::new();
    kak.insert((KeyAreaKind::Application, 0), TEST_KAK_APPLICATION_00);
    KeySet {
        header_key: Some(TEST_HEADER_KEY),
        key_area_keys: kak,
        ..KeySet::default()
    }
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

/// Serialize a minimal `PackagedContentMeta` (CNMT) blob matching the
/// layout `Cnmt::parse` expects. No extended header; content records
/// carry only the content id (hash/size zeroed).
fn build_cnmt_bytes(title_id: u64, version: u32, title_type: u8, contents: &[[u8; 16]]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&title_id.to_le_bytes());
    b.extend_from_slice(&version.to_le_bytes());
    b.push(title_type);
    b.push(0); // reserved
    b.extend_from_slice(&0u16.to_le_bytes()); // extended_header_size
    b.extend_from_slice(&(contents.len() as u16).to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes()); // content_meta_count
    b.push(0); // attributes
    b.push(0); // storage_id
    b.push(0); // install_type
    b.push(0); // reserved
    b.extend_from_slice(&0u64.to_le_bytes()); // required_download_system_version
    for cid in contents {
        b.extend_from_slice(&[0u8; 32]); // hash
        b.extend_from_slice(cid); // content_id
        b.extend_from_slice(&[0u8; 6]); // size (48-bit)
        b.push(CNMT_CONTENT_TYPE_PROGRAM);
        b.push(0); // id_offset
    }
    b
}

/// Build an encrypted Meta NCA (content_type = Meta) readable by
/// `read_meta_cnmt` under [`synthetic_keyset`]. Section 0 holds a PFS0
/// wrapping a single `{title_id:016x}.cnmt` file.
pub fn build_meta_nca(
    title_id: u64,
    version: u32,
    title_type: u8,
    contents: &[[u8; 16]],
) -> Vec<u8> {
    let cnmt = build_cnmt_bytes(title_id, version, title_type, contents);
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
    header[fs0_off + 4] = ENC_AES_CTR;
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
    let secure_specs: Vec<Hfs0FileSpec> = ncas
        .iter()
        .map(|(name, data)| Hfs0FileSpec {
            name: name.clone(),
            size: data.len() as u64,
            sha256: hash_first_chunk(data, DEFAULT_HASHED_REGION),
            hashed_region_size: DEFAULT_HASHED_REGION,
        })
        .collect();
    let natural_len = hfs0_mod::build_header(&secure_specs, &Hfs0LayoutHints::default())
        .unwrap()
        .bytes
        .len();
    let secure_header = hfs0_mod::build_header(
        &secure_specs,
        &Hfs0LayoutHints {
            target_total_header_size: Some(round_up_media_unit(natural_len)),
            first_file_data_offset: 0,
        },
    )
    .unwrap();

    let stub = hfs0_mod::build_header(
        &[],
        &Hfs0LayoutHints {
            target_total_header_size: Some(MEDIA_UNIT as usize),
            first_file_data_offset: 0,
        },
    )
    .unwrap();

    let nca_bytes: u64 = ncas.iter().map(|(_, d)| d.len() as u64).sum();
    let secure_unpadded = secure_header.bytes.len() as u64 + nca_bytes;
    let secure_total = secure_unpadded.div_ceil(MEDIA_UNIT) * MEDIA_UNIT;

    let root_spec = |name: &str, header: &[u8], size: u64| Hfs0FileSpec {
        name: name.into(),
        size,
        sha256: hash_first_chunk(header, DEFAULT_HASHED_REGION),
        hashed_region_size: DEFAULT_HASHED_REGION,
    };
    let root_specs = vec![
        root_spec("update", &stub.bytes, stub.bytes.len() as u64),
        root_spec("normal", &stub.bytes, stub.bytes.len() as u64),
        root_spec("secure", &secure_header.bytes, secure_total),
    ];
    let root_header = hfs0_mod::build_header(
        &root_specs,
        &Hfs0LayoutHints {
            target_total_header_size: Some(MEDIA_UNIT as usize),
            first_file_data_offset: 0,
        },
    )
    .unwrap();

    let secure_offset =
        XCI_PREFIX_SIZE as u64 + root_header.bytes.len() as u64 + stub.bytes.len() as u64 * 2;

    let mut out = vec![0u8; XCI_PREFIX_SIZE];
    out.extend_from_slice(&root_header.bytes);
    out.extend_from_slice(&stub.bytes);
    out.extend_from_slice(&stub.bytes);
    out.extend_from_slice(&secure_header.bytes);
    for (_, data) in ncas {
        out.extend_from_slice(data);
    }
    out.extend_from_slice(&vec![0u8; (secure_total - secure_unpadded) as usize]);

    let total_size = out.len() as u64;
    let prefix = build_xci_prefix(secure_offset, total_size, &root_header.bytes);
    out[..XCI_PREFIX_SIZE].copy_from_slice(&prefix);
    out
}
