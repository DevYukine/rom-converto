//! Shared test fixtures for building dummy CIA, TMD, Ticket, and Certificate
//! values. Used by `cia.rs::tests`, `verify/chain.rs::tests`, and any future
//! test modules that need a well-formed CIA without re-deriving the binrw
//! boilerplate. Signatures here are forged dummy bytes, so the fixtures are
//! only suitable for layout, hashing, and streaming tests, not RSA verification.

#![cfg(test)]

use crate::nintendo::ctr::constants::{NCCH_FLAGS_OFFSET, NCCH_FLAGS7_NOCRYPTO, NCCH_MAGIC};
use crate::nintendo::ctr::models::certificate::{Certificate, KeyType, PublicKey};
use crate::nintendo::ctr::models::cia::{CIA_HEADER_SIZE, CiaFile, CiaHeader, MetaData};
use crate::nintendo::ctr::models::signature::{SignatureData, SignatureType};
use crate::nintendo::ctr::models::ticket::{ContentIndex, Ticket, TicketData};
use crate::nintendo::ctr::models::title_metadata::{
    ContentChunkRecord, ContentInfoRecord, ContentType, TitleMetadata, TitleMetadataHeader,
};
use crate::util::ProgressReporter;
use binrw::{BinWrite, Endian};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Write as _};
use std::sync::Mutex;

/// Title id baked into [`synth_cia`]. Tests that round-trip a synth CIA
/// through verify can compare against this constant instead of duplicating
/// the literal.
pub const SYNTH_CIA_TITLE_ID: u64 = 0x0004000000030000;

/// Append a `BinWrite` value to `buf` using a fresh scratch cursor. Avoids
/// the Cursor-at-position-0 footgun that overwrites existing bytes when
/// passing `&mut buf` to a new Cursor for every write.
pub fn append_be<T: BinWrite<Args<'static> = ()>>(buf: &mut Vec<u8>, value: &T) {
    let mut scratch = Vec::new();
    value
        .write_options(&mut Cursor::new(&mut scratch), Endian::Big, ())
        .unwrap();
    buf.extend_from_slice(&scratch);
}

/// In-memory `ProgressReporter` that records `(total, inc_sum, finish_called)`
/// so tests can assert on counter totals.
#[derive(Default)]
pub struct TestProgress {
    inner: Mutex<(u64, u64, bool)>,
}

impl ProgressReporter for TestProgress {
    fn start(&self, total: u64, _message: &str) {
        let mut g = self.inner.lock().unwrap();
        g.0 = total;
    }
    fn inc(&self, delta: u64) {
        let mut g = self.inner.lock().unwrap();
        g.1 += delta;
    }
    fn finish(&self) {
        let mut g = self.inner.lock().unwrap();
        g.2 = true;
    }
}

/// Build a synthetic RSA-2048 certificate with the given name and signature
/// fill byte. All other fields use deterministic dummy values.
pub fn make_cert(name: &[u8], sig_fill: u8) -> Certificate {
    Certificate {
        signature_type: SignatureType::Rsa2048Sha256,
        signature: vec![sig_fill; 0x100],
        padding: vec![0x00; 0x3C],
        issuer: pad_to(b"Root", 0x40),
        key_type: KeyType::Rsa2048,
        name: pad_to(name, 0x40),
        expiration_time: 0x5F5E0F00,
        public_key: PublicKey::Rsa2048 {
            modulus: vec![0xFF; 0x100],
            public_exponent: 65537,
            padding: vec![0x00; 0x34],
        },
    }
}

/// Build a synthetic ticket for the given title id with `console_id = 0`
/// (global), version 1, and dummy keys.
pub fn make_ticket(title_id: u64) -> Ticket {
    Ticket {
        signature_data: SignatureData {
            signature_type: SignatureType::Rsa2048Sha256,
            signature: vec![0xBB; 0x100],
            padding: vec![0x00; 0x3C],
        },
        ticket_data: TicketData {
            issuer: pad_to(b"Root-CA00000003-XS0000000c", 0x40),
            ecc_public_key: vec![0x00; 0x3C],
            version: 1,
            ca_crl_version: 0,
            signer_crl_version: 0,
            title_key: vec![0xFF; 0x10],
            reserved1: 0,
            ticket_id: 0x0123456789ABCDEF,
            console_id: 0,
            title_id,
            reserved2: 0,
            ticket_title_version: 0x0100,
            reserved3: 0,
            license_type: 0,
            common_key_index: 1,
            reserved4: vec![0x00; 0x2A],
            eshop_account_id: 0,
            reserved5: 0,
            audit: 0,
            reserved6: vec![0x00; 0x42],
            limits: vec![0x00; 0x40],
            content_index: ContentIndex {
                header_word: 0,
                total_size: 22,
                data: vec![0x00; 20],
            },
        },
    }
}

/// Build a synthetic TMD for the given title id and content chunk descriptors.
/// Each record is `(content_id, content_index, data, hash)`. The info-records
/// hash chain is computed correctly so the resulting TMD passes its own
/// integrity checks.
pub fn make_tmd(title_id: u64, records: Vec<(u32, u16, Vec<u8>, [u8; 32])>) -> TitleMetadata {
    // Fixtures store plaintext bytes with a plaintext SHA-256, so they
    // model a decrypted/devkit CIA. Encrypted-flag is cleared so the
    // verifier hashes the stored bytes directly instead of trying to
    // AES-CBC decrypt them first.
    let content_chunk_records: Vec<ContentChunkRecord> = records
        .iter()
        .map(|(id, idx, data, hash)| ContentChunkRecord {
            content_id: *id,
            content_index: *idx,
            content_type: ContentType(0x0000),
            content_size: data.len() as u64,
            hash: hash.to_vec(),
        })
        .collect();

    // info_records[0].hash = SHA256(serialized chunk records [0..k])
    let info_hash_0 = sha256_serialized(&content_chunk_records);

    let mut content_info_records = vec![
        ContentInfoRecord {
            content_index_offset: 0,
            content_command_count: 0,
            hash: vec![0x00; 0x20],
        };
        64
    ];
    content_info_records[0] = ContentInfoRecord {
        content_index_offset: 0,
        content_command_count: content_chunk_records.len() as u16,
        hash: info_hash_0,
    };

    // header.content_info_records_hash = SHA256(serialized info records)
    let content_info_records_hash = sha256_serialized(&content_info_records);

    TitleMetadata {
        signature_data: SignatureData {
            signature_type: SignatureType::Rsa2048Sha256,
            signature: vec![0xCC; 0x100],
            padding: vec![0x00; 0x3C],
        },
        header: TitleMetadataHeader {
            signature_issuer: pad_to(b"Root-CA00000003-CP0000000b", 0x40),
            version: 1,
            ca_crl_version: 0,
            signer_crl_version: 0,
            reserved1: 0,
            system_version: 0,
            title_id,
            title_type: 0x00040010,
            group_id: 0,
            save_data_size: 0x00080000,
            srl_private_save_data_size: 0,
            reserved2: 0,
            srl_flag: 0,
            reserved3: vec![0x00; 0x31],
            access_rights: 0,
            title_version: 0x0100,
            content_count: content_chunk_records.len() as u16,
            boot_content: 0,
            padding: 0,
            content_info_records_hash,
        },
        content_info_records,
        content_chunk_records,
    }
}

/// Build and persist a complete synthetic CIA file with `content_size` bytes
/// of deterministic content. Returns the temp dir (drop guard), the on-disk
/// path, and the SHA-256 of the content data.
///
/// The TMD title id is [`SYNTH_CIA_TITLE_ID`] and the ticket `console_id` is
/// 0 (global). Signatures are forged dummy bytes, so downstream verifiers
/// reject the signature checks. All layout, hash, and streaming checks pass.
pub fn synth_cia(content_size: usize) -> (tempfile::TempDir, std::path::PathBuf, [u8; 32]) {
    let tmp = tempfile::tempdir().unwrap();
    let out_path = tmp.path().join("test.cia");

    // Deterministic content that won't accidentally look like a valid NCCH.
    let content_data: Vec<u8> = (0..content_size)
        .map(|i| (i as u8).wrapping_mul(37))
        .collect();
    let content_hash = sha256_array(&content_data);

    let cert_chain = vec![
        make_cert(b"CA00000003", 0xAA),
        make_cert(b"CP0000000b", 0xBB),
        make_cert(b"XS0000000c", 0xCC),
    ];
    let ticket = make_ticket(SYNTH_CIA_TITLE_ID);
    let tmd = make_tmd(
        SYNTH_CIA_TITLE_ID,
        vec![(0, 0, content_data.clone(), content_hash)],
    );

    // The CIA header must declare the real ticket and TMD lengths, since
    // their BinWrite impls do not pad to a fixed size.
    let ticket_size = serialized_size(&ticket);
    let tmd_size = serialized_size(&tmd);

    let cia = CiaFile {
        header: CiaHeader {
            header_size: CIA_HEADER_SIZE,
            cia_type: 0,
            version: 0,
            cert_chain_size: 0x0A00,
            ticket_size,
            tmd_size,
            meta_size: 0,
            content_size: content_size as u64,
            content_index: vec![0x00; 0x2000],
        },
        cert_chain,
        ticket,
        tmd,
        content_data,
        meta_data: None,
    };

    let mut buf = Vec::new();
    cia.write_options(&mut Cursor::new(&mut buf), Endian::Little, ())
        .unwrap();

    let mut f = std::fs::File::create(&out_path).unwrap();
    f.write_all(&buf).unwrap();
    f.flush().unwrap();

    (tmp, out_path, content_hash)
}

/// Build the bytes of a minimal NCCH header (0x200 bytes) with NoCrypto set
/// and every section size zero. `parse_ncch` accepts this without doing any
/// AES work, so it embeds cleanly as CIA content in fixtures.
pub fn make_ncch_header_bytes(title_id: u64) -> Vec<u8> {
    let mut bytes = vec![0u8; 0x200];
    bytes[0x100..0x104].copy_from_slice(NCCH_MAGIC.as_bytes());
    bytes[0x108..0x110].copy_from_slice(&title_id.to_le_bytes());
    bytes[NCCH_FLAGS_OFFSET + 7] = NCCH_FLAGS7_NOCRYPTO;
    bytes
}

/// Build a [`MetaData`] block (0x3AC0 bytes) filled with a per-field offset
/// pattern derived from `seed`, so any byte slippage between fields is
/// detectable by an equality check.
pub fn make_meta(seed: u8) -> MetaData {
    let pattern = |len: usize, offset: u8| -> Vec<u8> {
        (0..len)
            .map(|i| seed.wrapping_add(offset).wrapping_add((i & 0xFF) as u8))
            .collect()
    };
    MetaData {
        dependency_list: pattern(0x180, 0x10),
        reserved1: pattern(0x180, 0x20),
        core_version: 0x0000_0002,
        reserved2: pattern(0xFC, 0x30),
        icon_data: pattern(0x36C0, 0x40),
    }
}

/// Like [`synth_cia`] but the content is a valid (NoCrypto) NCCH header and
/// the CIA declares a [`MetaData`] block. Returns the temp dir, on-disk
/// path, the meta block, and its serialized size.
pub fn synth_cia_with_meta(
    meta: MetaData,
) -> (tempfile::TempDir, std::path::PathBuf, MetaData, u32) {
    let tmp = tempfile::tempdir().unwrap();
    let out_path = tmp.path().join("test.cia");

    let content_data = make_ncch_header_bytes(SYNTH_CIA_TITLE_ID);
    let content_hash = sha256_array(&content_data);

    let cert_chain = vec![
        make_cert(b"CA00000003", 0xAA),
        make_cert(b"CP0000000b", 0xBB),
        make_cert(b"XS0000000c", 0xCC),
    ];
    let ticket = make_ticket(SYNTH_CIA_TITLE_ID);
    let tmd = make_tmd(
        SYNTH_CIA_TITLE_ID,
        vec![(0, 0, content_data.clone(), content_hash)],
    );

    let ticket_size = serialized_size(&ticket);
    let tmd_size = serialized_size(&tmd);

    let meta_size: u32 = {
        let mut buf = Vec::new();
        meta.write_options(&mut Cursor::new(&mut buf), Endian::Little, ())
            .unwrap();
        buf.len() as u32
    };

    let cia = CiaFile {
        header: CiaHeader {
            header_size: CIA_HEADER_SIZE,
            cia_type: 0,
            version: 0,
            cert_chain_size: 0x0A00,
            ticket_size,
            tmd_size,
            meta_size,
            content_size: content_data.len() as u64,
            content_index: vec![0x00; 0x2000],
        },
        cert_chain,
        ticket,
        tmd,
        content_data,
        meta_data: Some(meta.clone()),
    };

    let mut buf = Vec::new();
    cia.write_options(&mut Cursor::new(&mut buf), Endian::Little, ())
        .unwrap();

    let mut f = std::fs::File::create(&out_path).unwrap();
    f.write_all(&buf).unwrap();
    f.flush().unwrap();

    (tmp, out_path, meta, meta_size)
}

/// Build and persist a synthetic encrypted-format CIA carrying one NoCrypto
/// NCCH per entry in `content_ids`, with `content_index` running 0,1,2,...
/// Each content is a distinct 0x200-byte header, so callers can pass ids whose
/// hex form contains digits A-F (e.g. `0x0000ABCD`) to exercise the per-content
/// `.ncch` read-back path. Returns the temp dir (drop guard), the on-disk path,
/// and the raw content bytes in record order.
pub fn synth_encrypted_cia_multi_content(
    content_ids: &[u32],
) -> (tempfile::TempDir, std::path::PathBuf, Vec<Vec<u8>>) {
    let tmp = tempfile::tempdir().unwrap();
    let out_path = tmp.path().join("test.cia");

    let contents: Vec<Vec<u8>> = content_ids
        .iter()
        .enumerate()
        .map(|(i, &id)| {
            let mut bytes = make_ncch_header_bytes(SYNTH_CIA_TITLE_ID);
            // Perturb the signature region (unused for NoCrypto) so every
            // content hashes differently, even when ids repeat.
            bytes[0x40] = i as u8;
            bytes[0x44..0x48].copy_from_slice(&id.to_le_bytes());
            bytes
        })
        .collect();

    let records: Vec<(u32, u16, Vec<u8>, [u8; 32])> = content_ids
        .iter()
        .enumerate()
        .map(|(i, &id)| {
            (
                id,
                i as u16,
                contents[i].clone(),
                sha256_array(&contents[i]),
            )
        })
        .collect();

    let content_data: Vec<u8> = contents.iter().flatten().copied().collect();

    let cert_chain = vec![
        make_cert(b"CA00000003", 0xAA),
        make_cert(b"CP0000000b", 0xBB),
        make_cert(b"XS0000000c", 0xCC),
    ];
    let ticket = make_ticket(SYNTH_CIA_TITLE_ID);
    let tmd = make_tmd(SYNTH_CIA_TITLE_ID, records);

    let ticket_size = serialized_size(&ticket);
    let tmd_size = serialized_size(&tmd);

    let cia = CiaFile {
        header: CiaHeader {
            header_size: CIA_HEADER_SIZE,
            cia_type: 0,
            version: 0,
            cert_chain_size: 0x0A00,
            ticket_size,
            tmd_size,
            meta_size: 0,
            content_size: content_data.len() as u64,
            content_index: vec![0x00; 0x2000],
        },
        cert_chain,
        ticket,
        tmd,
        content_data,
        meta_data: None,
    };

    let mut buf = Vec::new();
    cia.write_options(&mut Cursor::new(&mut buf), Endian::Little, ())
        .unwrap();

    let mut f = std::fs::File::create(&out_path).unwrap();
    f.write_all(&buf).unwrap();
    f.flush().unwrap();

    (tmp, out_path, contents)
}

// ---- internal helpers ----

fn pad_to(src: &[u8], len: usize) -> Vec<u8> {
    let mut v = src.to_vec();
    v.resize(len, 0);
    v
}

fn sha256_array(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    let digest = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&digest);
    arr
}

/// SHA-256 of a slice of `BinWrite` values serialized big-endian and
/// concatenated. Used for TMD info-records and content-chunk-records hash
/// chain construction.
fn sha256_serialized<T: BinWrite<Args<'static> = ()>>(items: &[T]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut cursor = Cursor::new(&mut buf);
        for item in items {
            item.write_options(&mut cursor, Endian::Big, ()).unwrap();
        }
    }
    let mut h = Sha256::new();
    h.update(&buf);
    h.finalize().to_vec()
}

fn serialized_size<T: BinWrite<Args<'static> = ()>>(value: &T) -> u32 {
    let mut buf = Vec::new();
    value
        .write_options(&mut Cursor::new(&mut buf), Endian::Big, ())
        .unwrap();
    buf.len() as u32
}
