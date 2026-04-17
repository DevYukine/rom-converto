//! Top-level Wii U title compressor.
//!
//! Dispatches each input to the right pipeline:
//!
//! - **Loadiine directory**: `meta/code/content` with decrypted files
//! - **NUS directory**: `title.tmd` + `title.tik` + `*.app`, decrypted
//!   on the fly via the Wii U common key
//! - **Disc image**: `.wud` or `.wux`, decrypted via a per-disc master
//!   key (sibling `game.key` or explicit path)
//!
//! Output is a single `.wua` that may bundle multiple titles
//! (base + update + DLC each in their own subfolder).
//!
//! The sync entry point is wrapped by the CLI / Tauri layers in
//! `tokio::task::spawn_blocking`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::nintendo::wup::compress_parallel::spawn_zarchive_pool;
use crate::nintendo::wup::constants::{
    COMPRESSED_BLOCK_SIZE, MAX_ZSTD_LEVEL, MIN_ZSTD_LEVEL, ZARCHIVE_DEFAULT_ZSTD_LEVEL,
};
use crate::nintendo::wup::disc::compress::{compress_disc_title, estimate_disc_uncompressed_bytes};
use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::loadiine::{
    compress_loadiine_title, detect_loadiine_title, estimate_loadiine_uncompressed_bytes,
};
use crate::nintendo::wup::nus::compress::{compress_nus_title, estimate_nus_uncompressed_bytes};
use crate::nintendo::wup::streaming_sink::{StreamingSink, spawn_stream_pipeline};
use crate::nintendo::wup::zarchive_writer::write_zarchive_tail;
use crate::util::ProgressReporter;

/// Recognised Wii U title layouts. The [`compress_titles`]
/// dispatcher picks one per input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleInputFormat {
    /// `meta/`, `code/`, `content/` directory with already-decrypted
    /// plain files.
    Loadiine,
    /// `title.tmd` + `title.tik` + `{contentId}.app` files requiring
    /// title-key decryption and FST walking.
    Nus,
    /// Wii U disc image: raw (`.wud`) or deduplicated (`.wux`).
    /// Requires a per-disc master key file.
    Disc,
}

/// One input title, optionally with a caller-supplied format hint
/// and disc-key override. Pass `format = None` to auto-detect.
#[derive(Debug, Clone)]
pub struct TitleInput {
    /// Input path. A directory for loadiine/NUS, a file for disc.
    pub dir: PathBuf,
    /// Optional explicit format, skipping auto-detection.
    pub format: Option<TitleInputFormat>,
    /// Optional override path for the disc master key. Ignored for
    /// non-disc inputs. If `None` and the input is a disc image, the
    /// loader falls back to `<input>.key` and then `game.key` in the
    /// same directory.
    pub key_path: Option<PathBuf>,
}

impl TitleInput {
    /// Convenience: build an auto-detect input.
    pub fn auto<P: Into<PathBuf>>(dir: P) -> Self {
        Self {
            dir: dir.into(),
            format: None,
            key_path: None,
        }
    }

    /// Convenience: build a disc input with an explicit key path.
    pub fn disc_with_key<P: Into<PathBuf>>(disc: P, key: PathBuf) -> Self {
        Self {
            dir: disc.into(),
            format: Some(TitleInputFormat::Disc),
            key_path: Some(key),
        }
    }
}

/// Runtime options for [`compress_titles`].
#[derive(Debug, Clone, Copy)]
pub struct WupCompressOptions {
    /// Zstd compression level (0..=22). 0 selects the Cemu default
    /// of [`ZARCHIVE_DEFAULT_ZSTD_LEVEL`].
    pub zstd_level: i32,
}

impl Default for WupCompressOptions {
    fn default() -> Self {
        Self {
            zstd_level: ZARCHIVE_DEFAULT_ZSTD_LEVEL,
        }
    }
}

/// Detect an input's title format. Handles both directories
/// (loadiine / NUS) and files (disc images). Returns
/// [`WupError::UnrecognizedTitleDirectory`] or
/// [`WupError::UnsupportedDiscFormat`] when nothing matches.
pub fn detect_title_format(path: &Path) -> WupResult<TitleInputFormat> {
    // Disc images: file with a `.wud` or `.wux` extension, or any
    // file whose first bytes are the WUX magic.
    if path.is_file() {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("wud") | Some("wux")) {
            return Ok(TitleInputFormat::Disc);
        }
        // As a second chance, sniff the magic bytes; lets `.iso`
        // wrappers around WUX still work.
        if let Ok(mut f) = std::fs::File::open(path) {
            use std::io::Read;
            let mut head = [0u8; 8];
            if f.read(&mut head).map(|n| n >= 8).unwrap_or(false)
                && &head[0..4] == b"WUX0"
                && head[4..8] == [0x2E, 0xD0, 0x99, 0x10]
            {
                return Ok(TitleInputFormat::Disc);
            }
        }
        return Err(WupError::UnsupportedDiscFormat(path.to_path_buf()));
    }

    if !path.is_dir() {
        return Err(WupError::UnrecognizedTitleDirectory(path.to_path_buf()));
    }
    let meta_xml = path.join("meta").join("meta.xml");
    let app_xml = path.join("code").join("app.xml");
    let cos_xml = path.join("code").join("cos.xml");
    if meta_xml.is_file() && app_xml.is_file() && cos_xml.is_file() {
        return Ok(TitleInputFormat::Loadiine);
    }
    // NUS covers both the canonical Nintendo layout
    // (`title.tmd` + `title.tik` + `{id}.app`) and the community
    // layout (`tmd.<N>` + optional `cetk.<N>` + `{id}` with no
    // extension). A TMD plus at least one content file is enough;
    // the ticket is optional because the loader can derive the title
    // key for any CDN-minted title.
    if has_any_tmd(path)? && has_any_content_file(path)? {
        return Ok(TitleInputFormat::Nus);
    }
    Err(WupError::UnrecognizedTitleDirectory(path.to_path_buf()))
}

/// True when the directory contains either a canonical `title.tmd`
/// or any `tmd.<digits>` file.
fn has_any_tmd(dir: &Path) -> WupResult<bool> {
    if dir.join("title.tmd").is_file() {
        return Ok(true);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if let Some(name) = entry.file_name().to_str()
            && let Some(rest) = name.strip_prefix("tmd.")
            && rest.chars().all(|c| c.is_ascii_digit())
            && !rest.is_empty()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// True when the directory has at least one content file of the
/// form `{id:08x}` or `{id:08x}.app`. Ignores `.h3` sidecars and
/// any other trailing extension.
fn has_any_content_file(dir: &Path) -> WupResult<bool> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        if is_content_filename(&name) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_content_filename(name: &str) -> bool {
    // Strip an optional ".app" suffix, then accept iff the remainder
    // is exactly 8 lowercase hex digits. This rules out `.h3`
    // sidecars (not 8 hex) and short partial names.
    let core = name.strip_suffix(".app").unwrap_or(name);
    core.len() == 8
        && core
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Derive the default `.wua` output path from an input title
/// directory name: `<dir>.wua` next to the directory.
pub fn derive_wua_path(title_dir: &Path) -> PathBuf {
    let mut out = title_dir.to_path_buf();
    // `with_extension` handles both "dir/title" and "dir/title.foo".
    if !out.set_extension("wua") {
        out.push(".wua");
    }
    out
}

/// Validate the zstd level before spinning up the writer. Zero is
/// treated as "use the Cemu default" and passed through unchanged.
fn validate_level(level: i32) -> WupResult<()> {
    if !(MIN_ZSTD_LEVEL..=MAX_ZSTD_LEVEL).contains(&level) {
        return Err(WupError::InvalidCompressionLevel {
            level,
            min: MIN_ZSTD_LEVEL,
            max: MAX_ZSTD_LEVEL,
        });
    }
    Ok(())
}

/// Compress one title into a single-file `.wua` archive at `output`.
/// The format of `input` is auto-detected.
pub fn compress_title(
    input: &Path,
    output: &Path,
    opts: WupCompressOptions,
    progress: &dyn ProgressReporter,
) -> WupResult<()> {
    compress_titles(&[TitleInput::auto(input)], output, opts, progress)
}

/// Async wrapper around [`compress_titles`]. Runs the sync pipeline
/// inside `spawn_blocking` and relays progress back via an atomic
/// byte counter polled every 100 ms, matching `z3ds::compress_rom`.
pub async fn compress_titles_async(
    titles: Vec<TitleInput>,
    output: PathBuf,
    opts: WupCompressOptions,
    progress: &dyn ProgressReporter,
) -> WupResult<()> {
    validate_level(opts.zstd_level)?;
    if titles.is_empty() {
        return Err(WupError::InvalidPath(
            "compress_titles_async called with no input titles".to_string(),
        ));
    }
    progress.start(0, "Compressing Wii U title");

    let events: Arc<Mutex<Vec<ProgressEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_for_task = events.clone();

    let mut handle = tokio::task::spawn_blocking(move || -> WupResult<()> {
        let shim = QueuedProgress {
            events: events_for_task,
        };
        compress_titles(&titles, &output, opts, &shim)
    });

    let pump = |progress: &dyn ProgressReporter| {
        let drained: Vec<ProgressEvent> = std::mem::take(&mut *events.lock().unwrap());
        apply_events(progress, drained);
    };

    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(result) => {
                result??;
                break;
            }
            Err(_) => pump(progress),
        }
    }
    pump(progress);
    progress.finish();
    Ok(())
}

/// Async convenience over a single title directory.
pub async fn compress_title_async(
    input: PathBuf,
    output: PathBuf,
    opts: WupCompressOptions,
    progress: &dyn ProgressReporter,
) -> WupResult<()> {
    compress_titles_async(vec![TitleInput::auto(input)], output, opts, progress).await
}

enum ProgressEvent {
    Start { total: u64, msg: String },
    Inc(u64),
}

struct QueuedProgress {
    events: Arc<Mutex<Vec<ProgressEvent>>>,
}

impl QueuedProgress {
    fn push(&self, ev: ProgressEvent) {
        let mut guard = self.events.lock().unwrap();
        if let (ProgressEvent::Inc(delta), Some(ProgressEvent::Inc(last))) = (&ev, guard.last_mut())
        {
            *last = last.saturating_add(*delta);
            return;
        }
        guard.push(ev);
    }
}

impl ProgressReporter for QueuedProgress {
    fn start(&self, total: u64, msg: &str) {
        self.push(ProgressEvent::Start {
            total,
            msg: msg.to_string(),
        });
    }

    fn inc(&self, delta: u64) {
        if delta == 0 {
            return;
        }
        self.push(ProgressEvent::Inc(delta));
    }

    fn finish(&self) {}
}

fn apply_events(progress: &dyn ProgressReporter, events: Vec<ProgressEvent>) {
    for ev in events {
        match ev {
            ProgressEvent::Start { total, msg } => progress.start(total, &msg),
            ProgressEvent::Inc(delta) => progress.inc(delta),
        }
    }
}

/// Compress one or more titles into a single `.wua` archive. Each
/// title lands under its own `<title_id>_v<version>/` folder, the
/// layout Cemu expects.
pub fn compress_titles(
    titles: &[TitleInput],
    output: &Path,
    opts: WupCompressOptions,
    progress: &(dyn ProgressReporter + Sync),
) -> WupResult<()> {
    validate_level(opts.zstd_level)?;
    if titles.is_empty() {
        return Err(WupError::InvalidPath(
            "compress_titles called with no input titles".to_string(),
        ));
    }
    // Resolve formats up front so a later title failing detection
    // doesn't waste work on earlier ones.
    let resolved: Vec<(PathBuf, TitleInputFormat, Option<PathBuf>)> = titles
        .iter()
        .map(|t| match t.format {
            Some(f) => Ok((t.dir.clone(), f, t.key_path.clone())),
            None => detect_title_format(&t.dir).map(|f| (t.dir.clone(), f, t.key_path.clone())),
        })
        .collect::<WupResult<Vec<_>>>()?;

    // Pre-scan every input to compute a deterministic total byte
    // count. Each scan parses the TMD + content 0 (disc, NUS) or
    // stats the file tree (loadiine), then sums the sizes of every
    // non-shared FST file. Uses the same skip predicate as the
    // streaming readers so the sum matches the bytes that flow
    // through the pipeline.
    let mut read_total_bytes: u64 = 0;
    for (path, format, key_path) in &resolved {
        let n = match format {
            TitleInputFormat::Loadiine => estimate_loadiine_uncompressed_bytes(path)?,
            TitleInputFormat::Nus => estimate_nus_uncompressed_bytes(path)?,
            TitleInputFormat::Disc => estimate_disc_uncompressed_bytes(path, key_path.as_deref())?,
        };
        read_total_bytes = read_total_bytes.saturating_add(n);
    }

    let file = std::fs::File::create(output)?;
    // 4 MiB BufWriter batches many 64 KiB block writes into a single
    // WriteFile syscall, cutting user <-> kernel transitions across
    // large archives. The OS write cache does the rest.
    let buf_writer = std::io::BufWriter::with_capacity(4 * 1024 * 1024, file);
    let pool = spawn_zarchive_pool(opts.zstd_level)?;

    // `total_blocks` is the exact count the streaming pipeline will
    // consume. Reads produce ceil(read_total_bytes / 64 KiB) blocks:
    // exact multiples land on a block boundary, anything else is
    // flushed as one padded trailing block.
    let total_blocks = if read_total_bytes == 0 {
        0
    } else {
        read_total_bytes.div_ceil(COMPRESSED_BLOCK_SIZE as u64)
    };

    // Writer-driven progress: `inc` fires per batch of blocks
    // committed to disk. Reads stay silent so the bar keeps moving
    // while the main thread is inside a long AES decrypt.
    let total_uncompressed_bytes = total_blocks * COMPRESSED_BLOCK_SIZE as u64;
    progress.start(
        total_uncompressed_bytes,
        "Reading and compressing Wii U titles",
    );

    // Run reads + compression + writes concurrently inside a scope.
    // After the scope exits, the writer thread has returned the
    // output file back to the main thread along with the hasher,
    // byte counter, and offset-records table we need to emit the
    // metadata sections.
    let (stream_result, tree) = std::thread::scope(|s| -> WupResult<_> {
        let (block_tx, handle) =
            spawn_stream_pipeline(s, pool, total_blocks, buf_writer, Some(progress));
        let mut sink = StreamingSink::new(block_tx);
        let silent = crate::util::NoProgress;

        let read_result: WupResult<()> = (|| {
            for (path, format, key_path) in &resolved {
                match format {
                    TitleInputFormat::Loadiine => {
                        let title = detect_loadiine_title(path)?
                            .ok_or_else(|| WupError::UnrecognizedTitleDirectory(path.clone()))?;
                        compress_loadiine_title(&title, &mut sink, &silent)?;
                    }
                    TitleInputFormat::Nus => {
                        compress_nus_title(path, &mut sink, &silent)?;
                    }
                    TitleInputFormat::Disc => {
                        compress_disc_title(path, key_path.as_deref(), &mut sink, &silent)?;
                    }
                }
            }
            Ok(())
        })();
        read_result?;

        // Pad and send the trailing partial block, dropping the sink
        // (which drops its block_tx copy) so the driver sees end of
        // stream.
        let tree = sink.flush_trailing()?;

        // Writer thread returns the finalized output state on join.
        let stream_result = handle.join()?;
        Ok((stream_result, tree))
    })?;

    // Streaming pipeline is done; now on the main thread with full
    // ownership of the inner writer. Switch to the "Finalizing"
    // indeterminate pulse while we emit the metadata tail and wait
    // for the OS to flush the multi-GB write cache on drop.
    progress.start(0, "Finalizing archive");

    let mut inner = stream_result.inner;
    let mut hasher = stream_result.hasher;
    let mut bytes_written = stream_result.bytes_written;
    let mut tree = tree;
    write_zarchive_tail(
        &mut inner,
        &mut hasher,
        &mut bytes_written,
        &stream_result.offset_records,
        &mut tree,
    )?;

    drop(inner);

    progress.finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;

    fn make_minimal_loadiine(root: &Path, title_id_hex: &str, title_version: u32) {
        std::fs::create_dir_all(root.join("meta")).unwrap();
        std::fs::create_dir_all(root.join("code")).unwrap();
        std::fs::create_dir_all(root.join("content")).unwrap();
        std::fs::write(root.join("meta").join("meta.xml"), b"<menu/>").unwrap();
        let app_xml = format!(
            "<app><title_id>{title_id_hex}</title_id><title_version>{title_version}</title_version></app>"
        );
        std::fs::write(root.join("code").join("app.xml"), app_xml.as_bytes()).unwrap();
        std::fs::write(root.join("code").join("cos.xml"), b"<cos/>").unwrap();
        std::fs::write(root.join("content").join("data.bin"), [0x42u8; 16]).unwrap();
    }

    #[test]
    fn detect_loadiine_layout() {
        let dir = tempfile::tempdir().unwrap();
        make_minimal_loadiine(dir.path(), "0005000E10102000", 32);
        assert_eq!(
            detect_title_format(dir.path()).unwrap(),
            TitleInputFormat::Loadiine
        );
    }

    #[test]
    fn detect_nus_layout() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("title.tik"), b"fake").unwrap();
        std::fs::write(dir.path().join("title.tmd"), b"fake").unwrap();
        std::fs::write(dir.path().join("00000000.app"), b"fake").unwrap();
        assert_eq!(
            detect_title_format(dir.path()).unwrap(),
            TitleInputFormat::Nus
        );
    }

    #[test]
    fn detect_no_intro_layout_with_cetk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tmd.80"), b"fake").unwrap();
        std::fs::write(dir.path().join("cetk.80"), b"fake").unwrap();
        std::fs::write(dir.path().join("00000000"), b"fake").unwrap();
        std::fs::write(dir.path().join("00000000.h3"), [0u8; 20]).unwrap();
        assert_eq!(
            detect_title_format(dir.path()).unwrap(),
            TitleInputFormat::Nus
        );
    }

    #[test]
    fn detect_no_intro_layout_without_ticket() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tmd.0"), b"fake").unwrap();
        std::fs::write(dir.path().join("00000000"), b"fake").unwrap();
        assert_eq!(
            detect_title_format(dir.path()).unwrap(),
            TitleInputFormat::Nus
        );
    }

    #[test]
    fn detect_skips_h3_sidecars_as_content() {
        // If the only file that looks content-ish is a `.h3`, the
        // directory is NOT a NUS title.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tmd.0"), b"fake").unwrap();
        std::fs::write(dir.path().join("00000000.h3"), [0u8; 20]).unwrap();
        let err = detect_title_format(dir.path());
        assert!(matches!(err, Err(WupError::UnrecognizedTitleDirectory(_))));
    }

    #[test]
    fn detect_rejects_junk_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("random.txt"), b"hello").unwrap();
        let err = detect_title_format(dir.path());
        assert!(matches!(err, Err(WupError::UnrecognizedTitleDirectory(_))));
    }

    #[test]
    fn compress_title_loadiine_produces_wua_readable_via_test_reader() {
        let dir = tempfile::tempdir().unwrap();
        let title_dir = dir.path().join("title");
        std::fs::create_dir_all(&title_dir).unwrap();
        make_minimal_loadiine(&title_dir, "0005000E10102000", 32);
        let output = dir.path().join("out.wua");

        compress_title(
            &title_dir,
            &output,
            WupCompressOptions::default(),
            &NoProgress,
        )
        .unwrap();

        // The produced archive should contain the three minimum
        // loadiine files plus the content payload.
        let bytes = std::fs::read(&output).unwrap();
        let reader =
            crate::nintendo::wup::zarchive_writer::tests::test_reader::TestReader::open(&bytes)
                .unwrap();
        let meta = reader.extract_file("0005000e10102000_v32/meta/meta.xml");
        let app = reader.extract_file("0005000e10102000_v32/code/app.xml");
        let cos = reader.extract_file("0005000e10102000_v32/code/cos.xml");
        let data = reader.extract_file("0005000e10102000_v32/content/data.bin");
        assert_eq!(meta, b"<menu/>");
        assert!(!app.is_empty());
        assert_eq!(cos, b"<cos/>");
        assert_eq!(data, vec![0x42u8; 16]);
    }

    #[test]
    fn compress_titles_bundles_two_loadiine_titles() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let update = dir.path().join("update");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&update).unwrap();
        make_minimal_loadiine(&base, "0005000010102000", 0);
        make_minimal_loadiine(&update, "0005000E10102000", 32);
        let output = dir.path().join("bundle.wua");

        compress_titles(
            &[TitleInput::auto(&base), TitleInput::auto(&update)],
            &output,
            WupCompressOptions::default(),
            &NoProgress,
        )
        .unwrap();

        let bytes = std::fs::read(&output).unwrap();
        let reader =
            crate::nintendo::wup::zarchive_writer::tests::test_reader::TestReader::open(&bytes)
                .unwrap();
        assert_eq!(
            reader.extract_file("0005000010102000_v0/meta/meta.xml"),
            b"<menu/>"
        );
        assert_eq!(
            reader.extract_file("0005000e10102000_v32/meta/meta.xml"),
            b"<menu/>"
        );
    }

    #[test]
    fn compress_titles_fires_two_progress_phases() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct PhaseRecorder {
            starts: Mutex<Vec<(u64, String)>>,
        }
        impl ProgressReporter for PhaseRecorder {
            fn start(&self, total: u64, msg: &str) {
                self.starts.lock().unwrap().push((total, msg.to_string()));
            }
            fn inc(&self, _delta: u64) {}
            fn finish(&self) {}
        }

        let dir = tempfile::tempdir().unwrap();
        let title_dir = dir.path().join("title");
        std::fs::create_dir_all(&title_dir).unwrap();
        make_minimal_loadiine(&title_dir, "0005000E10102000", 32);
        let output = dir.path().join("out.wua");

        let progress = PhaseRecorder::default();
        compress_titles(
            &[TitleInput::auto(&title_dir)],
            &output,
            WupCompressOptions::default(),
            &progress,
        )
        .unwrap();

        let starts = progress.starts.lock().unwrap();
        let messages: Vec<&str> = starts.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(
            messages,
            vec!["Reading and compressing Wii U titles", "Finalizing archive"]
        );
        assert!(starts[0].0 > 0, "first start must be determinate");
        assert_eq!(starts[1].0, 0, "second start must be indeterminate");
    }

    #[test]
    fn estimate_across_multi_title_loadiine_bundle_matches_phase1_deltas() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct RecordingProgress {
            phase: Mutex<u32>,
            phase1_deltas: Mutex<u64>,
            read_total: Mutex<u64>,
        }
        impl ProgressReporter for RecordingProgress {
            fn start(&self, total: u64, msg: &str) {
                let mut phase = self.phase.lock().unwrap();
                *phase += 1;
                if *phase == 1 && msg.starts_with("Reading and compressing") {
                    *self.read_total.lock().unwrap() = total;
                }
            }
            fn inc(&self, delta: u64) {
                if *self.phase.lock().unwrap() == 1 {
                    *self.phase1_deltas.lock().unwrap() += delta;
                }
            }
            fn finish(&self) {}
        }

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let update = dir.path().join("update");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&update).unwrap();
        make_minimal_loadiine(&base, "0005000010102000", 0);
        make_minimal_loadiine(&update, "0005000E10102000", 32);
        let output = dir.path().join("bundle.wua");

        let progress = RecordingProgress::default();
        compress_titles(
            &[TitleInput::auto(&base), TitleInput::auto(&update)],
            &output,
            WupCompressOptions::default(),
            &progress,
        )
        .unwrap();

        // The first `start`'s total must equal the sum of `inc`
        // deltas that land while it is the active reporter state.
        // Otherwise the bar would either never hit 100% or overshoot.
        let read_total = *progress.read_total.lock().unwrap();
        let phase1_deltas = *progress.phase1_deltas.lock().unwrap();
        assert_eq!(read_total, phase1_deltas);
        assert!(read_total > 0, "scan should discover non-empty total");
    }

    #[test]
    fn compress_titles_bundles_two_nus_titles() {
        use crate::nintendo::wup::common_keys::WII_U_COMMON_KEY;
        use crate::nintendo::wup::models::ticket::{WUP_TICKET_BASE_SIZE, WUP_TICKET_FORMAT_V1};
        use crate::nintendo::wup::models::tmd::{
            TmdContentFlags, WUP_TMD_CONTENT_ENTRY_SIZE, WUP_TMD_HEADER_SIZE,
        };
        use crate::nintendo::wup::nus::fst_parser::{
            FST_CLUSTER_ENTRY_SIZE, FST_FILE_ENTRY_SIZE, FST_HEADER_SIZE, FST_MAGIC,
        };
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

        // Build a minimal valid NUS title at `dir` with one content
        // directory holding one file named `foo.bin` that contains
        // `payload`. Returns the 128-byte payload for round-trip
        // verification.
        fn build_minimal_nus(
            dir: &Path,
            title_id: u64,
            title_version: u16,
            title_key: [u8; 16],
        ) -> Vec<u8> {
            let payload: Vec<u8> = (0u8..128).collect();

            // Ticket.
            let mut ticket = vec![0u8; WUP_TICKET_BASE_SIZE];
            ticket[0..4].copy_from_slice(&0x0001_0004u32.to_be_bytes());
            ticket[0x1BC] = WUP_TICKET_FORMAT_V1;
            let mut enc_key = title_key;
            let mut key_iv = [0u8; 16];
            key_iv[0..8].copy_from_slice(&title_id.to_be_bytes());
            encrypt(&WII_U_COMMON_KEY, &key_iv, &mut enc_key);
            ticket[0x1BF..0x1CF].copy_from_slice(&enc_key);
            ticket[0x1DC..0x1E4].copy_from_slice(&title_id.to_be_bytes());
            ticket[0x1E6..0x1E8].copy_from_slice(&title_version.to_be_bytes());
            std::fs::write(dir.join("title.tik"), &ticket).unwrap();

            let fst_size =
                (FST_HEADER_SIZE + 2 * FST_CLUSTER_ENTRY_SIZE + 3 * FST_FILE_ENTRY_SIZE + 32)
                    .max(0x200);

            // TMD with two content entries (FST + payload).
            let mut tmd = vec![0u8; WUP_TMD_HEADER_SIZE + 2 * WUP_TMD_CONTENT_ENTRY_SIZE];
            tmd[0..4].copy_from_slice(&0x0001_0004u32.to_be_bytes());
            tmd[0x18C..0x194].copy_from_slice(&title_id.to_be_bytes());
            tmd[0x1DC..0x1DE].copy_from_slice(&title_version.to_be_bytes());
            tmd[0x1DE..0x1E0].copy_from_slice(&2u16.to_be_bytes());
            let fst_e = WUP_TMD_HEADER_SIZE;
            tmd[fst_e + 6..fst_e + 8]
                .copy_from_slice(&TmdContentFlags::ENCRYPTED.bits().to_be_bytes());
            tmd[fst_e + 8..fst_e + 16].copy_from_slice(&(fst_size as u64).to_be_bytes());
            let pay_e = fst_e + WUP_TMD_CONTENT_ENTRY_SIZE;
            tmd[pay_e..pay_e + 4].copy_from_slice(&1u32.to_be_bytes());
            tmd[pay_e + 4..pay_e + 6].copy_from_slice(&1u16.to_be_bytes());
            tmd[pay_e + 6..pay_e + 8]
                .copy_from_slice(&TmdContentFlags::ENCRYPTED.bits().to_be_bytes());
            tmd[pay_e + 8..pay_e + 16].copy_from_slice(&(payload.len() as u64).to_be_bytes());
            std::fs::write(dir.join("title.tmd"), &tmd).unwrap();

            // FST: 2 clusters (FST + payload), 3 entries (root, content, foo.bin).
            let num_clusters: u32 = 2;
            let mut fst = vec![0u8; fst_size];
            fst[0..4].copy_from_slice(&FST_MAGIC.to_be_bytes());
            fst[4..8].copy_from_slice(&1u32.to_be_bytes());
            fst[8..12].copy_from_slice(&num_clusters.to_be_bytes());
            fst[FST_HEADER_SIZE + 0x08..FST_HEADER_SIZE + 0x10]
                .copy_from_slice(&title_id.to_be_bytes());
            fst[FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE + 0x08
                ..FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE + 0x10]
                .copy_from_slice(&title_id.to_be_bytes());
            let entries_start = FST_HEADER_SIZE + (num_clusters as usize) * FST_CLUSTER_ENTRY_SIZE;
            let num_entries = 3u32;
            fst[entries_start..entries_start + 4].copy_from_slice(&0x0100_0000u32.to_be_bytes());
            fst[entries_start + 8..entries_start + 12].copy_from_slice(&num_entries.to_be_bytes());
            let name_off = entries_start + (num_entries as usize) * FST_FILE_ENTRY_SIZE;
            fst[name_off] = 0;
            fst[name_off + 1..name_off + 9].copy_from_slice(b"content\0");
            fst[name_off + 9..name_off + 17].copy_from_slice(b"foo.bin\0");
            let de = entries_start + FST_FILE_ENTRY_SIZE;
            fst[de..de + 4].copy_from_slice(&(0x0100_0000u32 | 1u32).to_be_bytes());
            fst[de + 8..de + 12].copy_from_slice(&num_entries.to_be_bytes());
            let fe = de + FST_FILE_ENTRY_SIZE;
            fst[fe..fe + 4].copy_from_slice(&9u32.to_be_bytes());
            fst[fe + 8..fe + 12].copy_from_slice(&(payload.len() as u32).to_be_bytes());
            fst[fe + 14..fe + 16].copy_from_slice(&1u16.to_be_bytes());

            let mut fst_enc = fst;
            encrypt(&title_key, &[0u8; 16], &mut fst_enc);
            std::fs::write(dir.join("00000000.app"), &fst_enc).unwrap();

            let mut payload_enc = payload.clone();
            let mut payload_iv = [0u8; 16];
            payload_iv[1] = 1;
            encrypt(&title_key, &payload_iv, &mut payload_enc);
            std::fs::write(dir.join("00000001.app"), &payload_enc).unwrap();

            payload
        }

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let update = dir.path().join("update");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&update).unwrap();
        let base_payload = build_minimal_nus(&base, 0x0005_0000_1010_2000, 0, [0x11u8; 16]);
        let update_payload = build_minimal_nus(&update, 0x0005_000E_1010_2000, 32, [0x22u8; 16]);
        let output = dir.path().join("bundle.wua");

        compress_titles(
            &[TitleInput::auto(&base), TitleInput::auto(&update)],
            &output,
            WupCompressOptions::default(),
            &NoProgress,
        )
        .unwrap();

        let bytes = std::fs::read(&output).unwrap();
        let reader =
            crate::nintendo::wup::zarchive_writer::tests::test_reader::TestReader::open(&bytes)
                .unwrap();
        assert_eq!(
            reader.extract_file("0005000010102000_v0/content/foo.bin"),
            base_payload
        );
        assert_eq!(
            reader.extract_file("0005000e10102000_v32/content/foo.bin"),
            update_payload
        );
    }

    #[test]
    fn compress_titles_rejects_empty_input() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out.wua");
        let err = compress_titles(&[], &output, WupCompressOptions::default(), &NoProgress);
        assert!(matches!(err, Err(WupError::InvalidPath(_))));
    }

    #[test]
    fn compress_titles_rejects_bad_zstd_level() {
        let dir = tempfile::tempdir().unwrap();
        let title = dir.path().join("title");
        std::fs::create_dir_all(&title).unwrap();
        make_minimal_loadiine(&title, "0005000E10102000", 32);
        let output = dir.path().join("out.wua");
        let err = compress_titles(
            &[TitleInput::auto(&title)],
            &output,
            WupCompressOptions { zstd_level: 99 },
            &NoProgress,
        );
        assert!(matches!(err, Err(WupError::InvalidCompressionLevel { .. })));
    }

    #[test]
    fn detect_disc_by_wud_extension() {
        let dir = tempfile::tempdir().unwrap();
        let wud = dir.path().join("game.wud");
        // Single sector of zeros is enough to make it a file; the
        // format detector doesn't need more.
        std::fs::write(&wud, [0u8; 0x8000]).unwrap();
        assert_eq!(detect_title_format(&wud).unwrap(), TitleInputFormat::Disc);
    }

    #[test]
    fn detect_disc_by_wux_magic_without_extension() {
        let dir = tempfile::tempdir().unwrap();
        let wux = dir.path().join("game.bin");
        let mut bytes = vec![0u8; 0x20];
        bytes[0..4].copy_from_slice(b"WUX0");
        bytes[4..8].copy_from_slice(&[0x2E, 0xD0, 0x99, 0x10]);
        std::fs::write(&wux, &bytes).unwrap();
        assert_eq!(detect_title_format(&wux).unwrap(), TitleInputFormat::Disc);
    }

    #[test]
    fn detect_rejects_unknown_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("random.bin");
        std::fs::write(&f, b"not a disc").unwrap();
        let err = detect_title_format(&f);
        assert!(matches!(err, Err(WupError::UnsupportedDiscFormat(_))));
    }

    #[test]
    fn title_input_disc_with_key_has_format_and_key() {
        let inp = TitleInput::disc_with_key(
            PathBuf::from("/disks/game.wud"),
            PathBuf::from("/keys/g.key"),
        );
        assert_eq!(inp.format, Some(TitleInputFormat::Disc));
        assert_eq!(inp.key_path, Some(PathBuf::from("/keys/g.key")));
    }

    #[test]
    fn derive_wua_path_replaces_extension() {
        assert_eq!(
            derive_wua_path(Path::new("/roms/game")),
            PathBuf::from("/roms/game.wua")
        );
        assert_eq!(
            derive_wua_path(Path::new("/roms/game.dir")),
            PathBuf::from("/roms/game.wua")
        );
    }
}
