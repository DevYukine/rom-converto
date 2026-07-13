use crate::dat::digest::TrackDigests;
use crate::dat::model::{DatFileGame, DatFileSummary, PlaymatchGameFile};
use crate::util::{CancelToken, FileDigests};
use std::borrow::Cow;
use std::collections::HashSet;

#[derive(Debug, Clone, Default)]
pub struct LocalHashIndex {
    sha256: HashSet<String>,
    sha1: HashSet<String>,
    md5: HashSet<String>,
    crc_size: HashSet<(String, u64)>,
}

impl LocalHashIndex {
    pub fn insert(&mut self, d: &FileDigests) {
        if let Some(h) = &d.sha256 {
            self.sha256.insert(norm(h));
        }
        if let Some(h) = &d.sha1 {
            self.sha1.insert(norm(h));
        }
        if let Some(h) = &d.md5 {
            self.md5.insert(norm(h));
        }
        if let Some(c) = &d.crc32 {
            self.crc_size.insert((norm_crc(c), d.size_bytes));
        }
    }

    pub fn insert_tracks(&mut self, t: &[TrackDigests]) {
        for track in t {
            self.insert(&track.digests);
        }
    }

    /// Membership by hash precedence SHA-256 > SHA-1 > MD5 > CRC32+size. A file is
    /// present when the strongest hash both sides carry matches; the CRC32 rung
    /// additionally requires equal fileSize.
    pub fn contains(&self, f: &PlaymatchGameFile) -> bool {
        if let Some(h) = &f.sha256 {
            return self.sha256.contains(&norm(h));
        }
        if let Some(h) = &f.sha1 {
            return self.sha1.contains(&norm(h));
        }
        if let Some(h) = &f.md5 {
            return self.md5.contains(&norm(h));
        }
        if let (Some(c), Some(size)) = (&f.crc, f.file_size_in_bytes) {
            return self.crc_size.contains(&(norm_crc(c), size));
        }
        false
    }
}

fn norm(h: &str) -> String {
    h.trim().to_ascii_lowercase()
}

fn norm_crc(c: &str) -> String {
    format!("{:0>8}", c.trim().to_ascii_lowercase())
}

#[derive(Debug, Clone)]
pub struct FixdatEntry {
    pub game_name: String,
    pub missing: Vec<PlaymatchGameFile>,
}

/// A game is included iff at least one of its files is absent locally; only the
/// missing files are listed.
pub fn diff_library(games: &[DatFileGame], index: &LocalHashIndex) -> Vec<FixdatEntry> {
    let mut entries = Vec::new();
    for game in games {
        let Some(files) = &game.files else {
            continue;
        };
        let missing: Vec<PlaymatchGameFile> = files
            .iter()
            .filter(|f| !index.contains(f))
            .cloned()
            .collect();
        if !missing.is_empty() {
            entries.push(FixdatEntry {
                game_name: game.name.clone(),
                missing,
            });
        }
    }
    entries
}

/// Escape the five XML special characters for both attribute values and text
/// nodes.
pub fn xml_escape(s: &str) -> Cow<'_, str> {
    if !s.contains(['&', '<', '>', '"', '\'']) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    Cow::Owned(out)
}

/// Emit a Logiqx fixdat of the missing ROMs and disc images.
pub fn write_fixdat_xml<W: std::io::Write>(
    w: &mut W,
    dat: &DatFileSummary,
    entries: &[FixdatEntry],
) -> std::io::Result<()> {
    write_fixdat_xml_cancellable(w, dat, entries, &CancelToken::new())
}

pub fn write_fixdat_xml_cancellable<W: std::io::Write>(
    w: &mut W,
    dat: &DatFileSummary,
    entries: &[FixdatEntry],
    cancel: &CancelToken,
) -> std::io::Result<()> {
    check_cancel(cancel)?;
    let title = format!("fixdat - {}", dat.name);
    writeln!(w, "<?xml version=\"1.0\"?>")?;
    writeln!(
        w,
        "<!DOCTYPE datafile PUBLIC \"-//Logiqx//DTD ROM Management Datafile//EN\" \"http://www.logiqx.com/Dats/datafile.dtd\">"
    )?;
    writeln!(w, "<datafile>")?;
    writeln!(w, "\t<header>")?;
    writeln!(w, "\t\t<name>{}</name>", xml_escape(&title))?;
    writeln!(w, "\t\t<description>{}</description>", xml_escape(&title))?;
    writeln!(
        w,
        "\t\t<version>{}</version>",
        xml_escape(&dat.current_version)
    )?;
    writeln!(w, "\t</header>")?;

    for entry in entries {
        check_cancel(cancel)?;
        writeln!(w, "\t<game name=\"{}\">", xml_escape(&entry.game_name))?;
        writeln!(
            w,
            "\t\t<description>{}</description>",
            xml_escape(&entry.game_name)
        )?;
        for f in &entry.missing {
            check_cancel(cancel)?;
            write!(w, "\t\t<rom name=\"{}\"", xml_escape(&f.file_name))?;
            if let Some(size) = f.file_size_in_bytes {
                write!(w, " size=\"{size}\"")?;
            }
            if let Some(crc) = &f.crc {
                write!(w, " crc=\"{}\"", xml_escape(crc))?;
            }
            if let Some(md5) = &f.md5 {
                write!(w, " md5=\"{}\"", xml_escape(md5))?;
            }
            if let Some(sha1) = &f.sha1 {
                write!(w, " sha1=\"{}\"", xml_escape(sha1))?;
            }
            if let Some(sha256) = &f.sha256 {
                write!(w, " sha256=\"{}\"", xml_escape(sha256))?;
            }
            writeln!(w, "/>")?;
        }
        writeln!(w, "\t</game>")?;
    }

    check_cancel(cancel)?;
    writeln!(w, "</datafile>")?;
    Ok(())
}

fn check_cancel(cancel: &CancelToken) -> std::io::Result<()> {
    if cancel.is_cancelled() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "cancelled",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::model::NamedRef;

    fn gf(
        name: &str,
        size: Option<u64>,
        crc: Option<&str>,
        md5: Option<&str>,
        sha1: Option<&str>,
        sha256: Option<&str>,
    ) -> PlaymatchGameFile {
        PlaymatchGameFile {
            id: "id".to_string(),
            game_id: "g".to_string(),
            file_name: name.to_string(),
            file_size_in_bytes: size,
            crc: crc.map(str::to_string),
            md5: md5.map(str::to_string),
            sha1: sha1.map(str::to_string),
            sha256: sha256.map(str::to_string),
            current_in_latest_dat: true,
            last_seen_dat_version: None,
        }
    }

    fn digests(
        crc: Option<&str>,
        sha1: Option<&str>,
        md5: Option<&str>,
        sha256: Option<&str>,
        size: u64,
    ) -> FileDigests {
        FileDigests {
            crc32: crc.map(str::to_string),
            sha1: sha1.map(str::to_string),
            md5: md5.map(str::to_string),
            sha256: sha256.map(str::to_string),
            size_bytes: size,
        }
    }

    #[test]
    fn xml_escape_all_five() {
        assert_eq!(
            xml_escape("a & b < c > d \" e ' f"),
            "a &amp; b &lt; c &gt; d &quot; e &apos; f"
        );
    }

    #[test]
    fn xml_escape_borrows_when_clean() {
        assert!(matches!(xml_escape("clean text"), Cow::Borrowed(_)));
    }

    #[test]
    fn index_precedence_sha256_first() {
        let mut index = LocalHashIndex::default();
        index.insert(&digests(None, None, None, Some("ABCDEF"), 100));
        // sha256 matches even though the remote also carries a wrong sha1.
        let f = gf("x", Some(100), None, None, Some("wrong"), Some("abcdef"));
        assert!(index.contains(&f));
        // sha256 present but wrong short-circuits weaker rungs.
        let f2 = gf("y", Some(100), None, None, Some("right"), Some("nope"));
        assert!(!index.contains(&f2));
    }

    #[test]
    fn index_crc_needs_size() {
        let mut index = LocalHashIndex::default();
        index.insert(&digests(Some("1234abcd"), None, None, None, 100));
        assert!(index.contains(&gf("x", Some(100), Some("1234abcd"), None, None, None)));
        assert!(!index.contains(&gf("x", Some(999), Some("1234abcd"), None, None, None)));
        // No hash overlap at all.
        assert!(!index.contains(&gf("x", Some(100), Some("ffffffff"), None, None, None)));
    }

    #[test]
    fn index_crc_padding() {
        let mut index = LocalHashIndex::default();
        index.insert(&digests(Some("abcd"), None, None, None, 100));
        assert!(index.contains(&gf("x", Some(100), Some("0000abcd"), None, None, None)));
    }

    #[test]
    fn diff_lists_only_missing_files() {
        let mut index = LocalHashIndex::default();
        index.insert(&digests(None, Some("aaaa"), None, None, 100));
        let game = DatFileGame {
            id: "g".to_string(),
            name: "Some Game".to_string(),
            clone_of: None,
            current_in_latest_dat: true,
            files: Some(vec![
                gf("present.bin", Some(100), None, None, Some("aaaa"), None),
                gf("missing.bin", Some(200), None, None, Some("bbbb"), None),
            ]),
        };
        let entries = diff_library(&[game], &index);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].missing.len(), 1);
        assert_eq!(entries[0].missing[0].file_name, "missing.bin");
    }

    #[test]
    fn diff_skips_fully_present_game() {
        let mut index = LocalHashIndex::default();
        index.insert(&digests(None, Some("aaaa"), None, None, 100));
        let game = DatFileGame {
            id: "g".to_string(),
            name: "Some Game".to_string(),
            clone_of: None,
            current_in_latest_dat: true,
            files: Some(vec![gf(
                "present.bin",
                Some(100),
                None,
                None,
                Some("aaaa"),
                None,
            )]),
        };
        assert!(diff_library(&[game], &index).is_empty());
    }

    #[test]
    fn write_fixdat_golden() {
        let dat = DatFileSummary {
            id: "d".to_string(),
            name: "Test <Dat> & Co".to_string(),
            signature_group: NamedRef {
                id: "s".to_string(),
                name: "Group".to_string(),
            },
            platform: NamedRef {
                id: "p".to_string(),
                name: "Plat".to_string(),
            },
            company: None,
            current_version: "2026-06-01".to_string(),
            latest_dat_file_import: None,
            subset: None,
            tags: Vec::new(),
        };
        let entries = vec![FixdatEntry {
            game_name: "Some Game (USA)".to_string(),
            missing: vec![gf(
                "Some Game (USA).bin",
                Some(1234),
                Some("1234abcd"),
                None,
                Some("aaaa"),
                None,
            )],
        }];

        let cancel = CancelToken::new();
        cancel.cancel();
        let err =
            write_fixdat_xml_cancellable(&mut Vec::new(), &dat, &entries, &cancel).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);

        let mut buf = Vec::new();
        write_fixdat_xml(&mut buf, &dat, &entries).unwrap();
        let out = String::from_utf8(buf).unwrap();

        let expected = concat!(
            "<?xml version=\"1.0\"?>\n",
            "<!DOCTYPE datafile PUBLIC \"-//Logiqx//DTD ROM Management Datafile//EN\" \"http://www.logiqx.com/Dats/datafile.dtd\">\n",
            "<datafile>\n",
            "\t<header>\n",
            "\t\t<name>fixdat - Test &lt;Dat&gt; &amp; Co</name>\n",
            "\t\t<description>fixdat - Test &lt;Dat&gt; &amp; Co</description>\n",
            "\t\t<version>2026-06-01</version>\n",
            "\t</header>\n",
            "\t<game name=\"Some Game (USA)\">\n",
            "\t\t<description>Some Game (USA)</description>\n",
            "\t\t<rom name=\"Some Game (USA).bin\" size=\"1234\" crc=\"1234abcd\" sha1=\"aaaa\"/>\n",
            "\t</game>\n",
            "</datafile>\n",
        );
        assert_eq!(out, expected);
    }
}
