//! Loadiine-style directory walker.
//!
//! A "loadiine" title directory is a Wii U title that's already been
//! decrypted into plain `meta/`, `code/`, and `content/` subfolders,
//! the layout Cemu stores as `HOST_FS`. These are the inputs the WUA
//! writer's fast path accepts with no crypto work: walking the
//! directory tree and streaming each file's bytes straight into the
//! archive under `<titleId>_v<version>/<relative/path>`.
//!
//! The walker skips anything outside of `meta/`, `code/`, and
//! `content/` to match what Cemu expects, dropping stray siblings
//! to keep archives lean. Every directory's
//! children are sorted by byte value to make the walk deterministic
//! across filesystems, so rebuilding the same loadiine dir always
//! produces the same block layout inside the archive.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::nintendo::wup::app_xml::AppXml;
use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::zarchive_writer::ArchiveSink;
use crate::util::ProgressReporter;

/// Top-level subdirectories a loadiine title is allowed to contain.
/// Anything else at the root is silently ignored by the walker.
const ALLOWED_TOP_LEVEL: &[&str] = &["meta", "code", "content"];

/// Parsed metadata for a loadiine-shaped title directory, as
/// returned by [`detect_loadiine_title`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadiineTitle {
    pub dir: PathBuf,
    pub title_id: u64,
    pub title_version: u32,
}

impl LoadiineTitle {
    /// The archive subfolder this title lives under, as required by
    /// Cemu: `<16-hex titleId>_v<decimal version>`.
    pub fn archive_folder(&self) -> String {
        format!("{:016x}_v{}", self.title_id, self.title_version)
    }
}

/// One file the loadiine walker will feed into the archive writer.
///
/// `relative_path` is the path inside the title folder (e.g.
/// `meta/meta.xml`) and is used to construct the in-archive path.
/// `absolute_path` is the host filesystem path the caller opens for
/// reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadiineFile {
    pub relative_path: String,
    pub absolute_path: PathBuf,
}

/// Detect whether `dir` is a loadiine-shaped Wii U title directory
/// and, if so, read its title id and version from `code/app.xml`.
/// Returns `None` if any of the minimum required files is missing
/// so the caller can fall through to NUS detection later.
///
/// A loadiine title must contain `meta/meta.xml`, `code/app.xml`,
/// and `code/cos.xml` (the triad Cemu's `ParseXmlInfo` requires).
pub fn detect_loadiine_title(dir: &Path) -> WupResult<Option<LoadiineTitle>> {
    let meta_xml = dir.join("meta").join("meta.xml");
    let app_xml = dir.join("code").join("app.xml");
    let cos_xml = dir.join("code").join("cos.xml");
    if !meta_xml.is_file() || !app_xml.is_file() || !cos_xml.is_file() {
        return Ok(None);
    }
    let parsed = AppXml::read_from_path(&app_xml)?;
    Ok(Some(LoadiineTitle {
        dir: dir.to_path_buf(),
        title_id: parsed.title_id,
        title_version: parsed.title_version,
    }))
}

/// Walk a loadiine title directory and return every file under
/// `meta/`, `code/`, and `content/` that belongs in the archive.
/// Results are sorted deterministically per directory by byte-value
/// lexicographic order, so the output is stable across filesystems
/// and hosts.
///
/// Anything at the title root that isn't one of the three allowed
/// subdirectories is silently skipped.
pub fn walk_loadiine_files(title_dir: &Path) -> WupResult<Vec<LoadiineFile>> {
    if !title_dir.is_dir() {
        return Err(WupError::UnrecognizedTitleDirectory(
            title_dir.to_path_buf(),
        ));
    }
    let mut out: Vec<LoadiineFile> = Vec::new();
    for top in ALLOWED_TOP_LEVEL {
        let sub = title_dir.join(top);
        if !sub.is_dir() {
            continue;
        }
        walk_subdir(&sub, top, &mut out)?;
    }
    // No cross-top-level sort: entries already come out in the order
    // the top-level loop visits them (meta, code, content) with each
    // subtree sorted locally, which is sufficient for deterministic
    // output. The ZArchive writer resorts by its own compare
    // function later anyway.
    Ok(out)
}

/// Sum the on-disk size of every file the loadiine streamer would
/// feed into the archive. Cheap pre-scan so the caller can seed the
/// progress bar with a real byte total before reads begin.
pub fn estimate_loadiine_uncompressed_bytes(title_dir: &Path) -> WupResult<u64> {
    let files = walk_loadiine_files(title_dir)?;
    let mut total: u64 = 0;
    for file in &files {
        total = total.saturating_add(std::fs::metadata(&file.absolute_path)?.len());
    }
    Ok(total)
}

/// Stream every file in a loadiine title directory into `sink`,
/// nested under `<title_id>_v<version>/`. Used by the top-level
/// [`crate::nintendo::wup::compress::compress_titles`] dispatcher
/// for the loadiine branch. Reads each file in 1 MiB chunks so
/// individual hundreds-of-MB files don't inflate peak memory.
pub fn compress_loadiine_title(
    title: &LoadiineTitle,
    sink: &mut dyn ArchiveSink,
    progress: &dyn ProgressReporter,
) -> WupResult<()> {
    const READ_CHUNK_SIZE: usize = 1024 * 1024;

    let archive_folder = title.archive_folder();
    let files = walk_loadiine_files(&title.dir)?;
    let mut buffer = vec![0u8; READ_CHUNK_SIZE];
    for file in &files {
        let archive_path = format!("{}/{}", archive_folder, file.relative_path);
        sink.start_new_file(&archive_path)?;
        let mut reader = std::io::BufReader::new(std::fs::File::open(&file.absolute_path)?);
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            sink.append_data(&buffer[..n])?;
            progress.inc(n as u64);
        }
    }
    Ok(())
}

fn walk_subdir(dir: &Path, rel_prefix: &str, out: &mut Vec<LoadiineFile>) -> WupResult<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    // Deterministic sort by byte-value path so the walk order is
    // reproducible regardless of what `read_dir` returns first.
    entries.sort();
    for entry in entries {
        let file_name = match entry.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => continue,
        };
        let child_rel = format!("{rel_prefix}/{file_name}");
        if entry.is_dir() {
            walk_subdir(&entry, &child_rel, out)?;
        } else if entry.is_file() {
            out.push(LoadiineFile {
                relative_path: child_rel,
                absolute_path: entry,
            });
        }
        // Symlinks and other entries are ignored; Wii U titles
        // never ship them.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const MINIMAL_APP_XML: &[u8] =
        b"<app><title_id>0005000E10102000</title_id><title_version>32</title_version></app>";

    /// Build the bare minimum of a loadiine title: the three required
    /// XML files. Caller adds any other files they want on top.
    fn make_minimal_title(root: &Path) {
        fs::create_dir_all(root.join("meta")).unwrap();
        fs::create_dir_all(root.join("code")).unwrap();
        fs::write(root.join("meta").join("meta.xml"), b"<menu/>").unwrap();
        fs::write(root.join("code").join("app.xml"), MINIMAL_APP_XML).unwrap();
        fs::write(root.join("code").join("cos.xml"), b"<cos/>").unwrap();
    }

    #[test]
    fn detect_returns_none_for_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = detect_loadiine_title(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn detect_returns_none_if_meta_xml_missing() {
        let dir = tempfile::tempdir().unwrap();
        make_minimal_title(dir.path());
        fs::remove_file(dir.path().join("meta").join("meta.xml")).unwrap();
        let result = detect_loadiine_title(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn detect_returns_title_from_app_xml() {
        let dir = tempfile::tempdir().unwrap();
        make_minimal_title(dir.path());
        let title = detect_loadiine_title(dir.path()).unwrap().unwrap();
        assert_eq!(title.title_id, 0x0005_000E_1010_2000);
        assert_eq!(title.title_version, 32);
        assert_eq!(title.dir, dir.path());
    }

    #[test]
    fn archive_folder_matches_cemu_format() {
        let title = LoadiineTitle {
            dir: PathBuf::from("/anywhere"),
            title_id: 0x0005_000E_1010_2000,
            title_version: 32,
        };
        assert_eq!(title.archive_folder(), "0005000e10102000_v32");
    }

    #[test]
    fn archive_folder_lowercases_hex() {
        let title = LoadiineTitle {
            dir: PathBuf::from("/"),
            title_id: 0x0005_0000_ABCD_EF00,
            title_version: 0,
        };
        assert_eq!(title.archive_folder(), "00050000abcdef00_v0");
    }

    #[test]
    fn walk_returns_triad_for_minimal_title() {
        let dir = tempfile::tempdir().unwrap();
        make_minimal_title(dir.path());
        let files = walk_loadiine_files(dir.path()).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert!(paths.contains(&"meta/meta.xml"));
        assert!(paths.contains(&"code/app.xml"));
        assert!(paths.contains(&"code/cos.xml"));
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn walk_includes_content_dir_when_present() {
        let dir = tempfile::tempdir().unwrap();
        make_minimal_title(dir.path());
        fs::create_dir_all(dir.path().join("content").join("sub")).unwrap();
        fs::write(dir.path().join("content").join("a.bin"), b"aaaa").unwrap();
        fs::write(
            dir.path().join("content").join("sub").join("b.bin"),
            b"bbbb",
        )
        .unwrap();
        let files = walk_loadiine_files(dir.path()).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.relative_path.clone()).collect();
        assert!(paths.contains(&"content/a.bin".to_string()));
        assert!(paths.contains(&"content/sub/b.bin".to_string()));
    }

    #[test]
    fn walk_skips_unknown_top_level_junk() {
        let dir = tempfile::tempdir().unwrap();
        make_minimal_title(dir.path());
        // Stray files and folders at the root must be silently
        // dropped so the archive stays clean.
        fs::write(dir.path().join("README.txt"), b"junk").unwrap();
        fs::create_dir_all(dir.path().join("cache")).unwrap();
        fs::write(dir.path().join("cache").join("garbage"), b"junk").unwrap();
        let files = walk_loadiine_files(dir.path()).unwrap();
        for f in &files {
            assert!(
                f.relative_path.starts_with("meta/")
                    || f.relative_path.starts_with("code/")
                    || f.relative_path.starts_with("content/"),
                "walker must not yield top-level junk: {}",
                f.relative_path
            );
        }
    }

    #[test]
    fn walk_sort_is_deterministic() {
        // Create a bunch of files in different sub-subdirectories
        // twice and compare: the walk order must be identical
        // across independent `read_dir` invocations.
        fn build(dir: &Path) -> Vec<String> {
            make_minimal_title(dir);
            let content_sub = dir.join("content").join("sub");
            fs::create_dir_all(&content_sub).unwrap();
            // Insert in an intentionally scrambled order so we
            // catch any reliance on filesystem iteration order.
            for name in ["z.bin", "a.bin", "m.bin", "d.bin"] {
                fs::write(content_sub.join(name), b"data").unwrap();
            }
            walk_loadiine_files(dir)
                .unwrap()
                .into_iter()
                .map(|f| f.relative_path)
                .collect()
        }
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let paths_a = build(dir_a.path());
        let paths_b = build(dir_b.path());
        assert_eq!(paths_a, paths_b, "walk order must be deterministic");

        // The content/sub files in particular must be sorted
        // ascending by byte value.
        let sub_paths: Vec<_> = paths_a
            .iter()
            .filter(|p| p.starts_with("content/sub/"))
            .collect();
        assert_eq!(
            sub_paths,
            vec![
                "content/sub/a.bin",
                "content/sub/d.bin",
                "content/sub/m.bin",
                "content/sub/z.bin",
            ]
        );
    }

    #[test]
    fn estimate_uncompressed_bytes_sums_all_walked_files() {
        let dir = tempfile::tempdir().unwrap();
        make_minimal_title(dir.path());
        // Three XML files of known sizes plus a content payload.
        fs::create_dir_all(dir.path().join("content")).unwrap();
        fs::write(dir.path().join("content").join("data.bin"), vec![0u8; 8192]).unwrap();
        let total = estimate_loadiine_uncompressed_bytes(dir.path()).unwrap();
        // meta.xml (7) + app.xml (MINIMAL_APP_XML.len()) + cos.xml (6) + data.bin (8192)
        let expected = 7u64 + MINIMAL_APP_XML.len() as u64 + 6u64 + 8192u64;
        assert_eq!(total, expected);
    }

    #[test]
    fn walk_rejects_non_directory_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not_a_dir");
        fs::write(&path, b"file").unwrap();
        let err = walk_loadiine_files(&path);
        assert!(matches!(err, Err(WupError::UnrecognizedTitleDirectory(_))));
    }
}
