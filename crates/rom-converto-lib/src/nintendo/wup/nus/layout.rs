//! Ticket / TMD / content file discovery in a NUS-style directory.
//!
//! Handles two layouts:
//!
//! - **Canonical**: `title.tmd` + `title.tik` + `{id:08x}.app`.
//! - **Community**: `tmd.<version>` + optional `cetk.<version>` +
//!   extensionless `{id:08x}` content files plus `{id:08x}.h3` hash
//!   sidecars.
//!
//! When a directory has several `tmd.<N>` files, the highest-numbered
//! version wins. The matching `cetk.<N>` is preferred as the ticket;
//! otherwise the highest cetk, otherwise a derived ticket via
//! [`title_key_derive`][super::super::title_key_derive].

use std::path::{Path, PathBuf};

use crate::nintendo::wup::error::{WupError, WupResult};

/// Where the loader pulls the ticket (and title key) from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TicketSource {
    /// Ticket file on disk to parse.
    OnDisk(PathBuf),
    /// Derive the title key from the title id once the TMD has been
    /// read.
    Derive,
}

/// Maps a TMD content id to its on-disk filename. Tries the
/// `{id:08x}.app` form first and falls back to the extensionless
/// `{id:08x}` form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentFilenameResolver {
    root: PathBuf,
}

impl ContentFilenameResolver {
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self { root: root.into() }
    }

    /// Return the first existing candidate path for `content_id`, or
    /// `None` if neither form is on disk.
    pub fn resolve(&self, content_id: u32) -> Option<PathBuf> {
        let canonical = self.root.join(format!("{content_id:08x}.app"));
        if canonical.is_file() {
            return Some(canonical);
        }
        let extensionless = self.root.join(format!("{content_id:08x}"));
        if extensionless.is_file() {
            return Some(extensionless);
        }
        None
    }
}

/// Full resolution of one title directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NusLayout {
    pub tmd_path: PathBuf,
    pub ticket_source: TicketSource,
    pub content: ContentFilenameResolver,
}

impl NusLayout {
    /// Inspect `dir` and produce a layout description, or fail with
    /// [`WupError::UnrecognizedTitleDirectory`] if no TMD is found.
    pub fn discover(dir: &Path) -> WupResult<Self> {
        if !dir.is_dir() {
            return Err(WupError::UnrecognizedTitleDirectory(dir.to_path_buf()));
        }

        let entries: Vec<(PathBuf, String)> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                (e.path(), name)
            })
            .collect();

        // Pick the TMD. Canonical wins when present; otherwise take
        // the highest `tmd.<N>` by the decimal suffix.
        let mut canonical_tmd: Option<PathBuf> = None;
        let mut numbered_tmds: Vec<(u32, PathBuf)> = Vec::new();
        for (path, name) in &entries {
            if name == "title.tmd" {
                canonical_tmd = Some(path.clone());
            } else if let Some(rest) = name.strip_prefix("tmd.")
                && let Ok(version) = rest.parse::<u32>()
            {
                numbered_tmds.push((version, path.clone()));
            }
        }
        let (chosen_tmd_version, tmd_path) = if let Some(p) = canonical_tmd {
            // Canonical has no version hint; treat it as "unknown".
            (None, p)
        } else if !numbered_tmds.is_empty() {
            numbered_tmds.sort_by_key(|&(v, _)| v);
            let (v, p) = numbered_tmds.last().unwrap().clone();
            (Some(v), p)
        } else {
            return Err(WupError::UnrecognizedTitleDirectory(dir.to_path_buf()));
        };

        // Pick the ticket. Canonical `title.tik` wins; else try to
        // match the chosen TMD's version among `cetk.<N>` files;
        // else take the highest cetk; else fall back to derivation.
        let mut canonical_tik: Option<PathBuf> = None;
        let mut numbered_ceteks: Vec<(u32, PathBuf)> = Vec::new();
        for (path, name) in &entries {
            if name == "title.tik" {
                canonical_tik = Some(path.clone());
            } else if let Some(rest) = name.strip_prefix("cetk.")
                && let Ok(version) = rest.parse::<u32>()
            {
                numbered_ceteks.push((version, path.clone()));
            }
        }
        let ticket_source = if let Some(p) = canonical_tik {
            TicketSource::OnDisk(p)
        } else if !numbered_ceteks.is_empty() {
            numbered_ceteks.sort_by_key(|&(v, _)| v);
            let matched = chosen_tmd_version
                .and_then(|v| numbered_ceteks.iter().find(|(cv, _)| *cv == v).cloned());
            let chosen = matched.unwrap_or_else(|| numbered_ceteks.last().unwrap().clone());
            TicketSource::OnDisk(chosen.1)
        } else {
            TicketSource::Derive
        };

        Ok(Self {
            tmd_path,
            ticket_source,
            content: ContentFilenameResolver::new(dir),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    #[test]
    fn canonical_layout_wins_when_present() {
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "title.tmd");
        touch(dir.path(), "title.tik");
        touch(dir.path(), "00000000.app");
        let layout = NusLayout::discover(dir.path()).unwrap();
        assert_eq!(layout.tmd_path, dir.path().join("title.tmd"));
        assert_eq!(
            layout.ticket_source,
            TicketSource::OnDisk(dir.path().join("title.tik"))
        );
        assert_eq!(
            layout.content.resolve(0),
            Some(dir.path().join("00000000.app"))
        );
    }

    #[test]
    fn no_intro_with_cetk_matches_highest_version() {
        let dir = TempDir::new().unwrap();
        for v in [32u32, 48, 64, 80] {
            touch(dir.path(), &format!("tmd.{v}"));
        }
        touch(dir.path(), "cetk.80");
        touch(dir.path(), "00000000");
        touch(dir.path(), "00000000.h3");
        let layout = NusLayout::discover(dir.path()).unwrap();
        assert_eq!(layout.tmd_path, dir.path().join("tmd.80"));
        assert_eq!(
            layout.ticket_source,
            TicketSource::OnDisk(dir.path().join("cetk.80"))
        );
        assert_eq!(layout.content.resolve(0), Some(dir.path().join("00000000")));
    }

    #[test]
    fn no_intro_without_cetk_uses_derive() {
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "tmd.0");
        touch(dir.path(), "00000000");
        let layout = NusLayout::discover(dir.path()).unwrap();
        assert_eq!(layout.tmd_path, dir.path().join("tmd.0"));
        assert_eq!(layout.ticket_source, TicketSource::Derive);
    }

    #[test]
    fn cetk_without_version_match_falls_back_to_highest() {
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "tmd.32");
        touch(dir.path(), "cetk.64"); // weird but possible
        touch(dir.path(), "00000000");
        let layout = NusLayout::discover(dir.path()).unwrap();
        assert_eq!(layout.tmd_path, dir.path().join("tmd.32"));
        assert_eq!(
            layout.ticket_source,
            TicketSource::OnDisk(dir.path().join("cetk.64"))
        );
    }

    #[test]
    fn empty_or_junk_directory_is_rejected() {
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "random.bin");
        let err = NusLayout::discover(dir.path());
        assert!(matches!(err, Err(WupError::UnrecognizedTitleDirectory(_))));
    }

    #[test]
    fn h3_sidecar_is_not_mistaken_for_content() {
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "tmd.0");
        touch(dir.path(), "00000000.h3");
        // Resolver should refuse to match an `.h3` file for content
        // id 0 because no matching `00000000` or `00000000.app`
        // exists.
        let layout = NusLayout::discover(dir.path()).unwrap();
        assert_eq!(layout.content.resolve(0), None);
    }

    #[test]
    fn canonical_app_preferred_over_extensionless_when_both_present() {
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "tmd.0");
        touch(dir.path(), "00000001.app");
        touch(dir.path(), "00000001");
        let layout = NusLayout::discover(dir.path()).unwrap();
        assert_eq!(
            layout.content.resolve(1),
            Some(dir.path().join("00000001.app"))
        );
    }

    #[test]
    fn non_numeric_tmd_suffix_is_ignored() {
        let dir = TempDir::new().unwrap();
        touch(dir.path(), "tmd.cert");
        touch(dir.path(), "tmd.bak");
        touch(dir.path(), "00000000");
        let err = NusLayout::discover(dir.path());
        assert!(matches!(err, Err(WupError::UnrecognizedTitleDirectory(_))));
    }

    #[test]
    fn non_directory_input_is_rejected() {
        let dir = TempDir::new().unwrap();
        let f = dir.path().join("file.bin");
        std::fs::write(&f, b"x").unwrap();
        let err = NusLayout::discover(&f);
        assert!(matches!(err, Err(WupError::UnrecognizedTitleDirectory(_))));
    }
}
