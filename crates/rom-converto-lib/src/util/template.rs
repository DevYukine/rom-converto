//! Output-path templating from decoded ROM metadata.
//!
//! Resolves a user template string such as `{console}/{title}.{ext}` against
//! the metadata rom-converto already extracts, producing a sanitized relative
//! path. No external DAT is consulted; every token comes from the in-tool
//! [`InfoResult`]. Missing metadata degrades to the input basename so a run
//! never fails just because a token could not be resolved.

use crate::info::InfoResult;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

const MAX_COMPONENT_BYTES: usize = 200;

const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub struct TemplateTokens {
    pub title: Option<String>,
    pub title_id: Option<String>,
    pub region: Option<String>,
    pub console: Option<String>,
    pub serial: Option<String>,
    pub ext: String,
    pub basename: String,
}

impl TemplateTokens {
    pub fn new(info: Option<&InfoResult>, input: &Path, output_ext: &str) -> Self {
        let basename = input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output")
            .to_string();
        let ext = output_ext.trim_start_matches('.').to_string();

        let mut tokens = Self {
            title: None,
            title_id: None,
            region: None,
            console: None,
            serial: None,
            ext,
            basename,
        };

        let Some(info) = info else {
            return tokens;
        };

        match info {
            InfoResult::Ctr(c) => {
                tokens.title = ctr_title(c);
                tokens.title_id = non_empty(c.title_id.clone());
                tokens.region = c
                    .smdh
                    .as_ref()
                    .and_then(|s| s.region_names.first().cloned())
                    .and_then(non_empty);
                tokens.console = Some("3DS".to_string());
                tokens.serial = non_empty(c.product_code.clone());
            }
            InfoResult::Dol(d) => {
                tokens.title = d
                    .banner
                    .as_ref()
                    .and_then(|b| english_title(&b.titles, |t| (&t.language, &t.short_game_name)))
                    .or_else(|| non_empty(d.game_name.clone()));
                tokens.title_id = non_empty(d.game_id.clone());
                tokens.region = non_empty(d.region.clone());
                tokens.console = Some("GameCube".to_string());
                tokens.serial = non_empty(d.game_id.clone());
            }
            InfoResult::Rvl(r) => {
                tokens.title = r
                    .imet_names
                    .as_ref()
                    .and_then(|m| m.primary())
                    .map(str::to_string)
                    .and_then(non_empty)
                    .or_else(|| non_empty(r.game_name.clone()));
                tokens.title_id = r
                    .tmd
                    .as_ref()
                    .map(|t| format!("{:016X}", t.title_id))
                    .or_else(|| non_empty(r.game_id.clone()));
                tokens.region = non_empty(r.region.clone());
                tokens.console = Some("Wii".to_string());
                tokens.serial = non_empty(r.game_id.clone());
            }
            InfoResult::Wup(w) => {
                tokens.title = w
                    .meta
                    .as_ref()
                    .and_then(|m| m.long_names.primary())
                    .map(str::to_string)
                    .and_then(non_empty);
                tokens.title_id = non_empty(w.title_id_hex.clone());
                tokens.region = w
                    .meta
                    .as_ref()
                    .map(|m| m.region_names.join(", "))
                    .and_then(non_empty);
                tokens.console = Some("WiiU".to_string());
                tokens.serial = w.meta.as_ref().and_then(|m| m.product_code.clone());
            }
            InfoResult::Nx(n) => {
                tokens.title = n
                    .full
                    .as_ref()
                    .and_then(|f| f.control.as_ref())
                    .and_then(|c| nx_title(&c.titles));
                tokens.title_id = n
                    .full
                    .as_ref()
                    .map(|f| format!("{:016X}", f.application_title_id));
                tokens.console = Some("Switch".to_string());
            }
            InfoResult::Chd(_) => {
                tokens.console = Some("CHD".to_string());
            }
            InfoResult::Cso(_) => {
                tokens.console = Some("CSO".to_string());
            }
        }

        tokens.title = tokens.title.and_then(|t| non_empty(t.trim().to_string()));
        tokens
    }
}

fn non_empty(s: String) -> Option<String> {
    if s.trim().is_empty() { None } else { Some(s) }
}

fn english_title<T, F>(titles: &[T], pick: F) -> Option<String>
where
    F: Fn(&T) -> (&String, &String),
{
    titles
        .iter()
        .find(|t| pick(t).0 == "English")
        .map(|t| pick(t).1.clone())
        .or_else(|| titles.first().map(|t| pick(t).1.clone()))
        .and_then(non_empty)
}

fn ctr_title(c: &crate::info::CtrInfo) -> Option<String> {
    let smdh = c.smdh.as_ref()?;
    smdh.titles
        .iter()
        .find(|t| t.language == "English")
        .map(|t| t.short_description.clone())
        .or_else(|| smdh.titles.first().map(|t| t.short_description.clone()))
        .and_then(non_empty)
}

fn nx_title(titles: &[crate::nintendo::nx::info::NxNacpTitle]) -> Option<String> {
    titles
        .iter()
        .find(|t| {
            matches!(
                t.language.as_str(),
                "AmericanEnglish" | "BritishEnglish" | "English"
            )
        })
        .map(|t| t.name.clone())
        .or_else(|| titles.first().map(|t| t.name.clone()))
        .and_then(non_empty)
}

pub fn apply_template(template: &str, tokens: &TemplateTokens) -> Result<PathBuf> {
    if template.starts_with('/') || template.starts_with('\\') || has_drive_prefix(template) {
        bail!("output template must resolve to a relative path without parent traversal");
    }

    let substituted = substitute(template, tokens);

    let mut out = PathBuf::new();
    for raw in substituted.split(['/', '\\']) {
        let trimmed = raw.trim();
        if trimmed == ".." {
            bail!("output template must resolve to a relative path without parent traversal");
        }
        if trimmed.is_empty() || trimmed == "." {
            continue;
        }
        let component = sanitize_component(raw);
        if component.is_empty() {
            continue;
        }
        out.push(component);
    }

    if out.as_os_str().is_empty() {
        bail!("output template resolved to an empty path");
    }

    Ok(out)
}

fn has_drive_prefix(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn substitute(template: &str, tokens: &TemplateTokens) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match resolve_token(name, tokens) {
                    Some(value) => out.push_str(&value),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn resolve_token(name: &str, tokens: &TemplateTokens) -> Option<String> {
    let value = match name {
        "title" => tokens
            .title
            .clone()
            .unwrap_or_else(|| tokens.basename.clone()),
        "titleId" => tokens
            .title_id
            .clone()
            .unwrap_or_else(|| tokens.basename.clone()),
        "serial" => tokens
            .serial
            .clone()
            .unwrap_or_else(|| tokens.basename.clone()),
        "region" => tokens.region.clone().unwrap_or_default(),
        "console" => tokens.console.clone().unwrap_or_default(),
        "ext" => tokens.ext.clone(),
        "basename" => tokens.basename.clone(),
        _ => return None,
    };
    Some(neutralize_separators(&value))
}

fn neutralize_separators(s: &str) -> String {
    s.chars()
        .map(|c| if c == '/' || c == '\\' { '_' } else { c })
        .collect()
}

fn sanitize_component(s: &str) -> String {
    let mut cleaned = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() {
            continue;
        }
        match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => cleaned.push('_'),
            _ => cleaned.push(c),
        }
    }

    let cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
    let mut cleaned = truncate_bytes(&cleaned, MAX_COMPONENT_BYTES);

    let stem = cleaned.split('.').next().unwrap_or(&cleaned);
    if WINDOWS_RESERVED.contains(&stem.to_ascii_uppercase().as_str()) {
        cleaned.push('_');
    }

    cleaned
}

fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::info::{CtrInfo, DolInfo, InfoResult, NxInfo};
    use crate::nintendo::ctr::info::{CtrSmdhInfo, CtrSmdhTitle};
    use crate::nintendo::nx::info::{NxControl, NxFullInfo, NxNacpTitle};

    fn tokens(title: Option<&str>) -> TemplateTokens {
        TemplateTokens {
            title: title.map(str::to_string),
            title_id: Some("ABCD".to_string()),
            region: Some("USA".to_string()),
            console: Some("Wii".to_string()),
            serial: Some("RMCE01".to_string()),
            ext: "rvz".to_string(),
            basename: "game01".to_string(),
        }
    }

    #[test]
    fn substitutes_all_present_tokens() {
        let t = tokens(Some("Mario"));
        let p = apply_template("{console}/{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("Wii/Mario.rvz"));
    }

    #[test]
    fn missing_title_falls_back_to_basename() {
        let t = tokens(None);
        let p = apply_template("{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("game01.rvz"));
    }

    #[test]
    fn missing_region_collapses_component() {
        let mut t = tokens(Some("Mario"));
        t.region = None;
        let p = apply_template("{region}/{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("Mario.rvz"));
    }

    #[test]
    fn ext_uses_output_extension() {
        let t = TemplateTokens::new(None, Path::new("game.iso"), "rvz");
        let p = apply_template("{basename}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("game.rvz"));
    }

    #[test]
    fn basename_is_input_stem() {
        let t = TemplateTokens::new(None, Path::new("/roms/Super Game.iso"), "rvz");
        assert_eq!(t.basename, "Super Game");
    }

    #[test]
    fn token_internal_separator_does_not_split() {
        let mut t = tokens(Some("a/b"));
        t.title = Some("a/b".to_string());
        let p = apply_template("{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("a_b.rvz"));
    }

    #[test]
    fn illegal_chars_are_replaced() {
        let mut t = tokens(None);
        t.title = Some("a<b>c:d\"e|f?g*h".to_string());
        let p = apply_template("{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("a_b_c_d_e_f_g_h.rvz"));
    }

    #[test]
    fn windows_reserved_name_suffixed() {
        let mut t = tokens(None);
        t.title = Some("CON".to_string());
        let p = apply_template("{title}", &t).unwrap();
        assert_eq!(p, PathBuf::from("CON_"));
    }

    #[test]
    fn over_length_component_truncated() {
        let mut t = tokens(None);
        t.title = Some("a".repeat(500));
        let p = apply_template("{title}", &t).unwrap();
        let comp = p.to_str().unwrap();
        assert!(comp.len() <= MAX_COMPONENT_BYTES);
        assert!(std::str::from_utf8(comp.as_bytes()).is_ok());
    }

    #[test]
    fn parent_traversal_rejected() {
        let t = tokens(Some("Mario"));
        assert!(apply_template("../{title}.{ext}", &t).is_err());
    }

    #[test]
    fn absolute_path_rejected() {
        let t = tokens(Some("Mario"));
        assert!(apply_template("/etc/passwd.{ext}", &t).is_err());
    }

    #[test]
    fn drive_prefix_rejected() {
        let t = tokens(Some("Mario"));
        assert!(apply_template("C:\\windows\\{title}", &t).is_err());
    }

    #[test]
    fn unicode_title_preserved() {
        let mut t = tokens(None);
        t.title = Some("スーパーマリオ".to_string());
        let p = apply_template("{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("スーパーマリオ.rvz"));
    }

    #[test]
    fn control_chars_stripped() {
        let mut t = tokens(None);
        t.title = Some("a\u{7}b".to_string());
        let p = apply_template("{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("ab.rvz"));
    }

    #[test]
    fn unknown_token_left_literal() {
        let t = tokens(Some("Mario"));
        let p = apply_template("{bogus}-{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("{bogus}-Mario.rvz"));
    }

    #[test]
    fn nx_without_keys_falls_back() {
        let info = InfoResult::Nx(NxInfo {
            full: None,
            ..Default::default()
        });
        let t = TemplateTokens::new(Some(&info), Path::new("game.nsp"), "nsz");
        assert!(t.title.is_none());
        assert!(t.title_id.is_none());
        assert_eq!(t.console.as_deref(), Some("Switch"));
        let p = apply_template("{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("game.nsz"));
    }

    #[test]
    fn nx_with_control_prefers_english() {
        let info = InfoResult::Nx(NxInfo {
            full: Some(NxFullInfo {
                application_title_id: 0x0100000000010000,
                control: Some(NxControl {
                    titles: vec![
                        NxNacpTitle {
                            language: "Japanese".to_string(),
                            name: "マリオ".to_string(),
                            publisher: "N".to_string(),
                        },
                        NxNacpTitle {
                            language: "AmericanEnglish".to_string(),
                            name: "Mario".to_string(),
                            publisher: "N".to_string(),
                        },
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        });
        let t = TemplateTokens::new(Some(&info), Path::new("game.nsp"), "nsz");
        assert_eq!(t.title.as_deref(), Some("Mario"));
        assert_eq!(t.title_id.as_deref(), Some("0100000000010000"));
    }

    #[test]
    fn ctr_prefers_english_smdh_title() {
        let info = InfoResult::Ctr(CtrInfo {
            title_id: "0004000000030600".to_string(),
            product_code: "CTR-P-AXXE".to_string(),
            smdh: Some(CtrSmdhInfo {
                titles: vec![
                    CtrSmdhTitle {
                        language: "Japanese".to_string(),
                        short_description: "ジャパン".to_string(),
                        long_description: String::new(),
                        publisher: String::new(),
                    },
                    CtrSmdhTitle {
                        language: "English".to_string(),
                        short_description: "Super Mario".to_string(),
                        long_description: String::new(),
                        publisher: String::new(),
                    },
                ],
                region_names: vec!["USA".to_string()],
                ..Default::default()
            }),
            ..Default::default()
        });
        let t = TemplateTokens::new(Some(&info), Path::new("game.cia"), "z3ds");
        assert_eq!(t.title.as_deref(), Some("Super Mario"));
        assert_eq!(t.region.as_deref(), Some("USA"));
        assert_eq!(t.serial.as_deref(), Some("CTR-P-AXXE"));
        assert_eq!(t.console.as_deref(), Some("3DS"));
    }

    #[test]
    fn dol_falls_back_to_game_name_without_banner() {
        let info = InfoResult::Dol(DolInfo {
            game_id: "GALE01".to_string(),
            game_name: "Smash Bros".to_string(),
            region: "NTSC".to_string(),
            banner: None,
            ..Default::default()
        });
        let t = TemplateTokens::new(Some(&info), Path::new("game.iso"), "rvz");
        assert_eq!(t.title.as_deref(), Some("Smash Bros"));
        assert_eq!(t.serial.as_deref(), Some("GALE01"));
        assert_eq!(t.console.as_deref(), Some("GameCube"));
    }

    #[test]
    fn none_info_yields_only_basename_and_ext() {
        let t = TemplateTokens::new(None, Path::new("game.iso"), "rvz");
        assert!(t.title.is_none());
        assert!(t.title_id.is_none());
        assert!(t.region.is_none());
        assert!(t.console.is_none());
        assert!(t.serial.is_none());
        assert_eq!(t.ext, "rvz");
        assert_eq!(t.basename, "game");
    }
}
