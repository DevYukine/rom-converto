//! Minimal `code/app.xml` parser.
//!
//! Wii U loadiine titles store a small `app.xml` describing the
//! title: we only need two fields from it, `<title_id>` (64-bit hex)
//! and `<title_version>` (unsigned decimal). Rather than pull in a
//! full XML crate, we do a targeted tag extraction that matches the
//! machine-generated shape Cemu and every Wii U toolchain produces,
//! e.g. `<title_id type="hexBinary" length="8">0005000E10102000</title_id>`.

use crate::nintendo::wup::error::{WupError, WupResult};

/// Parsed subset of `code/app.xml`. Other fields are ignored in v1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppXml {
    pub title_id: u64,
    pub title_version: u32,
}

impl AppXml {
    /// Parse an in-memory `app.xml` byte buffer.
    pub fn from_bytes(xml: &[u8], source: &std::path::Path) -> WupResult<Self> {
        let text =
            std::str::from_utf8(xml).map_err(|_| WupError::InvalidAppXml(source.to_path_buf()))?;
        parse(text, source)
    }

    /// Parse from a filesystem path. The provided path is also the
    /// path echoed in any [`WupError::InvalidAppXml`] emitted.
    pub fn read_from_path(path: &std::path::Path) -> WupResult<Self> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes, path)
    }
}

fn parse(text: &str, source: &std::path::Path) -> WupResult<AppXml> {
    let title_id_str = extract_tag(text, "title_id")
        .ok_or_else(|| WupError::InvalidAppXml(source.to_path_buf()))?;
    let title_version_str = extract_tag(text, "title_version")
        .ok_or_else(|| WupError::InvalidAppXml(source.to_path_buf()))?;

    let title_id = u64::from_str_radix(title_id_str.trim(), 16)
        .map_err(|_| WupError::InvalidAppXml(source.to_path_buf()))?;
    let title_version = title_version_str
        .trim()
        .parse::<u32>()
        .map_err(|_| WupError::InvalidAppXml(source.to_path_buf()))?;

    Ok(AppXml {
        title_id,
        title_version,
    })
}

/// Find the text content of the first `<tag>...</tag>` in `xml`.
/// Handles attributes on the opening tag (e.g. `<title_id type="..." length="8">`)
/// and rejects near-miss matches like `<title_id_foo>`. Whitespace
/// around the value is preserved; callers trim as needed.
fn extract_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open_marker = format!("<{tag}");
    let close_marker = format!("</{tag}>");
    let mut search_from = 0usize;
    while let Some(found) = xml[search_from..].find(&open_marker) {
        let tag_pos = search_from + found;
        let after_marker = tag_pos + open_marker.len();
        // Reject `<title_id_foo>` etc. The next byte after the marker
        // must either close the tag (`>`) or start an attribute list
        // (whitespace).
        let next = xml.as_bytes().get(after_marker)?;
        if *next != b'>' && !next.is_ascii_whitespace() {
            search_from = tag_pos + 1;
            continue;
        }
        // Skip to the end of the opening tag (the first `>`).
        let gt_rel = xml[after_marker..].find('>')?;
        let content_start = after_marker + gt_rel + 1;
        let close_rel = xml[content_start..].find(&close_marker)?;
        let content_end = content_start + close_rel;
        return Some(&xml[content_start..content_end]);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_path() -> PathBuf {
        PathBuf::from("/fake/code/app.xml")
    }

    #[test]
    fn parses_typical_app_xml() {
        let xml = r#"<?xml version="1.0" encoding="utf-8" ?>
<app type="complex" access="full">
  <version type="unsignedInt" length="4">16</version>
  <os_version type="hexBinary" length="8">000500101000400A</os_version>
  <title_id type="hexBinary" length="8">0005000E10102000</title_id>
  <title_version type="unsignedInt" length="4">32</title_version>
  <sdk_version type="unsignedInt" length="4">21201</sdk_version>
</app>
"#;
        let parsed = AppXml::from_bytes(xml.as_bytes(), &fake_path()).unwrap();
        assert_eq!(parsed.title_id, 0x0005_000E_1010_2000);
        assert_eq!(parsed.title_version, 32);
    }

    #[test]
    fn parses_lowercase_hex() {
        let xml = r#"<app>
  <title_id type="hexBinary" length="8">000500001010a200</title_id>
  <title_version type="unsignedInt" length="4">0</title_version>
</app>
"#;
        let parsed = AppXml::from_bytes(xml.as_bytes(), &fake_path()).unwrap();
        assert_eq!(parsed.title_id, 0x0005_0000_1010_A200);
        assert_eq!(parsed.title_version, 0);
    }

    #[test]
    fn parses_without_attributes() {
        let xml = r#"<app>
<title_id>0005000012345678</title_id>
<title_version>7</title_version>
</app>
"#;
        let parsed = AppXml::from_bytes(xml.as_bytes(), &fake_path()).unwrap();
        assert_eq!(parsed.title_id, 0x0005_0000_1234_5678);
        assert_eq!(parsed.title_version, 7);
    }

    #[test]
    fn trims_whitespace_around_values() {
        let xml = "<app><title_id>  0005000012345678  </title_id><title_version> 42 </title_version></app>";
        let parsed = AppXml::from_bytes(xml.as_bytes(), &fake_path()).unwrap();
        assert_eq!(parsed.title_id, 0x0005_0000_1234_5678);
        assert_eq!(parsed.title_version, 42);
    }

    #[test]
    fn rejects_missing_title_id() {
        let xml = r#"<app><title_version>1</title_version></app>"#;
        let err = AppXml::from_bytes(xml.as_bytes(), &fake_path());
        assert!(matches!(err, Err(WupError::InvalidAppXml(_))));
    }

    #[test]
    fn rejects_missing_title_version() {
        let xml = r#"<app><title_id>0005000012345678</title_id></app>"#;
        let err = AppXml::from_bytes(xml.as_bytes(), &fake_path());
        assert!(matches!(err, Err(WupError::InvalidAppXml(_))));
    }

    #[test]
    fn rejects_malformed_hex_title_id() {
        let xml = r#"<app><title_id>NOT_HEX_DATA</title_id><title_version>1</title_version></app>"#;
        let err = AppXml::from_bytes(xml.as_bytes(), &fake_path());
        assert!(matches!(err, Err(WupError::InvalidAppXml(_))));
    }

    #[test]
    fn rejects_non_numeric_title_version() {
        let xml =
            r#"<app><title_id>0005000012345678</title_id><title_version>one</title_version></app>"#;
        let err = AppXml::from_bytes(xml.as_bytes(), &fake_path());
        assert!(matches!(err, Err(WupError::InvalidAppXml(_))));
    }

    #[test]
    fn rejects_non_utf8() {
        let bytes = [0xFFu8, 0xFE, 0xFD];
        let err = AppXml::from_bytes(&bytes, &fake_path());
        assert!(matches!(err, Err(WupError::InvalidAppXml(_))));
    }

    #[test]
    fn extract_tag_ignores_similar_prefix() {
        // `<title_id_something>` should not match `title_id`.
        let xml = r#"<title_id_foo>skip</title_id_foo><title_id>0005000012345678</title_id><title_version>0</title_version>"#;
        let parsed = AppXml::from_bytes(xml.as_bytes(), &fake_path()).unwrap();
        assert_eq!(parsed.title_id, 0x0005_0000_1234_5678);
    }

    #[test]
    fn reads_from_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.xml");
        std::fs::write(
            &path,
            b"<app><title_id>000500001AABBCC0</title_id><title_version>5</title_version></app>",
        )
        .unwrap();
        let parsed = AppXml::read_from_path(&path).unwrap();
        assert_eq!(parsed.title_id, 0x0005_0000_1AAB_BCC0);
        assert_eq!(parsed.title_version, 5);
    }
}
