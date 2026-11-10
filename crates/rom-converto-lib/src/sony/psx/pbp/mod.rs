//! PS1 disc images (`.cue`, CD-mode `.chd`, `.m3u`) to `EBOOT.PBP` files
//! for the PSP's built-in PS1 emulator; the layout is in `docs/formats.md`.

mod blocks;
mod disc;
mod error;
mod format;
mod images;
mod multidisc;
mod writer;

use std::path::{Path, PathBuf};

pub use blocks::BlockEncoding;
pub use disc::DiscId;
pub use error::{PsxPbpError, PsxPbpResult};

use disc::{DiscImage, read_chd_disc_id, read_playlist};
use format::{MAX_DISCS, MULTIDISC_TABLE_LEN};
use writer::{Assets, Eboot, FAT32_MAX_FILE};

use crate::disc::chd::extract_from_chd;
use crate::util::fs::has_ext;
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

/// How to build the EBOOTs. The images may be PNG or JPEG of any size;
/// each is fitted to the size the PSP shows it at.
#[derive(Debug, Clone, Default)]
pub struct PbpOptions {
    pub encoding: BlockEncoding,
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
/// Returns the EBOOTs written.
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
        multidisc::table(),
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

/// [`to_pbp`] with the multi-disc table given, so both multi-disc layouts
/// can be exercised whatever this build carries.
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
    let encoding = options.encoding;
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
        return publish(vec![(together, path)], progress, cancel);
    }

    progress.warn(&format!(
        "{} discs as one EBOOT each, sharing one memory card; save before a disc change, \
         then start the next disc's EBOOT",
        discs.len()
    ));
    let titles: Vec<String> = (1..=discs.len())
        .map(|n| format!("{title} (Disc {n})"))
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
    publish(eboots, progress, cancel)
}

/// Writes every EBOOT to a scratch file beside its path, then publishes
/// them together, so a failure leaves no EBOOT of the set behind and every
/// earlier file in place. A file that could pass FAT32's limit is refused
/// before the first byte.
fn publish(
    eboots: Vec<(Eboot<'_>, PathBuf)>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> PsxPbpResult<Vec<PathBuf>> {
    for (eboot, _) in &eboots {
        let size = eboot.max_size()?;
        if size > FAT32_MAX_FILE {
            return Err(PsxPbpError::ExceedsFat32 { size });
        }
    }
    let mut written = Vec::with_capacity(eboots.len());
    for (eboot, path) in &eboots {
        let (mut file, temp) = scratch_output_file(path)?.into_parts();
        eboot.write(&mut file, progress, cancel)?;
        written.push(temp);
    }
    publish_set(
        written
            .into_iter()
            .zip(&eboots)
            .map(|(temp, (_, path))| (temp, path.as_path()))
            .collect(),
        true,
    )?;
    Ok(eboots.into_iter().map(|(_, path)| path).collect())
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
        write_disc(dir.path(), "game (Disc 1)", &ps1_data_track("SLPS_999.98", 40));
        write_disc(dir.path(), "game (Disc 2)", &data_track_without_system_cnf(50));
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
            None,
            &CancelToken::new(),
        )
        .unwrap_err();
        assert!(matches!(err, PsxPbpError::Io(_)), "{err}");
        for folder in ["SLPS99998-DISC1", "SLPS99998-DISC2"] {
            let left: Vec<_> = std::fs::read_dir(out.join(folder))
                .map(|entries| entries.map(|e| e.unwrap().file_name()).collect())
                .unwrap_or_default();
            assert!(left.is_empty(), "{folder}: {left:?}");
        }
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
            matches!(err, PsxPbpError::NotPs1Disc { kind: "PS2 (CD)", .. }),
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
        let eboot = out.join("SLUS99999/EBOOT.PBP");
        assert!(!eboot.exists());
        assert!(!crate::util::scratch_output_exists(&eboot).unwrap());
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
