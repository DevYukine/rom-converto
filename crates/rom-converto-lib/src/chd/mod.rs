//! CHD (Compressed Hunks of Data) compression and extraction for CD and DVD
//! disc images, targeting the same V5 format chdman writes.
//!
//! CD input (`.cue`/`.bin`) keeps its sidecar files, so restoring a CHD back
//! to disc form is called extract rather than decompress; see
//! [`crate::chd::error`] for the failure modes.

use crate::chd::error::{ChdError, ChdResult};
use crate::util::iso9660::{DiscKind, detect_disc_kind};
use crate::util::{CancelToken, ProgressReporter};
use log::{info, warn};
use std::path::PathBuf;

/// CHD hunk codecs: the compressor set chdman implements and the raw
/// compress/decompress primitives the CD and DVD paths build on.
pub mod compression;
pub use compression::{
    ChdCodec, default_cd_codecs, default_dvd_codecs, deflate_level, lzma_level, parse_codec_list,
    validate_codecs, zstd_level,
};
pub mod batch;
pub use batch::*;
pub mod convert;
pub use convert::*;
pub mod digest;
pub use digest::*;
pub mod error;
pub mod extract;
pub use extract::*;
pub mod info;
pub mod layout;
pub(crate) use layout::*;
pub(crate) mod legacy;
pub(crate) mod map;
pub mod migrate;
pub use migrate::*;
pub(crate) mod models;
pub(crate) mod reader;
pub mod verify;
pub use verify::*;
pub(crate) mod writer;

/// chdman's `createdvd` default: two 2048-byte sectors per hunk.
pub const DVD_HUNK_BYTES_DEFAULT: u32 = 4096;
/// PPSSPP serves the PSP's 2048-byte block API straight from hunks
/// and warns about anything larger, so detected PSP input defaults
/// to single-sector hunks.
pub const DVD_HUNK_BYTES_PSP: u32 = 2048;

/// Options for CHD creation (CD and DVD modes).
#[derive(Debug, Clone, Default)]
pub struct ChdOptions {
    /// Hunk size override; DVD mode's default is picked per detected
    /// console ([`DVD_HUNK_BYTES_DEFAULT`] / [`DVD_HUNK_BYTES_PSP`]).
    pub hunk_size: Option<u32>,
    /// Codec list for the CHD header's compressor slots. `None` uses
    /// the per-mode chdman default ([`default_cd_codecs`] /
    /// [`default_dvd_codecs`]).
    pub codecs: Option<Vec<ChdCodec>>,
    /// Compression level in `1..=22`. `None` uses each codec's
    /// default level.
    pub level: Option<i32>,
    pub force: bool,
}

/// Validate the codec list and compression level in `opts` against
/// the CHD flavor being written.
pub(crate) fn validate_chd_options(opts: &ChdOptions, dvd: bool) -> ChdResult<()> {
    if let Some(codecs) = &opts.codecs {
        validate_codecs(codecs, dvd)?;
    }
    if let Some(level) = opts.level
        && !(1..=22).contains(&level)
    {
        return Err(ChdError::InvalidCompressionLevel(level));
    }
    Ok(())
}

/// Which CHD flavor to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscMode {
    Cd,
    Dvd,
    /// Laserdisc A/V CHD (`chdman createld`), written from a `.avi` rip.
    Ld,
}

/// Compress the disc image at `input_path` into a CHD at `output_path`;
/// on cancel the partial CHD is removed and a pre-existing overwrite
/// target is left untouched.
pub async fn convert_disc_to_chd(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    mode: Option<DiscMode>,
    opts: ChdOptions,
    cancel: CancelToken,
) -> ChdResult<()> {
    let is_avi = input_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("avi"));
    match mode {
        Some(DiscMode::Ld) if !is_avi => return Err(ChdError::LdModeNeedsAvi),
        Some(m @ (DiscMode::Cd | DiscMode::Dvd)) if is_avi => {
            return Err(ChdError::AviNeedsLdMode(m));
        }
        _ => {}
    }
    if is_avi {
        info!("LaserDisc AVI detected, writing LD CHD (createld)");
        return convert_avi_to_chd(progress, input_path, output_path, opts, cancel).await;
    }

    let is_cue = input_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cue"));
    match (mode, is_cue) {
        (None | Some(DiscMode::Cd), true) => {
            convert_to_chd(progress, input_path, output_path, opts, cancel).await
        }
        (Some(DiscMode::Dvd), true) => Err(ChdError::DvdModeNeedsIso),
        (Some(DiscMode::Cd), false) => {
            convert_iso_to_cd_chd(progress, input_path, output_path, opts, cancel).await
        }
        (Some(DiscMode::Dvd), false) => {
            convert_iso_to_chd(progress, input_path, output_path, opts, cancel).await
        }
        (Some(DiscMode::Ld), _) => unreachable!("Ld handled above"),
        (None, false) => {
            let detect_path = input_path.clone();
            let kind =
                tokio::task::spawn_blocking(move || detect_disc_kind(&detect_path)).await??;
            match kind {
                DiscKind::Ps1 | DiscKind::Ps2Cd => {
                    info!("{} detected, writing CD-mode CHD", kind.label());
                    if kind == DiscKind::Ps2Cd {
                        warn!(
                            "{:?} looks like a CD-media PS2 game; if the original disc had \
                             audio tracks, convert from its bin/cue instead so they survive",
                            input_path
                        );
                    }
                    convert_iso_to_cd_chd(progress, input_path, output_path, opts, cancel).await
                }
                DiscKind::Ps2Dvd | DiscKind::Psp | DiscKind::UnknownIso => {
                    info!("{} detected, writing DVD-mode CHD", kind.label());
                    convert_iso_to_chd_with_kind(
                        progress,
                        input_path,
                        output_path,
                        opts,
                        Some(kind),
                        cancel,
                    )
                    .await
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    /// Alternating compressible runs and xorshift noise so the codec
    /// slots and the store-raw path all appear in the map.
    pub(crate) fn mixed_iso(sectors: usize) -> Vec<u8> {
        let mut iso = vec![0u8; sectors * 2048];
        let mut state = 0xDEAD_BEEF_CAFE_1234u64;
        for (i, b) in iso.iter_mut().enumerate() {
            if (i / 4096).is_multiple_of(2) {
                *b = (i / 97) as u8;
            } else {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *b = state as u8;
            }
        }
        iso
    }
}
