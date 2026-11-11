//! PS1 disc images (`.cue`, CD-mode `.chd`, `.m3u`) to `EBOOT.PBP` files
//! for the PSP's built-in PS1 emulator; the layout is in `docs/formats.md`.

mod blocks;
mod disc;
mod error;
mod format;
mod images;
mod writer;

use std::path::{Path, PathBuf};

pub use blocks::BlockEncoding;
pub use disc::DiscId;
pub use error::{PsxPbpError, PsxPbpResult};

#[cfg(test)]
pub(crate) use disc::test_fixtures;

use disc::{DiscImage, check_chd, read_chd_disc_id, read_playlist};
use format::{MAX_DISCS, MULTIDISC_TABLE_LEN, POPS_MULTIDISC_TABLE};
use writer::{Assets, Eboot, FAT32_MAX_FILE, SFO_TITLE_SLOT};

use crate::disc::chd::extract_from_chd;
use crate::util::fs::has_ext;
use crate::util::template::truncate_bytes;
use crate::util::{
    CancelToken, DEFAULT_SPACE_HEADROOM, ProgressReporter, available_space, derive_output_stem,
    publish_set, scratch_output_file, space_shortfall, spawn_blocking_with_progress,
};

/// The loader the EBOOT runs first: it asks the firmware to start the PS1
/// emulator on its own PBP. Built from `resources/psx/data_psp`.
const DATA_PSP: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/psx/DATA.PSP"
));

/// The boot image shown while the emulator starts, rendered from
/// `resources/psx/startdat.svg`.
const STARTDAT_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/psx/startdat.png"
));

/// What to do when a game's EBOOT already exists in the output folder,
/// which may hold other games, such as `PSP/GAME`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExistingEboot {
    /// Replace it.
    #[default]
    Replace,
    /// Leave it and build nothing for this game.
    Keep,
    /// Fail with [`PsxPbpError::EbootExists`].
    Refuse,
}

/// How to build the EBOOTs. The images may be PNG or JPEG of any size;
/// each is fitted to the size the PSP shows it at.
#[derive(Debug, Clone, Default)]
pub struct PbpOptions {
    pub encoding: BlockEncoding,
    /// What to do about this game's EBOOTs already in the output folder.
    pub existing: ExistingEboot,
    /// Title on the XMB; the input's file name when unset.
    pub title: Option<String>,
    /// Disc ID when `SYSTEM.CNF` names none, or to override it.
    pub disc_id: Option<String>,
    /// Image shown while the emulator starts (480x272), in place of the
    /// built-in one.
    pub splash: Option<PathBuf>,
    /// Image the XMB shows as the game's icon (144x80).
    pub icon0: Option<PathBuf>,
    /// Image the XMB shows behind the game's entry (480x272).
    pub pic1: Option<PathBuf>,
}

/// Builds PS1 EBOOTs from `input` (`.cue`, `.chd` or `.m3u`) into
/// `output_dir`, one game folder each, ready to copy into `PSP/GAME`.
/// Returns the EBOOTs written, none when [`ExistingEboot::Keep`] kept the
/// game's existing ones.
///
/// # Errors
/// Returns [`PsxPbpError`] when an input cannot become an EBOOT (see its
/// variants), when an image is not a PNG or JPEG, on I/O failure, or when
/// `cancel` fires. Nothing is published unless every EBOOT was written.
pub async fn to_pbp(
    progress: &dyn ProgressReporter,
    input: &Path,
    output_dir: &Path,
    options: &PbpOptions,
    cancel: CancelToken,
) -> PsxPbpResult<Vec<PathBuf>> {
    build_eboots(
        progress,
        input,
        output_dir,
        options,
        Some(&POPS_MULTIDISC_TABLE),
        cancel,
    )
    .await
}

/// A disc as given: a cue sheet already read, or a CHD still to extract.
enum Input {
    Cue(DiscImage),
    Chd(PathBuf),
}

/// What the inputs and options alone decide, settled before any CHD is
/// extracted.
struct Checked {
    inputs: Vec<Input>,
    /// One per disc; disc 1's is the EBOOT's own ID.
    ids: Vec<DiscId>,
    images: Images,
}

/// The fitted images an EBOOT carries.
struct Images {
    startdat_png: Vec<u8>,
    icon0: Option<Vec<u8>>,
    pic1: Option<Vec<u8>>,
}

/// [`to_pbp`] with the multi-disc table given, so tests can exercise the
/// one-EBOOT-per-disc layout with `None`.
async fn build_eboots(
    progress: &dyn ProgressReporter,
    input: &Path,
    output_dir: &Path,
    options: &PbpOptions,
    table: Option<&'static [u8; MULTIDISC_TABLE_LEN]>,
    cancel: CancelToken,
) -> PsxPbpResult<Vec<PathBuf>> {
    let sources = if has_ext(input, "m3u") {
        read_playlist(input)?
    } else {
        vec![input.to_path_buf()]
    };
    let checked = {
        let options = options.clone();
        spawn_blocking_with_progress(progress, move |_| check_inputs(sources, &options)).await?
    };
    if let Some(existing) = existing_eboot(output_dir, &checked.ids[0], checked.inputs.len()) {
        match options.existing {
            ExistingEboot::Replace => {}
            ExistingEboot::Keep => {
                log::info!("Skipped, EBOOT exists: {}", existing.display());
                return Ok(Vec::new());
            }
            ExistingEboot::Refuse => return Err(PsxPbpError::EbootExists(existing)),
        }
    }

    // CHDs extract to the system temp folder, never beside the output,
    // which may be the Memory Stick. The folder goes when this returns.
    let chds: Vec<&Path> = checked
        .inputs
        .iter()
        .filter_map(|input| match input {
            Input::Chd(path) => Some(path.as_path()),
            Input::Cue(_) => None,
        })
        .collect();
    let scratch = if chds.is_empty() {
        None
    } else {
        Some(scratch_for(&chds, output_dir, required_bytes(input))?)
    };
    let mut discs = Vec::with_capacity(checked.inputs.len());
    for (i, input) in checked.inputs.into_iter().enumerate() {
        discs.push(match input {
            Input::Cue(disc) => disc,
            Input::Chd(chd) => {
                let dir = scratch.as_ref().expect("a scratch folder for every CHD");
                let cue = dir.path().join(format!("disc{}.cue", i + 1));
                extract_from_chd(progress, chd.clone(), cue.clone(), None, cancel.clone()).await?;
                DiscImage::open(&cue, &chd)?
            }
        });
    }

    let title = options.title.clone().unwrap_or_else(|| default_stem(input));
    let output_dir = output_dir.to_path_buf();
    let (encoding, existing) = (options.encoding, options.existing);
    let (ids, images) = (checked.ids, checked.images);
    let written = spawn_blocking_with_progress(progress, move |progress| {
        build(
            progress,
            &discs,
            &ids,
            &images,
            &title,
            &output_dir,
            encoding,
            existing,
            table,
            &cancel,
        )
    })
    .await;
    drop(scratch);
    written
}

/// Reads every cue sheet, every disc ID and every image, and parses
/// `--disc-id`, so a mistake in any of them costs no CHD extraction.
fn check_inputs(sources: Vec<PathBuf>, options: &PbpOptions) -> PsxPbpResult<Checked> {
    let given_id = options.disc_id.as_deref().map(DiscId::parse).transpose()?;
    let images = Images {
        startdat_png: match &options.splash {
            Some(path) => images::fit(path, images::SPLASH)?,
            None => STARTDAT_PNG.to_vec(),
        },
        icon0: options
            .icon0
            .as_deref()
            .map(|path| images::fit(path, images::ICON0))
            .transpose()?,
        pic1: options
            .pic1
            .as_deref()
            .map(|path| images::fit(path, images::PIC1))
            .transpose()?,
    };

    let mut inputs = Vec::with_capacity(sources.len());
    // Every disc is read, even under --disc-id, so a PS2 disc is refused.
    let mut read_ids = Vec::with_capacity(sources.len());
    for source in sources {
        if has_ext(&source, "cue") {
            let disc = DiscImage::open(&source, &source)?;
            read_ids.push(disc.read_disc_id()?);
            inputs.push(Input::Cue(disc));
        } else if has_ext(&source, "chd") {
            read_ids.push(read_chd_disc_id(&source)?);
            check_chd(&source)?;
            inputs.push(Input::Chd(source));
        } else {
            return Err(PsxPbpError::UnsupportedInput(source));
        }
    }
    let first_id = match given_id {
        Some(id) => id,
        None => read_ids[0].clone().ok_or_else(|| {
            PsxPbpError::NoDiscId(match &inputs[0] {
                Input::Cue(disc) => disc.source.clone(),
                Input::Chd(chd) => chd.clone(),
            })
        })?,
    };
    let mut ids = vec![first_id.clone()];
    // A later disc without a readable ID still boots under disc 1's.
    ids.extend(
        read_ids[1..]
            .iter()
            .map(|id| id.clone().unwrap_or_else(|| first_id.clone())),
    );
    Ok(Checked {
        inputs,
        ids,
        images,
    })
}

/// A private folder in the system temp folder with room for `chds`
/// extracted, and for the `eboot_bytes` written to `output_dir` too when
/// both share a disk, since the tracks stay until every EBOOT is written.
fn scratch_for(
    chds: &[&Path],
    output_dir: &Path,
    eboot_bytes: u64,
) -> PsxPbpResult<tempfile::TempDir> {
    let dir = tempfile::Builder::new().prefix("rom-converto-").tempdir()?;
    let tracks: u64 = chds
        .iter()
        .map(|chd| crate::disc::chd::info::read_info(chd).map_or(0, |info| info.logical_bytes))
        .sum();
    let needed = if shares_disk(dir.path(), output_dir) {
        tracks + eboot_bytes
    } else {
        tracks
    };
    if let Ok(available) = available_space(dir.path())
        && space_shortfall(available, needed, DEFAULT_SPACE_HEADROOM).is_some()
    {
        return Err(PsxPbpError::ScratchSpace {
            dir: dir.path().to_path_buf(),
            needed: needed.saturating_add(DEFAULT_SPACE_HEADROOM),
            available,
        });
    }
    Ok(dir)
}

/// Whether `scratch` and `output`, or its nearest existing ancestor, are on
/// one disk. Assumed so when that cannot be told.
fn shares_disk(scratch: &Path, output: &Path) -> bool {
    let existing = output
        .ancestors()
        .find(|path| path.exists())
        .unwrap_or(output);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::metadata(scratch), std::fs::metadata(existing)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev(),
            _ => true,
        }
    }
    #[cfg(not(unix))]
    {
        let volume = |path: &Path| {
            std::fs::canonicalize(path)
                .ok()
                .and_then(|path| path.components().next().map(|c| c.as_os_str().to_owned()))
        };
        match (volume(scratch), volume(existing)) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(&b),
            _ => true,
        }
    }
}

/// The EBOOT a build of `input` into `output_dir` would meet, if any: the
/// game's one-file folder or any of its per-disc ones, found from disc 1's
/// ID (or `disc_id`) without building anything.
///
/// # Errors
/// Returns [`PsxPbpError`] when disc 1 cannot be read or names no ID.
pub fn find_existing_eboot(
    input: &Path,
    output_dir: &Path,
    disc_id: Option<&str>,
) -> PsxPbpResult<Option<PathBuf>> {
    let sources = if has_ext(input, "m3u") {
        read_playlist(input)?
    } else {
        vec![input.to_path_buf()]
    };
    let id = match disc_id {
        Some(text) => DiscId::parse(text)?,
        None => {
            let first = &sources[0];
            let read = if has_ext(first, "cue") {
                DiscImage::open(first, first)?.read_disc_id()?
            } else if has_ext(first, "chd") {
                read_chd_disc_id(first)?
            } else {
                return Err(PsxPbpError::UnsupportedInput(first.clone()));
            };
            read.ok_or_else(|| PsxPbpError::NoDiscId(first.clone()))?
        }
    };
    Ok(existing_eboot(output_dir, &id, sources.len()))
}

/// The first EBOOT of the game `id` names, with `discs` discs, already in
/// `output_dir`: its one-file folder or any of its per-disc ones.
fn existing_eboot(output_dir: &Path, id: &DiscId, discs: usize) -> Option<PathBuf> {
    std::iter::once(id.sfo())
        .chain((1..=discs).map(|n| format!("{}-DISC{n}", id.sfo())))
        .map(|folder| output_dir.join(folder).join("EBOOT.PBP"))
        .find(|path| path.exists() && !is_empty_file(path))
}

/// An empty EBOOT is what a run killed while publishing on FAT32 or exFAT
/// leaves: the name claimed, the rename not done. It counts as missing.
fn is_empty_file(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|meta| meta.is_file() && meta.len() == 0)
}

/// The file stem a default title and output folder derive from, with any
/// archive and image extension peeled: `game.cue.zip` gives `game`.
fn default_stem(input: &Path) -> String {
    derive_output_stem(input.file_name().unwrap_or_default())
        .to_string_lossy()
        .into_owned()
}

/// The output folder a build of `input` defaults to: `<stem>_psp` beside it.
pub fn derive_output_dir(input: &Path) -> PathBuf {
    input.with_file_name(format!("{}_psp", default_stem(input)))
}

/// Bytes a build of `input` writes on the output disk at most: each disc
/// stored uncompressed behind its 1 MiB header, plus the PSTITLEIMG and the
/// built-in loader and boot image. Images given as options come on top.
/// Discs that cannot be read count zero; the build reports why.
pub fn required_bytes(input: &Path) -> u64 {
    let sources = if has_ext(input, "m3u") {
        read_playlist(input).unwrap_or_default()
    } else {
        vec![input.to_path_buf()]
    };
    let discs: u64 = sources
        .iter()
        .map(|source| {
            let image = if has_ext(source, "chd") {
                crate::disc::chd::info::read_info(source).map_or(0, |info| info.logical_bytes)
            } else {
                DiscImage::open(source, source).map_or(0, |disc| disc.image_size())
            };
            writer::max_disc_bytes(image)
        })
        .sum();
    discs + writer::max_fixed_bytes(DATA_PSP.len() + STARTDAT_PNG.len())
}

#[allow(clippy::too_many_arguments)]
fn build(
    progress: &dyn ProgressReporter,
    discs: &[DiscImage],
    ids: &[DiscId],
    images: &Images,
    title: &str,
    output_dir: &Path,
    encoding: BlockEncoding,
    existing: ExistingEboot,
    table: Option<&[u8; MULTIDISC_TABLE_LEN]>,
    cancel: &CancelToken,
) -> PsxPbpResult<Vec<PathBuf>> {
    let first_id = &ids[0];
    let assets = Assets {
        data_psp: DATA_PSP,
        startdat_png: &images.startdat_png,
        icon0: images.icon0.as_deref().unwrap_or_default(),
        pic1: images.pic1.as_deref().unwrap_or_default(),
    };

    let together = Eboot {
        title,
        id: first_id,
        discs: discs.iter().zip(ids).collect(),
        table: table.filter(|_| (2..=MAX_DISCS).contains(&discs.len())),
        encoding,
        assets: &assets,
    };
    let fits_one =
        discs.len() == 1 || (together.table.is_some() && together.max_size()? <= FAT32_MAX_FILE);
    if fits_one {
        if discs.len() > 1 {
            log::info!(
                "{} discs in one EBOOT; switch discs from the HOME menu",
                discs.len()
            );
        }
        let path = output_dir.join(first_id.sfo()).join("EBOOT.PBP");
        return publish(vec![(together, path)], existing, progress, cancel);
    }

    progress.warn(&format!(
        "{} discs as one EBOOT each, sharing one memory card; save before a disc change, \
         then start the next disc's EBOOT",
        discs.len()
    ));
    // The base title gives way so every disc keeps its suffix in the SFO
    // TITLE slot, which ends in a NUL.
    let titles: Vec<String> = (1..=discs.len())
        .map(|n| {
            let suffix = format!(" (Disc {n})");
            format!(
                "{}{suffix}",
                truncate_bytes(title, SFO_TITLE_SLOT as usize - 1 - suffix.len())
            )
        })
        .collect();
    let eboots = discs
        .iter()
        .zip(&titles)
        .enumerate()
        .map(|(n, (disc, disc_title))| {
            let eboot = Eboot {
                title: disc_title,
                id: first_id,
                discs: vec![(disc, first_id)],
                table: None,
                encoding,
                assets: &assets,
            };
            let path = output_dir
                .join(format!("{}-DISC{}", first_id.sfo(), n + 1))
                .join("EBOOT.PBP");
            (eboot, path)
        })
        .collect();
    publish(eboots, existing, progress, cancel)
}

/// Writes every EBOOT to a scratch file beside its path, then publishes
/// them together, so a failure leaves no EBOOT of the set behind and every
/// earlier file in place. Unless `existing` is [`ExistingEboot::Replace`],
/// publishing never replaces a file, so an EBOOT another run published
/// meanwhile is kept or refused as asked. A file that could pass FAT32's
/// limit is refused before the first byte. A folder this call creates is
/// removed again when no EBOOT is published.
fn publish(
    eboots: Vec<(Eboot<'_>, PathBuf)>,
    existing: ExistingEboot,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> PsxPbpResult<Vec<PathBuf>> {
    let mut claimed = Vec::new();
    let published = write_and_publish(eboots, existing, progress, cancel, &mut claimed);
    if !matches!(&published, Ok(paths) if !paths.is_empty()) {
        // Best effort: a folder another run wrote into meanwhile stays.
        for folder in claimed.iter().rev() {
            let _ = std::fs::remove_dir(folder);
        }
    }
    published
}

/// [`publish`] but for the cleanup, adding to `claimed` each folder it
/// creates, in order. Every scratch file is gone by the time it returns.
fn write_and_publish(
    eboots: Vec<(Eboot<'_>, PathBuf)>,
    existing: ExistingEboot,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
    claimed: &mut Vec<PathBuf>,
) -> PsxPbpResult<Vec<PathBuf>> {
    for (eboot, _) in &eboots {
        let size = eboot.max_size()?;
        if size > FAT32_MAX_FILE {
            return Err(PsxPbpError::ExceedsFat32 { size });
        }
    }
    for (_, path) in &eboots {
        let folder = path.parent().expect("an EBOOT path names its game folder");
        // The exists() check keeps a drive root, which create_dir refuses,
        // out of the claim.
        if let Some(output_dir) = folder.parent()
            && !output_dir.as_os_str().is_empty()
            && !output_dir.exists()
        {
            if let Some(parent) = output_dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            claim_dir(output_dir, claimed)?;
        }
        claim_dir(folder, claimed)?;
    }
    let mut written = Vec::with_capacity(eboots.len());
    for (eboot, path) in &eboots {
        let (mut file, temp) = scratch_output_file(path)?.into_parts();
        eboot.write(&mut file, progress, cancel)?;
        written.push(temp);
    }
    for (_, path) in &eboots {
        if is_empty_file(path) {
            std::fs::remove_file(path)?;
        }
    }
    let published = publish_set(
        written
            .into_iter()
            .zip(&eboots)
            .map(|(temp, (_, path))| (temp, path.as_path()))
            .collect(),
        existing == ExistingEboot::Replace,
    );
    match published {
        Ok(()) => Ok(eboots.into_iter().map(|(_, path)| path).collect()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let path = eboots
                .iter()
                .map(|(_, path)| path)
                .find(|path| path.exists())
                .cloned()
                .unwrap_or_else(|| eboots[0].1.clone());
            match existing {
                ExistingEboot::Keep => {
                    log::info!("Skipped, EBOOT exists: {}", path.display());
                    Ok(Vec::new())
                }
                _ => Err(PsxPbpError::EbootExists(path)),
            }
        }
        Err(err) => Err(err.into()),
    }
}

/// Creates `dir` and records it in `claimed`; a folder that already exists,
/// perhaps made by another run just now, is used but not claimed.
fn claim_dir(dir: &Path, claimed: &mut Vec<PathBuf>) -> std::io::Result<()> {
    match std::fs::create_dir(dir) {
        Ok(()) => claimed.push(dir.to_path_buf()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};

    use super::disc::test_fixtures::{ps1_data_track, write_disc};
    use super::*;
    use crate::sony::psp::pbp::{DATA_PSAR, ICON0_PNG, PARAM_SFO, PIC1_PNG, Pbp};
    use crate::sony::psp::{PsarKind, read_info};
    use crate::util::NoProgress;
    use crate::util::sfo::Sfo;

    fn word(bytes: &[u8], at: usize) -> u64 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()).into()
    }

    fn sfo_of(eboot: &[u8]) -> Sfo {
        let pbp = Pbp::read(&mut Cursor::new(eboot)).unwrap();
        let sfo = pbp.segments[PARAM_SFO];
        assert_eq!(sfo.size, 944);
        Sfo::parse(&eboot[sfo.offset as usize..(sfo.offset + sfo.size) as usize]).unwrap()
    }

    /// Inflates every block of the PSISOIMG at `iso` back into the image.
    fn read_back(eboot: &[u8], iso: usize, image_len: usize) -> Vec<u8> {
        let mut image = Vec::new();
        for k in 0..image_len.div_ceil(format::BLOCK_SIZE) {
            let entry = iso + 0x4000 + k * 32;
            let offset = word(eboot, entry) as usize;
            let size = u16::from_le_bytes([eboot[entry + 4], eboot[entry + 5]]) as usize;
            assert_ne!(size, format::BLOCK_SIZE, "block {k} stored raw");
            assert_eq!(
                &eboot[entry + 6..entry + 32],
                &[0u8; 26],
                "block {k} marker"
            );
            let at = iso + 0x10_0000 + offset;
            let mut block = Vec::new();
            flate2::read::DeflateDecoder::new(&eboot[at..at + size])
                .read_to_end(&mut block)
                .unwrap();
            assert_eq!(block.len(), format::BLOCK_SIZE);
            image.extend_from_slice(&block);
        }
        assert!(image[image_len..].iter().all(|&b| b == 0), "padding");
        image.truncate(image_len);
        image
    }

    /// Two discs with their own IDs and a playlist naming them, in `dir`.
    fn two_disc_playlist(dir: &Path) -> (PathBuf, Vec<u8>, Vec<u8>) {
        let disc1 = ps1_data_track("SLPS_999.98", 40);
        let disc2 = ps1_data_track("SLPS_999.97", 50);
        write_disc(dir, "game (Disc 1)", &disc1);
        write_disc(dir, "game (Disc 2)", &disc2);
        let m3u = dir.join("game.m3u");
        std::fs::write(&m3u, "game (Disc 1).cue\ngame (Disc 2).cue\n").unwrap();
        (m3u, disc1, disc2)
    }

    #[tokio::test]
    async fn a_playlist_with_the_table_becomes_one_pstitleimg_eboot() {
        static STAND_IN: [u8; MULTIDISC_TABLE_LEN] = [0x5A; MULTIDISC_TABLE_LEN];
        let dir = tempfile::tempdir().unwrap();
        let (m3u, disc1, disc2) = two_disc_playlist(dir.path());

        let written = build_eboots(
            &NoProgress,
            &m3u,
            &dir.path().join("out"),
            &PbpOptions::default(),
            Some(&STAND_IN),
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(written, vec![dir.path().join("out/SLPS99998/EBOOT.PBP")]);

        let eboot = std::fs::read(&written[0]).unwrap();
        let sfo = sfo_of(&eboot);
        assert_eq!(sfo.get_str("DISC_ID"), Some("SLPS99998"));
        assert_eq!(sfo.get_str("TITLE"), Some("game"));
        let psar = word(&eboot, 0x24) as usize;
        assert_eq!(&eboot[psar..psar + 16], b"PSTITLEIMG000000");
        assert_eq!(&eboot[psar + 0x264..psar + 0x270], b"_SLPS_99998\0");
        assert_eq!(&eboot[psar + 0x28C..psar + 0x2CC], &STAND_IN);
        assert!(eboot[psar + 0x2CC..psar + 0x30C].iter().all(|&b| b == 0));

        let offsets: Vec<usize> = (0..5)
            .map(|i| word(&eboot, psar + 0x200 + i * 4) as usize)
            .collect();
        assert_eq!(offsets[0], 0x8000);
        assert_eq!(offsets[1] % 0x8000, 0);
        assert_eq!(&offsets[2..], &[0, 0, 0]);
        for (offset, (image, id)) in offsets
            .iter()
            .zip([(&disc1, "_SLPS_99998"), (&disc2, "_SLPS_99997")])
        {
            let iso = psar + offset;
            assert_eq!(&eboot[iso..iso + 12], b"PSISOIMG0000");
            assert_eq!(&eboot[iso + 0x400..iso + 0x40B], id.as_bytes());
            assert!(read_back(&eboot, iso, image.len()) == *image);
        }
        let startdat = psar + word(&eboot, psar + 0x10) as usize;
        assert_eq!(&eboot[startdat..startdat + 8], b"STARTDAT");
        assert_eq!(&eboot[startdat + 0x50..], STARTDAT_PNG);
    }

    #[tokio::test]
    async fn a_later_disc_without_system_cnf_boots_under_disc_1s_id() {
        use super::disc::test_fixtures::data_track_without_system_cnf;

        static STAND_IN: [u8; MULTIDISC_TABLE_LEN] = [0x5A; MULTIDISC_TABLE_LEN];
        let dir = tempfile::tempdir().unwrap();
        write_disc(
            dir.path(),
            "game (Disc 1)",
            &ps1_data_track("SLPS_999.98", 40),
        );
        write_disc(
            dir.path(),
            "game (Disc 2)",
            &data_track_without_system_cnf(50),
        );
        let m3u = dir.path().join("game.m3u");
        std::fs::write(&m3u, "game (Disc 1).cue\ngame (Disc 2).cue\n").unwrap();

        let written = build_eboots(
            &NoProgress,
            &m3u,
            &dir.path().join("out"),
            &PbpOptions::default(),
            Some(&STAND_IN),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let eboot = std::fs::read(&written[0]).unwrap();
        let psar = word(&eboot, 0x24) as usize;
        let disc2 = psar + word(&eboot, psar + 0x204) as usize;
        assert_eq!(&eboot[disc2 + 0x400..disc2 + 0x40C], b"_SLPS_99998\0");
    }

    async fn build_one(input: &Path, out: &Path, options: &PbpOptions) -> Vec<u8> {
        let written = to_pbp(&NoProgress, input, out, options, CancelToken::new())
            .await
            .unwrap();
        assert_eq!(written.len(), 1);
        std::fs::read(&written[0]).unwrap()
    }

    /// The format reference's worked example: a 2,479,008-byte disc (66
    /// blocks), stored deflate, no icons, a 480x272 boot image.
    #[tokio::test]
    async fn lays_out_the_worked_example() {
        use image::Rgba;

        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 1054));
        let splash = dir.path().join("splash.png");
        // Already 480x272, so it is kept as given.
        let png = images::test_fixtures::png(480, 272, Rgba([0x10, 0x20, 0x30, 0xFF]));
        std::fs::write(&splash, &png).unwrap();
        let options = PbpOptions {
            encoding: BlockEncoding::Stored,
            splash: Some(splash),
            ..PbpOptions::default()
        };

        let eboot = build_one(&cue, &dir.path().join("out"), &options).await;
        assert_eq!(eboot.len(), 0x36_E750 + 0x50 + png.len());
        assert_eq!(word(&eboot, 0x24), 0x1_0000);
        assert_eq!(word(&eboot, 0x10_000 + 0x0C), 0x35_E750);
        assert_eq!(&eboot[0x36_E750..0x36_E758], b"STARTDAT");
        assert_eq!(&eboot[0x36_E750 + 0x50..], png.as_slice());
        for k in 0..66 {
            let entry = 0x14_000 + k * 32;
            assert_eq!(word(&eboot, entry), k as u64 * 0x9305);
            assert_eq!(&eboot[entry + 4..entry + 6], &[0x05, 0x93]);
        }
    }

    #[tokio::test]
    async fn deflate_eboot_reads_back_to_the_disc() {
        let dir = tempfile::tempdir().unwrap();
        let image = ps1_data_track("SCES_999.96", 700);
        let cue = write_disc(dir.path(), "My Game", &image);
        let eboot = build_one(&cue, &dir.path().join("out"), &PbpOptions::default()).await;

        let sfo = sfo_of(&eboot);
        assert_eq!(sfo.get_str("CATEGORY"), Some("ME"));
        assert_eq!(sfo.get_str("DISC_ID"), Some("SCES99996"));
        assert_eq!(sfo.get_str("TITLE"), Some("My Game"));
        assert_eq!(sfo.get_u32("BOOTABLE"), Some(1));

        let path = dir.path().join("out/SCES99996/EBOOT.PBP");
        assert_eq!(
            read_info(&path).unwrap().psar_kind,
            Some(PsarKind::Psisoimg)
        );
        let psar = Pbp::read(&mut Cursor::new(&eboot)).unwrap().segments[DATA_PSAR].offset as usize;
        assert_eq!(&eboot[psar + 0x400..psar + 0x40C], b"_SCES_99996\0");
        assert!(read_back(&eboot, psar, image.len()) == image);
    }

    #[tokio::test]
    async fn a_playlist_without_the_table_becomes_one_eboot_per_disc() {
        let dir = tempfile::tempdir().unwrap();
        let (m3u, disc1, disc2) = two_disc_playlist(dir.path());

        let written = build_eboots(
            &NoProgress,
            &m3u,
            &dir.path().join("out"),
            &PbpOptions::default(),
            None,
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            written,
            vec![
                dir.path().join("out/SLPS99998-DISC1/EBOOT.PBP"),
                dir.path().join("out/SLPS99998-DISC2/EBOOT.PBP"),
            ]
        );
        for (n, (path, image)) in written.iter().zip([&disc1, &disc2]).enumerate() {
            let eboot = std::fs::read(path).unwrap();
            let sfo = sfo_of(&eboot);
            assert_eq!(sfo.get_str("DISC_ID"), Some("SLPS99998"));
            assert_eq!(
                sfo.get_str("TITLE").unwrap(),
                format!("game (Disc {})", n + 1)
            );
            let psar = word(&eboot, 0x24) as usize;
            assert_eq!(&eboot[psar..psar + 12], b"PSISOIMG0000");
            assert_eq!(&eboot[psar + 0x400..psar + 0x40C], b"_SLPS_99998\0");
            assert!(read_back(&eboot, psar, image.len()) == *image);
        }
    }

    #[tokio::test]
    async fn per_disc_titles_keep_their_suffix_under_a_long_title() {
        let dir = tempfile::tempdir().unwrap();
        let (m3u, _, _) = two_disc_playlist(dir.path());
        let options = PbpOptions {
            title: Some("A".repeat(200)),
            ..PbpOptions::default()
        };

        let written = build_eboots(
            &NoProgress,
            &m3u,
            &dir.path().join("out"),
            &options,
            None,
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(written.len(), 2);
        for (n, path) in written.iter().enumerate() {
            let sfo = sfo_of(&std::fs::read(path).unwrap());
            let title = sfo.get_str("TITLE").unwrap();
            assert!(title.ends_with(&format!(" (Disc {})", n + 1)), "{title}");
            assert!(title.len() <= 127, "{} bytes", title.len());
        }
    }

    /// 301 data sectors are not a multiple of the CHD's 4-frame track
    /// padding, and the audio track stores a two-second INDEX 00, so both
    /// layout rules of the CHD round trip show up in the EBOOT.
    #[tokio::test]
    async fn a_chd_builds_the_same_eboot_as_its_cue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("game (Track 1).bin"),
            ps1_data_track("SLUS_999.99", 301),
        )
        .unwrap();
        let audio: Vec<u8> = (0..2352 * 200).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(dir.path().join("game (Track 2).bin"), &audio).unwrap();
        let cue = dir.path().join("game.cue");
        std::fs::write(
            &cue,
            "FILE \"game (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n\
             FILE \"game (Track 2).bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\n",
        )
        .unwrap();
        let chd = chd_of(&cue).await;

        let from_cue = build_one(&cue, &dir.path().join("a"), &PbpOptions::default()).await;
        let from_chd = build_one(&chd, &dir.path().join("b"), &PbpOptions::default()).await;
        assert!(from_cue == from_chd);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("b")).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "scratch tracks removed");
    }

    async fn chd_of(cue: &Path) -> PathBuf {
        use crate::disc::chd::{ChdOptions, convert_to_chd};

        let chd = cue.with_extension("chd");
        convert_to_chd(
            &NoProgress,
            cue.to_path_buf(),
            chd.clone(),
            ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        chd
    }

    #[tokio::test]
    async fn a_missing_image_fails_before_a_chd_is_extracted_and_names_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 40));
        let chd = chd_of(&cue).await;
        let icon = dir.path().join("cover.png");
        let options = PbpOptions {
            icon0: Some(icon.clone()),
            ..PbpOptions::default()
        };
        let err = to_pbp(
            &NoProgress,
            &chd,
            &dir.path().join("out"),
            &options,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, PsxPbpError::ReadImage { path, .. } if *path == icon),
            "{err}"
        );
        assert!(err.to_string().contains("cover.png"), "{err}");
    }

    #[tokio::test]
    async fn a_ps2_chd_is_refused_naming_the_chd() {
        use super::disc::test_fixtures::ps2_cd_data_track;

        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "ps2", &ps2_cd_data_track(40));
        let chd = chd_of(&cue).await;
        let err = to_pbp(
            &NoProgress,
            &chd,
            &dir.path().join("out"),
            &PbpOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, PsxPbpError::NotPs1Disc { path, .. } if *path == chd),
            "{err}"
        );
    }

    /// Counts the spans started; before the discs are written, only a CHD
    /// extraction starts one.
    #[derive(Default)]
    struct CountStarts(std::sync::atomic::AtomicUsize);

    impl ProgressReporter for CountStarts {
        fn start(&self, _: u64, _: &str) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn inc(&self, _: u64) {}
        fn finish(&self) {}
    }

    /// Disc 1 as a CHD, disc 2 as a flat ISO of 2048-byte sectors, and a
    /// playlist naming `game (Disc 2).chd`, which the caller makes from the
    /// ISO. Returns the playlist and the ISO.
    async fn chd_playlist_with_iso(dir: &Path) -> (PathBuf, PathBuf) {
        let disc1 = write_disc(dir, "game (Disc 1)", &ps1_data_track("SLPS_999.98", 40));
        chd_of(&disc1).await;
        let iso: Vec<u8> = ps1_data_track("SLPS_999.97", 50)
            .as_chunks::<2352>()
            .0
            .iter()
            .flat_map(|sector| &sector[24..24 + 2048])
            .copied()
            .collect();
        let disc2 = dir.join("game (Disc 2).iso");
        std::fs::write(&disc2, iso).unwrap();
        let m3u = dir.join("game.m3u");
        std::fs::write(&m3u, "game (Disc 1).chd\ngame (Disc 2).chd\n").unwrap();
        (m3u, disc2)
    }

    /// Builds `m3u`, returning its error and how many CHDs were extracted.
    async fn refuse_counting_extractions(m3u: &Path) -> (PsxPbpError, usize) {
        let progress = CountStarts::default();
        let err = to_pbp(
            &progress,
            m3u,
            &m3u.with_file_name("out"),
            &PbpOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        (err, progress.0.load(std::sync::atomic::Ordering::Relaxed))
    }

    #[tokio::test]
    async fn a_chd_an_eboot_cannot_carry_is_refused_before_any_chd_is_extracted() {
        let dir = tempfile::tempdir().unwrap();
        let (m3u, disc2) = chd_playlist_with_iso(dir.path()).await;
        // The CHD creator stores a flat ISO as a MODE1/2048 track; it takes
        // no such track from a cue.
        crate::disc::chd::convert_iso_to_cd_chd(
            &NoProgress,
            disc2.clone(),
            disc2.with_extension("chd"),
            crate::disc::chd::ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let (err, extracted) = refuse_counting_extractions(&m3u).await;
        assert!(err.to_string().contains("game (Disc 2).chd"), "{err}");
        assert_eq!(extracted, 0, "a CHD was extracted");
    }

    #[tokio::test]
    async fn a_dvd_chd_is_refused_before_any_chd_is_extracted() {
        let dir = tempfile::tempdir().unwrap();
        let (m3u, disc2) = chd_playlist_with_iso(dir.path()).await;
        let chd = disc2.with_extension("chd");
        crate::disc::chd::convert_iso_to_chd(
            &NoProgress,
            disc2,
            chd.clone(),
            crate::disc::chd::ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let (err, extracted) = refuse_counting_extractions(&m3u).await;
        assert!(
            matches!(&err, PsxPbpError::NoCdTracks(path) if *path == chd),
            "{err}"
        );
        assert_eq!(extracted, 0, "a CHD was extracted");
    }

    #[tokio::test]
    async fn a_missing_playlist_entry_is_named() {
        let dir = tempfile::tempdir().unwrap();
        write_disc(
            dir.path(),
            "game (Disc 1)",
            &ps1_data_track("SLPS_999.98", 40),
        );
        let m3u = dir.path().join("game.m3u");
        for entry in ["gone.cue", "gone.chd"] {
            std::fs::write(&m3u, format!("game (Disc 1).cue\n{entry}\n")).unwrap();
            let err = to_pbp(
                &NoProgress,
                &m3u,
                &dir.path().join("out"),
                &PbpOptions::default(),
                CancelToken::new(),
            )
            .await
            .unwrap_err();
            assert!(
                matches!(&err, PsxPbpError::Open { path, .. } if *path == dir.path().join(entry)),
                "{err}"
            );
        }
    }

    #[test]
    fn a_failed_disc_publishes_none_of_its_set() {
        let dir = tempfile::tempdir().unwrap();
        let (m3u, _, _) = two_disc_playlist(dir.path());
        let discs: Vec<DiscImage> = read_playlist(&m3u)
            .unwrap()
            .iter()
            .map(|cue| DiscImage::open(cue, cue).unwrap())
            .collect();
        std::fs::remove_file(dir.path().join("game (Disc 2).bin")).unwrap();
        let ids = vec![DiscId::parse("SLPS-99998").unwrap(); 2];
        let images = Images {
            startdat_png: STARTDAT_PNG.to_vec(),
            icon0: None,
            pic1: None,
        };
        let out = dir.path().join("out");
        let err = build(
            &NoProgress,
            &discs,
            &ids,
            &images,
            "game",
            &out,
            BlockEncoding::Stored,
            ExistingEboot::Replace,
            None,
            &CancelToken::new(),
        )
        .unwrap_err();
        assert!(matches!(err, PsxPbpError::Io(_)), "{err}");
        for folder in ["SLPS99998-DISC1", "SLPS99998-DISC2"] {
            assert!(!out.join(folder).exists(), "{folder} left behind");
        }
    }

    /// Another run may publish one disc of the set between the early check
    /// and this one's publish. Called through `build`, as `build_eboots`
    /// returns at the early check without reaching a publish.
    #[test]
    fn a_kept_disc_leaves_no_empty_folder_of_the_rest_of_its_set() {
        let dir = tempfile::tempdir().unwrap();
        let (m3u, _, _) = two_disc_playlist(dir.path());
        let discs: Vec<DiscImage> = read_playlist(&m3u)
            .unwrap()
            .iter()
            .map(|cue| DiscImage::open(cue, cue).unwrap())
            .collect();
        let ids = vec![DiscId::parse("SLPS-99998").unwrap(); 2];
        let images = Images {
            startdat_png: STARTDAT_PNG.to_vec(),
            icon0: None,
            pic1: None,
        };
        let out = dir.path().join("out");
        let kept = out.join("SLPS99998-DISC2/EBOOT.PBP");
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        std::fs::write(&kept, b"the other run").unwrap();

        let written = build(
            &NoProgress,
            &discs,
            &ids,
            &images,
            "game",
            &out,
            BlockEncoding::Stored,
            ExistingEboot::Keep,
            None,
            &CancelToken::new(),
        )
        .unwrap();
        assert!(written.is_empty(), "{written:?}");
        assert!(
            !out.join("SLPS99998-DISC1").exists(),
            "DISC1 folder left behind"
        );
        assert_eq!(std::fs::read(&kept).unwrap(), b"the other run");
    }

    #[tokio::test]
    async fn required_bytes_bounds_a_stored_eboot() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 301));
        let options = PbpOptions {
            encoding: BlockEncoding::Stored,
            ..PbpOptions::default()
        };
        let eboot = build_one(&cue, &dir.path().join("out"), &options).await;
        let bound = required_bytes(&cue);
        assert!(bound >= eboot.len() as u64, "{bound} < {}", eboot.len());
        assert!(bound > 301 * 2352 + 0x10_0000, "{bound}");
    }

    #[test]
    fn an_output_not_yet_made_shares_the_disk_of_its_existing_parent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(shares_disk(dir.path(), &dir.path().join("not/yet/made")));
    }

    #[test]
    fn default_output_peels_archive_and_image_extensions() {
        assert_eq!(
            derive_output_dir(Path::new("/roms/game.cue.zip")),
            Path::new("/roms/game_psp")
        );
        assert_eq!(
            derive_output_dir(Path::new("/roms/game.m3u")),
            Path::new("/roms/game_psp")
        );
    }

    #[tokio::test]
    async fn disc_id_option_names_a_disc_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "homebrew", &ps1_data_track("PSX.EXE", 40));
        let err = to_pbp(
            &NoProgress,
            &cue,
            &dir.path().join("out"),
            &PbpOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, PsxPbpError::NoDiscId(_)), "{err}");

        let options = PbpOptions {
            disc_id: Some("HBRW-00001".to_string()),
            ..PbpOptions::default()
        };
        let eboot = build_one(&cue, &dir.path().join("out"), &options).await;
        assert_eq!(sfo_of(&eboot).get_str("DISC_ID"), Some("HBRW00001"));
    }

    #[tokio::test]
    async fn refuses_a_ps2_cd_even_with_a_disc_id() {
        use super::disc::test_fixtures::ps2_cd_data_track;

        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "ps2", &ps2_cd_data_track(40));
        let options = PbpOptions {
            disc_id: Some("SLUS-99999".to_string()),
            ..PbpOptions::default()
        };
        let err = to_pbp(
            &NoProgress,
            &cue,
            &dir.path().join("out"),
            &options,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err,
                PsxPbpError::NotPs1Disc {
                    kind: "PS2 (CD)",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[tokio::test]
    async fn cancel_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 40));
        let cancel = CancelToken::new();
        cancel.cancel();
        let out = dir.path().join("out");
        let err = to_pbp(&NoProgress, &cue, &out, &PbpOptions::default(), cancel)
            .await
            .unwrap_err();
        assert!(matches!(err, PsxPbpError::Cancelled(_)), "{err}");
        assert!(!out.join("SLUS99999").exists(), "game folder left behind");
    }

    #[tokio::test]
    async fn a_cancelled_run_leaves_no_output_folder_it_created() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 40));
        let cancel = CancelToken::new();
        cancel.cancel();
        let out = dir.path().join("out");
        let err = to_pbp(&NoProgress, &cue, &out, &PbpOptions::default(), cancel)
            .await
            .unwrap_err();
        assert!(matches!(err, PsxPbpError::Cancelled(_)), "{err}");
        assert!(!out.exists(), "output folder left behind");
    }

    #[test]
    fn a_failed_run_removes_only_an_output_folder_it_created() {
        let dir = tempfile::tempdir().unwrap();
        let (m3u, _, _) = two_disc_playlist(dir.path());
        let discs: Vec<DiscImage> = read_playlist(&m3u)
            .unwrap()
            .iter()
            .map(|cue| DiscImage::open(cue, cue).unwrap())
            .collect();
        std::fs::remove_file(dir.path().join("game (Disc 2).bin")).unwrap();
        let ids = vec![DiscId::parse("SLPS-99998").unwrap(); 2];
        let images = Images {
            startdat_png: STARTDAT_PNG.to_vec(),
            icon0: None,
            pic1: None,
        };
        let fail_into = |out: &Path| {
            build(
                &NoProgress,
                &discs,
                &ids,
                &images,
                "game",
                out,
                BlockEncoding::Stored,
                ExistingEboot::Replace,
                None,
                &CancelToken::new(),
            )
            .unwrap_err()
        };

        let missing = dir.path().join("missing");
        let err = fail_into(&missing);
        assert!(matches!(err, PsxPbpError::Io(_)), "{err}");
        assert!(!missing.exists(), "created output folder left behind");

        let existing = dir.path().join("existing");
        std::fs::create_dir(&existing).unwrap();
        fail_into(&existing);
        assert!(existing.is_dir(), "pre-existing output folder removed");
    }

    #[tokio::test]
    async fn an_empty_eboot_is_absent_under_every_existing_policy() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 40));
        for existing in [
            ExistingEboot::Keep,
            ExistingEboot::Refuse,
            ExistingEboot::Replace,
        ] {
            let out = dir.path().join(format!("out_{existing:?}"));
            let eboot = out.join("SLUS99999/EBOOT.PBP");
            std::fs::create_dir_all(eboot.parent().unwrap()).unwrap();
            std::fs::write(&eboot, b"").unwrap();
            assert_eq!(find_existing_eboot(&cue, &out, None).unwrap(), None);

            let options = PbpOptions {
                existing,
                ..PbpOptions::default()
            };
            let written = to_pbp(&NoProgress, &cue, &out, &options, CancelToken::new())
                .await
                .unwrap_or_else(|err| panic!("{existing:?}: {err}"));
            assert_eq!(written, vec![eboot.clone()], "{existing:?}");
            assert!(
                std::fs::metadata(&eboot).unwrap().len() > 0,
                "{existing:?} left the empty EBOOT"
            );
        }
    }

    #[tokio::test]
    async fn an_existing_eboot_is_kept_or_refused_without_touching_other_games() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 40));
        let out = dir.path().join("GAME");
        let other = out.join("SLES99995/EBOOT.PBP");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, b"another game").unwrap();
        let build = |existing| {
            let options = PbpOptions {
                existing,
                ..PbpOptions::default()
            };
            let (cue, out) = (cue.clone(), out.clone());
            async move { to_pbp(&NoProgress, &cue, &out, &options, CancelToken::new()).await }
        };

        // Another game in the folder is no conflict under any policy.
        let written = build(ExistingEboot::Refuse).await.unwrap();
        assert_eq!(written, vec![out.join("SLUS99999/EBOOT.PBP")]);
        let built = std::fs::read(&written[0]).unwrap();

        assert_eq!(
            build(ExistingEboot::Keep).await.unwrap(),
            Vec::<PathBuf>::new()
        );
        let err = build(ExistingEboot::Refuse).await.unwrap_err();
        assert!(
            matches!(&err, PsxPbpError::EbootExists(path) if *path == written[0]),
            "{err}"
        );
        assert_eq!(std::fs::read(&written[0]).unwrap(), built);
        assert_eq!(std::fs::read(&other).unwrap(), b"another game");
    }

    /// Another run may publish the same game between the early check and
    /// this one's publish; Keep and Refuse never replace that file.
    #[test]
    fn publishing_never_replaces_an_eboot_another_run_wrote_unless_asked() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 40));
        let disc = DiscImage::open(&cue, &cue).unwrap();
        let ids = vec![DiscId::parse("SLUS-99999").unwrap()];
        let images = Images {
            startdat_png: STARTDAT_PNG.to_vec(),
            icon0: None,
            pic1: None,
        };
        let out = dir.path().join("out");
        let eboot = out.join("SLUS99999/EBOOT.PBP");
        let build_with = |existing| {
            build(
                &NoProgress,
                std::slice::from_ref(&disc),
                &ids,
                &images,
                "game",
                &out,
                BlockEncoding::Stored,
                existing,
                None,
                &CancelToken::new(),
            )
        };
        std::fs::create_dir_all(eboot.parent().unwrap()).unwrap();
        std::fs::write(&eboot, b"the other run").unwrap();

        assert_eq!(
            build_with(ExistingEboot::Keep).unwrap(),
            Vec::<PathBuf>::new()
        );
        let err = build_with(ExistingEboot::Refuse).unwrap_err();
        assert!(
            matches!(&err, PsxPbpError::EbootExists(path) if *path == eboot),
            "{err}"
        );
        assert_eq!(std::fs::read(&eboot).unwrap(), b"the other run");
        assert!(!crate::util::scratch_output_exists(&eboot).unwrap());

        assert_eq!(
            build_with(ExistingEboot::Replace).unwrap(),
            vec![eboot.clone()]
        );
        assert_ne!(std::fs::read(&eboot).unwrap(), b"the other run");
    }

    #[tokio::test]
    async fn an_existing_eboot_survives_a_failed_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 40));
        let out = dir.path().join("out");
        let eboot = out.join("SLUS99999/EBOOT.PBP");
        std::fs::create_dir_all(eboot.parent().unwrap()).unwrap();
        let original = vec![0x5Au8; 128];
        std::fs::write(&eboot, &original).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = to_pbp(&NoProgress, &cue, &out, &PbpOptions::default(), cancel)
            .await
            .unwrap_err();
        assert!(matches!(err, PsxPbpError::Cancelled(_)), "{err}");
        assert_eq!(std::fs::read(&eboot).unwrap(), original);
        assert!(!crate::util::scratch_output_exists(&eboot).unwrap());
    }

    #[tokio::test]
    async fn fits_cover_art_to_the_icon_and_background() {
        use image::{Rgb, Rgba};

        use super::images::test_fixtures::{jpeg, png};

        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 40));
        let cover = dir.path().join("cover.jpg");
        std::fs::write(&cover, jpeg(500, 500, Rgb([0x20, 0x40, 0x80]))).unwrap();
        let backdrop = dir.path().join("backdrop.png");
        std::fs::write(&backdrop, png(1920, 1080, Rgba([0x80, 0x40, 0x20, 0xFF]))).unwrap();
        let options = PbpOptions {
            icon0: Some(cover),
            pic1: Some(backdrop),
            ..PbpOptions::default()
        };

        let eboot = build_one(&cue, &dir.path().join("out"), &options).await;
        let pbp = Pbp::read(&mut Cursor::new(&eboot)).unwrap();
        for (segment, expected) in [(ICON0_PNG, (144, 80)), (PIC1_PNG, (480, 272))] {
            let at = pbp.segments[segment];
            let bytes = &eboot[at.offset as usize..(at.offset + at.size) as usize];
            assert_eq!(
                images::png_dimensions(bytes),
                Some(expected),
                "segment {segment}"
            );
            assert!(
                image::load_from_memory(bytes).is_ok(),
                "segment {segment} decodes"
            );
        }
        let icon = read_info(&dir.path().join("out/SLUS99999/EBOOT.PBP"))
            .unwrap()
            .icon
            .expect("info reads the icon back");
        assert_eq!((icon.width, icon.height), (144, 80));
    }

    #[tokio::test]
    async fn rejects_a_splash_that_is_not_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 40));
        let splash = dir.path().join("splash.jpg");
        std::fs::write(&splash, b"\xFF\xD8\xFF\xE0 not a jpeg").unwrap();
        let options = PbpOptions {
            splash: Some(splash),
            ..PbpOptions::default()
        };
        let err = to_pbp(
            &NoProgress,
            &cue,
            &dir.path().join("out"),
            &options,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, PsxPbpError::InvalidImage { .. }), "{err}");
    }

    #[test]
    fn embedded_assets_are_a_loader_and_a_480x272_png() {
        assert_eq!(&DATA_PSP[..4], b"\x7FELF");
        assert_eq!(images::png_dimensions(STARTDAT_PNG), Some((480, 272)));
    }
}
