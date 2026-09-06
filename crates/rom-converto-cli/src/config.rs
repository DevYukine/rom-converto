use rom_converto_lib::config::{
    ChdDefaults, CsoDefaults, DatDefaults, DiscDefaults, MergeOver, NxDefaults, Preset, UserConfig,
    WupDefaults,
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
    pub dat: DatDefaults,
}

pub fn resolve(cfg: &UserConfig, preset: Option<&Preset>) -> Effective {
    Effective {
        dol: DiscDefaults::merge_layers(preset.and_then(|p| p.dol.as_ref()), cfg.dol.as_ref()),
        rvl: DiscDefaults::merge_layers(preset.and_then(|p| p.rvl.as_ref()), cfg.rvl.as_ref()),
        nx: NxDefaults::merge_layers(preset.and_then(|p| p.nx.as_ref()), cfg.nx.as_ref()),
        chd: ChdDefaults::merge_layers(preset.and_then(|p| p.chd.as_ref()), cfg.chd.as_ref()),
        cso: CsoDefaults::merge_layers(preset.and_then(|p| p.cso.as_ref()), cfg.cso.as_ref()),
        wup: WupDefaults::merge_layers(preset.and_then(|p| p.wup.as_ref()), cfg.wup.as_ref()),
        dat: DatDefaults::merge_layers(preset.and_then(|p| p.dat.as_ref()), cfg.dat.as_ref()),
    }
}

pub fn conflict_from_str(s: &str) -> anyhow::Result<ConflictPolicy> {
    match s.to_ascii_lowercase().as_str() {
        "error" => Ok(ConflictPolicy::Error),
        "overwrite" => Ok(ConflictPolicy::Overwrite),
        "skip" => Ok(ConflictPolicy::Skip),
        "rename" => Ok(ConflictPolicy::Rename),
        "overwrite-invalid" => Ok(ConflictPolicy::OverwriteInvalid),
        other => anyhow::bail!(
            "invalid on_conflict value '{other}' in config: expected error, overwrite, skip, rename or overwrite-invalid"
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
        assert_eq!(
            cli_level.or(eff_empty.dol.level).unwrap_or(BUILTIN),
            BUILTIN
        );
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
        assert_eq!(
            conflict_from_str("overwrite-invalid").unwrap(),
            ConflictPolicy::OverwriteInvalid
        );
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
    fn merge_dat_preset_over_config() {
        let cfg = UserConfig {
            dat: Some(DatDefaults {
                api_base: Some("https://config.test/api/v2".into()),
                ..Default::default()
            }),
            ..UserConfig::default()
        };
        let preset = Preset {
            dat: Some(DatDefaults {
                api_base: Some("https://preset.test/api/v2".into()),
                ..Default::default()
            }),
            ..Preset::default()
        };
        let eff = resolve(&cfg, Some(&preset));
        assert_eq!(
            eff.dat.api_base.as_deref(),
            Some("https://preset.test/api/v2")
        );
    }

    #[test]
    fn merge_dat_config_used_when_no_preset() {
        let cfg = UserConfig {
            dat: Some(DatDefaults {
                api_base: Some("https://config.test/api/v2".into()),
                ..Default::default()
            }),
            ..UserConfig::default()
        };
        let eff = resolve(&cfg, None);
        assert_eq!(
            eff.dat.api_base.as_deref(),
            Some("https://config.test/api/v2")
        );
    }

    #[test]
    fn merge_dat_none_when_neither() {
        let eff = resolve(&UserConfig::default(), None);
        assert_eq!(eff.dat.api_base, None);
        assert_eq!(eff.dat.report, None);
    }

    #[test]
    fn merge_dat_field_independence() {
        let cfg = UserConfig {
            dat: Some(DatDefaults {
                api_base: Some("https://config.test/api/v2".into()),
                ..Default::default()
            }),
            ..UserConfig::default()
        };
        let preset = Preset {
            dat: Some(DatDefaults {
                report: Some(PathBuf::from("dat-report.json")),
                ..Default::default()
            }),
            ..Preset::default()
        };
        let eff = resolve(&cfg, Some(&preset));
        assert_eq!(
            eff.dat.api_base.as_deref(),
            Some("https://config.test/api/v2")
        );
        assert_eq!(eff.dat.report, Some(PathBuf::from("dat-report.json")));
    }

    #[test]
    fn merge_dat_checksum_bounds() {
        let cfg = UserConfig {
            dat: Some(DatDefaults {
                input_checksum_min: Some("sha1".into()),
                ..Default::default()
            }),
            ..UserConfig::default()
        };
        let preset = Preset {
            dat: Some(DatDefaults {
                input_checksum_max: Some("md5".into()),
                ..Default::default()
            }),
            ..Preset::default()
        };
        let eff = resolve(&cfg, Some(&preset));
        assert_eq!(eff.dat.input_checksum_min.as_deref(), Some("sha1"));
        assert_eq!(eff.dat.input_checksum_max.as_deref(), Some("md5"));
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
