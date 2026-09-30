//! Output-path templating from decoded ROM metadata.
//!
//! Resolves a user template string such as `{console}/{title}.{ext}` against
//! the metadata rom-converto already extracts, producing a sanitized relative
//! path. No external DAT is consulted; every token comes from the in-tool
//! [`InfoResult`], except the DAT-sourced tokens below, which the organize
//! op's DAT-matching pass fills in on [`TemplateTokens`] when a Playmatch
//! hash-verified match exists.
//!
//! Supported tokens: `{title}`, `{titleId}`, `{region}`, `{console}`,
//! `{serial}`, `{ext}`, `{basename}`, `{language}`, `{type}`, `{dat}`,
//! `{game}`, `{input_dir}` (the unit's directory relative to the scan
//! root; unlike every other token, its `/` separators are kept, so a
//! nested source layout is mirrored rather than flattened), and one token
//! per [`frontends::FRONTENDS`](crate::util::frontends::FRONTENDS) entry
//! (e.g. `{es}`), which resolves to that frontend's ROM-root folder name
//! for [`TemplateTokens::frontend_console`], or an empty string when the
//! frontend has no folder for that console (the segment is left out).
//! `{title}`, `{titleId}` and `{serial}` fall back to the input basename;
//! every other missing token resolves to the empty string, and a template
//! whose every component resolves empty is an error. Unknown tokens are
//! left as literal text.

use crate::info::{DetectedConsole, InfoResult, console_label, retro::RetroDetails};
use crate::util::frontends;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

const MAX_COMPONENT_BYTES: usize = 200;

const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Metadata values substitutable into an output path template.
#[derive(Clone)]
pub struct TemplateTokens {
    pub title: Option<String>,
    pub title_id: Option<String>,
    /// DAT primary region code when a `GameRef` matched; else the in-tool
    /// header region.
    pub region: Option<String>,
    pub console: Option<String>,
    /// The label frontend folder tokens resolve against, never `{console}`:
    /// the console label refined to a Color system by the cartridge header
    /// or the input extension, or the DAT platform label when the organize
    /// match overwrites the console fallback.
    pub frontend_console: Option<String>,
    pub serial: Option<String>,
    pub ext: String,
    pub basename: String,
    /// `{language}`: the DAT match's first tagged language, else its
    /// primary region's main language.
    pub language: Option<String>,
    /// `{type}`: the DAT match's release-type label (e.g. "Retail",
    /// "BIOS", "Beta").
    pub game_type: Option<String>,
    /// `{dat}`: the matched DAT file's name.
    pub dat: Option<String>,
    /// `{game}`: the matched game's DAT name.
    pub game: Option<String>,
    /// `{input_dir}`: the unit's directory relative to the scan root.
    pub input_dir: Option<String>,
}

impl TemplateTokens {
    /// Builds tokens from `info`, falling back to `input`'s basename and
    /// `output_ext` when metadata is missing or `info` is `None`. The
    /// DAT-sourced fields (`region` aside) start empty; the organize DAT
    /// match pass fills them in once a `GameRef` exists.
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
            frontend_console: None,
            serial: None,
            ext,
            basename,
            language: None,
            game_type: None,
            dat: None,
            game: None,
            input_dir: None,
        };

        let input_ext = input
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

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
                tokens.console = console_label(DetectedConsole::Ctr).map(str::to_string);
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
                tokens.console = console_label(DetectedConsole::Dol).map(str::to_string);
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
                tokens.console = console_label(DetectedConsole::Rvl).map(str::to_string);
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
                tokens.console = console_label(DetectedConsole::Wup).map(str::to_string);
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
                tokens.console = console_label(DetectedConsole::Nx).map(str::to_string);
            }
            InfoResult::Chd(_) => {
                tokens.console = console_label(DetectedConsole::Chd).map(str::to_string);
            }
            InfoResult::Cso(_) => {
                tokens.console = console_label(DetectedConsole::Cso).map(str::to_string);
            }
            InfoResult::Xbox(_) => {
                tokens.console = console_label(DetectedConsole::Xbox).map(str::to_string);
            }
            InfoResult::Xenon(_) => {
                tokens.console = console_label(DetectedConsole::Xenon).map(str::to_string);
            }
            InfoResult::Ps3(p) => {
                tokens.title = p.title.clone().and_then(non_empty);
                tokens.title_id = p.title_id.clone().and_then(non_empty);
                tokens.region = p.region.clone().and_then(non_empty);
                tokens.console = console_label(DetectedConsole::Ps3).map(str::to_string);
                tokens.serial = p.title_id.clone().and_then(non_empty);
            }
            InfoResult::Psx(p) => {
                tokens.title_id = p.title_id.clone().and_then(non_empty);
                tokens.console = match p.console.as_str() {
                    "PS1" | "PS2" => Some(p.console.clone()),
                    _ => None,
                };
                tokens.serial = p.title_id.clone().and_then(non_empty);
            }
            InfoResult::Psp(p) => {
                tokens.title = p.title.clone().and_then(non_empty);
                tokens.title_id = p.title_id.clone().and_then(non_empty);
                tokens.console = console_label(DetectedConsole::Psp).map(str::to_string);
                tokens.serial = p.title_id.clone().and_then(non_empty);
            }
            InfoResult::LaserDisc(_) => {
                tokens.console = console_label(DetectedConsole::LaserDisc).map(str::to_string);
            }
            InfoResult::Ntr(n) => {
                tokens.title = n
                    .banner
                    .as_ref()
                    .and_then(|b| b.titles.primary())
                    .map(str::to_string)
                    .and_then(non_empty)
                    .or_else(|| non_empty(n.game_title.clone()));
                tokens.title_id = non_empty(n.game_code.clone());
                tokens.console = console_label(DetectedConsole::Ntr).map(str::to_string);
                tokens.serial = non_empty(n.game_code.clone());
            }
            InfoResult::Pbp(p) => {
                tokens.title = p.title.clone().and_then(non_empty);
                tokens.title_id = p.disc_id.clone().and_then(non_empty);
                tokens.console = console_label(DetectedConsole::Pbp).map(str::to_string);
                tokens.serial = p.disc_id.clone().and_then(non_empty);
            }
            InfoResult::Vpk(v) => {
                tokens.title = v.title.clone().and_then(non_empty);
                tokens.title_id = v.title_id.clone().and_then(non_empty);
                tokens.console = console_label(DetectedConsole::Vpk).map(str::to_string);
                tokens.serial = v.title_id.clone().and_then(non_empty);
            }
            InfoResult::Pkg(p) => {
                tokens.title = p.title.clone().and_then(non_empty);
                tokens.title_id = p.title_id.clone().and_then(non_empty);
                tokens.console = console_label(DetectedConsole::Pkg).map(str::to_string);
                tokens.serial = p.title_id.clone().and_then(non_empty);
            }
            InfoResult::Ps4Pkg(p) => {
                tokens.title = p.title.clone().and_then(non_empty);
                tokens.title_id = p.title_id.clone().and_then(non_empty);
                tokens.console = console_label(DetectedConsole::Ps4Pkg).map(str::to_string);
                tokens.serial = p.title_id.clone().and_then(non_empty);
            }
            InfoResult::Ps5Pkg(p) => {
                tokens.title = p.title.clone().and_then(non_empty);
                tokens.title_id = p.title_id.clone().and_then(non_empty);
                tokens.console = console_label(DetectedConsole::Ps5Pkg).map(str::to_string);
                tokens.serial = p.title_id.clone().and_then(non_empty);
            }
            // Cartridge-era and Sega disc systems carry their console label on
            // the RetroDetails variant, and none of them is a conversion
            // target, so no other tokens are derived.
            InfoResult::Retro(r) => {
                tokens.console = Some(retro_label(&r.details).to_string());
            }
        }

        tokens.frontend_console = frontend_console_label(tokens.console.as_deref(), &input_ext);
        if let InfoResult::Retro(r) = info {
            let color = match &r.details {
                RetroDetails::GameBoy(dmg) => {
                    matches!(dmg.cgb_flag, 0x80 | 0xC0).then_some("Game Boy Color")
                }
                RetroDetails::WonderSwan(ws) => ws.color.then_some("WonderSwan Color"),
                RetroDetails::NeoGeoPocket(ngp) => {
                    (ngp.machine == 0x10).then_some("Neo Geo Pocket Color")
                }
                _ => None,
            };
            if let Some(label) = color {
                tokens.frontend_console = Some(label.to_string());
            }
        }

        tokens.title = tokens.title.and_then(|t| non_empty(t.trim().to_string()));
        tokens
    }
}

/// The frontend folder lookup label for a console: `.gbc`, `.wsc` and
/// `.ngc` inputs name the Color systems their mono labels would hide.
pub(crate) fn frontend_console_label(console: Option<&str>, input_ext: &str) -> Option<String> {
    console.map(|label| {
        match (label, input_ext) {
            ("Game Boy", "gbc") => "Game Boy Color",
            ("WonderSwan", "wsc") => "WonderSwan Color",
            ("Neo Geo Pocket", "ngc") => "Neo Geo Pocket Color",
            _ => label,
        }
        .to_string()
    })
}

/// The folder label for a cartridge-era or Sega disc system, keyed by the
/// [`RetroDetails`] variant the header parse produced.
pub(crate) fn retro_label(details: &RetroDetails) -> &'static str {
    match details {
        RetroDetails::Nes(_) => "NES",
        RetroDetails::Snes(_) => "SNES",
        RetroDetails::N64(_) => "N64",
        RetroDetails::GameBoy(_) => "Game Boy",
        RetroDetails::Gba(_) => "Game Boy Advance",
        RetroDetails::MegaDrive(_) => "Mega Drive",
        RetroDetails::MasterSystem(_) => "Master System",
        RetroDetails::GameGear(_) => "Game Gear",
        RetroDetails::VirtualBoy(_) => "Virtual Boy",
        RetroDetails::WonderSwan(_) => "WonderSwan",
        RetroDetails::NeoGeoPocket(_) => "Neo Geo Pocket",
        RetroDetails::Lynx(_) => "Lynx",
        RetroDetails::Atari7800(_) => "Atari 7800",
        RetroDetails::Sega32x(_) => "32X",
        RetroDetails::Fds(_) => "Famicom Disk System",
        RetroDetails::SegaSaturn(_) => "Saturn",
        RetroDetails::SegaCd(_) => "Sega CD",
        RetroDetails::Dreamcast(_) => "Dreamcast",
    }
}

/// The console label for a cartridge file extension, using the same strings
/// as [`retro_label`], for callers that only have a file name (a header
/// parse may be unavailable). `None` for extensions that are not cartridge
/// formats.
pub(crate) fn retro_label_for_ext(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "nes" => Some("NES"),
        "sfc" | "smc" => Some("SNES"),
        "z64" | "n64" | "v64" => Some("N64"),
        "gb" | "gbc" => Some("Game Boy"),
        "gba" => Some("Game Boy Advance"),
        "md" | "gen" | "smd" => Some("Mega Drive"),
        "32x" => Some("32X"),
        "sms" => Some("Master System"),
        "gg" => Some("Game Gear"),
        "vb" => Some("Virtual Boy"),
        "ws" | "wsc" => Some("WonderSwan"),
        "ngp" | "ngc" => Some("Neo Geo Pocket"),
        "lnx" => Some("Lynx"),
        "a78" => Some("Atari 7800"),
        "fds" => Some("Famicom Disk System"),
        _ => None,
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

/// Substitutes `tokens` into `template` and sanitizes the result into a
/// filesystem-safe relative path.
///
/// # Errors
/// Returns an error if `template` is absolute or any component resolves to
/// `..`.
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

/// Sanitizes an untrusted string into a safe filename stem: path
/// separators and other illegal filename characters become `_`, control
/// characters are dropped, and Windows reserved names get a suffix. Use
/// for embedded metadata (title IDs, game IDs) that becomes part of an
/// output filename, so a hostile ROM cannot steer the write path.
pub fn sanitize_file_stem(stem: &str) -> String {
    sanitize_component(stem)
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
    // `input_dir` names a real (trusted) relative directory, possibly with
    // several components; apply_template's per-component split/sanitize
    // pass below still guards it, so it skips the separator neutralization
    // every other (metadata-sourced) token gets.
    if name == "input_dir" {
        return Some(tokens.input_dir.clone().unwrap_or_default());
    }
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
        "language" => tokens.language.clone().unwrap_or_default(),
        "type" => tokens.game_type.clone().unwrap_or_default(),
        "dat" => tokens.dat.clone().unwrap_or_default(),
        "game" => tokens.game.clone().unwrap_or_default(),
        _ if frontends::is_frontend(name) => {
            let console_label = tokens.frontend_console.as_deref().unwrap_or("");
            frontends::frontend_dir(name, console_label)
                .map(str::to_string)
                .unwrap_or_default()
        }
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
    use crate::atari::handy::HandyInfo;
    use crate::bandai::ws::WsInfo;
    use crate::info::{
        ChdInfo, CtrInfo, DolInfo, InfoResult, NxInfo, RetroInfo, retro::RetroDetails,
    };
    use crate::nintendo::ctr::info::{CtrSmdhInfo, CtrSmdhTitle};
    use crate::nintendo::dmg::DmgInfo;
    use crate::nintendo::nx::info::{NxControl, NxFullInfo, NxNacpTitle};
    use crate::sega::sms::SmsInfo;
    use crate::snk::ngp::NgpInfo;

    fn tokens(title: Option<&str>) -> TemplateTokens {
        TemplateTokens {
            title: title.map(str::to_string),
            title_id: Some("ABCD".to_string()),
            region: Some("USA".to_string()),
            console: Some("Wii".to_string()),
            frontend_console: Some("Wii".to_string()),
            serial: Some("RMCE01".to_string()),
            ext: "rvz".to_string(),
            basename: "game01".to_string(),
            language: Some("EN".to_string()),
            game_type: Some("Retail".to_string()),
            dat: Some("Nintendo - Wii".to_string()),
            game: Some("Mario Kart Wii".to_string()),
            input_dir: Some("wii/mario".to_string()),
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
    fn sanitize_file_stem_neutralizes_traversal() {
        assert_eq!(sanitize_file_stem("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(sanitize_file_stem("AB:CD\\EF"), "AB_CD_EF");
        assert_eq!(sanitize_file_stem("GAME01"), "GAME01");
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
    fn dat_tokens_resolve() {
        let t = tokens(Some("Mario"));
        let p = apply_template("{type}/{language}/{dat}/{game}.{ext}", &t).unwrap();
        assert_eq!(
            p,
            PathBuf::from("Retail/EN/Nintendo - Wii/Mario Kart Wii.rvz")
        );
    }

    #[test]
    fn missing_dat_tokens_collapse_to_empty() {
        let mut t = tokens(Some("Mario"));
        t.language = None;
        t.game_type = None;
        t.dat = None;
        t.game = None;
        let p = apply_template("{type}{language}{dat}{game}{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("Mario.rvz"));
    }

    #[test]
    fn frontend_token_resolves_known_console() {
        let mut t = tokens(Some("Mario"));
        t.console = Some("NES".to_string());
        t.frontend_console = Some("NES".to_string());
        let p = apply_template("{es}/{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("nes/Mario.rvz"));
    }

    #[test]
    fn frontend_token_resolves_the_refined_label() {
        let mut t = tokens(Some("Mario"));
        t.console = Some("Game Boy".to_string());
        t.frontend_console = Some("Game Boy Color".to_string());
        let p = apply_template("{es}/{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("gbc/Mario.rvz"));
    }

    /// A CHD unit whose DAT match names a platform resolves the frontend
    /// folders of that platform's row, where the generic CHD label has
    /// none.
    #[test]
    fn chd_unit_with_a_dat_platform_resolves_frontend_folders() {
        let mut t = TemplateTokens::new(
            Some(&InfoResult::Chd(ChdInfo::default())),
            Path::new("game.chd"),
            "",
        );
        assert_eq!(t.console.as_deref(), Some("CHD"));
        assert!(t.frontend_console.as_deref() == Some("CHD"));
        // No frontend documents a folder for the generic label, so its
        // segment collapses.
        assert_eq!(
            apply_template("{es}/{title}", &t).unwrap(),
            PathBuf::from("game")
        );

        // The organize DAT pass writes the matched platform label over the
        // generic CHD label in both fields.
        t.console = Some("Sega Saturn".to_string());
        t.frontend_console = Some("Sega Saturn".to_string());
        assert_eq!(
            apply_template("{es}/{title}", &t).unwrap(),
            PathBuf::from("saturn/game")
        );
    }

    /// A minimal Game Boy cartridge header payload.
    fn dmg_info(cgb_flag: u8) -> DmgInfo {
        DmgInfo {
            logo_valid: true,
            title: "ZELDA".to_string(),
            manufacturer_code: None,
            cgb_flag,
            cgb: None,
            sgb_flag: 0,
            cart_type: 0,
            cart_type_name: None,
            rom_bytes: None,
            ram_bytes: None,
            destination: 0,
            destination_name: None,
            licensee: "01".to_string(),
            version: 0,
            header_checksum: 0,
            computed_header_checksum: 0,
            header_checksum_valid: true,
            global_checksum: 0,
            computed_global_checksum: 0,
            global_checksum_valid: true,
        }
    }

    fn ws_info() -> WsInfo {
        WsInfo {
            publisher_id: 0,
            color: false,
            game_id: 0,
            save_type: 0,
            save: None,
            version: 0,
            checksum: 0,
            computed_checksum: 0,
            checksum_valid: false,
        }
    }

    fn ngp_info() -> NgpInfo {
        NgpInfo {
            license: "SNK".to_string(),
            startup_address: 0,
            catalog_id: 0,
            subcatalog_id: 0,
            machine: 0,
            machine_name: None,
            title: "GAME".to_string(),
        }
    }

    /// Frontend folders come from the Color rows for `.gbc`, `.wsc` and
    /// `.ngc` inputs and for a CGB-capable Game Boy header, while
    /// `{console}` keeps its mono label.
    #[test]
    fn frontend_folders_use_the_color_labels() {
        let folders = |ext: &str, details: RetroDetails| {
            let t = TemplateTokens::new(
                Some(&InfoResult::Retro(RetroInfo {
                    file_size: 0,
                    details,
                })),
                Path::new(&format!("game{ext}")),
                "",
            );
            (
                t.console.clone(),
                t.frontend_console.clone(),
                apply_template("{es}", &t).unwrap(),
            )
        };

        let (console, frontend, folder) = folders(".gbc", RetroDetails::GameBoy(dmg_info(0x00)));
        assert_eq!(console.as_deref(), Some("Game Boy"));
        assert_eq!(frontend.as_deref(), Some("Game Boy Color"));
        assert_eq!(folder, PathBuf::from("gbc"));

        // The CGB flags 0x80 (enhanced) and 0xC0 (exclusive) both name the
        // Color system, whatever the extension.
        let (console, frontend, folder) = folders(".gb", RetroDetails::GameBoy(dmg_info(0x80)));
        assert_eq!(console.as_deref(), Some("Game Boy"));
        assert_eq!(frontend.as_deref(), Some("Game Boy Color"));
        assert_eq!(folder, PathBuf::from("gbc"));
        let (console, frontend, folder) = folders(".gb", RetroDetails::GameBoy(dmg_info(0xC0)));
        assert_eq!(console.as_deref(), Some("Game Boy"));
        assert_eq!(frontend.as_deref(), Some("Game Boy Color"));
        assert_eq!(folder, PathBuf::from("gbc"));

        // A DMG-only header on a .gb input stays mono.
        let (console, frontend, folder) = folders(".gb", RetroDetails::GameBoy(dmg_info(0x00)));
        assert_eq!(console.as_deref(), Some("Game Boy"));
        assert_eq!(frontend.as_deref(), Some("Game Boy"));
        assert_eq!(folder, PathBuf::from("gb"));

        let (console, frontend, folder) = folders(".wsc", RetroDetails::WonderSwan(ws_info()));
        assert_eq!(console.as_deref(), Some("WonderSwan"));
        assert_eq!(frontend.as_deref(), Some("WonderSwan Color"));
        assert_eq!(folder, PathBuf::from("wonderswancolor"));

        let (console, frontend, folder) = folders(".ngc", RetroDetails::NeoGeoPocket(ngp_info()));
        assert_eq!(console.as_deref(), Some("Neo Geo Pocket"));
        assert_eq!(frontend.as_deref(), Some("Neo Geo Pocket Color"));
        assert_eq!(folder, PathBuf::from("ngpc"));

        // The parsed color flags name the Color systems whatever the
        // extension, and their mono headers stay mono.
        let color_ws = WsInfo {
            color: true,
            ..ws_info()
        };
        let (console, frontend, folder) = folders(".ws", RetroDetails::WonderSwan(color_ws));
        assert_eq!(console.as_deref(), Some("WonderSwan"));
        assert_eq!(frontend.as_deref(), Some("WonderSwan Color"));
        assert_eq!(folder, PathBuf::from("wonderswancolor"));
        let (_, frontend, folder) = folders(".ws", RetroDetails::WonderSwan(ws_info()));
        assert_eq!(frontend.as_deref(), Some("WonderSwan"));
        assert_eq!(folder, PathBuf::from("wonderswan"));

        let color_ngp = NgpInfo {
            machine: 0x10,
            ..ngp_info()
        };
        let (console, frontend, folder) = folders(".ngp", RetroDetails::NeoGeoPocket(color_ngp));
        assert_eq!(console.as_deref(), Some("Neo Geo Pocket"));
        assert_eq!(frontend.as_deref(), Some("Neo Geo Pocket Color"));
        assert_eq!(folder, PathBuf::from("ngpc"));
        let (_, frontend, folder) = folders(".ngp", RetroDetails::NeoGeoPocket(ngp_info()));
        assert_eq!(frontend.as_deref(), Some("Neo Geo Pocket"));
        assert_eq!(folder, PathBuf::from("ngp"));
    }

    #[test]
    fn frontend_token_empty_for_unknown_console() {
        let mut t = tokens(Some("Mario"));
        t.console = Some("PS5".to_string());
        t.frontend_console = Some("PS5".to_string());
        let p = apply_template("{es}{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("Mario.rvz"));
    }

    #[test]
    fn input_dir_token_resolves() {
        let t = tokens(Some("Mario"));
        let p = apply_template("{input_dir}/{title}.{ext}", &t).unwrap();
        assert_eq!(p, PathBuf::from("wii/mario/Mario.rvz"));
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

    /// A minimal SMS header payload; Master System and Game Gear share it.
    fn sms_info() -> SmsInfo {
        SmsInfo {
            header_offset: 0x1ff0,
            product_code: 0,
            version: 0,
            region_code: 4,
            region: None,
            rom_size_code: 0,
            rom_size_kb: None,
            checksum: 0,
            computed_checksum: 0,
            checksum_valid: false,
        }
    }

    #[test]
    fn retro_console_comes_from_details_variant() {
        let console_for = |details: RetroDetails| {
            TemplateTokens::new(
                Some(&InfoResult::Retro(RetroInfo {
                    file_size: 0,
                    details,
                })),
                Path::new("game.rom"),
                "zip",
            )
            .console
        };
        assert_eq!(
            console_for(RetroDetails::Lynx(HandyInfo {
                bank0_page_size: 512,
                bank1_page_size: 0,
                version: 1,
                cart_name: "Chip's Challenge".to_string(),
                manufacturer: "Atari".to_string(),
                rotation: 0,
                rotation_name: None,
            }))
            .as_deref(),
            Some("Lynx")
        );
        assert_eq!(
            console_for(RetroDetails::MasterSystem(sms_info())).as_deref(),
            Some("Master System")
        );
        assert_eq!(
            console_for(RetroDetails::GameGear(sms_info())).as_deref(),
            Some("Game Gear")
        );
    }
}
