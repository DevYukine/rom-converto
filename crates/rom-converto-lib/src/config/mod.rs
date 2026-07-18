//! The `rom-converto.toml` config file: discovery, parsing, and the
//! per-format default and preset structures. Precedence is
//! flag > preset > config default > built-in default; presets and
//! top-level format defaults never merge, a preset fully replaces the
//! top-level defaults for the formats it sets.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;

mod write;
pub use write::*;

pub const CONFIG_FILENAMES: [&str; 2] = ["rom-converto.toml", ".rom-converto.toml"];
pub const USER_CONFIG_SUBPATH: &str = "rom-converto/config.toml";

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    pub dol: Option<DiscDefaults>,
    pub rvl: Option<DiscDefaults>,
    pub nx: Option<NxDefaults>,
    pub chd: Option<ChdDefaults>,
    pub cso: Option<CsoDefaults>,
    pub wup: Option<WupDefaults>,
    pub dat: Option<DatDefaults>,
    #[serde(default)]
    pub presets: HashMap<String, Preset>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiscDefaults {
    /// Zstandard compression level, -22 to 22. Defaults to 22.
    pub level: Option<i32>,
    /// Chunk size in bytes; must be a power of two between 32 KiB and 2 MiB.
    /// Defaults to 128 KiB.
    pub chunk_size: Option<u32>,
    pub on_conflict: Option<String>,
    pub output_dir: Option<PathBuf>,
    pub report: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct NxDefaults {
    /// Zstd compression level, 1 to 22. Defaults to 18.
    pub level: Option<i32>,
    pub mode: Option<String>,
    pub block_size_exp: Option<u8>,
    pub on_conflict: Option<String>,
    pub output_dir: Option<PathBuf>,
    pub report: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChdDefaults {
    pub hunk_size: Option<u32>,
    /// Codec list, e.g. `["cdlz", "cdzl", "cdfl"]`.
    pub codecs: Option<Vec<String>>,
    /// Per-codec compression level.
    pub level: Option<i32>,
    pub on_conflict: Option<String>,
    pub output_dir: Option<PathBuf>,
    pub report: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CsoDefaults {
    pub block_size: Option<u32>,
    pub on_conflict: Option<String>,
    pub output_dir: Option<PathBuf>,
    pub report: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct WupDefaults {
    /// Zstd compression level, 0 to 22. Defaults to 6.
    pub level: Option<i32>,
    pub on_conflict: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct DatDefaults {
    pub api_base: Option<String>,
    pub report: Option<PathBuf>,
    /// Minimum checksum tier always computed before consulting Playmatch: crc32, md5, sha1, sha256.
    pub input_checksum_min: Option<String>,
    /// Maximum checksum tier escalation may reach when the floor tier alone does not verify.
    pub input_checksum_max: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub dol: Option<DiscDefaults>,
    pub rvl: Option<DiscDefaults>,
    pub nx: Option<NxDefaults>,
    pub chd: Option<ChdDefaults>,
    pub cso: Option<CsoDefaults>,
    pub wup: Option<WupDefaults>,
    pub dat: Option<DatDefaults>,
}

/// Ordered list of paths to probe for a config file. An explicit path
/// short-circuits discovery and is the only candidate when present.
pub fn candidate_paths(
    explicit: Option<&Path>,
    cwd: &Path,
    user_config_dir: Option<&Path>,
) -> Vec<PathBuf> {
    if let Some(explicit) = explicit {
        return vec![explicit.to_path_buf()];
    }
    let mut paths = Vec::new();
    for name in CONFIG_FILENAMES {
        paths.push(cwd.join(name));
    }
    if let Some(dir) = user_config_dir {
        paths.push(dir.join(USER_CONFIG_SUBPATH));
    }
    paths
}

/// Returns the first existing config path following the search order, or
/// `None` when no config exists. An explicit path that does not exist is
/// an error so a mistyped `--config` is not silently ignored.
pub fn discover_config_path(explicit: Option<&Path>) -> anyhow::Result<Option<PathBuf>> {
    let cwd = std::env::current_dir().context("cannot determine the current directory")?;
    let user_dir = dirs::config_dir();
    let candidates = candidate_paths(explicit, &cwd, user_dir.as_deref());

    if let Some(explicit) = explicit {
        if explicit.is_file() {
            return Ok(Some(explicit.to_path_buf()));
        }
        anyhow::bail!("config file not found: {}", explicit.display());
    }

    Ok(candidates.into_iter().find(|p| p.is_file()))
}

/// Path to the per-user config file, used as the write target when no
/// config file exists yet to save a preset into. Does not check that the
/// file or its parent directory exists.
pub fn user_config_write_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join(USER_CONFIG_SUBPATH))
}

/// Loads and parses the config, applying the discovery order. A missing
/// config (without an explicit path) yields the built-in defaults. A
/// missing explicit path or malformed TOML is a hard error so user
/// mistakes are not masked.
pub fn load_config(explicit: Option<&Path>) -> anyhow::Result<UserConfig> {
    let Some(path) = discover_config_path(explicit)? else {
        return Ok(UserConfig::default());
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read config file: {}", path.display()))?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    parse_str(&content, base).with_context(|| format!("invalid config file: {}", path.display()))
}

/// Like `load_config`, but leaves `output_dir`/`report` untouched instead of
/// resolving relative ones against the config file's directory. Used by the
/// GUI so a preset saved back keeps a hand-authored relative path relative
/// instead of baking in a machine-specific absolute one.
pub fn load_config_raw(explicit: Option<&Path>) -> anyhow::Result<UserConfig> {
    let Some(path) = discover_config_path(explicit)? else {
        return Ok(UserConfig::default());
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read config file: {}", path.display()))?;
    toml::from_str(&content).with_context(|| format!("invalid config file: {}", path.display()))
}

/// Validates the preset name against the config. An unknown name is an
/// error listing the available presets (sorted) so the message is
/// actionable.
pub fn resolve_preset(cfg: &UserConfig, name: Option<&str>) -> anyhow::Result<Option<Preset>> {
    let Some(name) = name else {
        return Ok(None);
    };
    if let Some(preset) = cfg.presets.get(name) {
        return Ok(Some(preset.clone()));
    }
    let mut available: Vec<&str> = cfg.presets.keys().map(String::as_str).collect();
    available.sort_unstable();
    if available.is_empty() {
        anyhow::bail!("unknown preset '{name}': the config defines no presets");
    }
    anyhow::bail!(
        "unknown preset '{name}': available presets are {}",
        available.join(", ")
    );
}

fn parse_str(content: &str, base: &Path) -> anyhow::Result<UserConfig> {
    let mut cfg: UserConfig = toml::from_str(content)?;
    resolve_paths(&mut cfg, base);
    Ok(cfg)
}

/// Relative `output_dir`/`report` paths resolve against the config
/// file's own directory, not the process CWD, so a config in the user
/// config dir behaves the same from any working directory.
fn resolve_paths(cfg: &mut UserConfig, base: &Path) {
    resolve_disc(cfg.dol.as_mut(), base);
    resolve_disc(cfg.rvl.as_mut(), base);
    resolve_nx(cfg.nx.as_mut(), base);
    resolve_chd(cfg.chd.as_mut(), base);
    resolve_cso(cfg.cso.as_mut(), base);
    resolve_dat(cfg.dat.as_mut(), base);
    for preset in cfg.presets.values_mut() {
        resolve_disc(preset.dol.as_mut(), base);
        resolve_disc(preset.rvl.as_mut(), base);
        resolve_nx(preset.nx.as_mut(), base);
        resolve_chd(preset.chd.as_mut(), base);
        resolve_cso(preset.cso.as_mut(), base);
        resolve_dat(preset.dat.as_mut(), base);
    }
}

fn resolve_relative(base: &Path, p: &mut Option<PathBuf>) {
    if let Some(path) = p
        && path.is_relative()
    {
        *path = base.join(&*path);
    }
}

fn resolve_disc(d: Option<&mut DiscDefaults>, base: &Path) {
    if let Some(d) = d {
        resolve_relative(base, &mut d.output_dir);
        resolve_relative(base, &mut d.report);
    }
}

fn resolve_nx(d: Option<&mut NxDefaults>, base: &Path) {
    if let Some(d) = d {
        resolve_relative(base, &mut d.output_dir);
        resolve_relative(base, &mut d.report);
    }
}

fn resolve_chd(d: Option<&mut ChdDefaults>, base: &Path) {
    if let Some(d) = d {
        resolve_relative(base, &mut d.output_dir);
        resolve_relative(base, &mut d.report);
    }
}

fn resolve_cso(d: Option<&mut CsoDefaults>, base: &Path) {
    if let Some(d) = d {
        resolve_relative(base, &mut d.output_dir);
        resolve_relative(base, &mut d.report);
    }
}

fn resolve_dat(d: Option<&mut DatDefaults>, base: &Path) {
    if let Some(d) = d {
        resolve_relative(base, &mut d.report);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "/tmp/cfg";

    fn base() -> &'static Path {
        Path::new(BASE)
    }

    #[test]
    fn parses_minimal_config() {
        let cfg = parse_str("[dol]\nlevel = 5\n", base()).unwrap();
        assert_eq!(cfg.dol.unwrap().level, Some(5));
    }

    #[test]
    fn parses_preset() {
        let cfg = parse_str("[presets.archive.nx]\nlevel = 22\n", base()).unwrap();
        let preset = &cfg.presets["archive"];
        assert_eq!(preset.nx.as_ref().unwrap().level, Some(22));
    }

    #[test]
    fn empty_config_is_default() {
        let cfg = parse_str("", base()).unwrap();
        assert!(cfg.dol.is_none());
        assert!(cfg.nx.is_none());
        assert!(cfg.presets.is_empty());
    }

    #[test]
    fn unknown_field_is_error() {
        assert!(parse_str("[dol]\nbogus = 1\n", base()).is_err());
    }

    #[test]
    fn malformed_toml_is_error() {
        assert!(parse_str("this is = = not toml", base()).is_err());
    }

    #[test]
    fn malformed_config_message_includes_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rom-converto.toml");
        std::fs::write(&path, "this is = = not toml").unwrap();
        let err = load_config(Some(&path)).unwrap_err().to_string();
        assert!(err.contains(&path.display().to_string()));
    }

    #[test]
    fn missing_explicit_config_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        let err = load_config(Some(&path)).unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn unknown_preset_lists_available() {
        let cfg = parse_str(
            "[presets.archive.dol]\nlevel = 22\n[presets.fast.dol]\nlevel = 3\n",
            base(),
        )
        .unwrap();
        let err = resolve_preset(&cfg, Some("nope")).unwrap_err().to_string();
        assert!(err.contains("archive"));
        assert!(err.contains("fast"));
    }

    #[test]
    fn unknown_preset_with_no_presets_is_clear() {
        let cfg = UserConfig::default();
        let err = resolve_preset(&cfg, Some("nope")).unwrap_err().to_string();
        assert!(err.contains("no presets"));
    }

    #[test]
    fn known_preset_resolves() {
        let cfg = parse_str("[presets.archive.dol]\nlevel = 22\n", base()).unwrap();
        assert!(resolve_preset(&cfg, Some("archive")).unwrap().is_some());
        assert!(resolve_preset(&cfg, None).unwrap().is_none());
    }

    #[test]
    fn relative_output_dir_resolved_against_config_dir() {
        let cfg = parse_str("[dol]\noutput_dir = \"out\"\n", base()).unwrap();
        assert_eq!(
            cfg.dol.unwrap().output_dir.unwrap(),
            Path::new(BASE).join("out")
        );
    }

    #[test]
    fn absolute_output_dir_unchanged() {
        let cfg = parse_str("[dol]\noutput_dir = \"/abs/out\"\n", base()).unwrap();
        assert_eq!(cfg.dol.unwrap().output_dir.unwrap(), Path::new("/abs/out"));
    }

    #[test]
    fn parses_dat_api_base() {
        let cfg = parse_str(
            "[dat]\napi_base = \"https://example.test/api/v2\"\n",
            base(),
        )
        .unwrap();
        assert_eq!(
            cfg.dat.unwrap().api_base.as_deref(),
            Some("https://example.test/api/v2")
        );
    }

    #[test]
    fn dat_unknown_field_is_error() {
        assert!(parse_str("[dat]\nbogus = 1\n", base()).is_err());
    }

    #[test]
    fn dat_relative_report_resolved_against_config_dir() {
        let cfg = parse_str("[dat]\nreport = \"dat-report.json\"\n", base()).unwrap();
        assert_eq!(
            cfg.dat.unwrap().report.unwrap(),
            Path::new(BASE).join("dat-report.json")
        );
    }

    #[test]
    fn preset_dat_overrides_config_dat() {
        let cfg = parse_str(
            "[dat]\napi_base = \"https://config.test/api/v2\"\n[presets.p.dat]\napi_base = \"https://preset.test/api/v2\"\n",
            base(),
        )
        .unwrap();
        assert_eq!(
            cfg.presets["p"].dat.as_ref().unwrap().api_base.as_deref(),
            Some("https://preset.test/api/v2")
        );
        assert_eq!(
            cfg.dat.as_ref().unwrap().api_base.as_deref(),
            Some("https://config.test/api/v2")
        );
    }

    #[test]
    fn relative_preset_path_resolved_against_config_dir() {
        let cfg = parse_str("[presets.a.cso]\nreport = \"r.json\"\n", base()).unwrap();
        assert_eq!(
            cfg.presets["a"]
                .cso
                .as_ref()
                .unwrap()
                .report
                .as_ref()
                .unwrap(),
            &Path::new(BASE).join("r.json")
        );
    }

    #[test]
    fn discover_prefers_cwd_over_user_dir() {
        let cwd = Path::new("/work");
        let user = Path::new("/home/u/.config");
        let paths = candidate_paths(None, cwd, Some(user));
        let cwd_idx = paths.iter().position(|p| p.starts_with(cwd)).unwrap();
        let user_idx = paths.iter().position(|p| p.starts_with(user)).unwrap();
        assert!(cwd_idx < user_idx);
        assert_eq!(paths[0], cwd.join("rom-converto.toml"));
        assert_eq!(paths[1], cwd.join(".rom-converto.toml"));
    }

    #[test]
    fn explicit_path_short_circuits_discovery() {
        let explicit = Path::new("/etc/custom.toml");
        let paths = candidate_paths(Some(explicit), Path::new("/work"), Some(Path::new("/u")));
        assert_eq!(paths, vec![explicit.to_path_buf()]);
    }
}
