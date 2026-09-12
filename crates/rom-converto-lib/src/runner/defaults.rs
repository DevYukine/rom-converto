//! Config-file and preset defaults applied to a request before dispatch.

use super::invalid_arg;
use super::models::{RunOptions, RunRequest};
use crate::config::MergeOver;
use anyhow::Result;

pub(crate) fn apply_config_defaults(mut req: RunRequest) -> Result<RunRequest> {
    let config_path = req.config.clone().or_else(|| req.options.config.clone());
    let preset_name = req.preset.clone().or_else(|| req.options.preset.clone());
    let config = crate::config::load_config(config_path.as_deref())
        .map_err(|err| invalid_arg(err.to_string()))?;
    let preset = crate::config::resolve_preset(&config, preset_name.as_deref())
        .map_err(|err| invalid_arg(err.to_string()))?;

    let operation = req.operation.clone();
    match operation.split_once('.').map(|(family, _)| family) {
        Some("dol") => apply_disc_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.dol.as_ref()),
            config.dol.as_ref(),
        ),
        Some("rvl") => apply_disc_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.rvl.as_ref()),
            config.rvl.as_ref(),
        ),
        Some("nx") => apply_nx_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.nx.as_ref()),
            config.nx.as_ref(),
        ),
        Some("chd") => apply_chd_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.chd.as_ref()),
            config.chd.as_ref(),
        ),
        Some("cso") => apply_cso_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.cso.as_ref()),
            config.cso.as_ref(),
        ),
        Some("wup") => apply_wup_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.wup.as_ref()),
            config.wup.as_ref(),
        ),
        Some("dat") => apply_dat_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.dat.as_ref()),
            config.dat.as_ref(),
        ),
        _ if operation == "organize" => apply_organize_defaults(
            &mut req.options,
            preset.as_ref().and_then(|p| p.organize.as_ref()),
            config.organize.as_ref(),
        ),
        _ => {}
    }
    Ok(req)
}

pub(crate) fn apply_disc_defaults(
    options: &mut RunOptions,
    top: Option<&crate::config::DiscDefaults>,
    base: Option<&crate::config::DiscDefaults>,
) {
    let defaults = crate::config::DiscDefaults::merge_layers(top, base);
    fill(&mut options.level, defaults.level);
    fill(&mut options.chunk_size, defaults.chunk_size);
    fill(&mut options.on_conflict, defaults.on_conflict);
    fill(&mut options.output_dir, defaults.output_dir);
    fill(&mut options.report, defaults.report);
}

pub(crate) fn apply_nx_defaults(
    options: &mut RunOptions,
    top: Option<&crate::config::NxDefaults>,
    base: Option<&crate::config::NxDefaults>,
) {
    let defaults = crate::config::NxDefaults::merge_layers(top, base);
    fill(&mut options.level, defaults.level);
    fill(&mut options.mode, defaults.mode);
    fill(
        &mut options.block_size_exp,
        defaults.block_size_exp.map(u32::from),
    );
    fill(&mut options.on_conflict, defaults.on_conflict);
    fill(&mut options.output_dir, defaults.output_dir);
    fill(&mut options.report, defaults.report);
}

pub(crate) fn apply_chd_defaults(
    options: &mut RunOptions,
    top: Option<&crate::config::ChdDefaults>,
    base: Option<&crate::config::ChdDefaults>,
) {
    let defaults = crate::config::ChdDefaults::merge_layers(top, base);
    fill(&mut options.hunk_size, defaults.hunk_size);
    fill(&mut options.on_conflict, defaults.on_conflict);
    fill(&mut options.output_dir, defaults.output_dir);
    fill(&mut options.report, defaults.report);
}

pub(crate) fn apply_cso_defaults(
    options: &mut RunOptions,
    top: Option<&crate::config::CsoDefaults>,
    base: Option<&crate::config::CsoDefaults>,
) {
    let defaults = crate::config::CsoDefaults::merge_layers(top, base);
    fill(&mut options.block_size, defaults.block_size);
    fill(&mut options.on_conflict, defaults.on_conflict);
    fill(&mut options.output_dir, defaults.output_dir);
    fill(&mut options.report, defaults.report);
}

pub(crate) fn apply_wup_defaults(
    options: &mut RunOptions,
    top: Option<&crate::config::WupDefaults>,
    base: Option<&crate::config::WupDefaults>,
) {
    let defaults = crate::config::WupDefaults::merge_layers(top, base);
    fill(&mut options.level, defaults.level);
    fill(&mut options.on_conflict, defaults.on_conflict);
}

pub(crate) fn apply_dat_defaults(
    options: &mut RunOptions,
    top: Option<&crate::config::DatDefaults>,
    base: Option<&crate::config::DatDefaults>,
) {
    let defaults = crate::config::DatDefaults::merge_layers(top, base);
    fill(&mut options.api_base, defaults.api_base);
    fill(&mut options.report, defaults.report);
    fill(&mut options.input_checksum_min, defaults.input_checksum_min);
    fill(&mut options.input_checksum_max, defaults.input_checksum_max);
}

pub(crate) fn apply_organize_defaults(
    options: &mut RunOptions,
    top: Option<&crate::config::OrganizeDefaults>,
    base: Option<&crate::config::OrganizeDefaults>,
) {
    let defaults = crate::config::OrganizeDefaults::merge_layers(top, base);
    fill(&mut options.output_dir, defaults.output_dir);
    fill(&mut options.output_template, defaults.output_template);
    fill(&mut options.on_conflict, defaults.on_conflict);
    fill(&mut options.report, defaults.report);
    fill(&mut options.dat, defaults.dat);
    fill(&mut options.move_source, defaults.move_source);
    fill(&mut options.playlists, defaults.playlists);
}

pub(crate) fn fill<T>(slot: &mut Option<T>, value: Option<T>) {
    if slot.is_none() {
        *slot = value;
    }
}
