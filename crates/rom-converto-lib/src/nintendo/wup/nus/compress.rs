//! Stream a NUS-layout Wii U title into a ZArchive writer.
//!
//! Ticket, TMD, and content filenames come from
//! [`NusLayout::discover`] so both the canonical Nintendo layout
//! (`title.tik` + `title.tmd` + `{id:08x}.app`) and the community
//! layout (`cetk.<N>` + `tmd.<N>` + `{id:08x}`) work. When a ticket
//! is absent the title key is derived via
//! [`crate::nintendo::wup::title_key_derive::derive_title_key`].
//!
//! [`ContentLoader`] decrypts only the ranges covered by each FST file.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::nus::content_reader::{decrypt_raw_range, read_at};
use crate::nintendo::wup::nus::content_stream::{ContentLoader, DirectoryContentSource};
use crate::nintendo::wup::nus::fst_parser::{VirtualFs, parse_fst_ranges};
use crate::nintendo::wup::nus::layout::{NusLayout, TicketSource};
use crate::nintendo::wup::nus::ticket_parser::{TitleKey, read_ticket_file};
use crate::nintendo::wup::nus::tmd_parser::read_tmd_file;
use crate::nintendo::wup::title_key_derive::derive_title_key;
use crate::util::Cancelled;
use crate::util::ProgressReporter;
use crate::zar::ZarWriter;

fn parse_content0_fst(path: &Path, title_key: &TitleKey) -> WupResult<VirtualFs> {
    let mut file = File::open(path)?;
    let content_len = file.metadata()?.len();
    parse_fst_ranges(content_len, |offset, len| {
        decrypt_raw_range(
            |at, out| read_at(&mut file, at, out),
            title_key,
            0,
            offset,
            len,
        )
    })
}

pub(crate) struct NusTitlePlan {
    layout: NusLayout,
    tmd: crate::nintendo::wup::models::WupTmd,
    title_key: TitleKey,
    fs: VirtualFs,
}

impl NusTitlePlan {
    pub(crate) fn uncompressed_bytes(&self) -> u64 {
        self.fs
            .files
            .iter()
            .filter(|file| !file.is_shared)
            .map(|file| u64::from(file.file_size))
            .fold(0, u64::saturating_add)
    }
}

pub(crate) fn prepare_nus_title(title_dir: &Path) -> WupResult<NusTitlePlan> {
    let layout = NusLayout::discover(title_dir)?;
    let tmd = read_tmd_file(&layout.tmd_path)?;
    let title_key = match &layout.ticket_source {
        TicketSource::OnDisk(path) => {
            let (ticket, key) = read_ticket_file(path)?;
            if ticket.title_id != tmd.title_id {
                return Err(WupError::InvalidTicket);
            }
            key
        }
        TicketSource::Derive => TitleKey(derive_title_key(tmd.title_id)),
    };
    let content_0 = tmd.contents.first().ok_or(WupError::InvalidTmd)?;
    let content_0_path =
        layout
            .content
            .resolve(content_0.content_id)
            .ok_or(WupError::ContentNotFound {
                content_id: content_0.content_id,
            })?;
    let fs = parse_content0_fst(&content_0_path, &title_key)?;
    Ok(NusTitlePlan {
        layout,
        tmd,
        title_key,
        fs,
    })
}

/// Compress one NUS-format title into `sink`. Returns
/// `(title_id, title_version)` read from the TMD for caller logging.
/// The TMD is the source of truth since a ticket may not exist.
pub fn compress_nus_title<W: Write>(
    title_dir: &Path,
    sink: &mut ZarWriter<'_, W>,
    progress: &dyn ProgressReporter,
) -> WupResult<(u64, u16)> {
    compress_nus_title_with_cancel(title_dir, sink, progress, None)
}

pub(crate) fn compress_nus_title_with_cancel<W: Write>(
    title_dir: &Path,
    sink: &mut ZarWriter<'_, W>,
    progress: &dyn ProgressReporter,
    cancelled: Option<&AtomicBool>,
) -> WupResult<(u64, u16)> {
    let plan = prepare_nus_title(title_dir)?;
    compress_prepared_nus_title_with_cancel(plan, sink, progress, cancelled)
}

pub(crate) fn compress_prepared_nus_title_with_cancel<W: Write>(
    plan: NusTitlePlan,
    sink: &mut ZarWriter<'_, W>,
    progress: &dyn ProgressReporter,
    cancelled: Option<&AtomicBool>,
) -> WupResult<(u64, u16)> {
    let NusTitlePlan {
        layout,
        tmd,
        title_key,
        fs,
    } = plan;
    let title_id = tmd.title_id;
    let title_version = tmd.title_version;
    let archive_folder = format!("{:016x}_v{}", title_id, title_version);

    // Files extending beyond this title's shipped delta content are
    // inherited from the base title and remain absent from this overlay.
    let source = DirectoryContentSource::with_resolver(layout.content);
    let mut loader = ContentLoader::new(source, title_key, &tmd, &fs);
    let mut skipped: u32 = 0;
    for vfile in &fs.files {
        if cancelled.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Cancelled.into());
        }
        let archive_path = format!("{archive_folder}/{}", vfile.path);
        let mut started = false;
        match loader.stream_file(vfile, |bytes| {
            if !started {
                sink.start_file(&archive_path)?;
                started = true;
            }
            sink.append_data(bytes)?;
            progress.inc(bytes.len() as u64);
            Ok(())
        }) {
            Ok(_) => {
                if !started {
                    sink.start_file(&archive_path)?;
                }
            }
            Err(WupError::FileInheritedFromOtherTitle { .. }) => {
                skipped = skipped.saturating_add(1);
            }
            Err(e) => return Err(e),
        }
    }
    if skipped > 0 {
        log::info!(
            "Skipped {skipped} file(s) in title {title_id:016x} v{title_version}: \
             cluster data not shipped in this title, stacked .wua fills them in"
        );
    }

    Ok((title_id, title_version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::common_keys::WII_U_COMMON_KEY;
    use crate::nintendo::wup::models::ticket::{WUP_TICKET_BASE_SIZE, WUP_TICKET_FORMAT_V1};
    use crate::nintendo::wup::models::tmd::{
        TmdContentEntry, TmdContentFlags, WUP_TMD_CONTENT_ENTRY_SIZE, WUP_TMD_HEADER_SIZE,
    };
    use crate::nintendo::wup::nus::fst_parser::{
        FST_CLUSTER_ENTRY_SIZE, FST_FILE_ENTRY_SIZE, FST_HEADER_SIZE, FST_MAGIC,
    };
    use crate::util::NoProgress;
    use aes::{
        Aes128,
        cipher::{BlockModeEncrypt, KeyIvInit},
    };
    use block_padding::NoPadding;
    use cbc::Encryptor;

    type Aes128CbcEnc = Encryptor<Aes128>;

    const TEST_TITLE_ID: u64 = 0x0005_000E_1234_5678;
    const TEST_TITLE_VERSION: u16 = 32;
    const TEST_PLAIN_TITLE_KEY: [u8; 16] = [
        0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
        0x99,
    ];

    fn encrypt_in_place(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
        Aes128CbcEnc::new_from_slices(key, iv)
            .unwrap()
            .encrypt_padded::<NoPadding>(data, data.len())
            .unwrap();
    }

    /// Build a synthetic NUS title directory with:
    ///
    /// - A valid ticket encrypting TEST_PLAIN_TITLE_KEY.
    /// - A TMD with two content entries: content 0 (FST, cluster
    ///   index 0) and content 1 (data, cluster index 1).
    /// - Two .app files: content 0 holds a synthetic FST describing
    ///   one virtual file ("content/hello.bin"), content 1 holds
    ///   the plaintext of that virtual file under AES-CBC.
    ///
    /// Returns the expected plaintext bytes of the virtual file.
    fn make_synthetic_nus_title(dir: &Path) -> Vec<u8> {
        // Ticket
        let ticket = {
            let mut bytes = vec![0u8; WUP_TICKET_BASE_SIZE];
            bytes[0..4].copy_from_slice(&0x0001_0004u32.to_be_bytes());
            bytes[0x1BC] = WUP_TICKET_FORMAT_V1;
            let mut iv = [0u8; 16];
            iv[0..8].copy_from_slice(&TEST_TITLE_ID.to_be_bytes());
            let mut encrypted_key = TEST_PLAIN_TITLE_KEY;
            encrypt_in_place(&WII_U_COMMON_KEY, &iv, &mut encrypted_key);
            bytes[0x1BF..0x1CF].copy_from_slice(&encrypted_key);
            bytes[0x1DC..0x1E4].copy_from_slice(&TEST_TITLE_ID.to_be_bytes());
            bytes[0x1E6..0x1E8].copy_from_slice(&TEST_TITLE_VERSION.to_be_bytes());
            bytes
        };
        std::fs::write(dir.join("title.tik"), &ticket).unwrap();

        // Plaintext virtual file content. Pick a 32-byte payload
        // so the decrypted buffer for content 1 can be just one
        // AES block without padding gymnastics.
        let plaintext: Vec<u8> = (0u8..32).collect();

        // TMD with 2 content entries.
        let contents = [
            TmdContentEntry {
                content_id: 0x0000_0000,
                index: 0,
                flags: TmdContentFlags::ENCRYPTED,
                size: 0, // not used by our parser
                hash: [0u8; 32],
            },
            TmdContentEntry {
                content_id: 0x0000_0001,
                index: 1,
                flags: TmdContentFlags::ENCRYPTED,
                size: plaintext.len() as u64,
                hash: [0u8; 32],
            },
        ];
        let tmd = {
            let mut bytes =
                vec![0u8; WUP_TMD_HEADER_SIZE + contents.len() * WUP_TMD_CONTENT_ENTRY_SIZE];
            bytes[0..4].copy_from_slice(&0x0001_0004u32.to_be_bytes());
            bytes[0x180] = 1;
            bytes[0x18C..0x194].copy_from_slice(&TEST_TITLE_ID.to_be_bytes());
            bytes[0x1DC..0x1DE].copy_from_slice(&TEST_TITLE_VERSION.to_be_bytes());
            bytes[0x1DE..0x1E0].copy_from_slice(&(contents.len() as u16).to_be_bytes());
            for (i, entry) in contents.iter().enumerate() {
                let start = WUP_TMD_HEADER_SIZE + i * WUP_TMD_CONTENT_ENTRY_SIZE;
                bytes[start..start + 4].copy_from_slice(&entry.content_id.to_be_bytes());
                bytes[start + 4..start + 6].copy_from_slice(&entry.index.to_be_bytes());
                bytes[start + 6..start + 8].copy_from_slice(&entry.flags.bits().to_be_bytes());
                bytes[start + 8..start + 16].copy_from_slice(&entry.size.to_be_bytes());
                bytes[start + 16..start + 48].copy_from_slice(&entry.hash);
            }
            bytes
        };
        std::fs::write(dir.join("title.tmd"), &tmd).unwrap();

        // Build a small FST describing:
        //   0: root       (end_index = 3)
        //   1: content dir (end_index = 3, parent = 0)
        //   2: hello.bin  (cluster = 1, offset = 0, size = 32)
        let num_entries: u32 = 3;
        let num_clusters: u32 = 2;

        let mut name_table = Vec::new();
        let mut name_offsets = std::collections::HashMap::new();
        for name in ["", "content", "hello.bin"] {
            name_offsets.insert(name.to_string(), name_table.len() as u32);
            name_table.extend_from_slice(name.as_bytes());
            name_table.push(0);
        }

        let header_size = FST_HEADER_SIZE;
        let cluster_table_size = (num_clusters as usize) * FST_CLUSTER_ENTRY_SIZE;
        let entries_size = (num_entries as usize) * FST_FILE_ENTRY_SIZE;
        let fst_total = header_size + cluster_table_size + entries_size + name_table.len();
        // Pad to 16 bytes so AES-CBC encryption of the whole FST
        // plaintext (content 0) is block-aligned.
        let aes_padded = fst_total.div_ceil(16) * 16;
        let mut fst_plain = vec![0u8; aes_padded];

        // Header
        fst_plain[0..4].copy_from_slice(&FST_MAGIC.to_be_bytes());
        fst_plain[4..8].copy_from_slice(&1u32.to_be_bytes()); // offset_factor
        fst_plain[8..12].copy_from_slice(&num_clusters.to_be_bytes());

        // Cluster table (both raw mode, both owned by TEST_TITLE_ID).
        let c0 = header_size;
        fst_plain[c0 + 0x08..c0 + 0x10].copy_from_slice(&TEST_TITLE_ID.to_be_bytes());
        fst_plain[c0 + 0x14] = 0; // cluster 0 hash_mode = Raw
        fst_plain[c0 + FST_CLUSTER_ENTRY_SIZE + 0x08..c0 + FST_CLUSTER_ENTRY_SIZE + 0x10]
            .copy_from_slice(&TEST_TITLE_ID.to_be_bytes());
        fst_plain[c0 + FST_CLUSTER_ENTRY_SIZE + 0x14] = 0; // cluster 1 hash_mode = Raw

        // File entries.
        let entries_start = header_size + cluster_table_size;
        let write_entry = |buf: &mut Vec<u8>,
                           idx: usize,
                           is_dir: bool,
                           name: &str,
                           a: u32,
                           b: u32,
                           cluster: u16| {
            let start = entries_start + idx * FST_FILE_ENTRY_SIZE;
            let type_flag: u8 = if is_dir { 0x01 } else { 0x00 };
            let name_offset = name_offsets[name];
            let type_and_name = ((type_flag as u32) << 24) | (name_offset & 0x00FF_FFFF);
            buf[start..start + 4].copy_from_slice(&type_and_name.to_be_bytes());
            buf[start + 4..start + 8].copy_from_slice(&a.to_be_bytes());
            buf[start + 8..start + 12].copy_from_slice(&b.to_be_bytes());
            buf[start + 12..start + 14].copy_from_slice(&0u16.to_be_bytes());
            buf[start + 14..start + 16].copy_from_slice(&cluster.to_be_bytes());
        };
        write_entry(&mut fst_plain, 0, true, "", 0, num_entries, 0);
        write_entry(&mut fst_plain, 1, true, "content", 0, num_entries, 0);
        write_entry(
            &mut fst_plain,
            2,
            false,
            "hello.bin",
            0,
            plaintext.len() as u32,
            1,
        );

        let names_start = entries_start + entries_size;
        fst_plain[names_start..names_start + name_table.len()].copy_from_slice(&name_table);

        // Encrypt content 0 (the FST) with cluster-0 IV.
        let mut content_0_encrypted = fst_plain;
        let mut iv_0 = [0u8; 16];
        iv_0[0] = 0;
        iv_0[1] = 0;
        encrypt_in_place(&TEST_PLAIN_TITLE_KEY, &iv_0, &mut content_0_encrypted);
        std::fs::write(dir.join("00000000.app"), &content_0_encrypted).unwrap();

        // Encrypt content 1 (the data) with cluster-1 IV.
        let mut content_1_encrypted = plaintext.clone();
        let mut iv_1 = [0u8; 16];
        iv_1[0] = 0;
        iv_1[1] = 1;
        encrypt_in_place(&TEST_PLAIN_TITLE_KEY, &iv_1, &mut content_1_encrypted);
        std::fs::write(dir.join("00000001.app"), &content_1_encrypted).unwrap();

        plaintext
    }

    #[test]
    fn end_to_end_nus_title_ends_up_in_archive() {
        let dir = tempfile::tempdir().unwrap();
        let expected_payload = make_synthetic_nus_title(dir.path());

        let mut archive = Vec::new();
        let mut writer = ZarWriter::new(&mut archive, 2).unwrap();
        let (title_id, title_version) =
            compress_nus_title(dir.path(), &mut writer, &NoProgress).unwrap();
        assert_eq!(title_id, TEST_TITLE_ID);
        assert_eq!(title_version, TEST_TITLE_VERSION);
        writer.finish().unwrap();

        // Decode the produced archive and verify the virtual file
        // landed under the title folder with byte-identical payload.
        let mut reader = crate::zar::ZarReader::open(std::io::Cursor::new(&archive)).unwrap();
        let index = reader
            .lookup("0005000e12345678_v32/content/hello.bin")
            .unwrap();
        let mut extracted = Vec::new();
        reader.read_file(index, &mut extracted).unwrap();
        assert_eq!(extracted, expected_payload);
    }

    /// Capture every `progress.inc` delta so a test can assert the
    /// scan's total matches what actually streams through.
    #[derive(Default)]
    struct RecordingProgress {
        events: std::sync::Mutex<Vec<u64>>,
    }
    impl crate::util::ProgressReporter for RecordingProgress {
        fn start(&self, _total: u64, _msg: &str) {}
        fn inc(&self, delta: u64) {
            self.events.lock().unwrap().push(delta);
        }
        fn finish(&self) {}
    }

    #[test]
    fn estimate_matches_actual_inc_sum() {
        let dir = tempfile::tempdir().unwrap();
        let expected_payload = make_synthetic_nus_title(dir.path());

        let estimate = prepare_nus_title(dir.path()).unwrap().uncompressed_bytes();
        assert_eq!(estimate, expected_payload.len() as u64);

        // Drive the real compress path and sum every inc delta. The
        // estimate must equal the sum so the progress bar lands at
        // exactly 100% when reads finish.
        let mut writer = ZarWriter::new(Vec::new(), 2).unwrap();
        let progress = RecordingProgress::default();
        compress_nus_title(dir.path(), &mut writer, &progress).unwrap();
        let actual: u64 = progress.events.lock().unwrap().iter().sum();
        assert_eq!(estimate, actual);
    }

    #[test]
    fn rejects_directory_with_no_tmd_at_all() {
        // Layout resolver requires a TMD file to read title id and
        // content list. Without one the directory is unrecognizable.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("title.tik"), b"junk").unwrap();
        let mut writer = ZarWriter::new(Vec::new(), 2).unwrap();
        let err = compress_nus_title(dir.path(), &mut writer, &NoProgress);
        assert!(matches!(err, Err(WupError::UnrecognizedTitleDirectory(_))));
    }

    #[test]
    fn rejects_junk_tmd_bytes() {
        // A present-but-unparseable TMD surfaces as a parse error, not
        // as a missing-file error.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("title.tmd"), b"junk").unwrap();
        let mut writer = ZarWriter::new(Vec::new(), 2).unwrap();
        let err = compress_nus_title(dir.path(), &mut writer, &NoProgress);
        assert!(matches!(
            err,
            Err(WupError::InvalidTmd) | Err(WupError::IoError(_))
        ));
    }
}
