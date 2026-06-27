use rom_converto_lib::config::{
    ChdDefaults, CsoDefaults, DiscDefaults, NxDefaults, Preset, UserConfig, WupDefaults,
};
use rom_converto_lib::util::ConflictPolicy;

/// Per-format defaults with the preset layer already merged over the
/// config-file layer. Each command arm reads its own format here and
/// layers the explicit CLI flag on top, giving the precedence
/// flag > preset > config > built-in.
#[derive(Debug, Default, Clone)]
pub struct Effective {
    pub dol: DiscDefaults,
    pub rvl: DiscDefaults,
    pub nx: NxDefaults,
    pub chd: ChdDefaults,
    pub cso: CsoDefaults,
    pub wup: WupDefaults,
}

pub fn resolve(cfg: &UserConfig, preset: Option<&Preset>) -> Effective {
    Effective {
        dol: merge_disc(preset.and_then(|p| p.dol.as_ref()), cfg.dol.as_ref()),
        rvl: merge_disc(preset.and_then(|p| p.rvl.as_ref()), cfg.rvl.as_ref()),
        nx: merge_nx(preset.and_then(|p| p.nx.as_ref()), cfg.nx.as_ref()),
        chd: merge_chd(preset.and_then(|p| p.chd.as_ref()), cfg.chd.as_ref()),
        cso: merge_cso(preset.and_then(|p| p.cso.as_ref()), cfg.cso.as_ref()),
        wup: merge_wup(preset.and_then(|p| p.wup.as_ref()), cfg.wup.as_ref()),
    }
}

fn merge_disc(top: Option<&DiscDefaults>, base: Option<&DiscDefaults>) -> DiscDefaults {
    DiscDefaults {
        level: pick(top.and_then(|t| t.level), base.and_then(|b| b.level)),
        chunk_size: pick(
            top.and_then(|t| t.chunk_size),
            base.and_then(|b| b.chunk_size),
        ),
        on_conflict: pick(
            top.and_then(|t| t.on_conflict.clone()),
            base.and_then(|b| b.on_conflict.clone()),
        ),
        output_dir: pick(
            top.and_then(|t| t.output_dir.clone()),
            base.and_then(|b| b.output_dir.clone()),
        ),
        report: pick(
            top.and_then(|t| t.report.clone()),
            base.and_then(|b| b.report.clone()),
        ),
    }
}

fn merge_nx(top: Option<&NxDefaults>, base: Option<&NxDefaults>) -> NxDefaults {
    NxDefaults {
        level: pick(top.and_then(|t| t.level), base.and_then(|b| b.level)),
        mode: pick(
            top.and_then(|t| t.mode.clone()),
            base.and_then(|b| b.mode.clone()),
        ),
        block_size_exp: pick(
            top.and_then(|t| t.block_size_exp),
            base.and_then(|b| b.block_size_exp),
        ),
        on_conflict: pick(
            top.and_then(|t| t.on_conflict.clone()),
            base.and_then(|b| b.on_conflict.clone()),
        ),
        output_dir: pick(
            top.and_then(|t| t.output_dir.clone()),
            base.and_then(|b| b.output_dir.clone()),
        ),
        report: pick(
            top.and_then(|t| t.report.clone()),
            base.and_then(|b| b.report.clone()),
        ),
    }
}

fn merge_chd(top: Option<&ChdDefaults>, base: Option<&ChdDefaults>) -> ChdDefaults {
    ChdDefaults {
        hunk_size: pick(top.and_then(|t| t.hunk_size), base.and_then(|b| b.hunk_size)),
        on_conflict: pick(
            top.and_then(|t| t.on_conflict.clone()),
            base.and_then(|b| b.on_conflict.clone()),
        ),
        output_dir: pick(
            top.and_then(|t| t.output_dir.clone()),
            base.and_then(|b| b.output_dir.clone()),
        ),
        report: pick(
            top.and_then(|t| t.report.clone()),
            base.and_then(|b| b.report.clone()),
        ),
    }
}

fn merge_cso(top: Option<&CsoDefaults>, base: Option<&CsoDefaults>) -> CsoDefaults {
    CsoDefaults {
        block_size: pick(
            top.and_then(|t| t.block_size),
            base.and_then(|b| b.block_size),
        ),
        on_conflict: pick(
            top.and_then(|t| t.on_conflict.clone()),
            base.and_then(|b| b.on_conflict.clone()),
        ),
        output_dir: pick(
            top.and_then(|t| t.output_dir.clone()),
            base.and_then(|b| b.output_dir.clone()),
        ),
        report: pick(
            top.and_then(|t| t.report.clone()),
            base.and_then(|b| b.report.clone()),
        ),
    }
}

fn merge_wup(top: Option<&WupDefaults>, base: Option<&WupDefaults>) -> WupDefaults {
    WupDefaults {
        level: pick(top.and_then(|t| t.level), base.and_then(|b| b.level)),
        on_conflict: pick(
            top.and_then(|t| t.on_conflict.clone()),
            base.and_then(|b| b.on_conflict.clone()),
        ),
    }
}

fn pick<T>(top: Option<T>, base: Option<T>) -> Option<T> {
    top.or(base)
}

pub fn conflict_from_str(s: &str) -> anyhow::Result<ConflictPolicy> {
    match s.to_ascii_lowercase().as_str() {
        "error" => Ok(ConflictPolicy::Error),
        "overwrite" => Ok(ConflictPolicy::Overwrite),
        "skip" => Ok(ConflictPolicy::Skip),
        "rename" => Ok(ConflictPolicy::Rename),
        other => anyhow::bail!(
            "invalid on_conflict value '{other}' in config: expected error, overwrite, skip or rename"
        ),
    }
}

/// Resolves the config/preset on_conflict fallback for a command arm.
/// `None` means the config left it unset, so the built-in `error`
/// policy applies.
pub fn policy_fallback(s: &Option<String>) -> anyhow::Result<ConflictPolicy> {
    match s {
        Some(value) => conflict_from_str(value),
        None => Ok(ConflictPolicy::Error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn cfg_with_dol(d: DiscDefaults) -> UserConfig {
        UserConfig {
            dol: Some(d),
            ..UserConfig::default()
        }
    }

    fn preset_with_dol(d: DiscDefaults) -> Preset {
        Preset {
            dol: Some(d),
            ..Preset::default()
        }
    }

    #[test]
    fn merge_preset_over_config() {
        let cfg = cfg_with_dol(DiscDefaults {
            level: Some(5),
            ..Default::default()
        });
        let preset = preset_with_dol(DiscDefaults {
            level: Some(22),
            ..Default::default()
        });
        let eff = resolve(&cfg, Some(&preset));
        assert_eq!(eff.dol.level, Some(22));
    }

    #[test]
    fn merge_config_used_when_no_preset() {
        let cfg = cfg_with_dol(DiscDefaults {
            level: Some(5),
            ..Default::default()
        });
        let eff = resolve(&cfg, None);
        assert_eq!(eff.dol.level, Some(5));
    }

    #[test]
    fn merge_none_when_neither() {
        let eff = resolve(&UserConfig::default(), None);
        assert_eq!(eff.dol.level, None);
    }

    #[test]
    fn merge_field_independence() {
        let cfg = cfg_with_dol(DiscDefaults {
            level: Some(5),
            ..Default::default()
        });
        let preset = preset_with_dol(DiscDefaults {
            chunk_size: Some(131072),
            ..Default::default()
        });
        let eff = resolve(&cfg, Some(&preset));
        assert_eq!(eff.dol.level, Some(5));
        assert_eq!(eff.dol.chunk_size, Some(131072));
    }

    #[test]
    fn flag_over_preset_in_arm() {
        const BUILTIN: i32 = 22;
        let cfg = cfg_with_dol(DiscDefaults {
            level: Some(5),
            ..Default::default()
        });
        let preset = preset_with_dol(DiscDefaults {
            level: Some(10),
            ..Default::default()
        });
        let eff = resolve(&cfg, Some(&preset));

        let cli_level = Some(3);
        assert_eq!(cli_level.or(eff.dol.level).unwrap_or(BUILTIN), 3);

        let cli_level: Option<i32> = None;
        assert_eq!(cli_level.or(eff.dol.level).unwrap_or(BUILTIN), 10);

        let eff_empty = resolve(&UserConfig::default(), None);
        let cli_level: Option<i32> = None;
        assert_eq!(cli_level.or(eff_empty.dol.level).unwrap_or(BUILTIN), BUILTIN);
    }

    #[test]
    fn merge_preserves_paths() {
        let cfg = cfg_with_dol(DiscDefaults {
            output_dir: Some(PathBuf::from("/out")),
            ..Default::default()
        });
        let eff = resolve(&cfg, None);
        assert_eq!(eff.dol.output_dir, Some(PathBuf::from("/out")));
    }

    #[test]
    fn conflict_from_str_roundtrip() {
        assert_eq!(conflict_from_str("error").unwrap(), ConflictPolicy::Error);
        assert_eq!(
            conflict_from_str("Overwrite").unwrap(),
            ConflictPolicy::Overwrite
        );
        assert_eq!(conflict_from_str("SKIP").unwrap(), ConflictPolicy::Skip);
        assert_eq!(conflict_from_str("rename").unwrap(), ConflictPolicy::Rename);
    }

    #[test]
    fn conflict_from_str_unknown_errors() {
        assert!(conflict_from_str("bogus").is_err());
    }

    #[test]
    fn policy_fallback_none_is_error() {
        assert_eq!(policy_fallback(&None).unwrap(), ConflictPolicy::Error);
    }

    #[test]
    fn policy_fallback_some_parses() {
        assert_eq!(
            policy_fallback(&Some("skip".to_string())).unwrap(),
            ConflictPolicy::Skip
        );
    }

    #[test]
    fn presets_map_is_keyed() {
        let mut presets = HashMap::new();
        presets.insert("a".to_string(), preset_with_dol(DiscDefaults::default()));
        let cfg = UserConfig {
            presets,
            ..UserConfig::default()
        };
        assert!(cfg.presets.contains_key("a"));
    }
}
