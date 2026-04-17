use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::nus::content_stream::{
    ContentLoader, DirectoryContentSource, decrypt_content_0,
};
use crate::nintendo::wup::nus::fst_parser::parse_fst;
use crate::nintendo::wup::nus::layout::{NusLayout, TicketSource};
use crate::nintendo::wup::nus::ticket_parser::{TitleKey, read_ticket_file};
use crate::nintendo::wup::nus::tmd_parser::read_tmd_file;
use crate::nintendo::wup::title_key_derive::derive_title_key;
use crate::util::ProgressReporter;

/// Decrypt one NUS-format title into a loadiine-style directory tree
/// under `output_dir`. Returns `(title_id, title_version)` from the
/// TMD.
pub fn decrypt_nus_title(
    title_dir: &Path,
    output_dir: &Path,
    progress: &dyn ProgressReporter,
) -> WupResult<(u64, u16)> {
    let layout = NusLayout::discover(title_dir)?;
    let tmd = read_tmd_file(&layout.tmd_path)?;
    let title_id = tmd.title_id;
    let title_version = tmd.title_version;

    let title_key = match &layout.ticket_source {
        TicketSource::OnDisk(path) => {
            let (ticket, key) = read_ticket_file(path)?;
            if ticket.title_id != title_id {
                return Err(WupError::InvalidTicket);
            }
            key
        }
        TicketSource::Derive => TitleKey(derive_title_key(title_id)),
    };

    let content_0 = tmd.contents.first().ok_or(WupError::InvalidTmd)?;
    let content_0_path =
        layout
            .content
            .resolve(content_0.content_id)
            .ok_or(WupError::ContentNotFound {
                content_id: content_0.content_id,
            })?;
    let encrypted_content_0 =
        std::fs::read(&content_0_path).map_err(|_| WupError::ContentNotFound {
            content_id: content_0.content_id,
        })?;
    let decrypted_content_0 = decrypt_content_0(encrypted_content_0, &title_key)?;
    let fs = parse_fst(&decrypted_content_0)?;

    std::fs::create_dir_all(output_dir)?;

    let source = DirectoryContentSource::with_resolver(layout.content);
    let mut loader = ContentLoader::new(source, title_key, &tmd, &fs);
    let mut skipped: u32 = 0;
    for vfile in &fs.files {
        match loader.extract_file(vfile) {
            Ok(bytes) => {
                let out_path = output_dir.join(&vfile.path);
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&out_path, &bytes)?;
                progress.inc(bytes.len() as u64);
            }
            Err(WupError::FileInheritedFromOtherTitle { .. }) => {
                skipped = skipped.saturating_add(1);
            }
            Err(e) => return Err(e),
        }
    }
    if skipped > 0 {
        log::info!(
            "skipped {skipped} file(s) in title {title_id:016x} v{title_version}: \
             cluster data not shipped in this title"
        );
    }

    Ok((title_id, title_version))
}

pub async fn decrypt_nus_title_async(
    title_dir: PathBuf,
    output_dir: PathBuf,
    progress: &dyn ProgressReporter,
) -> WupResult<()> {
    progress.start(0, "Decrypting Wii U title");

    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_for_task = bytes_done.clone();

    let mut handle = tokio::task::spawn_blocking(move || -> WupResult<()> {
        let shim = AtomicBytesProgress {
            bytes_done: bytes_done_for_task,
        };
        decrypt_nus_title(&title_dir, &output_dir, &shim).map(|_| ())
    });

    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(result) => {
                result??;
                break;
            }
            Err(_) => {
                let delta = bytes_done.swap(0, Ordering::Relaxed);
                if delta > 0 {
                    progress.inc(delta);
                }
            }
        }
    }
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    progress.finish();
    Ok(())
}

struct AtomicBytesProgress {
    bytes_done: Arc<AtomicU64>,
}

impl ProgressReporter for AtomicBytesProgress {
    fn start(&self, _total: u64, _msg: &str) {}
    fn inc(&self, delta: u64) {
        self.bytes_done.fetch_add(delta, Ordering::Relaxed);
    }
    fn finish(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::common_keys::WII_U_COMMON_KEY;
    use crate::nintendo::wup::models::ticket::{WUP_TICKET_BASE_SIZE, WUP_TICKET_FORMAT_V1};
    use crate::nintendo::wup::models::tmd::{
        TmdContentFlags, WUP_TMD_CONTENT_ENTRY_SIZE, WUP_TMD_HEADER_SIZE,
    };
    use crate::nintendo::wup::nus::content_stream::{
        HASHED_BLOCK_DATA_SIZE, HASHED_BLOCK_HASH_SIZE, HASHED_BLOCK_SIZE,
    };
    use crate::nintendo::wup::nus::fst_parser::{
        FST_CLUSTER_ENTRY_SIZE, FST_FILE_ENTRY_SIZE, FST_HEADER_SIZE, FST_MAGIC,
    };
    use crate::nintendo::wup::title_key_derive::derive_title_key;
    use crate::util::NoProgress;
    use aes::{
        Aes128,
        cipher::{BlockEncryptMut, KeyIvInit},
    };
    use block_padding::NoPadding;
    use cbc::Encryptor;

    type Aes128CbcEnc = Encryptor<Aes128>;

    fn encrypt(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
        Aes128CbcEnc::new_from_slices(key, iv)
            .unwrap()
            .encrypt_padded_mut::<NoPadding>(data, data.len())
            .unwrap();
    }

    /// Parameterised fixture for a synthetic NUS title with one raw
    /// FST content and one payload cluster. Each test flips the
    /// fields it cares about on the default.
    struct NusFixture {
        title_id: u64,
        title_version: u16,
        title_key: [u8; 16],
        /// `None` = no ticket on disk (forces the Derive path).
        /// `Some(id)` = write a ticket whose title_id is `id`, which
        /// can mismatch `title_id` to trigger the consistency check.
        ticket_title_id: Option<u64>,
        /// High byte of the FST file entry's type_and_name_offset.
        /// `0x80` marks the file as inherited-from-base.
        file_type_byte: u8,
        /// When `Some`, overrides the FST file entry's size so the
        /// extent-past-cluster path can be exercised.
        file_size_override: Option<u32>,
        /// `true` puts cluster 1 in hash-interleaved mode.
        hashed_payload: bool,
    }

    impl Default for NusFixture {
        fn default() -> Self {
            Self {
                title_id: 0x0005_000E_1234_5678,
                title_version: 32,
                title_key: [0x99u8; 16],
                ticket_title_id: Some(0x0005_000E_1234_5678),
                file_type_byte: 0x00,
                file_size_override: None,
                hashed_payload: false,
            }
        }
    }

    impl NusFixture {
        /// Write ticket (if enabled), TMD, FST, and payload to `dir`.
        /// Returns the plaintext payload bytes the decrypter should
        /// emit under `content/foo.bin`.
        fn build(&self, dir: &Path) -> Vec<u8> {
            if let Some(ticket_title_id) = self.ticket_title_id {
                let mut ticket = vec![0u8; WUP_TICKET_BASE_SIZE];
                ticket[0..4].copy_from_slice(&0x0001_0004u32.to_be_bytes());
                ticket[0x1BC] = WUP_TICKET_FORMAT_V1;
                let mut enc_title_key = self.title_key;
                let mut iv = [0u8; 16];
                iv[0..8].copy_from_slice(&ticket_title_id.to_be_bytes());
                encrypt(&WII_U_COMMON_KEY, &iv, &mut enc_title_key);
                ticket[0x1BF..0x1CF].copy_from_slice(&enc_title_key);
                ticket[0x1DC..0x1E4].copy_from_slice(&ticket_title_id.to_be_bytes());
                ticket[0x1E6..0x1E8].copy_from_slice(&self.title_version.to_be_bytes());
                std::fs::write(dir.join("title.tik"), &ticket).unwrap();
            }

            let payload_plain: Vec<u8> = (0u8..128).collect();

            let payload_flags = if self.hashed_payload {
                TmdContentFlags::ENCRYPTED | TmdContentFlags::HASHED
            } else {
                TmdContentFlags::ENCRYPTED
            };
            let fst_size =
                (FST_HEADER_SIZE + 2 * FST_CLUSTER_ENTRY_SIZE + 3 * FST_FILE_ENTRY_SIZE + 32)
                    .max(0x200);

            let mut tmd = vec![0u8; WUP_TMD_HEADER_SIZE + 2 * WUP_TMD_CONTENT_ENTRY_SIZE];
            tmd[0..4].copy_from_slice(&0x0001_0004u32.to_be_bytes());
            tmd[0x18C..0x194].copy_from_slice(&self.title_id.to_be_bytes());
            tmd[0x1DC..0x1DE].copy_from_slice(&self.title_version.to_be_bytes());
            tmd[0x1DE..0x1E0].copy_from_slice(&2u16.to_be_bytes());
            let fst_entry_off = WUP_TMD_HEADER_SIZE;
            tmd[fst_entry_off + 4..fst_entry_off + 6].copy_from_slice(&0u16.to_be_bytes());
            tmd[fst_entry_off + 6..fst_entry_off + 8]
                .copy_from_slice(&TmdContentFlags::ENCRYPTED.bits().to_be_bytes());
            tmd[fst_entry_off + 8..fst_entry_off + 16]
                .copy_from_slice(&(fst_size as u64).to_be_bytes());
            let payload_entry_off = fst_entry_off + WUP_TMD_CONTENT_ENTRY_SIZE;
            tmd[payload_entry_off..payload_entry_off + 4].copy_from_slice(&1u32.to_be_bytes());
            tmd[payload_entry_off + 4..payload_entry_off + 6].copy_from_slice(&1u16.to_be_bytes());
            tmd[payload_entry_off + 6..payload_entry_off + 8]
                .copy_from_slice(&payload_flags.bits().to_be_bytes());
            tmd[payload_entry_off + 8..payload_entry_off + 16]
                .copy_from_slice(&(payload_plain.len() as u64).to_be_bytes());
            std::fs::write(dir.join("title.tmd"), &tmd).unwrap();

            let num_clusters: u32 = 2;
            let mut fst = vec![0u8; fst_size];
            fst[0..4].copy_from_slice(&FST_MAGIC.to_be_bytes());
            fst[4..8].copy_from_slice(&1u32.to_be_bytes());
            fst[8..12].copy_from_slice(&num_clusters.to_be_bytes());

            fst[FST_HEADER_SIZE + 0x08..FST_HEADER_SIZE + 0x10]
                .copy_from_slice(&self.title_id.to_be_bytes());
            fst[FST_HEADER_SIZE + 0x14] = 0;
            fst[FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE + 0x08
                ..FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE + 0x10]
                .copy_from_slice(&self.title_id.to_be_bytes());
            fst[FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE + 0x14] =
                if self.hashed_payload { 2 } else { 0 };

            let entries_start = FST_HEADER_SIZE + (num_clusters as usize) * FST_CLUSTER_ENTRY_SIZE;
            let num_entries = 3u32;
            fst[entries_start..entries_start + 4].copy_from_slice(&0x0100_0000u32.to_be_bytes());
            fst[entries_start + 8..entries_start + 12].copy_from_slice(&num_entries.to_be_bytes());

            let name_table_off = entries_start + (num_entries as usize) * FST_FILE_ENTRY_SIZE;
            fst[name_table_off] = 0;
            fst[name_table_off + 1..name_table_off + 9].copy_from_slice(b"content\0");
            fst[name_table_off + 9..name_table_off + 17].copy_from_slice(b"foo.bin\0");

            let dir_entry_off = entries_start + FST_FILE_ENTRY_SIZE;
            fst[dir_entry_off..dir_entry_off + 4]
                .copy_from_slice(&(0x0100_0000u32 | 1u32).to_be_bytes());
            fst[dir_entry_off + 8..dir_entry_off + 12].copy_from_slice(&num_entries.to_be_bytes());

            let file_entry_off = dir_entry_off + FST_FILE_ENTRY_SIZE;
            let file_name_and_type = ((self.file_type_byte as u32) << 24) | 9u32;
            fst[file_entry_off..file_entry_off + 4]
                .copy_from_slice(&file_name_and_type.to_be_bytes());
            fst[file_entry_off + 4..file_entry_off + 8].copy_from_slice(&0u32.to_be_bytes());
            let effective_size = self
                .file_size_override
                .unwrap_or(payload_plain.len() as u32);
            fst[file_entry_off + 8..file_entry_off + 12]
                .copy_from_slice(&effective_size.to_be_bytes());
            fst[file_entry_off + 14..file_entry_off + 16].copy_from_slice(&1u16.to_be_bytes());

            // Encrypt cluster 0 (FST) raw with IV=0.
            let mut fst_enc = fst;
            encrypt(&self.title_key, &[0u8; 16], &mut fst_enc);
            std::fs::write(dir.join("00000000.app"), &fst_enc).unwrap();

            // Cluster 1: raw or hashed.
            if self.hashed_payload {
                let encrypted = encrypt_as_single_hashed_block(&self.title_key, &payload_plain);
                std::fs::write(dir.join("00000001.app"), &encrypted).unwrap();
            } else {
                let mut enc = payload_plain.clone();
                let mut iv = [0u8; 16];
                iv[1] = 1;
                encrypt(&self.title_key, &iv, &mut enc);
                std::fs::write(dir.join("00000001.app"), &enc).unwrap();
            }

            payload_plain
        }
    }

    /// Build one physical 0x10000-byte hashed block that decrypts to
    /// a 0xFC00-byte payload. Any bytes beyond the supplied payload
    /// are left as zero padding. The hash prefix's H0 slot 0 seeds
    /// the data IV.
    fn encrypt_as_single_hashed_block(title_key: &[u8; 16], payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() <= HASHED_BLOCK_DATA_SIZE);

        let mut hash_plain = [0u8; HASHED_BLOCK_HASH_SIZE];
        for (i, b) in hash_plain.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(1);
        }
        // H0 slot 0 seeds the data IV (first 16 bytes of hash_plain).
        let data_iv: [u8; 16] = hash_plain[..16].try_into().expect("16 bytes");

        let mut data_plain = vec![0u8; HASHED_BLOCK_DATA_SIZE];
        data_plain[..payload.len()].copy_from_slice(payload);

        let mut hash_enc = hash_plain;
        encrypt(title_key, &[0u8; 16], &mut hash_enc);
        encrypt(title_key, &data_iv, &mut data_plain);

        let mut block = vec![0u8; HASHED_BLOCK_SIZE];
        block[..HASHED_BLOCK_HASH_SIZE].copy_from_slice(&hash_enc);
        block[HASHED_BLOCK_HASH_SIZE..].copy_from_slice(&data_plain);
        block
    }

    #[test]
    fn decrypt_writes_flat_file_tree() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let fx = NusFixture::default();
        let expected = fx.build(dir.path());

        let (id, ver) = decrypt_nus_title(dir.path(), out.path(), &NoProgress).unwrap();
        assert_eq!(id, fx.title_id);
        assert_eq!(ver, fx.title_version);
        let written = std::fs::read(out.path().join("content").join("foo.bin")).unwrap();
        assert_eq!(written, expected);
    }

    #[test]
    fn decrypt_rejects_ticket_tmd_title_id_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let fx = NusFixture {
            title_id: 0x0005_000E_AAAA_0000,
            ticket_title_id: Some(0x0005_000E_BBBB_0000),
            ..NusFixture::default()
        };
        fx.build(dir.path());

        let err = decrypt_nus_title(dir.path(), out.path(), &NoProgress);
        assert!(matches!(err, Err(WupError::InvalidTicket)));
    }

    #[test]
    fn decrypt_derives_title_key_when_no_ticket_present() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();

        // Known-vector title_id whose PBKDF2 derivation is pinned in
        // title_key_derive::tests. Matching that here keeps this test
        // decoupled from the exact key bytes.
        let title_id = 0x0005_000E_1010_1E00u64;
        let derived = derive_title_key(title_id);
        let fx = NusFixture {
            title_id,
            title_key: derived,
            ticket_title_id: None,
            ..NusFixture::default()
        };
        let expected = fx.build(dir.path());

        let (id, _) = decrypt_nus_title(dir.path(), out.path(), &NoProgress).unwrap();
        assert_eq!(id, title_id);
        let written = std::fs::read(out.path().join("content").join("foo.bin")).unwrap();
        assert_eq!(written, expected);
    }

    #[test]
    fn decrypt_skips_file_flagged_shared_by_type_bit_7() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        // Bit 7 set, bit 0 clear: "file, inherited from base".
        let fx = NusFixture {
            file_type_byte: 0x80,
            ..NusFixture::default()
        };
        fx.build(dir.path());

        decrypt_nus_title(dir.path(), out.path(), &NoProgress).unwrap();
        assert!(
            !out.path().join("content").join("foo.bin").exists(),
            "shared file must not be emitted"
        );
    }

    #[test]
    fn decrypt_skips_file_that_extends_past_its_cluster() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        // Payload cluster decrypts to 128 bytes; claim 1 MiB in the
        // FST so extract_file returns FileInheritedFromOtherTitle via
        // the extent check.
        let fx = NusFixture {
            file_size_override: Some(0x10_0000),
            ..NusFixture::default()
        };
        fx.build(dir.path());

        decrypt_nus_title(dir.path(), out.path(), &NoProgress).unwrap();
        assert!(
            !out.path().join("content").join("foo.bin").exists(),
            "oversized file must not be emitted"
        );
    }

    #[test]
    fn decrypt_round_trips_hashed_mode_payload() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let fx = NusFixture {
            hashed_payload: true,
            ..NusFixture::default()
        };
        let expected = fx.build(dir.path());

        decrypt_nus_title(dir.path(), out.path(), &NoProgress).unwrap();
        let written = std::fs::read(out.path().join("content").join("foo.bin")).unwrap();
        assert_eq!(written, expected);
    }
}
