//! PS1 disc images to `EBOOT.PBP` files the PSP's built-in PS1 emulator
//! runs on custom firmware.
//!
//! An EBOOT carries a 944-byte `PARAM.SFO` (category `ME`), a small loader
//! module (`DATA.PSP`), the disc data in 0x9300-byte deflate-framed blocks
//! behind one PSISOIMG header per disc, and a STARTDAT boot image after the
//! last disc. Inputs are cue sheets with raw 2352-byte sectors, CD-mode
//! CHDs (extracted to a scratch cue sheet first) and `.m3u` playlists of
//! either.
//!
//! A playlist becomes one EBOOT with in-game disc switching when the build
//! carries the multi-disc table and the discs fit one file; otherwise each
//! disc becomes its own EBOOT carrying disc 1's ID, so they share one
//! memory card.

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

use disc::{DiscImage, read_playlist};
use format::{MAX_DISCS, MULTIDISC_TABLE_LEN};
use writer::{Assets, Eboot, FAT32_MAX_FILE};

use crate::disc::chd::extract_from_chd;
use crate::util::{CancelToken, ProgressReporter, atomic_write, spawn_blocking_with_progress};

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
/// `cancel` fires. Nothing is published for an EBOOT that failed.
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
    let sources = if has_extension(input, "m3u") {
        read_playlist(input)?
    } else {
        vec![input.to_path_buf()]
    };

    std::fs::create_dir_all(output_dir)?;
    // Extracted CHD tracks live beside the output so the space the run
    // needs is checked on one disk; the folder goes when this returns.
    let mut scratch = None;
    let mut cues = Vec::with_capacity(sources.len());
    for (i, source) in sources.iter().enumerate() {
        if has_extension(source, "cue") {
            cues.push(source.clone());
        } else if has_extension(source, "chd") {
            let dir = match &scratch {
                Some(dir) => dir,
                None => scratch.insert(
                    tempfile::Builder::new()
                        .prefix(".rom-converto-")
                        .tempdir_in(output_dir)?,
                ),
            };
            let cue = dir.path().join(format!("disc{}.cue", i + 1));
            extract_from_chd(progress, source.clone(), cue.clone(), None, cancel.clone()).await?;
            cues.push(cue);
        } else {
            return Err(PsxPbpError::UnsupportedInput(source.clone()));
        }
    }

    let title = options.title.clone().unwrap_or_else(|| {
        input
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    let output_dir = output_dir.to_path_buf();
    let options = options.clone();
    let written = spawn_blocking_with_progress(progress, move |progress| {
        build(
            progress,
            &cues,
            &title,
            &output_dir,
            &options,
            table,
            &cancel,
        )
    })
    .await;
    drop(scratch);
    written
}

/// The output folder a build of `input` defaults to: `<stem>_psp` beside it.
pub fn derive_output_dir(input: &Path) -> PathBuf {
    let mut name = input.file_stem().unwrap_or_default().to_os_string();
    name.push("_psp");
    input.with_file_name(name)
}

/// Bytes a build of `input` needs on the output disk: the disc images,
/// which the EBOOTs hold at most, plus the tracks a CHD is extracted to
/// first. Zero for an input that cannot be read; the build reports why.
pub fn required_bytes(input: &Path) -> u64 {
    let sources = if has_extension(input, "m3u") {
        read_playlist(input).unwrap_or_default()
    } else {
        vec![input.to_path_buf()]
    };
    sources
        .iter()
        .map(|source| {
            if has_extension(source, "chd") {
                crate::disc::chd::info::read_info(source).map_or(0, |info| 2 * info.logical_bytes)
            } else {
                DiscImage::open(source).map_or(0, |disc| disc.image_size())
            }
        })
        .sum()
}

fn build(
    progress: &dyn ProgressReporter,
    cues: &[PathBuf],
    title: &str,
    output_dir: &Path,
    options: &PbpOptions,
    table: Option<&[u8; MULTIDISC_TABLE_LEN]>,
    cancel: &CancelToken,
) -> PsxPbpResult<Vec<PathBuf>> {
    let discs = cues
        .iter()
        .map(|cue| DiscImage::open(cue))
        .collect::<PsxPbpResult<Vec<_>>>()?;
    let first_id = match &options.disc_id {
        Some(text) => DiscId::parse(text)?,
        None => discs[0]
            .read_disc_id()?
            .ok_or_else(|| PsxPbpError::NoDiscId(discs[0].cue.clone()))?,
    };
    let mut ids = vec![first_id.clone()];
    for disc in &discs[1..] {
        // A later disc without a readable ID still boots under disc 1's.
        ids.push(disc.read_disc_id()?.unwrap_or_else(|| first_id.clone()));
    }

    let startdat_png = match &options.splash {
        Some(path) => images::fit(path, images::SPLASH)?,
        None => STARTDAT_PNG.to_vec(),
    };
    let icon0 = options
        .icon0
        .as_deref()
        .map(|path| images::fit(path, images::ICON0))
        .transpose()?;
    let pic1 = options
        .pic1
        .as_deref()
        .map(|path| images::fit(path, images::PIC1))
        .transpose()?;
    let assets = Assets {
        data_psp: DATA_PSP,
        startdat_png: &startdat_png,
        icon0: icon0.as_deref().unwrap_or_default(),
        pic1: pic1.as_deref().unwrap_or_default(),
    };

    let together = Eboot {
        title,
        id: &first_id,
        discs: discs.iter().zip(&ids).collect(),
        table: table.filter(|_| (2..=MAX_DISCS).contains(&discs.len())),
        encoding: options.encoding,
        assets: &assets,
    };
    let fits_one =
        discs.len() == 1 || (together.table.is_some() && together.max_size()? <= FAT32_MAX_FILE);
    if fits_one {
        if discs.len() > 1 {
            progress.warn(&format!(
                "{} discs in one EBOOT; switch discs from the HOME menu",
                discs.len()
            ));
        }
        let path = output_dir.join(first_id.sfo()).join("EBOOT.PBP");
        publish(&together, &path, progress, cancel)?;
        return Ok(vec![path]);
    }

    progress.warn(&format!(
        "{} discs as one EBOOT each, sharing one memory card; save before a disc change, \
         then start the next disc's EBOOT",
        discs.len()
    ));
    let mut written = Vec::with_capacity(discs.len());
    for (n, disc) in discs.iter().enumerate() {
        let disc_title = format!("{title} (Disc {})", n + 1);
        let one = Eboot {
            title: &disc_title,
            id: &first_id,
            discs: vec![(disc, &first_id)],
            table: None,
            encoding: options.encoding,
            assets: &assets,
        };
        let path = output_dir
            .join(format!("{}-DISC{}", first_id.sfo(), n + 1))
            .join("EBOOT.PBP");
        publish(&one, &path, progress, cancel)?;
        written.push(path);
    }
    Ok(written)
}

/// Writes `eboot` to `path`, refusing before the first byte a file that
/// could pass FAT32's limit.
fn publish(
    eboot: &Eboot<'_>,
    path: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> PsxPbpResult<()> {
    let size = eboot.max_size()?;
    if size > FAT32_MAX_FILE {
        return Err(PsxPbpError::ExceedsFat32 { size });
    }
    atomic_write(path, true, |out| {
        eboot.write(out, progress, cancel).map(|_| ())
    })
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
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
        let disc1 = ps1_data_track("SLPS_020.05", 40);
        let disc2 = ps1_data_track("SLPS_020.06", 50);
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
        assert_eq!(written, vec![dir.path().join("out/SLPS02005/EBOOT.PBP")]);

        let eboot = std::fs::read(&written[0]).unwrap();
        let sfo = sfo_of(&eboot);
        assert_eq!(sfo.get_str("DISC_ID"), Some("SLPS02005"));
        assert_eq!(sfo.get_str("TITLE"), Some("game"));
        let psar = word(&eboot, 0x24) as usize;
        assert_eq!(&eboot[psar..psar + 16], b"PSTITLEIMG000000");
        assert_eq!(&eboot[psar + 0x264..psar + 0x270], b"_SLPS_02005\0");
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
            .zip([(&disc1, "_SLPS_02005"), (&disc2, "_SLPS_02006")])
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

    async fn build_one(input: &Path, out: &Path, options: &PbpOptions) -> Vec<u8> {
        let written = to_pbp(&NoProgress, input, out, options, CancelToken::new())
            .await
            .unwrap();
        assert_eq!(written.len(), 1);
        std::fs::read(&written[0]).unwrap()
    }

    /// The format reference's worked example: a 2,479,008-byte disc (66
    /// blocks), stored deflate, no icons, a 486-byte boot image.
    #[tokio::test]
    async fn lays_out_the_worked_example() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 1054));
        let splash = dir.path().join("splash.png");
        // A 480x272 PNG header padded to 486 bytes: kept as given, unread.
        let mut png = images::PNG_SIGNATURE.to_vec();
        png.extend_from_slice(b"\0\0\0\x0DIHDR");
        png.extend_from_slice(&480u32.to_be_bytes());
        png.extend_from_slice(&272u32.to_be_bytes());
        png.resize(486, 0);
        std::fs::write(&splash, &png).unwrap();
        let options = PbpOptions {
            encoding: BlockEncoding::Stored,
            splash: Some(splash),
            ..PbpOptions::default()
        };

        let eboot = build_one(&cue, &dir.path().join("out"), &options).await;
        assert_eq!(eboot.len(), 3_598_726);
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
        let image = ps1_data_track("SCES_123.45", 700);
        let cue = write_disc(dir.path(), "My Game", &image);
        let eboot = build_one(&cue, &dir.path().join("out"), &PbpOptions::default()).await;

        let sfo = sfo_of(&eboot);
        assert_eq!(sfo.get_str("CATEGORY"), Some("ME"));
        assert_eq!(sfo.get_str("DISC_ID"), Some("SCES12345"));
        assert_eq!(sfo.get_str("TITLE"), Some("My Game"));
        assert_eq!(sfo.get_u32("BOOTABLE"), Some(1));

        let path = dir.path().join("out/SCES12345/EBOOT.PBP");
        assert_eq!(
            read_info(&path).unwrap().psar_kind,
            Some(PsarKind::Psisoimg)
        );
        let psar = Pbp::read(&mut Cursor::new(&eboot)).unwrap().segments[DATA_PSAR].offset as usize;
        assert_eq!(&eboot[psar + 0x400..psar + 0x40C], b"_SCES_12345\0");
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
                dir.path().join("out/SLPS02005-DISC1/EBOOT.PBP"),
                dir.path().join("out/SLPS02005-DISC2/EBOOT.PBP"),
            ]
        );
        for (n, (path, image)) in written.iter().zip([&disc1, &disc2]).enumerate() {
            let eboot = std::fs::read(path).unwrap();
            let sfo = sfo_of(&eboot);
            assert_eq!(sfo.get_str("DISC_ID"), Some("SLPS02005"));
            assert_eq!(
                sfo.get_str("TITLE").unwrap(),
                format!("game (Disc {})", n + 1)
            );
            let psar = word(&eboot, 0x24) as usize;
            assert_eq!(&eboot[psar..psar + 12], b"PSISOIMG0000");
            assert_eq!(&eboot[psar + 0x400..psar + 0x40C], b"_SLPS_02005\0");
            assert!(read_back(&eboot, psar, image.len()) == *image);
        }
    }

    #[tokio::test]
    async fn a_chd_builds_the_same_eboot_as_its_cue() {
        use crate::disc::chd::{ChdOptions, convert_to_chd};

        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_000.01", 300));
        let chd = dir.path().join("game.chd");
        convert_to_chd(
            &NoProgress,
            cue.clone(),
            chd.clone(),
            ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let from_cue = build_one(&cue, &dir.path().join("a"), &PbpOptions::default()).await;
        let from_chd = build_one(&chd, &dir.path().join("b"), &PbpOptions::default()).await;
        assert!(from_cue == from_chd);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("b")).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "scratch tracks removed");
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
    async fn cancel_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_000.01", 40));
        let cancel = CancelToken::new();
        cancel.cancel();
        let out = dir.path().join("out");
        let err = to_pbp(&NoProgress, &cue, &out, &PbpOptions::default(), cancel)
            .await
            .unwrap_err();
        assert!(matches!(err, PsxPbpError::Cancelled(_)), "{err}");
        assert!(!out.join("SLUS00001/EBOOT.PBP").exists());
    }

    #[tokio::test]
    async fn fits_cover_art_to_the_icon_and_background() {
        use image::{Rgb, Rgba};

        use super::images::test_fixtures::{jpeg, png};

        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_000.01", 40));
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
        let icon = read_info(&dir.path().join("out/SLUS00001/EBOOT.PBP"))
            .unwrap()
            .icon
            .expect("info reads the icon back");
        assert_eq!((icon.width, icon.height), (144, 80));
    }

    #[tokio::test]
    async fn rejects_a_splash_that_is_not_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_000.01", 40));
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
