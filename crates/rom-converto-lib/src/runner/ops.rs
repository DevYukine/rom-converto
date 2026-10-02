//! Operation table and the per-format handlers behind it: one [`OpSpec`]
//! per dispatchable operation name, plus the shared output-resolution and
//! file-op bookkeeping every convert handler runs through.

use super::dat::{dat_fixdat, dat_identify, dat_rename, dat_scan, dat_verify};
use super::models::{
    COULD_NOT_VERIFY, ComparisonData, HashRow, PlaylistPlanData, PlaylistsData, RunComparisonData,
    RunData, RunOptions, RunPlansData, RunRequest, RunResponse, RunRow, RunStatus, VERIFIED_VALID,
    VerifyReport, VerifyVerdict, WupTitleInputOption,
};
use super::ops_misc::{cue_to_cso, cue_to_iso, ntr_decrypt, ntr_encrypt, nx_merge, nx_split};
use super::ops_ms::{
    xbox_convert, xbox_extract, xenon_compress, xenon_convert, xenon_extract, xenon_verify,
};
use super::ops_sony::{ps3_decrypt, psp_extract, psp_to_iso, vita_extract};
use super::organize::{FileIdentity, organize};
use super::{RUN_SCHEMA, invalid_arg, is_cancelled_error, planned_verb, record_verb};
use crate::cso::{CsoCompressOptions, CsoFormat};
use crate::disc::chd::error::ChdError;
use crate::disc::chd::{ChdCodec, ChdOptions, DiscMode};
use crate::nintendo::disc::legacy::{ALL_MIGRATE_FORMATS, DOL_MIGRATE_FORMATS, MigrateOptions};
use crate::nintendo::disc::rvz::RvzCompressOptions;
use crate::util::fs::{file_len, has_ext};
use crate::util::verify::unverifiable;
use crate::util::{
    CancelToken, Cancelled, ConflictPolicy, ConflictResolution, DEFAULT_SPACE_HEADROOM, FileStatus,
    HashAlgo, OutputExists, OutputVerify, PlanDecision, PlanLine, ProgressReporter, ReportRecord,
    ReportRecordInput, ReportTotals, ResolvedInput, VerifyOutcome, available_space,
    chd_media_label, format_bytes, hash_file, parse_algos, resolve_conflict, resolve_conflict_by,
    space_shortfall, spawn_blocking_with_progress, verify_existing_cached, verify_existing_output,
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(crate) type OpFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<RunResponse>> + 'a>>;

/// One dispatchable operation: its canonical name, the aliases accepted for
/// it, the extensions a recursive run scans (`None` when the operation has
/// no batch mode), what it needs of its input and output, and its handler.
pub(crate) struct OpSpec {
    name: &'static str,
    aliases: &'static [&'static str],
    batch_exts: Option<&'static [&'static str]>,
    /// Extensions an archive input is searched for; `None` reuses
    /// `batch_exts`. A few operations read formats a recursive run does not
    /// scan for: a legacy container or a LaserDisc source is a valid single
    /// input, including as an archive member.
    input_exts: Option<&'static [&'static str]>,
    /// False for read-only operations, which need no output space.
    writes_output: bool,
    /// Bytes the output needs when the source file's own size is the wrong
    /// estimate: a decoded ISO, a merge's inputs, a source tree.
    required_bytes: Option<fn(&RunRequest, &Path) -> u64>,
    run: for<'a> fn(RunRequest, &'a dyn ProgressReporter, CancelToken) -> OpFuture<'a>,
}

pub(crate) static OPS: &[OpSpec] = &[
    OpSpec {
        name: "cso.compress",
        aliases: &[],
        batch_exts: Some(&["iso"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(cso_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "cso.decompress",
        aliases: &[],
        batch_exts: Some(&["cso", "zso", "dax"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(cso_decompress(req, progress, cancel)),
    },
    OpSpec {
        name: "cso.verify",
        aliases: &[],
        batch_exts: Some(&["cso", "zso", "dax"]),
        input_exts: None,
        writes_output: false,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(cso_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "cso.to_chd",
        aliases: &["cso.to-chd"],
        batch_exts: Some(&["cso", "zso", "dax"]),
        input_exts: None,
        writes_output: true,
        required_bytes: Some(|_, source| {
            crate::cso::info::read_info(source)
                .map(|info| info.uncompressed_size)
                .unwrap_or_else(|_| file_len(source))
        }),
        run: |req, progress, cancel| Box::pin(cso_to_chd(req, progress, cancel)),
    },
    OpSpec {
        name: "chd.compress",
        aliases: &[],
        batch_exts: Some(&["iso", "cue"]),
        input_exts: Some(&["iso", "cue", "avi"]),
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(chd_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "chd.migrate",
        aliases: &[],
        batch_exts: Some(&["chd"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(chd_migrate(req, progress, cancel)),
    },
    OpSpec {
        name: "chd.extract",
        aliases: &[],
        batch_exts: Some(&["chd"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(chd_extract(req, progress, cancel)),
    },
    OpSpec {
        name: "chd.verify",
        aliases: &[],
        batch_exts: Some(&["chd"]),
        input_exts: None,
        writes_output: false,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(chd_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "chd.to_cso",
        aliases: &["chd.to-cso"],
        batch_exts: Some(&["chd"]),
        input_exts: None,
        writes_output: true,
        required_bytes: Some(|_, source| {
            crate::disc::chd::info::read_info(source)
                .map(|info| info.logical_bytes)
                .unwrap_or_else(|_| file_len(source))
        }),
        run: |req, progress, cancel| Box::pin(chd_to_cso(req, progress, cancel)),
    },
    OpSpec {
        name: "dol.compress",
        aliases: &[],
        batch_exts: Some(&["iso", "gcm"]),
        input_exts: Some(&["iso", "gcm", "gcz"]),
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(rvz_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "dol.decompress",
        aliases: &[],
        batch_exts: Some(&["rvz"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(rvz_decompress(req, progress, cancel)),
    },
    OpSpec {
        name: "dol.migrate",
        aliases: &[],
        batch_exts: Some(&["gcz", "iso"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| {
            Box::pin(migrate_disc(req, progress, cancel, DOL_MIGRATE_FORMATS))
        },
    },
    OpSpec {
        name: "dol.verify",
        aliases: &[],
        batch_exts: Some(&["iso", "gcm", "rvz", "gcz"]),
        input_exts: None,
        writes_output: false,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(dol_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "rvl.compress",
        aliases: &[],
        batch_exts: Some(&["iso", "wbfs"]),
        input_exts: Some(&["iso", "wbfs", "gcz", "wia"]),
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(rvz_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "rvl.decompress",
        aliases: &[],
        batch_exts: Some(&["rvz"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(rvz_decompress(req, progress, cancel)),
    },
    OpSpec {
        name: "rvl.migrate",
        aliases: &[],
        batch_exts: Some(&["wia", "gcz", "iso"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| {
            Box::pin(migrate_disc(req, progress, cancel, ALL_MIGRATE_FORMATS))
        },
    },
    OpSpec {
        name: "rvl.verify",
        aliases: &[],
        batch_exts: Some(&["iso", "wbfs", "rvz", "wia", "gcz"]),
        input_exts: None,
        writes_output: false,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(rvl_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "rvz.compress",
        aliases: &[],
        batch_exts: Some(&["iso", "gcm", "wbfs"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(rvz_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "rvz.decompress",
        aliases: &[],
        batch_exts: Some(&["rvz"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(rvz_decompress(req, progress, cancel)),
    },
    OpSpec {
        name: "rvz.migrate",
        aliases: &[],
        batch_exts: Some(&["wia", "gcz", "iso"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| {
            Box::pin(migrate_disc(req, progress, cancel, ALL_MIGRATE_FORMATS))
        },
    },
    OpSpec {
        name: "ctr.cdn_to_cia",
        aliases: &["ctr.cdn-to-cia"],
        batch_exts: None,
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(ctr_cdn_to_cia(req, progress, cancel)),
    },
    OpSpec {
        name: "ctr.generate_cdn_ticket",
        aliases: &["ctr.generate-cdn-ticket"],
        batch_exts: None,
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(ctr_generate_cdn_ticket(req, progress, cancel)),
    },
    OpSpec {
        name: "ctr.decrypt",
        aliases: &[],
        batch_exts: Some(&["cia", "3ds", "cci", "cxi"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(ctr_decrypt(req, progress, cancel)),
    },
    OpSpec {
        name: "ctr.encrypt",
        aliases: &[],
        batch_exts: Some(&["cia", "3ds", "cci", "cxi"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(ctr_encrypt(req, progress, cancel)),
    },
    OpSpec {
        name: "ctr.compress",
        aliases: &[],
        batch_exts: Some(&["cia", "cci", "3ds", "cxi", "3dsx"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(ctr_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "ctr.decompress",
        aliases: &[],
        batch_exts: Some(&["zcia", "zcci", "zcxi", "z3dsx"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(ctr_decompress(req, progress, cancel)),
    },
    OpSpec {
        name: "ctr.convert",
        aliases: &[],
        batch_exts: Some(&["cia", "3ds", "cci"]),
        input_exts: None,
        writes_output: true,
        required_bytes: Some(|req, source| {
            crate::nintendo::ctr::convert::converted_size(source, req.options.trim.unwrap_or(false))
                .unwrap_or_else(|_| file_len(source))
        }),
        run: |req, progress, cancel| Box::pin(ctr_convert(req, progress, cancel)),
    },
    OpSpec {
        name: "ctr.verify",
        aliases: &[],
        batch_exts: Some(&["cia", "3ds", "cci", "cxi", "zcia", "zcci", "zcxi"]),
        input_exts: None,
        writes_output: false,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(ctr_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "nx.compress",
        aliases: &[],
        batch_exts: Some(&["nsp", "xci", "nca"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(nx_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "nx.decompress",
        aliases: &[],
        batch_exts: Some(&["nsz", "xcz", "ncz"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(nx_decompress(req, progress, cancel)),
    },
    OpSpec {
        name: "nx.decrypt",
        aliases: &[],
        batch_exts: Some(&["nsp", "xci"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(nx_decrypt(req, progress, cancel)),
    },
    OpSpec {
        name: "nx.verify",
        aliases: &[],
        batch_exts: Some(&["nsp", "xci", "nca", "nsz", "xcz", "ncz", "dnsp", "dxci"]),
        input_exts: None,
        writes_output: false,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(nx_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "wup.compress",
        aliases: &[],
        batch_exts: Some(&["wud", "wux"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(wup_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "wup.decrypt",
        aliases: &[],
        batch_exts: None,
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(wup_decrypt(req, progress, cancel)),
    },
    OpSpec {
        name: "wup.verify",
        aliases: &[],
        batch_exts: Some(&["wud", "wux", "wua"]),
        input_exts: None,
        writes_output: false,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(wup_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "wup.to_wux",
        aliases: &["wup.to-wux"],
        batch_exts: Some(&["wud"]),
        input_exts: None,
        writes_output: true,
        required_bytes: Some(|_, source| {
            // A continuation part is converted with its game_part1.wud, so
            // it needs no output space of its own.
            if crate::nintendo::wup::disc::is_split_continuation(source) {
                0
            } else {
                crate::nintendo::wup::disc::logical_disc_size(source)
                    .unwrap_or_else(|_| file_len(source))
            }
        }),
        run: |req, progress, cancel| Box::pin(wup_to_wux(req, progress, cancel)),
    },
    OpSpec {
        name: "wup.to_wud",
        aliases: &["wup.to-wud"],
        batch_exts: Some(&["wux"]),
        input_exts: None,
        writes_output: true,
        required_bytes: Some(|_, source| {
            crate::nintendo::wup::disc::logical_disc_size(source)
                .unwrap_or_else(|_| file_len(source))
        }),
        run: |req, progress, cancel| Box::pin(wup_to_wud(req, progress, cancel)),
    },
    OpSpec {
        name: "cue.merge",
        aliases: &[],
        batch_exts: Some(&["cue"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(cue_merge(req, progress, cancel)),
    },
    OpSpec {
        name: "cue.to_iso",
        aliases: &["cue.to-iso"],
        batch_exts: Some(&["cue"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(cue_to_iso(req, progress, cancel)),
    },
    OpSpec {
        name: "cue.to_cso",
        aliases: &["cue.to-cso"],
        batch_exts: Some(&["cue"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(cue_to_cso(req, progress, cancel)),
    },
    OpSpec {
        name: "ntr.encrypt",
        aliases: &["nds.encrypt"],
        batch_exts: Some(&["nds", "dsi"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(ntr_encrypt(req, progress, cancel)),
    },
    OpSpec {
        name: "ntr.decrypt",
        aliases: &["nds.decrypt"],
        batch_exts: Some(&["nds", "dsi"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(ntr_decrypt(req, progress, cancel)),
    },
    OpSpec {
        name: "nx.merge",
        aliases: &[],
        batch_exts: None,
        input_exts: None,
        writes_output: true,
        required_bytes: Some(|req, _| {
            req.options
                .inputs
                .iter()
                .flatten()
                .map(|input| match input {
                    WupTitleInputOption::Path(path) | WupTitleInputOption::Object { path, .. } => {
                        file_len(path)
                    }
                })
                .sum()
        }),
        run: |req, progress, cancel| Box::pin(nx_merge(req, progress, cancel)),
    },
    OpSpec {
        name: "nx.split",
        aliases: &[],
        batch_exts: None,
        input_exts: Some(&["nsp", "xci"]),
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(nx_split(req, progress, cancel)),
    },
    OpSpec {
        name: "ps3.decrypt",
        aliases: &[],
        batch_exts: Some(&["iso"]),
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(ps3_decrypt(req, progress, cancel)),
    },
    OpSpec {
        name: "psp.to_iso",
        aliases: &["psp.to-iso"],
        batch_exts: None,
        input_exts: Some(&["pbp", "pkg"]),
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(psp_to_iso(req, progress, cancel)),
    },
    OpSpec {
        name: "psp.extract",
        aliases: &[],
        batch_exts: None,
        input_exts: Some(&["pbp"]),
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(psp_extract(req, progress, cancel)),
    },
    OpSpec {
        name: "vita.extract",
        aliases: &[],
        batch_exts: None,
        input_exts: Some(&["pkg"]),
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(vita_extract(req, progress, cancel)),
    },
    OpSpec {
        name: "xbox.convert",
        aliases: &[],
        batch_exts: None,
        input_exts: Some(&["iso"]),
        writes_output: true,
        required_bytes: Some(|_, source| {
            if !source.is_dir() {
                return file_len(source);
            }
            crate::microsoft::xbox::input_total_bytes(source).unwrap_or_else(|_| file_len(source))
        }),
        run: |req, progress, cancel| Box::pin(xbox_convert(req, progress, cancel)),
    },
    OpSpec {
        name: "xbox.extract",
        aliases: &[],
        batch_exts: None,
        input_exts: Some(&["xiso", "iso"]),
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(xbox_extract(req, progress, cancel)),
    },
    OpSpec {
        name: "xenon.compress",
        aliases: &[],
        batch_exts: None,
        input_exts: Some(&["iso"]),
        writes_output: true,
        required_bytes: Some(|_, source| {
            if !source.is_dir() {
                return file_len(source);
            }
            crate::microsoft::xenon::total_input_bytes(source).unwrap_or_else(|_| file_len(source))
        }),
        run: |req, progress, cancel| Box::pin(xenon_compress(req, progress, cancel)),
    },
    OpSpec {
        name: "xenon.extract",
        aliases: &[],
        batch_exts: None,
        input_exts: Some(&["zar"]),
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(xenon_extract(req, progress, cancel)),
    },
    OpSpec {
        name: "xenon.convert",
        aliases: &[],
        batch_exts: None,
        input_exts: Some(&["iso"]),
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(xenon_convert(req, progress, cancel)),
    },
    OpSpec {
        name: "xenon.verify",
        aliases: &[],
        batch_exts: None,
        input_exts: Some(&["zar"]),
        writes_output: false,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(xenon_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "playlist.write",
        aliases: &["playlist"],
        batch_exts: None,
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(playlist_write(req, progress, cancel)),
    },
    OpSpec {
        name: "dat.verify",
        aliases: &[],
        batch_exts: None,
        input_exts: None,
        writes_output: false,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(dat_verify(req, progress, cancel)),
    },
    OpSpec {
        name: "dat.scan",
        aliases: &[],
        batch_exts: None,
        input_exts: None,
        writes_output: false,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(dat_scan(req, progress, cancel)),
    },
    OpSpec {
        name: "dat.rename",
        aliases: &[],
        batch_exts: None,
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(dat_rename(req, progress, cancel)),
    },
    OpSpec {
        name: "dat.identify",
        aliases: &[],
        batch_exts: None,
        input_exts: None,
        writes_output: false,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(dat_identify(req, progress, cancel)),
    },
    OpSpec {
        name: "dat.fixdat",
        aliases: &[],
        batch_exts: None,
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(dat_fixdat(req, progress, cancel)),
    },
    OpSpec {
        name: "hash",
        aliases: &[],
        batch_exts: Some(&[
            "iso", "gcm", "wbfs", "rvz", "gcz", "wia", "nkit", "chd", "cso", "zso", "dax", "cue",
            "cia", "3ds", "cci", "cxi", "3dsx", "zcia", "zcci", "zcxi", "z3dsx", "nsp", "xci",
            "nca", "nsz", "xcz", "ncz", "dnsp", "dxci", "wud", "wux",
        ]),
        input_exts: None,
        writes_output: false,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(hash(req, progress, cancel)),
    },
    OpSpec {
        name: "organize",
        aliases: &[],
        batch_exts: None,
        input_exts: None,
        writes_output: true,
        required_bytes: None,
        run: |req, progress, cancel| Box::pin(organize(req, progress, cancel)),
    },
    OpSpec {
        name: "info",
        aliases: &["info.read"],
        batch_exts: None,
        input_exts: None,
        writes_output: false,
        required_bytes: None,
        run: |req, _progress, _cancel| Box::pin(std::future::ready(info(req))),
    },
];

impl OpSpec {
    fn input_exts(&self) -> &'static [&'static str] {
        self.input_exts.or(self.batch_exts).unwrap_or(&[])
    }
}

pub(crate) fn find_op(operation: &str) -> Option<&'static OpSpec> {
    OPS.iter()
        .find(|op| op.name == operation || op.aliases.contains(&operation))
}

/// Every dispatchable operation name, canonical names and aliases alike.
pub(crate) fn operation_names() -> &'static [&'static str] {
    static NAMES: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
        OPS.iter()
            .flat_map(|op| std::iter::once(op.name).chain(op.aliases.iter().copied()))
            .collect()
    });
    NAMES.as_slice()
}

pub(crate) async fn run_single_request(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let Some(op) = find_op(&req.operation) else {
        let other = &req.operation;
        return Err(invalid_arg(format!("unknown operation {other:?}")));
    };
    (op.run)(req, progress, cancel).await
}

pub(crate) async fn run_batch_request(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let root = required_input(&req)?;
    // A recursive run staged onto a single file still processes that file.
    if !root.is_dir() {
        return run_single_request(req, progress, cancel).await;
    }
    let op = find_op(&req.operation)
        .ok_or_else(|| invalid_arg(format!("unknown operation {:?}", req.operation)))?;
    // A CDN dump is a tree of title directories, not of files, so its batch
    // form enumerates the subdirectories instead of scanning for extensions.
    let (files, wanted) = if op.name == "ctr.cdn_to_cia" {
        (cdn_content_dirs(&root)?, "title directories".to_string())
    } else {
        let exts = batch_exts(&req.operation)?;
        let files =
            crate::util::fs::collect_files_with_exts(&root, exts, req.options.max_depth, &cancel)
                .with_context(|| format!("scanning {}", root.display()))?;
        (files, format!("files with extensions {}", exts.join(", ")))
    };
    if files.is_empty() {
        return Err(invalid_arg(format!(
            "no {wanted} found in {}",
            root.display()
        )));
    }

    if op.writes_output && !req.dry_run && !req.options.skip_space_check.unwrap_or(false) {
        let required = files
            .iter()
            .map(|p| {
                op.required_bytes
                    .map_or_else(|| file_len(p), |estimate| estimate(&req, p))
            })
            .sum();
        preflight_space(req.options.output_dir.as_deref().unwrap_or(&root), required)?;
    }

    let started = Instant::now();
    progress.batch_start(files.len() as u64, files.iter().map(|p| file_len(p)).sum());
    let mut records = Vec::new();
    let mut plans = Vec::new();
    let mut hashes = Vec::new();
    let child_options = child_options(&req.options);
    for input in files {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let mut child = req.clone();
        child.input = Some(input.clone());
        child.output = None;
        child.options = child_options.clone();
        child.options.output_dir = mirrored_output_dir(&req.options, &root, &input);
        let mut new_records = Vec::new();
        match run_single_request(child, progress, cancel.clone()).await {
            Ok(mut response) => {
                match response.data.take() {
                    Some(RunData::Plan(line)) if req.dry_run => {
                        progress.row(&RunRow::Plan(line.clone()));
                        plans.push(line);
                    }
                    Some(RunData::Hash(digests)) => {
                        let row = HashRow {
                            path: input.clone(),
                            digests,
                        };
                        progress.row(&RunRow::Hash(row.clone()));
                        hashes.push(row);
                    }
                    _ => {}
                }
                if response.records.is_empty() {
                    new_records.push(ReportRecord::new(ReportRecordInput {
                        input_path: input.display().to_string(),
                        output_path: String::new(),
                        operation: planned_verb(&req.operation, req.dry_run),
                        status: FileStatus::Ok,
                        input_bytes: file_len(&input),
                        output_bytes: 0,
                        elapsed_ms: 0,
                        error: None,
                    }));
                } else {
                    new_records.append(&mut response.records);
                }
            }
            Err(_) if cancel.is_cancelled() => return Err(Cancelled.into()),
            Err(err) if is_cancelled_error(&err) => return Err(err),
            // An existing output under `on_conflict error` skips that file and
            // leaves the run passing, the way the CLI's batches always did.
            Err(err) if OutputExists::in_chain(&err) => {
                progress.warn(&err.to_string());
                new_records.push(ReportRecord::new(ReportRecordInput {
                    input_path: input.display().to_string(),
                    output_path: String::new(),
                    operation: planned_verb(&req.operation, req.dry_run),
                    status: FileStatus::Skipped,
                    input_bytes: file_len(&input),
                    output_bytes: 0,
                    elapsed_ms: 0,
                    error: Some(err.to_string()),
                }));
            }
            Err(err) => new_records.push(ReportRecord::new(ReportRecordInput {
                input_path: input.display().to_string(),
                output_path: String::new(),
                operation: planned_verb(&req.operation, req.dry_run),
                status: FileStatus::Failed,
                input_bytes: file_len(&input),
                output_bytes: 0,
                elapsed_ms: 0,
                error: Some(err.to_string()),
            })),
        }
        for record in new_records {
            progress.row(&RunRow::Record(record.clone()));
            records.push(record);
        }
        progress.batch_advance(file_len(&input));
    }

    let totals = totals_for_records(&records, elapsed_ms(started));
    let status = if totals.failed == 0 {
        RunStatus::Ok
    } else if totals.ok == 0 && totals.skipped == 0 {
        RunStatus::Failed
    } else {
        RunStatus::PartialFailure
    };
    Ok(RunResponse {
        schema: RUN_SCHEMA,
        ok: status == RunStatus::Ok,
        status: status.as_i32(),
        code: status.code().to_string(),
        message: batch_message(&totals),
        details: None,
        totals: Some(totals),
        records,
        events: Vec::new(),
        data: if !plans.is_empty() {
            Some(RunData::Plans(RunPlansData { plans }))
        } else if !hashes.is_empty() {
            Some(RunData::Hashes(hashes))
        } else {
            None
        },
    })
}

/// The CDN title directories of a recursive `ctr.cdn_to_cia` run: every
/// immediate subdirectory of `root` that is not OS junk.
fn cdn_content_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = std::fs::read_dir(root)
        .with_context(|| format!("scanning {}", root.display()))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_none_or(|name| !crate::util::fs::is_os_junk_dir(name))
        })
        .collect::<Vec<_>>();
    dirs.sort();
    Ok(dirs)
}

pub(crate) async fn cso_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let format = cso_format(req.options.format.as_deref().unwrap_or("cso"))?;
    let input = required_input(&req)?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| basis.with_extension(format.extension()),
            operation: "cso.compress",
            verify: OutputVerify::Cso,
        },
        cancel,
        |input, output, cancel| async move {
            let opts = CsoCompressOptions {
                format,
                block_size: req.options.block_size,
                force: true,
            };
            crate::cso::compress_to_cso(progress, input, output, opts, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn cso_decompress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| basis.with_extension("iso"),
            operation: "cso.decompress",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::cso::decompress_from_cso(progress, input, output, true, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn cso_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    crate::cso::verify_cso(
        progress,
        input.clone(),
        req.options.full.unwrap_or(true),
        cancel,
    )
    .await?;
    Ok(RunResponse::ok("CSO verification passed.", None))
}

pub(crate) async fn chd_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| basis.with_extension("chd"),
            operation: "chd.compress",
            verify: OutputVerify::Chd,
        },
        cancel,
        |input, output, cancel| async move {
            let opts = chd_options(req)?;
            let mode = disc_mode(req.options.mode.as_deref())?;
            crate::disc::chd::convert_disc_to_chd(progress, input, output, mode, opts, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn chd_migrate(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    // A migrated CHD keeps the .chd extension, so the derived name carries a
    // v5 infix to stay off its own source.
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::disc::chd::migrated_chd_path(basis),
            operation: "chd.migrate",
            verify: OutputVerify::Chd,
        },
        cancel,
        |input, output, cancel| async move {
            crate::disc::chd::migrate_chd_to_v5(progress, input, output, chd_options(req)?, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn chd_extract(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    // The derived extension comes from the staged source, not the output
    // basis: an archive's basis names a file that does not exist yet.
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            derive: &|basis, source| basis.with_extension(chd_extract_ext(source)),
            operation: "chd.extract",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::disc::chd::extract_from_chd(
                progress,
                input,
                output,
                req.options.parent.clone(),
                cancel,
            )
            .await
            .map_err(anyhow::Error::from)
        },
    )
    .await
}

/// Derived output extension: DVD restores `.iso`, CD `.cue` beside its `.bin`.
/// LaserDisc and unreadable inputs get no extension here; `chd_extract_target`
/// rejects them before planning.
fn chd_extract_ext(input: &Path) -> &'static str {
    crate::disc::chd::reader::open_chd_sync(input)
        .and_then(|handle| crate::disc::chd::extract::extract_ext(handle.flavor()))
        .unwrap_or("")
}

/// An explicit path keeps its extension or gets the missing mode extension;
/// a contradicting extension is refused. A derived name appends it unless present.
/// A target or any CD track bin resolving to the input is rejected as an invalid argument.
fn chd_extract_target(
    output: PathBuf,
    source: &Path,
    input: &Path,
    explicit: bool,
) -> Result<PathBuf> {
    let flavor = crate::disc::chd::reader::open_chd_sync(source)
        .map_err(|err| anyhow::anyhow!("opening {}: {err}", source.display()))?
        .flavor();
    let ext = crate::disc::chd::extract::extract_ext(flavor)?;
    let target = if explicit {
        if output.is_dir() {
            return Err(invalid_arg(format!(
                "{} is a directory; set output_dir (--output-dir <DIR>) to extract into it",
                output.display()
            )));
        }
        crate::disc::chd::extract::extract_target(flavor, output).map_err(|err| match err {
            ChdError::CdExtractNeedsCue(_) | ChdError::DvdExtractNeedsImage(_) => {
                invalid_arg(err.to_string())
            }
            _ => err.into(),
        })?
    } else if output
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case(ext))
    {
        output
    } else {
        let mut output = output.into_os_string();
        output.push(".");
        output.push(ext);
        output.into()
    };
    if let Ok(input_identity) = FileIdentity::probe(input) {
        let bins = (flavor == crate::disc::chd::reader::ChdFlavor::Cd)
            .then(|| crate::disc::chd::extract::possible_track_bins(&target));
        for candidate in std::iter::once(std::borrow::Cow::Borrowed(target.as_path()))
            .chain(bins.into_iter().flatten().map(std::borrow::Cow::Owned))
        {
            if FileIdentity::probe(&candidate).is_ok_and(|identity| identity == input_identity) {
                return Err(invalid_arg(format!(
                    "{} is the input file; extracting would overwrite it",
                    candidate.display()
                )));
            }
        }
    }
    Ok(target)
}

pub(crate) async fn chd_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    crate::disc::chd::verify_chd(
        progress,
        input,
        req.options.parent.clone(),
        req.options.fix.unwrap_or(false),
        cancel,
    )
    .await?;
    Ok(RunResponse::ok("CHD verification passed.", None))
}

pub(crate) async fn cso_to_chd(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| basis.with_extension("chd"),
            operation: "cso.to_chd",
            verify: OutputVerify::Chd,
        },
        cancel,
        |input, output, cancel| async move {
            let opts = chd_options(req)?;
            crate::pipeline::cso_to_chd(
                progress,
                input,
                output,
                disc_mode(req.options.mode.as_deref())?,
                opts,
                cancel,
            )
            .await
        },
    )
    .await
}

pub(crate) async fn chd_to_cso(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let format = cso_format(req.options.format.as_deref().unwrap_or("cso"))?;
    let input = required_input(&req)?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| basis.with_extension(format.extension()),
            operation: "chd.to_cso",
            verify: OutputVerify::Cso,
        },
        cancel,
        |input, output, cancel| async move {
            let opts = CsoCompressOptions {
                format,
                block_size: req.options.block_size,
                force: true,
            };
            crate::pipeline::chd_to_cso(progress, input, output, opts, cancel).await
        },
    )
    .await
}

pub(crate) async fn rvz_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::nintendo::disc::rvz::derive_rvz_path(basis),
            operation: &req.operation,
            verify: OutputVerify::Rvz,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::disc::rvz::compress_disc(
                &input,
                &output,
                rvz_options(req),
                progress,
                cancel,
            )
            .await
            .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn rvz_decompress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::nintendo::disc::rvz::derive_disc_path(basis),
            operation: &req.operation,
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            if has_ext(&output, "wbfs") {
                crate::nintendo::disc::rvz::decompress_disc_to_wbfs(
                    &input, &output, progress, cancel,
                )
                .await
                .map_err(anyhow::Error::from)
            } else {
                crate::nintendo::disc::rvz::decompress_disc(&input, &output, progress, cancel)
                    .await
                    .map_err(anyhow::Error::from)
            }
        },
    )
    .await
}

pub(crate) async fn migrate_disc(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
    allowed: &'static [crate::nintendo::disc::legacy::LegacyFormat],
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    // A real run gates inside the lib; a dry run never gets there, so it has
    // to refuse a non-legacy or wrong-console input before planning a write.
    if req.dry_run {
        match crate::nintendo::disc::legacy::detect_legacy_format(&input)? {
            None => anyhow::bail!(
                "input is not a GCZ, WIA, or NKit image; use compress for .iso/.gcm/.wbfs"
            ),
            Some(format) => crate::nintendo::disc::legacy::ensure_format_allowed(format, allowed)?,
        }
    }
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::nintendo::disc::rvz::derive_rvz_path(basis),
            operation: &req.operation,
            verify: OutputVerify::Rvz,
        },
        cancel,
        |input, output, cancel| async move {
            let migrate = MigrateOptions {
                skip_verify: req.options.skip_verify.unwrap_or(false),
                deep_verify: req.options.deep.unwrap_or(false)
                    || req.options.deep_verify.unwrap_or(false),
            };
            crate::nintendo::disc::legacy::migrate_disc(
                &input,
                &output,
                rvz_options(req),
                migrate,
                allowed,
                progress,
                cancel,
            )
            .await
            .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn hash(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let algos = req
        .options
        .algo
        .as_deref()
        .map(|s| parse_algos(s).map_err(invalid_arg))
        .transpose()?
        .unwrap_or_else(|| vec![HashAlgo::Crc32, HashAlgo::Sha1]);
    let cache = req.ctx.hash_cache.as_deref();
    let digest = match cache.and_then(|c| c.lookup_raw(&input, &algos)) {
        Some(digest) => digest,
        None => {
            let digest = hash_file(&input, &algos, progress, &cancel)?;
            if let Some(cache) = cache {
                cache.store_raw(&input, &digest);
            }
            digest
        }
    };
    Ok(RunResponse::ok(
        "Hash complete.",
        Some(RunData::Hash(digest)),
    ))
}

pub(crate) async fn ctr_decrypt(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::nintendo::ctr::derive_decrypted_path(basis),
            operation: "ctr.decrypt",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::ctr::decrypt_rom(&input, &output, progress, cancel).await
        },
    )
    .await
}

pub(crate) async fn ctr_encrypt(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::nintendo::ctr::derive_encrypted_path(basis),
            operation: "ctr.encrypt",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::ctr::encrypt_rom(&input, &output, progress, cancel).await
        },
    )
    .await
}

pub(crate) async fn ctr_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::nintendo::ctr::z3ds::derive_compressed_path(basis),
            operation: "ctr.compress",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::ctr::z3ds::compress_rom(
                &input,
                &output,
                req.options.level,
                req.options.allow_encrypted.unwrap_or(false),
                progress,
                cancel,
            )
            .await
            .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn ctr_decompress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::nintendo::ctr::z3ds::derive_decompressed_path(basis),
            operation: "ctr.decompress",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::ctr::z3ds::decompress_rom(&input, &output, progress, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn ctr_convert(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::nintendo::ctr::convert::derive_converted_path(basis),
            operation: "ctr.convert",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::ctr::convert::convert_rom(
                &input,
                &output,
                req.options.trim.unwrap_or(false),
                progress,
                cancel,
            )
            .await
        },
    )
    .await
}

pub(crate) async fn ctr_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result = crate::nintendo::ctr::verify::verify_ctr(
        &input,
        &crate::nintendo::ctr::verify::CtrVerifyOptions {
            verify_content_hashes: req.options.content_hashes.unwrap_or(false),
        },
        progress,
        &cancel,
    )
    .await?;
    Ok(RunResponse::ok(
        "CTR verification complete.",
        Some(RunData::CtrVerify(result)),
    ))
}

pub(crate) async fn ctr_cdn_to_cia(
    mut req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    if req.options.output_dir.is_none() {
        req.options.output_dir = req.options.output_dir_cia.clone();
    }
    let input = required_input(&req)?;
    let cia_output = output_or(&req, &input, || {
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("title");
        input.with_file_name(format!("{name}.cia"))
    })?;
    let compress = req.options.compress.unwrap_or(false);
    let output = if compress {
        crate::nintendo::ctr::z3ds::derive_compressed_path(&cia_output)
    } else {
        cia_output.clone()
    };
    let operation = "ctr.cdn_to_cia";
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &output,
        operation,
        OutputVerify::None,
        &cancel,
    )
    .await?;
    let Some(resolved) = plan.output else {
        return Ok(skipped(&input, &output, operation, None));
    };
    if let Some(line) = plan.line {
        return Ok(planned(line));
    }
    let policy = conflict_policy(&req)?;
    let opts = crate::nintendo::ctr::CdnToCiaOptions {
        cdn_dir: input.clone(),
        output: Some(cia_output),
        cleanup: req.options.cleanup.unwrap_or(false),
        recursive: false,
        ensure_ticket_exists: req.options.ensure_ticket_exists.unwrap_or(false),
        decrypt: req.options.decrypt.unwrap_or(false),
        compress,
        output_dir: req.options.output_dir.clone(),
        // The runner settled the conflict already; `overwrite-invalid` would
        // make the conversion skip the rewrite the plan just approved.
        on_conflict: match policy {
            ConflictPolicy::OverwriteInvalid => ConflictPolicy::Overwrite,
            other => other,
        },
    };
    run_file_op(&input, &resolved, operation, file_len, || async {
        crate::nintendo::ctr::convert_cdn_to_cia(opts, progress, progress, cancel).await
    })
    .await
}

pub(crate) async fn ctr_generate_cdn_ticket(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = output_or(&req, &input, || input.join("ticket.tik"))?;
    let operation = "ctr.generate_cdn_ticket";
    let plan = prepare_output(
        progress,
        &req,
        &input,
        &desired,
        operation,
        OutputVerify::None,
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, operation, None));
    };
    if let Some(line) = plan.line {
        return Ok(planned(line));
    }
    crate::nintendo::ctr::generate_ticket_from_cdn_with_publish(
        &input,
        &output,
        &cancel,
        conflict_policy(&req)? == ConflictPolicy::Overwrite,
    )
    .await?;
    Ok(
        RunResponse::ok("CDN ticket generated.", None).with_record(ReportRecord::new(
            ReportRecordInput {
                input_path: input.display().to_string(),
                output_path: output.display().to_string(),
                operation: record_verb("ctr.generate_cdn_ticket"),
                status: FileStatus::Ok,
                input_bytes: file_len(&input),
                output_bytes: file_len(&output),
                elapsed_ms: 0,
                error: None,
            },
        )),
    )
}

pub(crate) async fn dol_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result = crate::nintendo::dol::verify::verify_dol(
        &input,
        &crate::nintendo::dol::verify::DolVerifyOptions {
            full: req.options.full.unwrap_or(false),
        },
        progress,
        &cancel,
    )?;
    Ok(RunResponse::ok(
        "DOL verification complete.",
        Some(RunData::DolVerify(result)),
    ))
}

pub(crate) async fn rvl_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result = crate::nintendo::rvl::verify::verify_rvl(
        &input,
        &crate::nintendo::rvl::verify::RvlVerifyOptions {
            full: req.options.full.unwrap_or(false),
        },
        progress,
        &cancel,
    )?;
    Ok(RunResponse::ok(
        "RVL verification complete.",
        Some(RunData::RvlVerify(result)),
    ))
}

pub(crate) async fn nx_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = required_input(req)?;
    let (keys, missing_keys) = nx_keys_for_run(req)?;
    let mut response = convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::nintendo::nx::derive_compressed_path(basis),
            operation: "nx.compress",
            verify: OutputVerify::Nx(Box::new(keys.clone())),
        },
        cancel,
        |input, output, cancel| async move {
            let kind = crate::nintendo::nx::detect_container(&input)?;
            let mut opts = crate::nintendo::nx::NxCompressOptions::for_kind(kind);
            if let Some(level) = req.options.level {
                opts.level = level;
            }
            if let Some(mode) = req.options.mode.as_deref() {
                opts.mode = nx_mode(mode, req.options.block_size_exp)?;
            }
            crate::nintendo::nx::compress_container_async(
                input, output, opts, keys, progress, cancel,
            )
            .await
            .map_err(anyhow::Error::from)
        },
    )
    .await?;
    if let Some(RunData::Plan(line)) = &mut response.data {
        line.missing_keys = missing_keys;
    }
    Ok(response)
}

pub(crate) async fn nx_decompress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let keys = crate::nintendo::nx::load_keyset(req.options.keys.as_deref())?;
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::nintendo::nx::derive_decompressed_path(basis),
            operation: "nx.decompress",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::nx::decompress_container_async(input, output, keys, progress, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn nx_decrypt(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let (keys, missing_keys) = nx_keys_for_run(&req)?;
    let mut response = convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::nintendo::nx::derive_decrypted_path(basis),
            operation: "nx.decrypt",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::nx::decrypt_container_async(input, output, keys, progress, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await?;
    if let Some(RunData::Plan(line)) = &mut response.data {
        line.missing_keys = missing_keys;
    }
    Ok(response)
}

pub(crate) async fn nx_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let keys = crate::nintendo::nx::load_keyset(req.options.keys.as_deref())?;
    let result = crate::nintendo::nx::verify_container_async(input, keys, progress, cancel).await?;
    Ok(RunResponse::ok(
        "NX verification complete.",
        Some(RunData::NxVerify(result)),
    ))
}

pub(crate) async fn wup_compress(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let req = &req;
    let input = req
        .input
        .clone()
        .or_else(|| first_wup_input(req))
        .ok_or_else(|| invalid_arg("input path is required"))?;
    convert_op(
        progress,
        req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| crate::nintendo::wup::derive_wua_path(basis),
            operation: "wup.compress",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            let mut opts = crate::nintendo::wup::WupCompressOptions::default();
            if let Some(level) = req.options.level {
                opts.zstd_level = level;
            }
            let titles = wup_titles(req, &input)?;
            crate::nintendo::wup::compress_titles_async(titles, output, opts, progress, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn wup_decrypt(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let desired = req
        .output
        .clone()
        .ok_or_else(|| invalid_arg("output path is required"))?;
    let plan = prepare_output_dir(&req, &input, &desired, "wup.decrypt")?;
    let Some(output) = plan.output else {
        return Ok(skipped(&input, &desired, "wup.decrypt", None));
    };
    if let Some(line) = plan.line {
        return Ok(planned(line));
    }
    let started = Instant::now();
    crate::nintendo::wup::decrypt_nus_title_async(input.clone(), output.clone(), progress, cancel)
        .await?;
    Ok(
        RunResponse::ok("WUP decrypt complete.", None).with_record(dir_op_record(
            &input,
            &output,
            "wup.decrypt",
            0,
            started,
        )),
    )
}

pub(crate) async fn wup_verify(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let result =
        crate::nintendo::wup::verify_wup_async(input, req.options.key.clone(), progress, cancel)
            .await?;
    Ok(RunResponse::ok(
        "WUP verification complete.",
        Some(RunData::WupVerify(result)),
    ))
}

pub(crate) async fn wup_to_wux(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    if crate::nintendo::wup::disc::is_split_continuation(&input) {
        return Ok(skipped_already_done(
            &input,
            "wup.to_wux",
            "split continuation part",
            &"converted together with game_part1.wud",
        ));
    }
    if crate::nintendo::wup::disc::sector_stream::is_wux_file(&input) {
        return Ok(skipped_already_done(
            &input,
            "wup.to_wux",
            "already WUX",
            &"input is already a WUX image",
        ));
    }
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| basis.with_extension("wux"),
            operation: "wup.to_wux",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::wup::wud_to_wux(progress, input, output, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn wup_to_wud(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    // A plain file that lacks the WUX magic is not a disc image; without
    // this check a .wud input would fail with "output already exists"
    // instead. Archive members are rejected once opened.
    if input.is_file()
        && !crate::util::archive::is_archive_path(&input)
        && !crate::nintendo::wup::disc::sector_stream::is_wux_file(&input)
    {
        return Err(crate::nintendo::wup::WupError::UnsupportedDiscFormat(input).into());
    }
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|basis, _| basis.with_extension("wud"),
            operation: "wup.to_wud",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            crate::nintendo::wup::wux_to_wud(progress, input, output, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn cue_merge(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    if req.output.is_none() {
        return Err(invalid_arg("output path is required"));
    }
    convert_op(
        progress,
        &req,
        ConvertTarget {
            input: &input,
            derive: &|_, _| unreachable!("cue.merge requires an explicit output"),
            operation: "cue.merge",
            verify: OutputVerify::None,
        },
        cancel,
        |input, output, cancel| async move {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            crate::disc::cue::merge::merge_bin(progress, input, output, true, cancel)
                .await
                .map_err(anyhow::Error::from)
        },
    )
    .await
}

pub(crate) async fn playlist_write(
    req: RunRequest,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<RunResponse> {
    let input = required_input(&req)?;
    if !input.is_dir() {
        return Err(invalid_arg(format!(
            "playlist input must be a directory: {}",
            input.display()
        )));
    }
    let extensions = req
        .options
        .extensions
        .as_deref()
        .unwrap_or("cue,chd,iso,cso,zso")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let extension_refs = extensions.iter().map(String::as_str).collect::<Vec<_>>();
    let mode = match req.options.playlist_mode.as_deref().unwrap_or("multiple") {
        "multiple" => crate::playlist::PlaylistMode::Multiple,
        "always" => crate::playlist::PlaylistMode::Always,
        other => return Err(invalid_arg(format!("invalid playlist_mode {other:?}"))),
    };
    let plans = crate::playlist::plan_playlists(
        &crate::playlist::PlaylistOptions {
            scan_dir: &input,
            output_dir: req.options.output_dir.as_deref(),
            extensions: &extension_refs,
            mode,
            max_depth: req.options.max_depth,
        },
        &cancel,
    )?;
    let started = Instant::now();
    let policy = conflict_policy(&req)?;
    let mut records = Vec::new();
    let mut playlists = Vec::new();
    for plan in plans {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        if plan.has_duplicate_numbers {
            progress.warn(&format!(
                "Duplicate disc numbers in set {}, including all entries",
                plan.base_title
            ));
        }
        let entry_exts = plan
            .contents
            .lines()
            .filter_map(|line| Path::new(line).extension())
            .filter_map(|ext| ext.to_str());
        if let Some(mixed) = crate::util::mixed_playlist_extensions(entry_exts) {
            progress.warn(&format!(
                "Mixed track formats ({mixed}) in set {}; emulators expect every disc \
                 in a playlist to use the same format",
                plan.base_title
            ));
        }
        let resolution = resolve_conflict(&plan.m3u_path, policy)?;
        let output = match &resolution {
            ConflictResolution::Write(path) => path.clone(),
            ConflictResolution::Skip => plan.m3u_path.clone(),
        };
        playlists.push(PlaylistPlanData {
            base_title: plan.base_title.clone(),
            output: output.clone(),
            contents: plan.contents.clone(),
            disc_count: plan.disc_count,
            has_duplicate_numbers: plan.has_duplicate_numbers,
        });
        if req.dry_run {
            records.push(ReportRecord::new(ReportRecordInput {
                input_path: input.display().to_string(),
                output_path: output.display().to_string(),
                operation: planned_verb("playlist.write", true),
                status: if matches!(resolution, ConflictResolution::Skip) {
                    FileStatus::Skipped
                } else {
                    FileStatus::Ok
                },
                input_bytes: 0,
                output_bytes: 0,
                elapsed_ms: 0,
                error: None,
            }));
            continue;
        }
        match resolution {
            ConflictResolution::Write(path) => {
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&path, plan.contents).await?;
                records.push(ReportRecord::new(ReportRecordInput {
                    input_path: input.display().to_string(),
                    output_path: path.display().to_string(),
                    operation: record_verb("playlist.write"),
                    status: FileStatus::Ok,
                    input_bytes: 0,
                    output_bytes: file_len(&path),
                    elapsed_ms: 0,
                    error: None,
                }));
            }
            ConflictResolution::Skip => records.push(ReportRecord::new(ReportRecordInput {
                input_path: input.display().to_string(),
                output_path: plan.m3u_path.display().to_string(),
                operation: record_verb("playlist.write"),
                status: FileStatus::Skipped,
                input_bytes: 0,
                output_bytes: 0,
                elapsed_ms: 0,
                error: None,
            })),
        }
    }
    let totals = totals_for_records(&records, elapsed_ms(started));
    Ok(RunResponse {
        schema: RUN_SCHEMA,
        ok: true,
        status: RunStatus::Ok.as_i32(),
        code: RunStatus::Ok.code().to_string(),
        message: batch_message(&totals),
        details: None,
        totals: Some(totals),
        records,
        events: Vec::new(),
        data: Some(RunData::Playlists(PlaylistsData { playlists })),
    })
}

pub(crate) fn info(req: RunRequest) -> Result<RunResponse> {
    let input = required_input(&req)?;
    let keys_path = req.options.keys.clone();
    let info = crate::info::read_info(
        &input,
        &crate::info::InfoOptions {
            keys_path,
            parent_path: None,
        },
    )?;
    Ok(RunResponse::ok("Info read.", Some(RunData::Info(info))))
}

/// Shared convert prelude: resolve the output path, short-circuit a skip or
/// a dry run, stage an archive input, check free space, then run `run` under
/// the file-op bookkeeping. Records and plan lines name the input the caller
/// staged; the conversion reads the extracted member when that is an archive.
pub(crate) struct ConvertTarget<'a> {
    pub input: &'a Path,
    /// Default output for the input's basis (the input itself, or for an
    /// archive the member's name placed next to the archive), plus the real
    /// file the conversion reads for handlers that name their output from
    /// the source's own header.
    pub derive: &'a dyn Fn(&Path, &Path) -> PathBuf,
    pub operation: &'a str,
    pub verify: OutputVerify,
}

pub(crate) async fn convert_op<F, Fut>(
    progress: &dyn ProgressReporter,
    req: &RunRequest,
    target: ConvertTarget<'_>,
    cancel: CancelToken,
    run: F,
) -> Result<RunResponse>
where
    F: FnOnce(PathBuf, PathBuf, CancelToken) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let ConvertTarget {
        input,
        derive,
        operation,
        verify,
    } = target;
    let op = find_op(operation)
        .ok_or_else(|| invalid_arg(format!("unknown operation {operation:?}")))?;
    // Output templates and `chd.extract` read the member's header, so an
    // archive is staged up front for them. Otherwise the member listing alone
    // names the output, and extraction waits until the plan says write.
    let mut resolved = if req.options.output_template.is_some() || operation == "chd.extract" {
        stage_input(input, operation).await?
    } else {
        None
    };
    let mut selection = None;
    let basis = match &resolved {
        Some(staged) => staged.output_basis().to_path_buf(),
        None => {
            selection = probe_selection(input, op).await?;
            selection
                .as_ref()
                .map(|selected| selected.output_basis(input))
                .transpose()?
                .unwrap_or_else(|| input.to_path_buf())
        }
    };
    let source = staged_path(&resolved, input);
    let desired = output_or(req, source, || derive(&basis, source))?;
    let desired = if operation == "chd.extract" {
        chd_extract_target(desired, source, input, req.output.is_some())?
    } else {
        desired
    };
    let plan = prepare_output(
        progress,
        req,
        input,
        &desired,
        operation,
        verify.clone(),
        &cancel,
    )
    .await?;
    let Some(output) = plan.output else {
        return Ok(skipped(
            input,
            &desired,
            operation,
            plan.kept_valid.then(|| VERIFIED_VALID.to_string()),
        ));
    };
    if let Some(mut line) = plan.line {
        if operation.starts_with("chd.") {
            line.media = chd_media_label(input);
        } else if operation.starts_with("nx.") {
            line.media = nx_media_label(input);
        }
        return Ok(planned(line));
    }
    if resolved.is_none() {
        resolved = stage_input_with_selection(input, operation, selection.take()).await?;
    }
    let source = staged_path(&resolved, input).to_path_buf();
    if !req.options.skip_space_check.unwrap_or(false) {
        let required = required_bytes(op, req, &source).await;
        preflight_space(output.parent().unwrap_or(&output), required)?;
    }
    let output_size = if operation == "chd.extract" {
        extracted_output_size
    } else {
        file_len
    };
    let target = output.clone();
    let run_cancel = cancel.clone();
    let mut response = run_file_op(input, &output, operation, output_size, move || {
        run(source, target, run_cancel)
    })
    .await?;
    if req.options.verify_after.unwrap_or(false)
        && let Some(RunData::Comparison(data)) = &mut response.data
    {
        data.comparison.verify = run_comparison_verify(progress, &output, verify, &cancel).await;
        data.comparison.output_sha1 = spawn_blocking_with_progress(progress, {
            let output = output.clone();
            let cancel = cancel.clone();
            move |progress| hash_file(&output, &[HashAlgo::Sha1], progress, &cancel)
        })
        .await
        .ok()
        .and_then(|d| d.sha1);
        // A cancel during the verify pass reads as a failed check; report
        // the cancellation instead.
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
    }
    Ok(response)
}

/// Response for a dry-run plan line, with the record a real run of it
/// would produce.
pub(crate) fn planned(line: PlanLine) -> RunResponse {
    let record = plan_record(&line);
    RunResponse::ok("Dry run planned.", Some(RunData::Plan(line))).with_record(record)
}

/// Report record for a dry-run plan line: a keep decision is a skip with
/// its reason, everything else is ok with nothing written yet.
fn plan_record(line: &PlanLine) -> ReportRecord {
    let (status, error) = match line.decision {
        PlanDecision::Skip => (FileStatus::Skipped, Some("output exists")),
        PlanDecision::KeepValid => (FileStatus::Skipped, Some(VERIFIED_VALID)),
        _ => (FileStatus::Ok, None),
    };
    ReportRecord::new(ReportRecordInput {
        input_path: line.input.display().to_string(),
        output_path: line.output.display().to_string(),
        operation: planned_verb(&line.operation, true),
        status,
        input_bytes: input_size(&line.input),
        output_bytes: 0,
        elapsed_ms: 0,
        error: error.map(str::to_string),
    })
}

/// Media label for an NX dry-run plan line: the container the input names.
/// `nx.merge` and `nx.split` name theirs from the chosen format instead.
fn nx_media_label(input: &Path) -> Option<String> {
    let label = path_ext(input).to_ascii_uppercase();
    matches!(
        label.as_str(),
        "NSP" | "XCI" | "NCA" | "NSZ" | "XCZ" | "NCZ"
    )
    .then_some(label)
}

/// Stages an archive input for `operation`.
pub(crate) async fn stage_input(input: &Path, operation: &str) -> Result<Option<ResolvedInput>> {
    stage_input_with_selection(input, operation, None).await
}

async fn stage_input_with_selection(
    input: &Path,
    operation: &str,
    selection: Option<crate::util::ArchiveSelection>,
) -> Result<Option<ResolvedInput>> {
    let exts = find_op(operation).map_or(&[][..], OpSpec::input_exts);
    if exts.is_empty() {
        return Ok(None);
    }
    let input = input.to_path_buf();
    tokio::task::spawn_blocking(move || {
        crate::util::resolve_input_with_selection(&input, exts, selection)
    })
    .await?
    .map(Some)
}

async fn probe_selection(
    input: &Path,
    op: &OpSpec,
) -> Result<Option<crate::util::ArchiveSelection>> {
    let exts = op.input_exts();
    if exts.is_empty() {
        return Ok(None);
    }
    let input = input.to_path_buf();
    tokio::task::spawn_blocking(move || crate::util::probe_archive(&input, exts)).await?
}

/// The file a conversion reads: the staged member, else `input` itself.
pub(crate) fn staged_path<'a>(resolved: &'a Option<ResolvedInput>, input: &'a Path) -> &'a Path {
    resolved.as_ref().map_or(input, ResolvedInput::path)
}

/// Bytes a conversion of `source` needs at the output: the operation's own
/// estimate when it has one, the referenced tracks for a cue sheet, else
/// the file.
async fn required_bytes(op: &OpSpec, req: &RunRequest, source: &Path) -> u64 {
    if let Some(estimate) = op.required_bytes {
        return estimate(req, source);
    }
    if has_ext(source, "cue") {
        return crate::disc::cue::referenced_files_size(source)
            .await
            .unwrap_or_else(|_| file_len(source));
    }
    file_len(source)
}

/// Refuses the write when the output filesystem lacks `required_bytes` plus
/// headroom. The output dir may not exist yet, so the nearest existing
/// ancestor is probed; an unreadable filesystem does not block the run.
pub(crate) fn preflight_space(output_dir: &Path, required_bytes: u64) -> Result<()> {
    let probe = output_dir
        .ancestors()
        .find(|p| p.exists())
        .unwrap_or(output_dir);
    if let Ok(available) = available_space(probe)
        && space_shortfall(available, required_bytes, DEFAULT_SPACE_HEADROOM).is_some()
    {
        anyhow::bail!(
            "Not enough free space at {}: need about {}, only {} available. Skip the free space check to proceed anyway.",
            output_dir.display(),
            format_bytes(required_bytes.saturating_add(DEFAULT_SPACE_HEADROOM)),
            format_bytes(available),
        );
    }
    Ok(())
}

/// Total bytes written by a CHD extraction: the named output plus, for cue
/// sheets, every data file the sheet references.
pub(crate) fn extracted_output_size(output: &Path) -> u64 {
    let mut total = file_len(output);
    if has_ext(output, "cue")
        && let Ok(text) = std::fs::read_to_string(output)
    {
        let dir = output.parent().unwrap_or_else(|| Path::new("."));
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("FILE ")
                && let Some(name) = rest.split('"').nth(1)
            {
                total += file_len(&dir.join(name));
            }
        }
    }
    total
}

/// Format-specific integrity check for the comparison card. Unlike
/// [`verify_existing_output`], this never treats "could not check" as a
/// pass: every arm classifies its error through the shared fail-closed
/// [`unverifiable`] filter, so a decode, parse, truncation, or hash
/// mismatch reports a failed verification and only cancellation, unusable
/// keys, worker-pool trouble, and environment io errors leave the check
/// without a verdict.
async fn run_comparison_verify(
    progress: &dyn ProgressReporter,
    output: &Path,
    target: OutputVerify,
    cancel: &CancelToken,
) -> Option<VerifyReport> {
    let report = |ok: bool, round_trip: bool| VerifyReport {
        ok,
        round_trip,
        verdict: if ok {
            VerifyVerdict::Verified
        } else {
            VerifyVerdict::Failed
        },
        message: if ok {
            "Verified"
        } else {
            "Verification failed"
        }
        .to_string(),
    };
    let could_not = |err: String| VerifyReport {
        ok: false,
        round_trip: false,
        verdict: VerifyVerdict::Unverified,
        message: format!("{COULD_NOT_VERIFY}{err}"),
    };
    // Zip and Raw reuse the overwrite-invalid check wholesale and only map
    // its verdict onto a report; only cancellation escapes it as an error.
    Some(match target {
        OutputVerify::None => return None,
        OutputVerify::Chd => {
            match crate::disc::chd::verify_chd(
                progress,
                output.to_path_buf(),
                None,
                false,
                cancel.clone(),
            )
            .await
            {
                Ok(()) => report(true, true),
                Err(e) if unverifiable(&e) => could_not(e.to_string()),
                Err(_) => report(false, false),
            }
        }
        OutputVerify::Cso => {
            match crate::cso::verify_cso(progress, output.to_path_buf(), true, cancel.clone()).await
            {
                Ok(()) => report(true, true),
                Err(e) if unverifiable(&e) => could_not(e.to_string()),
                Err(_) => report(false, false),
            }
        }
        OutputVerify::Rvz => {
            let output = output.to_path_buf();
            let cancel = cancel.clone();
            match tokio::task::spawn_blocking(move || {
                crate::nintendo::disc::rvz::verify_rvz_structure(&output, &cancel)
            })
            .await
            {
                Ok(Ok(structure)) => report(structure.ok(), false),
                Ok(Err(e)) if unverifiable(&e) => could_not(e.to_string()),
                Ok(Err(_)) => report(false, false),
                // The blocking task never produced a verdict:
                // infrastructure, not data, like the Chd/Cso worker pools.
                Err(e) => could_not(e.to_string()),
            }
        }
        OutputVerify::Zip { .. } | OutputVerify::Raw { .. } => {
            match verify_existing_output(progress, output, target, cancel.clone()).await {
                Ok(VerifyOutcome::Valid) => report(true, false),
                Ok(VerifyOutcome::Invalid) => report(false, false),
                Ok(VerifyOutcome::Unverified(cause)) => could_not(cause),
                Err(e) => could_not(e.to_string()),
            }
        }
        OutputVerify::Nx(keys) => {
            if keys.header_key.is_none() {
                return Some(could_not("keyset has no header key".to_string()));
            }
            match crate::nintendo::nx::verify_container_async(
                output.to_path_buf(),
                *keys,
                progress,
                cancel.clone(),
            )
            .await
            {
                Ok(result) => report(result.ok, result.ok),
                Err(e) if unverifiable(&e) => could_not(e.to_string()),
                Err(_) => report(false, false),
            }
        }
    })
}

pub(crate) fn chd_options(req: &RunRequest) -> Result<ChdOptions> {
    Ok(ChdOptions {
        hunk_size: req.options.hunk_size,
        codecs: opt_chd_codecs(req)?,
        level: req.options.level,
        force: true,
    })
}

pub(crate) fn rvz_options(req: &RunRequest) -> RvzCompressOptions {
    let mut opts = RvzCompressOptions::default();
    if let Some(level) = req.options.level {
        opts.compression_level = level;
    }
    if let Some(chunk_size) = req.options.chunk_size {
        opts.chunk_size = chunk_size;
    }
    opts
}

#[derive(Debug)]
pub(crate) struct PreparedOutput {
    pub(crate) output: Option<PathBuf>,
    pub(crate) line: Option<PlanLine>,
    /// True when a real run kept an existing output because it verified
    /// valid under `overwrite-invalid`. Never set for a dry run, and never
    /// for an operation carrying [`OutputVerify::None`]: an output nobody
    /// checked is never claimed valid.
    pub(crate) kept_valid: bool,
    /// True when an existing output failed verification under
    /// `overwrite-invalid` and the returned output path rewrites it. Set
    /// only for that verdict: a brand-new output never sets it, so callers
    /// announce a rewrite exactly when one replaces an invalid file.
    pub(crate) rewrite_invalid: bool,
}

/// An existing-output refusal carrying the [`OutputExists`] marker, so a
/// batched run skips the file the way [`prepare_output`]'s refusal does.
fn output_exists(desired: &Path, message: String) -> anyhow::Error {
    anyhow::Error::new(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        OutputExists(desired.to_path_buf()),
    ))
    .context(message)
}

/// Directory-output twin of [`prepare_output`]. Directories cannot
/// auto-number, so `rename` is rejected; `skip` keeps a directory that
/// already holds files and `error` refuses it. `overwrite` removes an
/// existing file at the path and writes into a non-empty directory as is.
pub(crate) fn prepare_output_dir(
    req: &RunRequest,
    input: &Path,
    desired: &Path,
    operation: &str,
) -> Result<PreparedOutput> {
    let policy = conflict_policy(req)?;
    let occupied =
        desired.is_file() || (desired.is_dir() && std::fs::read_dir(desired)?.next().is_some());
    let write = if !occupied {
        true
    } else if desired.is_file() {
        match policy {
            ConflictPolicy::Overwrite => true,
            ConflictPolicy::Skip | ConflictPolicy::OverwriteInvalid => false,
            _ => {
                return Err(output_exists(
                    desired,
                    format!(
                        "output path exists and is a file, use --on-conflict overwrite to replace it: {}",
                        desired.display()
                    ),
                ));
            }
        }
    } else {
        match policy {
            ConflictPolicy::Overwrite => true,
            ConflictPolicy::Skip | ConflictPolicy::OverwriteInvalid => false,
            ConflictPolicy::Rename => anyhow::bail!(
                "rename is not supported for directory outputs, use overwrite/skip/error: {}",
                desired.display()
            ),
            ConflictPolicy::Error => {
                return Err(output_exists(
                    desired,
                    format!(
                        "output directory is not empty, use overwrite to replace it: {}",
                        desired.display()
                    ),
                ));
            }
        }
    };
    if write && !req.dry_run && desired.is_file() {
        std::fs::remove_file(desired)?;
    }
    let line = req.dry_run.then(|| PlanLine {
        operation: operation.to_string(),
        input: input.to_path_buf(),
        output: desired.to_path_buf(),
        decision: if !write {
            crate::util::PlanDecision::Skip
        } else if occupied {
            crate::util::PlanDecision::Overwrite
        } else {
            crate::util::PlanDecision::New
        },
        media: None,
        missing_keys: None,
    });
    if !write && !req.dry_run {
        log::info!("Skipped, output exists: {}", desired.display());
    }
    Ok(PreparedOutput {
        output: write.then(|| desired.to_path_buf()),
        line,
        kept_valid: false,
        rewrite_invalid: false,
    })
}

pub(crate) async fn prepare_output(
    progress: &dyn ProgressReporter,
    req: &RunRequest,
    input: &Path,
    desired: &Path,
    operation: &str,
    verify: OutputVerify,
    cancel: &CancelToken,
) -> Result<PreparedOutput> {
    prepare_output_inner(
        progress, req, input, desired, operation, verify, cancel, false, false,
    )
    .await
}

/// [`prepare_output`] with the rewrite announcement suppressed: organize
/// logs it only after its write guards pass. `released` is a dry run's
/// mirror of a source this run frees: the desired path still exists on
/// disk, but a real run finds it gone, so the dry run plans it as new.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn prepare_output_quiet(
    progress: &dyn ProgressReporter,
    req: &RunRequest,
    input: &Path,
    desired: &Path,
    operation: &str,
    verify: OutputVerify,
    cancel: &CancelToken,
    released: bool,
) -> Result<PreparedOutput> {
    prepare_output_inner(
        progress, req, input, desired, operation, verify, cancel, true, released,
    )
    .await
}

/// File taking `output`'s slot: a CD-mode `chd.extract` also writes one bin per
/// track, so any existing `<stem>.bin` or `<stem> (Track N).bin` occupies the slot.
fn output_occupant(operation: &str, output: &Path) -> Option<PathBuf> {
    if output.exists() {
        Some(output.to_path_buf())
    } else if operation == "chd.extract"
        && output
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"))
    {
        crate::disc::chd::extract::possible_track_bins(output).find(|bin| bin.exists())
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
async fn prepare_output_inner(
    progress: &dyn ProgressReporter,
    req: &RunRequest,
    input: &Path,
    desired: &Path,
    operation: &str,
    verify: OutputVerify,
    cancel: &CancelToken,
    quiet_rewrite: bool,
    released: bool,
) -> Result<PreparedOutput> {
    let policy = conflict_policy(req)?;
    let absent = req.dry_run && released;
    let resolution = if absent {
        ConflictResolution::Write(desired.to_path_buf())
    } else {
        resolve_conflict_by(desired, policy, |path| output_occupant(operation, path))?
    };
    if req.dry_run {
        let output = match &resolution {
            ConflictResolution::Write(p) => p.clone(),
            ConflictResolution::Skip => desired.to_path_buf(),
        };
        let decision = if absent {
            crate::util::PlanDecision::New
        } else if policy == ConflictPolicy::OverwriteInvalid && desired.exists() {
            match verify_existing(req, progress, desired, verify, cancel).await? {
                VerifyOutcome::Valid => crate::util::PlanDecision::KeepValid,
                VerifyOutcome::Invalid => crate::util::PlanDecision::RewriteInvalid,
                // Nobody checked this output, so a dry run cannot claim
                // it verified; a plain skip keeps the real run in step.
                VerifyOutcome::Unverified(_) => crate::util::PlanDecision::Skip,
            }
        } else if matches!(&resolution, ConflictResolution::Write(p) if p == desired)
            && output_occupant(operation, desired).is_some()
        {
            crate::util::PlanDecision::Overwrite
        } else {
            crate::util::classify(desired, &resolution)
        };
        return Ok(PreparedOutput {
            output: Some(output.clone()),
            line: Some(PlanLine {
                operation: operation.to_string(),
                input: input.to_path_buf(),
                output,
                decision,
                media: None,
                missing_keys: None,
            }),
            kept_valid: false,
            rewrite_invalid: false,
        });
    }

    match resolution {
        ConflictResolution::Write(path) => Ok(PreparedOutput {
            output: Some(path),
            line: None,
            kept_valid: false,
            rewrite_invalid: false,
        }),
        ConflictResolution::Skip
            if policy == ConflictPolicy::OverwriteInvalid && desired.exists() =>
        {
            match verify_existing(req, progress, desired, verify, cancel).await? {
                VerifyOutcome::Valid => {
                    log::info!("Kept, output verified valid: {}", desired.display());
                    Ok(PreparedOutput {
                        output: None,
                        line: None,
                        kept_valid: true,
                        rewrite_invalid: false,
                    })
                }
                VerifyOutcome::Unverified(_) => {
                    log::info!("Kept, output exists (unverified): {}", desired.display());
                    Ok(PreparedOutput {
                        output: None,
                        line: None,
                        kept_valid: false,
                        rewrite_invalid: false,
                    })
                }
                VerifyOutcome::Invalid => {
                    if !quiet_rewrite {
                        log::info!(
                            "Rewriting, output failed verification: {}",
                            desired.display()
                        );
                    }
                    Ok(PreparedOutput {
                        output: Some(desired.to_path_buf()),
                        line: None,
                        kept_valid: false,
                        rewrite_invalid: true,
                    })
                }
            }
        }
        ConflictResolution::Skip => {
            let existing = output_occupant(operation, desired);
            log::info!(
                "Skipped, output exists: {}",
                existing.as_deref().unwrap_or(desired).display()
            );
            Ok(PreparedOutput {
                output: None,
                line: None,
                kept_valid: false,
                rewrite_invalid: false,
            })
        }
    }
}

/// Existing-output check for `overwrite-invalid`, through the request's hash
/// cache when one is attached so an unchanged valid output is not re-read.
async fn verify_existing(
    req: &RunRequest,
    progress: &dyn ProgressReporter,
    path: &Path,
    verify: OutputVerify,
    cancel: &CancelToken,
) -> Result<VerifyOutcome> {
    match req.ctx.hash_cache.as_deref() {
        Some(cache) => verify_existing_cached(cache, progress, path, verify, cancel.clone()).await,
        None => verify_existing_output(progress, path, verify, cancel.clone()).await,
    }
}

/// `output_size` measures what was written: [`extracted_output_size`] for
/// outputs that are a cue sheet plus its data files, [`file_len`] otherwise.
pub(crate) async fn run_file_op<F, Fut>(
    input: &Path,
    output: &Path,
    operation: &str,
    output_size: fn(&Path) -> u64,
    run: F,
) -> Result<RunResponse>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    if let Some(parent) = output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let started = Instant::now();
    let input_bytes = input_size(input);
    run().await?;
    let output_bytes = output_size(output);
    let record = ReportRecord::new(ReportRecordInput {
        input_path: input.display().to_string(),
        output_path: output.display().to_string(),
        operation: record_verb(operation),
        status: FileStatus::Ok,
        input_bytes,
        output_bytes,
        elapsed_ms: elapsed_ms(started),
        error: None,
    });
    let mut response = RunResponse::ok(
        format!("Wrote {}", output.display()),
        Some(RunData::Comparison(RunComparisonData {
            comparison: comparison_data(input, output, input_bytes, output_bytes),
        })),
    )
    .with_record(record);
    if input_bytes == 0 && output_bytes == 0 {
        response.data = None;
    }
    Ok(response)
}

/// Shared directory-output prelude: resolve the output directory, short-circuit
/// a skip or a dry run, check free space, then run `run` into it. `run`
/// returns the bytes it wrote and any operation data for the response.
pub(crate) async fn dir_op<F, Fut>(
    req: &RunRequest,
    input: &Path,
    desired: &Path,
    operation: &str,
    required_bytes: fn(&Path) -> u64,
    run: F,
) -> Result<RunResponse>
where
    F: FnOnce(PathBuf, PathBuf) -> Fut,
    Fut: std::future::Future<Output = Result<(u64, Option<RunData>)>>,
{
    let plan = prepare_output_dir(req, input, desired, operation)?;
    let Some(output) = plan.output else {
        return Ok(skipped(input, desired, operation, None));
    };
    if let Some(line) = plan.line {
        return Ok(planned(line));
    }
    let resolved = stage_input(input, operation).await?;
    let source = staged_path(&resolved, input).to_path_buf();
    if !req.options.skip_space_check.unwrap_or(false) {
        preflight_space(&output, required_bytes(&source))?;
    }
    let started = Instant::now();
    let (output_bytes, data) = run(source, output.clone()).await?;
    Ok(
        RunResponse::ok(format!("Wrote {}", output.display()), data).with_record(dir_op_record(
            input,
            &output,
            operation,
            output_bytes,
            started,
        )),
    )
}

/// The output directory of a directory-output operation: the request's
/// `output`, else `options.output_dir`.
pub(crate) fn required_output_dir(req: &RunRequest) -> Result<PathBuf> {
    req.output
        .clone()
        .or_else(|| req.options.output_dir.clone())
        .ok_or_else(|| invalid_arg("output path is required"))
}

/// Record for a finished directory-output operation.
pub(crate) fn dir_op_record(
    input: &Path,
    output: &Path,
    operation: &str,
    output_bytes: u64,
    started: Instant,
) -> ReportRecord {
    ReportRecord::new(ReportRecordInput {
        input_path: input.display().to_string(),
        output_path: output.display().to_string(),
        operation: record_verb(operation),
        status: FileStatus::Ok,
        input_bytes: input_size(input),
        output_bytes,
        elapsed_ms: elapsed_ms(started),
        error: None,
    })
}

/// Bytes a record reports for its input. A directory input (a Wii U title
/// tree, an XISO source folder, a CDN dump) counts every file it holds, the
/// way the space preflight already sizes one.
fn input_size(input: &Path) -> u64 {
    if !input.is_dir() {
        if !has_ext(input, "wud") {
            return file_len(input);
        }
        // A game_part1.wud record reports the whole part set it converts.
        return crate::nintendo::wup::disc::sector_stream::discover_split_parts(input)
            .iter()
            .map(|part| file_len(part))
            .sum();
    }
    crate::util::fs::collect_all_files(input, None, &CancelToken::new())
        .map(|files| files.iter().map(|file| file_len(file)).sum())
        .unwrap_or(0)
}

/// A skip because the input is already in the target format, distinct from
/// a conflict-policy skip: there is no output path, and the record's error
/// carries the detection reason.
pub(crate) fn skipped_already_done(
    input: &Path,
    operation: &str,
    message: &str,
    reason: &dyn std::fmt::Display,
) -> RunResponse {
    RunResponse::ok(format!("Skipped {}: {message}", input.display()), None).with_record(
        ReportRecord::new(ReportRecordInput {
            input_path: input.display().to_string(),
            output_path: String::new(),
            operation: record_verb(operation),
            status: FileStatus::Skipped,
            input_bytes: 0,
            output_bytes: 0,
            elapsed_ms: 0,
            error: Some(reason.to_string()),
        }),
    )
}

/// Loads the NX keyset. A dry run falls back to an empty keyset and reports
/// why the real one is missing, so the plan line can flag it; a real run
/// refuses.
pub(crate) fn nx_keys_for_run(
    req: &RunRequest,
) -> Result<(crate::nintendo::nx::KeySet, Option<String>)> {
    match crate::nintendo::nx::load_keyset(req.options.keys.as_deref()) {
        Ok(keys) => Ok((keys, None)),
        Err(err) if req.dry_run => Ok((
            crate::nintendo::nx::KeySet::default(),
            Some(err.to_string()),
        )),
        Err(err) => Err(err.into()),
    }
}

/// A plain skip of an existing output; `detail`, the kept-valid marker for
/// an output that verified valid under `overwrite-invalid`, lands on the
/// record's error so batched runs can tell the two apart.
pub(crate) fn skipped(
    input: &Path,
    desired: &Path,
    operation: &str,
    detail: Option<String>,
) -> RunResponse {
    let existing = output_occupant(operation, desired);
    RunResponse::ok(
        format!(
            "Skipped existing {}",
            existing.as_deref().unwrap_or(desired).display()
        ),
        None,
    )
    .with_record(ReportRecord::new(ReportRecordInput {
        input_path: input.display().to_string(),
        output_path: desired.display().to_string(),
        operation: record_verb(operation),
        status: FileStatus::Skipped,
        input_bytes: 0,
        output_bytes: 0,
        elapsed_ms: 0,
        error: detail,
    }))
}

pub(crate) fn totals_for(record: &ReportRecord) -> ReportTotals {
    totals_for_records(std::slice::from_ref(record), record.elapsed_ms)
}

pub(crate) fn totals_for_records(records: &[ReportRecord], elapsed_ms: u64) -> ReportTotals {
    let mut totals = ReportTotals {
        total_files: records.len(),
        elapsed_ms,
        ..ReportTotals::default()
    };
    for record in records {
        match record.status {
            FileStatus::Ok => totals.ok += 1,
            FileStatus::Skipped => totals.skipped += 1,
            FileStatus::Failed => totals.failed += 1,
        }
        totals.total_input_bytes += record.input_bytes;
        totals.total_output_bytes += record.output_bytes;
    }
    totals
}

pub(crate) fn batch_message(totals: &ReportTotals) -> String {
    if totals.failed == 0 {
        format!(
            "{} files completed ({} ok, {} skipped).",
            totals.total_files, totals.ok, totals.skipped
        )
    } else {
        format!("{} of {} files failed.", totals.failed, totals.total_files)
    }
}

/// Output directory for one file of a recursive run: `output_dir` with the
/// file's subpath under `root` appended, so the run mirrors the source tree
/// instead of flattening it into one directory. An `output_template` already
/// carries its own layout, so it keeps the plain output directory as its base.
fn mirrored_output_dir(options: &RunOptions, root: &Path, input: &Path) -> Option<PathBuf> {
    let dir = options.output_dir.as_deref()?;
    if options.output_template.is_some() {
        return Some(dir.to_path_buf());
    }
    Some(match input.strip_prefix(root).ok().and_then(Path::parent) {
        Some(rel) => dir.join(rel),
        None => dir.to_path_buf(),
    })
}

pub(crate) fn child_options(options: &RunOptions) -> RunOptions {
    let mut options = options.clone();
    options.recursive = None;
    options.report = None;
    options
}

/// Extensions a recursive run of `operation` scans.
pub(crate) fn batch_exts(operation: &str) -> Result<&'static [&'static str]> {
    find_op(operation)
        .and_then(|op| op.batch_exts)
        .ok_or_else(|| {
            invalid_arg(format!(
                "operation {operation:?} does not support recursive runs"
            ))
        })
}

pub(crate) fn wup_titles(
    req: &RunRequest,
    fallback: &Path,
) -> Result<Vec<crate::nintendo::wup::TitleInput>> {
    let Some(inputs) = req.options.inputs.as_ref() else {
        return Ok(vec![crate::nintendo::wup::TitleInput::auto(
            fallback.to_path_buf(),
        )]);
    };
    let mut titles = Vec::with_capacity(inputs.len());
    for input in inputs {
        match input {
            WupTitleInputOption::Path(path) => {
                titles.push(crate::nintendo::wup::TitleInput::auto(path))
            }
            WupTitleInputOption::Object {
                path,
                format,
                key,
                key_path,
            } => {
                let format = format.as_deref().map(wup_format).transpose()?;
                titles.push(crate::nintendo::wup::TitleInput {
                    dir: path.clone(),
                    format,
                    key_path: key.clone().or_else(|| key_path.clone()),
                });
            }
        }
    }
    if titles.is_empty() {
        return Err(invalid_arg("options.inputs must not be empty"));
    }
    Ok(titles)
}

pub(crate) fn first_wup_input(req: &RunRequest) -> Option<PathBuf> {
    let first = req.options.inputs.as_ref()?.first()?;
    match first {
        WupTitleInputOption::Path(path) => Some(path.clone()),
        WupTitleInputOption::Object { path, .. } => Some(path.clone()),
    }
}

pub(crate) fn wup_format(value: &str) -> Result<crate::nintendo::wup::TitleInputFormat> {
    match value {
        "loadiine" => Ok(crate::nintendo::wup::TitleInputFormat::Loadiine),
        "nus" => Ok(crate::nintendo::wup::TitleInputFormat::Nus),
        "disc" => Ok(crate::nintendo::wup::TitleInputFormat::Disc),
        other => Err(invalid_arg(format!("invalid WUP input format {other:?}"))),
    }
}

pub(crate) fn required_input(req: &RunRequest) -> Result<PathBuf> {
    req.input
        .clone()
        .ok_or_else(|| invalid_arg("input path is required"))
}

/// The request's explicit output, else `default` re-rooted into `output_dir`
/// or shaped by `output_template`. Template tokens read `source`, the file
/// the conversion consumes (an archive's extracted member).
pub(crate) fn output_or(
    req: &RunRequest,
    source: &Path,
    default: impl FnOnce() -> PathBuf,
) -> Result<PathBuf> {
    if req.output.is_some() && req.options.output_template.as_deref().is_some() {
        return Err(invalid_arg(
            "output_template conflicts with an explicit output path",
        ));
    }
    if let Some(output) = req.output.clone() {
        return Ok(output);
    }
    let derived = default();
    if let (Some(template), Some(input)) =
        (req.options.output_template.as_deref(), req.input.as_deref())
    {
        let ext = derived.extension().and_then(|s| s.to_str()).unwrap_or("");
        let keys_path = req.options.keys.clone();
        let info = crate::info::read_info(
            source,
            &crate::info::InfoOptions {
                keys_path,
                parent_path: None,
            },
        )
        .ok();
        let tokens = crate::util::TemplateTokens::new(info.as_ref(), source, ext);
        let rel = crate::util::apply_template(template, &tokens)?;
        let base = req
            .options
            .output_dir
            .clone()
            .or_else(|| input.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        return Ok(base.join(rel));
    }
    Ok(crate::util::place_in_dir(
        &derived,
        req.options.output_dir.as_deref(),
    ))
}

pub(crate) fn opt_chd_codecs(req: &RunRequest) -> Result<Option<Vec<ChdCodec>>> {
    req.options
        .codecs
        .as_ref()
        .map(|names| {
            names
                .iter()
                .map(|name| name.parse::<ChdCodec>())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| invalid_arg(err.to_string()))
        })
        .transpose()
}

pub(crate) fn conflict_policy(req: &RunRequest) -> Result<ConflictPolicy> {
    let Some(value) = req.options.on_conflict.as_deref() else {
        return Ok(req.ctx.default_conflict.unwrap_or(ConflictPolicy::Error));
    };
    match value {
        "error" => Ok(ConflictPolicy::Error),
        "overwrite" => Ok(ConflictPolicy::Overwrite),
        "skip" => Ok(ConflictPolicy::Skip),
        "rename" => Ok(ConflictPolicy::Rename),
        "overwrite-invalid" | "overwrite_invalid" => Ok(ConflictPolicy::OverwriteInvalid),
        other => Err(invalid_arg(format!("invalid on_conflict value {other:?}"))),
    }
}

pub(crate) fn cso_format(value: &str) -> Result<CsoFormat> {
    match value {
        "cso" | "CSO" => Ok(CsoFormat::Cso),
        "zso" | "ZSO" => Ok(CsoFormat::Zso),
        "dax" | "DAX" => Err(invalid_arg(
            "DAX is decode-only and cannot be a compression target",
        )),
        other => Err(invalid_arg(format!("invalid CSO format {other:?}"))),
    }
}

pub(crate) fn disc_mode(value: Option<&str>) -> Result<Option<DiscMode>> {
    match value {
        None | Some("auto") => Ok(None),
        Some("cd") => Ok(Some(DiscMode::Cd)),
        Some("dvd") => Ok(Some(DiscMode::Dvd)),
        Some("ld") => Ok(Some(DiscMode::Ld)),
        Some(other) => Err(invalid_arg(format!("invalid CHD mode {other:?}"))),
    }
}

pub(crate) fn nx_mode(
    value: &str,
    block_size_exp: Option<u32>,
) -> Result<crate::nintendo::nx::NczMode> {
    match value {
        "solid" => Ok(crate::nintendo::nx::NczMode::Solid),
        "block" => Ok(crate::nintendo::nx::NczMode::Block {
            size_exp: block_size_exp
                .map(u8::try_from)
                .transpose()
                .map_err(|_| invalid_arg("options.block_size_exp must fit in u8"))?
                .unwrap_or(20),
        }),
        other => Err(invalid_arg(format!("invalid NX mode {other:?}"))),
    }
}

pub(crate) fn comparison_data(
    input: &Path,
    output: &Path,
    input_bytes: u64,
    output_bytes: u64,
) -> ComparisonData {
    let ratio_pct = (input_bytes > 0).then(|| {
        let saved = (1.0 - output_bytes as f64 / input_bytes as f64) * 100.0;
        (saved * 10.0).round() / 10.0
    });
    ComparisonData {
        input_bytes,
        output_bytes,
        ratio_pct,
        input_format: path_ext(input).to_ascii_uppercase(),
        output_format: path_ext(output).to_ascii_uppercase(),
        output_sha1: None,
        verify: None,
    }
}

pub(crate) fn path_ext(path: &Path) -> &str {
    path.extension().and_then(|s| s.to_str()).unwrap_or("")
}

pub(crate) fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::super::{run_json, run_json_with_progress};
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn write_iso(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, (0..4 * 2048).map(|i| i as u8).collect::<Vec<_>>()).unwrap();
        path
    }

    async fn write_cd_chd(dir: &Path) -> (PathBuf, Vec<u8>) {
        let chds = dir.join("chds");
        let payload = vec![0u8; 4 * 2352];
        std::fs::write(dir.join("cd.bin"), &payload).unwrap();
        let cue = dir.join("cd.cue");
        std::fs::write(
            &cue,
            "FILE \"cd.bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n",
        )
        .unwrap();
        let compress = json!({
            "operation": "chd.compress",
            "input": cue,
            "options": { "mode": "cd", "output_dir": chds }
        });
        let res = run_json(&compress.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        (chds.join("cd.chd"), payload)
    }

    async fn write_dvd_chd(dir: &Path) -> (PathBuf, Vec<u8>) {
        let chds = dir.join("chds");
        let payload = crate::disc::chd::test_fixtures::mixed_iso(3);
        let iso = dir.join("dvd.iso");
        std::fs::write(&iso, &payload).unwrap();
        let compress = json!({
            "operation": "chd.compress",
            "input": iso,
            "options": { "mode": "dvd", "output_dir": chds }
        });
        let res = run_json(&compress.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        (chds.join("dvd.chd"), payload)
    }

    fn write_zip(archive: &Path, member: &str, data: &[u8]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(archive).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file(member, opts).unwrap();
        zip.write_all(data).unwrap();
        zip.finish().unwrap();
    }

    /// Cancels the token when the verify pass starts, after the conversion
    /// itself has finished.
    struct CancelOnVerify(CancelToken);

    impl ProgressReporter for CancelOnVerify {
        fn start(&self, _: u64, msg: &str) {
            if msg.starts_with("Verifying") {
                self.0.cancel();
            }
        }
        fn inc(&self, _: u64) {}
        fn finish(&self) {}
    }

    #[tokio::test]
    async fn verify_after_cancel_reports_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let input = write_iso(dir.path(), "game.iso");
        let cancel = CancelToken::new();
        let req = json!({
            "operation": "cso.compress",
            "input": input,
            "options": { "verify_after": true }
        });
        let res =
            run_json_with_progress(&req.to_string(), &CancelOnVerify(cancel.clone()), cancel).await;
        assert_eq!(res.status, RunStatus::Cancelled.as_i32(), "{res:?}");
    }

    /// A flipped hunk byte is a verified integrity failure: the CHD's payload
    /// is incompressible noise, so its hunks are stored raw and the flip
    /// survives decode to fail the map entry's CRC-16: a mismatch verdict,
    /// never a could-not-run.
    #[tokio::test]
    async fn comparison_verify_reports_a_flipped_hunk_byte_as_failed() {
        use crate::disc::chd::convert_iso_to_chd;
        let dir = tempfile::tempdir().unwrap();
        let mut iso = vec![0u8; 4 * 16 * 1024];
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        for byte in iso.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();
        let chd_path = dir.path().join("game.chd");
        convert_iso_to_chd(
            &crate::util::NoProgress,
            iso_path,
            chd_path.clone(),
            ChdOptions {
                hunk_size: None,
                codecs: None,
                level: None,
                force: false,
            },
            CancelToken::new(),
        )
        .await
        .unwrap();
        let mut bytes = std::fs::read(&chd_path).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        std::fs::write(&chd_path, &bytes).unwrap();

        let report = run_comparison_verify(
            &crate::util::NoProgress,
            &chd_path,
            OutputVerify::Chd,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert!(!report.ok, "{report:?}");
        assert_eq!(report.verdict, VerifyVerdict::Failed, "{report:?}");
        assert_eq!(report.message, "Verification failed");
    }

    /// A verify that cannot read its input at all never produces a verdict:
    /// the report says could-not-verify, not "Verification failed".
    #[tokio::test]
    async fn comparison_verify_reports_an_unreadable_output_as_could_not_verify() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.chd");
        let report = run_comparison_verify(
            &crate::util::NoProgress,
            &missing,
            OutputVerify::Chd,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert!(!report.ok, "{report:?}");
        assert_eq!(report.verdict, VerifyVerdict::Unverified, "{report:?}");
        assert!(
            report
                .message
                .starts_with(crate::runner::models::COULD_NOT_VERIFY),
            "{}",
            report.message
        );
    }

    /// A truncated RVZ is a failed verification, not a could-not-run: the
    /// fail-closed classifier reads truncation as the container being
    /// broken, so the comparison card never shows a green pass or a
    /// transient-looking unverified state for it.
    #[tokio::test]
    async fn comparison_verify_reports_a_truncated_rvz_as_failed() {
        use crate::nintendo::disc::rvz::RvzCompressOptions;
        use crate::nintendo::dol::test_fixtures::make_fake_gamecube_iso;
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, make_fake_gamecube_iso(5 * 1024 * 1024 + 123)).unwrap();
        let rvz = dir.path().join("game.rvz");
        crate::nintendo::disc::rvz::compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &crate::util::NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        // The group table closes the file, so cutting the tail removes it
        // along with the last group's data: either way, not valid.
        let full = std::fs::read(&rvz).unwrap();
        std::fs::write(&rvz, &full[..full.len() - 4096]).unwrap();

        let report = run_comparison_verify(
            &crate::util::NoProgress,
            &rvz,
            OutputVerify::Rvz,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert!(!report.ok, "{report:?}");
        assert_eq!(report.verdict, VerifyVerdict::Failed, "{report:?}");
        assert_eq!(report.message, "Verification failed");
    }

    /// A flipped byte inside a CSO's compressed block data decodes to
    /// garbage the full pass rejects: a verified failure under the
    /// fail-closed classifier, never a could-not-run.
    #[tokio::test]
    async fn comparison_verify_reports_a_flipped_cso_block_as_failed() {
        use crate::cso::models::CsoFormat;
        use crate::cso::writer::write_cso_blocking;
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let data: Vec<u8> = (0..8 * 2048usize).map(|i| (i / 7) as u8).collect();
        std::fs::write(&iso, &data).unwrap();
        let packed = dir.path().join("game.cso");
        write_cso_blocking(
            &iso,
            &packed,
            CsoFormat::Cso,
            2048,
            0,
            &std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            &CancelToken::new(),
        )
        .unwrap();
        let mut bytes = std::fs::read(&packed).unwrap();
        // Past the header and index, inside compressed block data.
        let data_start = 0x18 + (8 + 1) * 4;
        let mid = data_start + (bytes.len() - data_start) / 2;
        bytes[mid] ^= 0xFF;
        std::fs::write(&packed, &bytes).unwrap();

        let report = run_comparison_verify(
            &crate::util::NoProgress,
            &packed,
            OutputVerify::Cso,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert!(!report.ok, "{report:?}");
        assert_eq!(report.verdict, VerifyVerdict::Failed, "{report:?}");
        assert_eq!(report.message, "Verification failed");
    }

    #[tokio::test]
    async fn read_only_batch_skips_space_preflight() {
        let dir = tempfile::tempdir().unwrap();
        // A sparse file claims far more than any test volume has free.
        std::fs::File::create(dir.path().join("game.cso"))
            .unwrap()
            .set_len(1 << 43)
            .unwrap();
        let req = json!({
            "operation": "cso.verify",
            "input": dir.path(),
            "options": { "recursive": true }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.message.contains("free space"), "{}", res.message);
        assert_eq!(res.totals.unwrap().total_files, 1);
    }

    #[tokio::test]
    async fn dry_run_on_archive_does_not_extract() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("game.zip");
        write_zip(&archive, "member.iso", b"payload-bytes");
        // Corrupt the stored member so listing still works but extraction
        // fails its CRC check: a passing dry run proves nothing was extracted.
        let mut bytes = std::fs::read(&archive).unwrap();
        let at = bytes
            .windows(b"payload-bytes".len())
            .position(|w| w == b"payload-bytes")
            .unwrap();
        bytes[at..at + 13].copy_from_slice(b"XXXXXXXXXXXXX");
        std::fs::write(&archive, bytes).unwrap();

        let req = json!({ "operation": "cso.compress", "input": archive, "dry_run": true });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        assert_eq!(
            res.records[0].output_path,
            dir.path().join("member.cso").display().to_string()
        );

        let req = json!({ "operation": "cso.compress", "input": archive });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok, "{res:?}");
    }

    #[tokio::test]
    async fn single_dry_run_returns_record_and_writes_report() {
        let dir = tempfile::tempdir().unwrap();
        let input = write_iso(dir.path(), "game.iso");
        std::fs::write(dir.path().join("game.cso"), b"x").unwrap();
        let report = dir.path().join("report.json");
        let req = json!({
            "operation": "cso.compress",
            "input": input,
            "dry_run": true,
            "options": { "on_conflict": "skip", "report": report }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let record = &res.records[0];
        assert_eq!(record.operation, "compress (dry run)");
        assert_eq!(record.status, FileStatus::Skipped);
        assert_eq!(record.error.as_deref(), Some("output exists"));
        assert_eq!(record.input_bytes, 4 * 2048);
        assert_eq!(
            record.output_path,
            dir.path().join("game.cso").display().to_string()
        );
        assert_eq!(res.totals.unwrap().skipped, 1);
        assert!(report.exists());
    }

    #[tokio::test]
    async fn verify_after_sets_sha1_and_verify() {
        let dir = tempfile::tempdir().unwrap();
        let input = write_iso(dir.path(), "game.iso");
        let req = json!({
            "operation": "cso.compress",
            "input": input,
            "options": { "verify_after": true }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.unwrap()).unwrap();
        let comparison = &data["comparison"];
        assert_eq!(comparison["output_sha1"].as_str().unwrap().len(), 40);
        assert_eq!(comparison["verify"]["ok"], true);
        assert_eq!(comparison["verify"]["round_trip"], true);
        assert_eq!(comparison["verify"]["message"], "Verified");
    }

    #[tokio::test]
    async fn skip_space_check_bypasses_preflight() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.chd");
        // A sparse file claims far more than any test volume has free.
        std::fs::File::create(&input)
            .unwrap()
            .set_len(1 << 43)
            .unwrap();
        let req = json!({ "operation": "chd.to_cso", "input": input });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok);
        assert!(
            res.message
                .ends_with("Skip the free space check to proceed anyway."),
            "{}",
            res.message
        );

        let req = json!({
            "operation": "chd.to_cso",
            "input": input,
            "options": { "skip_space_check": true }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(!res.ok);
        assert!(!res.message.contains("free space"), "{}", res.message);
    }

    #[tokio::test]
    async fn chd_dry_run_plan_has_media() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.cue");
        std::fs::write(&input, b"FILE \"game.bin\" BINARY\n").unwrap();
        let req = json!({ "operation": "chd.compress", "input": input, "dry_run": true });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.unwrap()).unwrap();
        assert_eq!(data["media"], "CD");
    }

    #[tokio::test]
    async fn wup_to_wux_skips_continuation_and_wux_input() {
        let dir = tempfile::tempdir().unwrap();
        // A continuation part is converted together with its
        // game_part1.wud sibling, so a run over it alone skips.
        let part1 = dir.path().join("game_part1.wud");
        std::fs::write(&part1, b"part1").unwrap();
        let input = dir.path().join("game_part2.wud");
        std::fs::write(&input, b"continuation").unwrap();
        let req = json!({ "operation": "wup.to_wux", "input": input });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        assert_eq!(res.records[0].status, FileStatus::Skipped);
        assert_eq!(res.records[0].output_path, "");
        assert!(!dir.path().join("game_part2.wux").exists());

        // A WUX input is already the target format.
        let wux_input = dir.path().join("image.wux");
        crate::nintendo::wup::disc::wux_reader::write_wux_for_test(
            &wux_input,
            &[[0xAA; crate::nintendo::wup::disc::sector_stream::SECTOR_SIZE]],
        )
        .unwrap();
        let req = json!({ "operation": "wup.to_wux", "input": wux_input });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        assert_eq!(res.records[0].status, FileStatus::Skipped);
        assert_eq!(res.records[0].output_path, "");
    }

    #[test]
    fn prepare_output_dir_policies() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("title");
        let out = dir.path().join("out");
        let req = |policy: &str, dry_run: bool| RunRequest {
            schema: None,
            operation: "wup.decrypt".to_string(),
            input: Some(input.clone()),
            output: Some(out.clone()),
            config: None,
            preset: None,
            options: RunOptions {
                on_conflict: Some(policy.to_string()),
                ..RunOptions::default()
            },
            dry_run,
            ctx: Default::default(),
        };
        let prepared =
            prepare_output_dir(&req("error", false), &input, &out, "wup.decrypt").unwrap();
        assert_eq!(prepared.output.as_deref(), Some(out.as_path()));

        std::fs::create_dir(&out).unwrap();
        let prepared =
            prepare_output_dir(&req("error", false), &input, &out, "wup.decrypt").unwrap();
        assert!(prepared.output.is_some(), "empty dir is writable");

        std::fs::write(out.join("x"), b"x").unwrap();
        let err =
            prepare_output_dir(&req("error", false), &input, &out, "wup.decrypt").unwrap_err();
        assert!(err.to_string().starts_with("output directory is not empty"));
        let err =
            prepare_output_dir(&req("rename", false), &input, &out, "wup.decrypt").unwrap_err();
        assert!(
            err.to_string()
                .starts_with("rename is not supported for directory outputs")
        );
        let prepared =
            prepare_output_dir(&req("skip", false), &input, &out, "wup.decrypt").unwrap();
        assert!(prepared.output.is_none());
        let prepared = prepare_output_dir(
            &req("overwrite-invalid", false),
            &input,
            &out,
            "wup.decrypt",
        )
        .unwrap();
        assert!(prepared.output.is_none());
        let prepared =
            prepare_output_dir(&req("overwrite", false), &input, &out, "wup.decrypt").unwrap();
        assert!(prepared.output.is_some());

        let prepared =
            prepare_output_dir(&req("overwrite", true), &input, &out, "wup.decrypt").unwrap();
        assert_eq!(
            prepared.line.unwrap().decision,
            crate::util::PlanDecision::Overwrite
        );
        let prepared = prepare_output_dir(&req("skip", true), &input, &out, "wup.decrypt").unwrap();
        assert_eq!(
            prepared.line.unwrap().decision,
            crate::util::PlanDecision::Skip
        );
    }

    /// An Nx target with no header key is unverifiable: `overwrite-invalid`
    /// keeps the existing output rather than rewriting it, but (unlike a
    /// verified match) never claims it valid.
    #[tokio::test]
    async fn prepare_output_keeps_an_unverifiable_nx_output_without_claiming_it_valid() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("title.nsp");
        let desired = dir.path().join("title.nsz");
        std::fs::write(&input, b"input").unwrap();
        std::fs::write(&desired, b"stale nsz").unwrap();

        let req = RunRequest {
            schema: None,
            operation: "nx.compress".to_string(),
            input: Some(input.clone()),
            output: None,
            config: None,
            preset: None,
            options: RunOptions {
                on_conflict: Some("overwrite-invalid".to_string()),
                ..RunOptions::default()
            },
            dry_run: false,
            ctx: Default::default(),
        };
        let prepared = prepare_output(
            &crate::util::NoProgress,
            &req,
            &input,
            &desired,
            "nx.compress",
            OutputVerify::Nx(Box::default()),
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert!(prepared.output.is_none(), "kept, not rewritten");
        assert!(
            !prepared.kept_valid,
            "nobody checked it, so it is not claimed valid"
        );
        assert_eq!(std::fs::read(&desired).unwrap(), b"stale nsz");
    }

    /// `rewrite_invalid` is set only when an existing output verified
    /// invalid: a fresh output and a kept-valid output never announce a
    /// rewrite.
    #[tokio::test]
    async fn prepare_output_flags_a_rewrite_only_for_an_invalid_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game.bin");
        let desired = dir.path().join("game.out");
        std::fs::write(&input, b"payload").unwrap();
        let crc = crate::util::hash::CRC32.checksum(b"payload");
        let verify = || OutputVerify::Raw {
            crc,
            size: b"payload".len() as u64,
        };
        let req = RunRequest {
            schema: None,
            operation: "organize".to_string(),
            input: Some(input.clone()),
            output: None,
            config: None,
            preset: None,
            options: RunOptions {
                on_conflict: Some("overwrite-invalid".to_string()),
                ..RunOptions::default()
            },
            dry_run: false,
            ctx: Default::default(),
        };
        let prepare = || async {
            prepare_output_quiet(
                &crate::util::NoProgress,
                &req,
                &input,
                &desired,
                "copy",
                verify(),
                &CancelToken::new(),
                false,
            )
            .await
            .unwrap()
        };

        // Nothing at the path: a fresh write, not a rewrite.
        let fresh = prepare().await;
        assert!(fresh.output.is_some());
        assert!(!fresh.rewrite_invalid, "a fresh output is no rewrite");

        // A valid output is kept.
        std::fs::write(&desired, b"payload").unwrap();
        let kept = prepare().await;
        assert!(kept.kept_valid && !kept.rewrite_invalid);

        // A corrupt output is rewritten and says so.
        std::fs::write(&desired, b"corrupt!").unwrap();
        let rewritten = prepare().await;
        assert!(rewritten.output.is_some());
        assert!(rewritten.rewrite_invalid, "an invalid output is rewritten");
    }

    /// A dry run over an existing output that nobody can check
    /// (`OutputVerify::None`) plans a plain skip (the real run keeps the
    /// file), and its record says "output exists", not a verified keep.
    #[tokio::test]
    async fn dry_run_with_unverifiable_output_plans_a_plain_skip() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("title.nsp");
        let desired = dir.path().join("title.nsz");
        std::fs::write(&input, b"input").unwrap();
        std::fs::write(&desired, b"existing").unwrap();

        let req = RunRequest {
            schema: None,
            operation: "nx.compress".to_string(),
            input: Some(input.clone()),
            output: None,
            config: None,
            preset: None,
            options: RunOptions {
                on_conflict: Some("overwrite-invalid".to_string()),
                ..RunOptions::default()
            },
            dry_run: true,
            ctx: Default::default(),
        };
        let prepared = prepare_output(
            &crate::util::NoProgress,
            &req,
            &input,
            &desired,
            "nx.compress",
            OutputVerify::None,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        let line = prepared.line.expect("dry run plans a line");
        assert_eq!(line.decision, crate::util::PlanDecision::Skip);

        let record = plan_record(&line);
        assert_eq!(record.status, FileStatus::Skipped);
        assert_eq!(record.error.as_deref(), Some("output exists"));
    }

    #[tokio::test]
    async fn archive_input_extracted() {
        let dir = tempfile::tempdir().unwrap();
        let iso = write_iso(dir.path(), "member.iso");
        let archive = dir.path().join("game.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("member.iso", opts).unwrap();
        zip.write_all(&std::fs::read(&iso).unwrap()).unwrap();
        zip.finish().unwrap();
        std::fs::remove_file(&iso).unwrap();

        let req = json!({ "operation": "cso.compress", "input": archive });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        assert!(dir.path().join("member.cso").exists());
        assert_eq!(res.records[0].input_path, archive.display().to_string());
    }

    #[tokio::test]
    async fn chd_to_cso_dry_run_derives_member_output_next_to_archive() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("disc.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("game.chd", opts).unwrap();
        zip.write_all(b"not a real chd").unwrap();
        zip.finish().unwrap();

        let req = json!({ "operation": "chd.to_cso", "input": archive, "dry_run": true });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.unwrap()).unwrap();
        assert_eq!(data["input"].as_str(), archive.to_str());
        assert_eq!(
            data["output"].as_str(),
            dir.path().join("game.cso").to_str()
        );
        assert!(!dir.path().join("game.cso").exists());
    }

    #[tokio::test]
    async fn chd_extract_dry_run_names_target_by_flavour() {
        let dir = tempfile::tempdir().unwrap();
        let (dvd_chd, _) = write_dvd_chd(dir.path()).await;
        let (cd_chd, _) = write_cd_chd(dir.path()).await;

        for (chd, mode, expected) in [(&dvd_chd, "dvd", "dvd.iso"), (&cd_chd, "cd", "cd.cue")] {
            let plan = json!({
                "operation": "chd.extract",
                "input": chd,
                "dry_run": true
            });
            let res = run_json(&plan.to_string(), CancelToken::new()).await;
            assert!(res.ok, "{mode}: {res:?}");
            let data = serde_json::to_value(res.data.unwrap()).unwrap();
            assert_eq!(
                data["output"].as_str(),
                chd.with_file_name(expected).to_str(),
                "{mode}"
            );
        }

        // An archive member has no basis on disk, so the flavour has to come
        // off the staged member: this dry run does extract it.
        let zips = dir.path().join("zips");
        std::fs::create_dir(&zips).unwrap();
        let archive = zips.join("disc.zip");
        write_zip(&archive, "cd.chd", &std::fs::read(&cd_chd).unwrap());
        let plan = json!({ "operation": "chd.extract", "input": archive, "dry_run": true });
        let res = run_json(&plan.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.unwrap()).unwrap();
        assert_eq!(data["output"].as_str(), zips.join("cd.cue").to_str());
        assert!(!zips.join("cd.cue").exists());
    }

    #[tokio::test]
    async fn chd_extract_settles_explicit_output_before_planning() {
        let dir = tempfile::tempdir().unwrap();
        let (chd_path, payload) = write_cd_chd(dir.path()).await;
        let chds = chd_path.parent().unwrap();

        let output = chds.join("out");
        let extract = json!({
            "operation": "chd.extract",
            "input": chd_path,
            "output": output
        });
        let res = run_json(&extract.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        assert_eq!(
            res.records[0].output_path,
            output.with_extension("cue").display().to_string()
        );
        assert!(output.with_extension("cue").exists());
        assert_eq!(
            std::fs::read(output.with_extension("bin")).unwrap(),
            payload
        );
        assert!(!output.exists());

        let invalid_output = chds.join("x.iso");
        let plan = json!({
            "operation": "chd.extract",
            "input": chd_path,
            "output": invalid_output,
            "dry_run": true
        });
        let res = run_json(&plan.to_string(), CancelToken::new()).await;
        assert!(!res.ok, "{res:?}");
        assert_eq!(res.status, RunStatus::InvalidArgument.as_i32(), "{res:?}");
        assert!(!invalid_output.exists());
        assert!(!invalid_output.with_extension("cue").exists());
        assert!(!invalid_output.with_extension("bin").exists());

        let output_dir = chds.join("folder");
        std::fs::create_dir(&output_dir).unwrap();
        let extract = json!({
            "operation": "chd.extract",
            "input": chd_path,
            "output": output_dir
        });
        let res = run_json(&extract.to_string(), CancelToken::new()).await;
        assert!(!res.ok, "{res:?}");
        assert_eq!(res.status, RunStatus::InvalidArgument.as_i32(), "{res:?}");
        assert!(!output_dir.with_extension("cue").exists());
        assert!(!output_dir.with_extension("bin").exists());
    }

    #[tokio::test]
    async fn chd_extract_template_preserves_dotted_names_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        let (chd_path, payload) = write_cd_chd(dir.path()).await;
        let dotted_input = chd_path.with_file_name("Game v1.1.chd");
        std::fs::rename(&chd_path, &dotted_input).unwrap();
        let output_dir = dir.path().join("extracted");
        let existing_dir = output_dir.join("Game v1.1");
        std::fs::create_dir_all(&existing_dir).unwrap();

        let extract = json!({
            "operation": "chd.extract",
            "input": dotted_input,
            "options": {
                "output_dir": output_dir,
                "output_template": "{basename}",
                "on_conflict": "error"
            }
        });
        let res = run_json(&extract.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let cue_path = output_dir.join("Game v1.1.cue");
        assert_eq!(res.records[0].output_path, cue_path.display().to_string());
        let sheet = crate::disc::cue::CueParser::new(&cue_path)
            .parse()
            .await
            .unwrap();
        assert_eq!(sheet.files[0].filename, "Game v1.1.bin");
        assert_eq!(
            std::fs::read(cue_path.with_extension("bin")).unwrap(),
            payload
        );
    }

    #[tokio::test]
    async fn chd_extract_dvd_ignores_unrelated_bin() {
        let dir = tempfile::tempdir().unwrap();
        let (chd_path, payload) = write_dvd_chd(dir.path()).await;
        let chds = chd_path.parent().unwrap();

        let output = chds.join("x.iso");
        let unrelated_bin = output.with_extension("bin");
        let existing_bin = b"keep this unrelated bin";
        std::fs::write(&unrelated_bin, existing_bin).unwrap();
        let extract = json!({
            "operation": "chd.extract",
            "input": chd_path,
            "output": output,
            "options": { "on_conflict": "error" }
        });
        let res = run_json(&extract.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        assert_eq!(std::fs::read(&output).unwrap(), payload);
        assert_eq!(std::fs::read(&unrelated_bin).unwrap(), existing_bin);
    }

    #[tokio::test]
    async fn chd_extract_refuses_input_as_output() {
        let dir = tempfile::tempdir().unwrap();
        let (dvd_chd, _) = write_dvd_chd(dir.path()).await;
        let dvd_bytes = std::fs::read(&dvd_chd).unwrap();
        let misnamed_dvd = dir.path().join("game.iso");
        std::fs::copy(&dvd_chd, &misnamed_dvd).unwrap();
        let (cd_chd, _) = write_cd_chd(dir.path()).await;
        let cd_bytes = std::fs::read(&cd_chd).unwrap();
        let misnamed_cd = dir.path().join("game.bin");
        std::fs::copy(&cd_chd, &misnamed_cd).unwrap();
        let archive = dir.path().join("games.zip");
        write_zip(&archive, "dvd.chd", &dvd_bytes);
        let archive_bytes = std::fs::read(&archive).unwrap();

        for (input, output, original) in [
            (&misnamed_dvd, Some(&misnamed_dvd), dvd_bytes.as_slice()),
            (&misnamed_dvd, None, dvd_bytes.as_slice()),
            (&misnamed_cd, None, cd_bytes.as_slice()),
            (&archive, Some(&archive), archive_bytes.as_slice()),
        ] {
            let extract = json!({
                "operation": "chd.extract",
                "input": input,
                "output": output,
                "options": { "on_conflict": "overwrite" }
            });
            let res = run_json(&extract.to_string(), CancelToken::new()).await;
            assert!(!res.ok, "{res:?}");
            assert_eq!(res.status, RunStatus::InvalidArgument.as_i32(), "{res:?}");
            assert_eq!(std::fs::read(input).unwrap().as_slice(), original);
        }
    }

    #[tokio::test]
    async fn chd_extract_conflict_policy_covers_sidecar_bin() {
        let dir = tempfile::tempdir().unwrap();
        let (chd_path, payload) = write_cd_chd(dir.path()).await;
        let chds = chd_path.parent().unwrap();

        let existing_bin = b"keep this bin unchanged";
        let bin_path = chds.join("cd.bin");
        std::fs::write(&bin_path, existing_bin).unwrap();
        let request = |policy, dry_run| {
            json!({
                "operation": "chd.extract",
                "input": chd_path,
                "dry_run": dry_run,
                "options": { "on_conflict": policy }
            })
        };

        let plan = run_json(&request("overwrite", true).to_string(), CancelToken::new()).await;
        assert!(plan.ok, "{plan:?}");
        let Some(RunData::Plan(line)) = plan.data else {
            panic!("expected extraction plan");
        };
        assert_eq!(line.decision, PlanDecision::Overwrite);
        assert!(!chds.join("cd.cue").exists());
        assert_eq!(std::fs::read(&bin_path).unwrap(), existing_bin);

        let errored = run_json(&request("error", false).to_string(), CancelToken::new()).await;
        assert!(!errored.ok, "{errored:?}");
        assert!(
            errored.message.contains(bin_path.to_str().unwrap()),
            "{errored:?}"
        );
        assert!(!chds.join("cd.cue").exists());
        assert_eq!(std::fs::read(&bin_path).unwrap(), existing_bin);

        for policy in ["skip", "overwrite-invalid"] {
            let skipped = run_json(&request(policy, false).to_string(), CancelToken::new()).await;
            assert!(skipped.ok, "{policy}: {skipped:?}");
            assert_eq!(skipped.records[0].status, FileStatus::Skipped);
            assert!(
                skipped.message.contains(bin_path.to_str().unwrap()),
                "{policy}: {skipped:?}"
            );
            assert_eq!(
                skipped.records[0].output_path,
                chds.join("cd.cue").display().to_string()
            );
            assert!(!chds.join("cd.cue").exists());
            assert_eq!(std::fs::read(&bin_path).unwrap(), existing_bin);
        }

        let renamed = run_json(&request("rename", false).to_string(), CancelToken::new()).await;
        assert!(renamed.ok, "{renamed:?}");
        let renamed_cue = chds.join("cd (1).cue");
        let sheet = crate::disc::cue::CueParser::new(&renamed_cue)
            .parse()
            .await
            .unwrap();
        assert_eq!(sheet.files[0].filename, "cd (1).bin");
        assert_eq!(
            std::fs::read(renamed_cue.with_extension("bin")).unwrap(),
            payload
        );
        assert!(!chds.join("cd.cue").exists());
        assert_eq!(std::fs::read(&bin_path).unwrap(), existing_bin);
    }

    #[tokio::test]
    async fn chd_extract_conflict_policy_covers_track_bins() {
        let dir = tempfile::tempdir().unwrap();
        let (chd_path, _) = write_cd_chd(dir.path()).await;
        let chds = chd_path.parent().unwrap();

        let existing_bin = b"keep this track bin unchanged";
        let bin_path = chds.join("cd (Track 2).bin");
        std::fs::write(&bin_path, existing_bin).unwrap();
        let extract = json!({
            "operation": "chd.extract",
            "input": chd_path,
            "options": { "on_conflict": "error" }
        });
        let res = run_json(&extract.to_string(), CancelToken::new()).await;
        assert!(!res.ok, "{res:?}");
        assert!(res.message.contains(bin_path.to_str().unwrap()), "{res:?}");
        assert!(!chds.join("cd.cue").exists());
        assert_eq!(std::fs::read(&bin_path).unwrap(), existing_bin);
    }

    #[tokio::test]
    async fn batch_conflict_error_skips_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        write_iso(dir.path(), "game.iso");
        std::fs::write(dir.path().join("game.cso"), b"x").unwrap();
        let req = json!({
            "operation": "cso.compress",
            "input": dir.path(),
            "options": { "recursive": true }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let totals = res.totals.unwrap();
        assert_eq!((totals.ok, totals.skipped, totals.failed), (0, 1, 0));
        assert_eq!(res.records[0].status, FileStatus::Skipped);
    }

    #[tokio::test]
    async fn hash_recursive_on_a_file_hashes_that_file() {
        let dir = tempfile::tempdir().unwrap();
        let input = write_iso(dir.path(), "game.iso");
        let req = json!({
            "operation": "hash",
            "input": input,
            "options": { "recursive": true }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.unwrap()).unwrap();
        assert!(data["crc32"].is_string(), "{data}");
    }

    #[tokio::test]
    async fn cdn_to_cia_existing_output_plans_and_skips() {
        let dir = tempfile::tempdir().unwrap();
        let title = dir.path().join("title_a");
        std::fs::create_dir(&title).unwrap();
        let cia = dir.path().join("title_a.cia");
        std::fs::write(&cia, b"existing").unwrap();
        let req = |dry_run: bool| {
            json!({
                "operation": "ctr.cdn_to_cia",
                "input": title,
                "dry_run": dry_run,
                "options": { "on_conflict": "skip" }
            })
        };

        let res = run_json(&req(true).to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.unwrap()).unwrap();
        assert_eq!(data["decision"], "Skip");
        assert_eq!(data["output"].as_str(), cia.to_str());
        assert_eq!(res.records[0].status, FileStatus::Skipped);

        // The conversion is never entered, so an empty CDN directory is not
        // an error: the runner settles the conflict itself.
        let res = run_json(&req(false).to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        assert!(res.message.starts_with("Skipped existing"), "{res:?}");
        assert_eq!(res.records[0].status, FileStatus::Skipped);
    }

    #[tokio::test]
    async fn cdn_to_cia_recursive_plans_each_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["title_a", "title_b", ".Trashes"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        let req = json!({
            "operation": "ctr.cdn_to_cia",
            "input": dir.path(),
            "dry_run": true,
            "options": { "recursive": true }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.unwrap()).unwrap();
        let outputs: Vec<&str> = data["plans"]
            .as_array()
            .unwrap()
            .iter()
            .map(|plan| plan["output"].as_str().unwrap())
            .collect();
        assert_eq!(
            outputs,
            [
                dir.path().join("title_a.cia").to_str().unwrap(),
                dir.path().join("title_b.cia").to_str().unwrap(),
            ]
        );
        assert!(
            res.records
                .iter()
                .all(|record| record.operation == "cdn-to-cia (dry run)"),
            "{:?}",
            res.records
        );
    }

    #[tokio::test]
    async fn hash_recursive_returns_rows() {
        let dir = tempfile::tempdir().unwrap();
        write_iso(dir.path(), "a.iso");
        write_iso(dir.path(), "b.iso");
        let req = json!({
            "operation": "hash",
            "input": dir.path(),
            "options": { "recursive": true }
        });
        let res = run_json(&req.to_string(), CancelToken::new()).await;
        assert!(res.ok, "{res:?}");
        let data = serde_json::to_value(res.data.unwrap()).unwrap();
        let rows = data.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r["digests"]["crc32"].is_string()));
    }
}
