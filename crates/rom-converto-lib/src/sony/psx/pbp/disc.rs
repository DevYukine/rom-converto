//! A PS1 disc as the EBOOT builder reads it: every `.bin` of a cue sheet
//! back to back, the tracks laid over them, and the disc ID its
//! `SYSTEM.CNF` names.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use super::error::{PsxPbpError, PsxPbpResult};
use super::format::MAX_SECTORS;
use crate::disc::cd::{FRAME_SIZE, MAX_TRACKS, SECTOR_SIZE};
use crate::disc::chd::extract::track_bin_paths;
use crate::disc::chd::gdi::{TrackChain, TrackSource};
use crate::disc::chd::layout::{
    cd_track_metadata_text, chd_layout_is_padded, chd_track_decoded_size, chd_validate_cd_hunks,
    chd_validate_frame_total,
};
use crate::disc::chd::reader::cue_generator::{generate_cue_sheet, parse_chd_track_metadata};
use crate::disc::chd::reader::{ChdFlavor, open_chd_sync};
use crate::disc::cue::CueParser;
use crate::disc::cue::models::{CueSheet, FileType, Msf, TrackType};
use crate::sony::disc_source::{ChdSectors, CueSectors};
use crate::util::iso9660::{DiscKind, SectorSource, read_volume};

/// One track of a disc, positioned in the whole image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiscTrack {
    pub number: u8,
    pub audio: bool,
    /// The track's `INDEX 01`, in sectors from the start of the image.
    pub start: u32,
}

/// The disc a cue sheet describes.
#[derive(Debug, Clone)]
pub(crate) struct DiscImage {
    cue: PathBuf,
    /// The file errors name: the cue itself, or the CHD it was extracted
    /// from.
    pub source: PathBuf,
    files: Vec<(PathBuf, u64)>,
    pub tracks: Vec<DiscTrack>,
}

impl DiscImage {
    /// Reads the cue sheet at `cue` and lays its tracks over the files it
    /// names; see [`lay_out`]. Errors name `source`, the file the user
    /// gave, or the file that cannot be read.
    pub fn open(cue: &Path, source: &Path) -> PsxPbpResult<Self> {
        let reader = BufReader::new(File::open(cue).map_err(cannot_read(cue))?);
        let sheet = CueParser::new(cue).parse_reader(reader)?;
        let folder = cue.parent().unwrap_or(Path::new("."));
        let files = sheet
            .files
            .iter()
            .map(|file| {
                let path = folder.join(&file.filename);
                let size = std::fs::metadata(&path).map_err(cannot_read(&path))?.len();
                Ok((path, size))
            })
            .collect::<PsxPbpResult<Vec<_>>>()?;
        let sizes: Vec<u64> = files.iter().map(|&(_, size)| size).collect();
        let tracks = lay_out(&sheet, &sizes, source)?;
        Ok(Self {
            cue: cue.to_path_buf(),
            source: source.to_path_buf(),
            files,
            tracks,
        })
    }

    /// Bytes of raw sectors across every file.
    pub fn image_size(&self) -> u64 {
        self.files.iter().map(|(_, size)| size).sum()
    }

    /// Raw sectors across every file.
    pub fn sectors(&self) -> u64 {
        self.image_size() / SECTOR_SIZE as u64
    }

    /// Streams the image's bytes, file after file in cue order.
    pub fn reader(&self) -> impl Read + use<> {
        TrackChain::new(
            self.files
                .iter()
                .map(|(path, bytes)| TrackSource {
                    path: path.clone(),
                    bytes: *bytes,
                    sector_width: SECTOR_SIZE,
                })
                .collect(),
        )
    }

    /// Reads the disc ID from the boot executable `SYSTEM.CNF` names; see
    /// [`disc_id_of`].
    pub fn read_disc_id(&self) -> PsxPbpResult<Option<DiscId>> {
        let mut sectors = CueSectors::open(&self.cue).map_err(cannot_read(&self.source))?;
        disc_id_of(&mut sectors, &self.source)
    }
}

/// Lays the tracks of `sheet` over its FILEs, `sizes` bytes long,
/// refusing what a PS1 EBOOT cannot carry: non-binary files, 2048- or
/// 2336-byte sectors, silence that is not stored, partial sectors, tracks
/// out of order, a data track that does not start the image, track numbers
/// a TOC cannot hold, and a disc past 99:59:74. Errors name `source`.
fn lay_out(sheet: &CueSheet, sizes: &[u64], source: &Path) -> PsxPbpResult<Vec<DiscTrack>> {
    let path = || source.to_path_buf();
    if sheet.tracks.len() > MAX_TRACKS {
        return Err(PsxPbpError::TooManyTracks {
            path: path(),
            count: sheet.tracks.len(),
        });
    }
    // Modes before file sizes, so a bin of 2048-byte sectors is refused for
    // its mode rather than as partial 2352-byte sectors.
    for (i, track) in sheet.tracks.iter().enumerate() {
        let number = usize::from(track.number);
        if !(1..=MAX_TRACKS).contains(&number) {
            return Err(PsxPbpError::TrackNumberOutOfRange {
                path: path(),
                track: track.number,
            });
        }
        if number != i + 1 {
            return Err(PsxPbpError::TrackOutOfOrder {
                path: path(),
                track: track.number,
            });
        }
        if !matches!(
            track.track_type,
            TrackType::Audio | TrackType::Mode1_2352 | TrackType::Mode2_2352
        ) {
            return Err(PsxPbpError::UnsupportedTrackMode {
                path: path(),
                track: track.number,
                mode: track.track_type.cue_string(),
            });
        }
    }

    let mut file_starts = Vec::with_capacity(sizes.len());
    let mut sectors = 0u64;
    for (file, &size) in sheet.files.iter().zip(sizes) {
        if !matches!(file.file_type, FileType::Binary) {
            return Err(PsxPbpError::NotBinary {
                path: path(),
                file: file.filename.clone(),
            });
        }
        if size == 0 || !size.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(PsxPbpError::PartialSector {
                path: path(),
                file: file.filename.clone(),
                size,
            });
        }
        file_starts.push(sectors);
        sectors += size / SECTOR_SIZE as u64;
    }
    if sectors > MAX_SECTORS {
        return Err(PsxPbpError::DiscTooLong {
            path: path(),
            sectors,
            max: MAX_SECTORS,
        });
    }

    let mut tracks: Vec<DiscTrack> = Vec::with_capacity(sheet.tracks.len());
    for track in &sheet.tracks {
        let silence = |gap: Option<Msf>| gap.is_some_and(|msf| msf.to_lba() > 0);
        if silence(track.pregap) || silence(track.postgap) {
            return Err(PsxPbpError::PregapNotStored {
                path: path(),
                track: track.number,
            });
        }
        let index = track
            .primary_index_lba()
            .ok_or_else(|| PsxPbpError::MissingTrackStart {
                path: path(),
                track: track.number,
            })?;
        let (&file_start, &file_bytes) = file_starts
            .get(track.file_index)
            .zip(sizes.get(track.file_index))
            .ok_or_else(|| PsxPbpError::TrackWithoutFile {
                path: path(),
                track: track.number,
            })?;
        if u64::from(index) >= file_bytes / SECTOR_SIZE as u64 {
            return Err(PsxPbpError::TrackPastEnd {
                path: path(),
                track: track.number,
            });
        }
        let start =
            u32::try_from(file_start + u64::from(index)).map_err(|_| PsxPbpError::DiscTooLong {
                path: path(),
                sectors,
                max: MAX_SECTORS,
            })?;
        if tracks.last().is_some_and(|before| start <= before.start) {
            return Err(PsxPbpError::TrackOutOfOrder {
                path: path(),
                track: track.number,
            });
        }
        tracks.push(DiscTrack {
            number: track.number,
            audio: matches!(track.track_type, TrackType::Audio),
            start,
        });
    }
    match tracks.first() {
        None => return Err(PsxPbpError::NoDataTrack(path())),
        Some(first) if first.audio => return Err(PsxPbpError::NoDataTrack(path())),
        Some(first) if first.start > 0 => {
            return Err(PsxPbpError::DataTrackNotAtStart {
                path: path(),
                start: first.start,
            });
        }
        Some(_) => {}
    }
    Ok(tracks)
}

/// Refuses a CHD before it is extracted: one other than version 5, without
/// CD tracks or with tracks past its stream, which extraction refuses, and
/// otherwise what [`DiscImage::open`] would refuse once it is, by laying out
/// the cue sheet and bins extraction writes. Errors name `chd`.
pub(crate) fn check_chd(chd: &Path) -> PsxPbpResult<()> {
    let handle = open_chd_sync(chd).map_err(cannot_read(chd))?;
    let text = cd_track_metadata_text(&handle.metadata)
        .filter(|_| handle.flavor() == ChdFlavor::Cd)
        .ok_or_else(|| PsxPbpError::NoCdTracks(chd.to_path_buf()))?;
    let tracks = parse_chd_track_metadata(&text).map_err(cannot_read(chd))?;
    chd_validate_cd_hunks(handle.header.hunk_bytes).map_err(cannot_read(chd))?;
    let physical_frames = handle.header.logical_bytes / FRAME_SIZE as u64;
    let padded = chd_layout_is_padded(&tracks, physical_frames);
    chd_validate_frame_total(&tracks, padded, physical_frames).map_err(cannot_read(chd))?;
    // Named like the scratch cue extraction writes, so the sheet parses alike.
    let names: Vec<String> = track_bin_paths(Path::new("disc.cue"), tracks.len())
        .iter()
        .map(|bin| {
            bin.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let sheet = CueParser::new(chd).parse_bytes(generate_cue_sheet(&names, &tracks).as_bytes())?;
    let sizes: Vec<u64> = tracks.iter().map(chd_track_decoded_size).collect();
    lay_out(&sheet, &sizes, chd)?;
    Ok(())
}

/// Maps a failure to open or read `path` to [`PsxPbpError::Open`].
fn cannot_read<E: Into<anyhow::Error>>(path: &Path) -> impl FnOnce(E) -> PsxPbpError {
    move |source| PsxPbpError::Open {
        path: path.to_path_buf(),
        source: source.into(),
    }
}

/// [`DiscImage::read_disc_id`] for a CHD, read in place before it is
/// extracted.
pub(crate) fn read_chd_disc_id(chd: &Path) -> PsxPbpResult<Option<DiscId>> {
    disc_id_of(&mut ChdSectors::open(chd).map_err(cannot_read(chd))?, chd)
}

/// Reads the disc ID from the boot executable `SYSTEM.CNF` names.
/// `Ok(None)` when the data track has no ISO9660 file system, no
/// `SYSTEM.CNF` (the BIOS then boots `PSX.EXE`), or a boot file not named
/// like a serial.
///
/// # Errors
/// Returns [`PsxPbpError::NotPs1Disc`] for a PS2 or PSP disc.
fn disc_id_of<S: SectorSource>(src: &mut S, source: &Path) -> PsxPbpResult<Option<DiscId>> {
    let Some(volume) = read_volume(src)? else {
        return Ok(None);
    };
    match volume.kind {
        DiscKind::Ps1 | DiscKind::UnknownIso => {}
        DiscKind::Ps2Cd | DiscKind::Ps2Dvd | DiscKind::Psp => {
            return Err(PsxPbpError::NotPs1Disc {
                path: source.to_path_buf(),
                kind: volume.kind.label(),
            });
        }
    }
    let info = crate::sony::psx::read(src, &volume)?;
    info.title_id.as_deref().map(DiscId::parse).transpose()
}

/// A PS1 disc ID: four letters and five digits, such as `SLUS99999`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscId {
    prefix: String,
    number: String,
}

impl DiscId {
    /// Parses `SLUS-99999`, `SLUS_999.99` or `SLUS99999`, in any case.
    ///
    /// # Errors
    /// Returns [`PsxPbpError::InvalidDiscId`] for any other shape.
    pub fn parse(text: &str) -> PsxPbpResult<Self> {
        let invalid = || PsxPbpError::InvalidDiscId(text.to_string());
        let trimmed = text.trim();
        let prefix = trimmed.get(..4).ok_or_else(invalid)?;
        let rest = trimmed[4..]
            .strip_prefix(['-', '_', ' '])
            .unwrap_or(&trimmed[4..]);
        let number: String = match rest.split_once('.') {
            Some((high, low)) if high.len() == 3 && low.len() == 2 => format!("{high}{low}"),
            Some(_) => return Err(invalid()),
            None => rest.to_string(),
        };
        if !prefix.bytes().all(|b| b.is_ascii_alphabetic())
            || number.len() != 5
            || !number.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(invalid());
        }
        Ok(Self {
            prefix: prefix.to_ascii_uppercase(),
            number,
        })
    }

    /// The `PARAM.SFO` form, `SLUS99999`.
    pub fn sfo(&self) -> String {
        format!("{}{}", self.prefix, self.number)
    }

    /// The PSISOIMG and PSTITLEIMG form, `_SLUS_99999`.
    pub fn image(&self) -> String {
        format!("_{}_{}", self.prefix, self.number)
    }
}

/// Reads the discs an `.m3u` playlist lists, in order. A relative entry,
/// `..` included, is relative to the playlist's folder; an absolute one is
/// kept as it is.
pub(crate) fn read_playlist(path: &Path) -> PsxPbpResult<Vec<PathBuf>> {
    let text = std::fs::read_to_string(path).map_err(cannot_read(path))?;
    let folder = path.parent().unwrap_or(Path::new("."));
    let mut discs = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let entry = line.trim();
        if entry.is_empty() || entry.starts_with('#') {
            continue;
        }
        discs.push(folder.join(entry));
    }
    if discs.is_empty() {
        return Err(PsxPbpError::EmptyPlaylist(path.to_path_buf()));
    }
    Ok(discs)
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use std::path::{Path, PathBuf};

    use crate::util::iso9660::test_fixtures::{IsoSpec, make_iso};

    /// A PS1 data track whose `SYSTEM.CNF` boots `boot`, as raw MODE2
    /// sectors, `sectors` long.
    pub fn ps1_data_track(boot: &str, sectors: u32) -> Vec<u8> {
        let cnf = format!("BOOT = cdrom:\\{boot};1\r\nTCB = 4\r\n");
        raw_track(
            &make_iso(&IsoSpec {
                system_id: b"PLAYSTATION",
                volume_sectors: sectors,
                root_entries: &[(b"SYSTEM.CNF;1", false)],
                file_content: cnf.as_bytes(),
            }),
            sectors,
        )
    }

    /// A PS2 CD data track, whose `SYSTEM.CNF` boots through `BOOT2`, as
    /// raw MODE2 sectors, `sectors` long.
    pub fn ps2_cd_data_track(sectors: u32) -> Vec<u8> {
        raw_track(
            &make_iso(&IsoSpec {
                system_id: b"PLAYSTATION",
                volume_sectors: sectors,
                root_entries: &[(b"SYSTEM.CNF;1", false)],
                file_content: b"BOOT2 = cdrom0:\\SLUS_999.99;1\r\nVER = 1.00\r\n",
            }),
            sectors,
        )
    }

    /// A data track with no `SYSTEM.CNF`, which the BIOS boots through
    /// `PSX.EXE`, as raw MODE2 sectors, `sectors` long.
    pub fn data_track_without_system_cnf(sectors: u32) -> Vec<u8> {
        raw_track(
            &make_iso(&IsoSpec {
                system_id: b"PLAYSTATION",
                volume_sectors: sectors,
                root_entries: &[],
                file_content: &[],
            }),
            sectors,
        )
    }

    /// `iso` as raw MODE2 sectors, `sectors` long, with deterministic filler
    /// past its end.
    fn raw_track(iso: &[u8], sectors: u32) -> Vec<u8> {
        let mut raw = Vec::with_capacity(sectors as usize * 2352);
        for lba in 0..sectors as usize {
            let mut sector = vec![0u8; 2352];
            sector[1..11].fill(0xFF);
            sector[15] = 2;
            if let Some(user) = iso.get(lba * 2048..(lba + 1) * 2048) {
                sector[24..24 + 2048].copy_from_slice(user);
            } else {
                // Deterministic filler, so a misplaced block shows in a compare.
                for (i, byte) in sector[24..24 + 2048].iter_mut().enumerate() {
                    *byte = (lba * 31 + i) as u8;
                }
            }
            raw.extend_from_slice(&sector);
        }
        raw
    }

    /// Writes `name.bin` holding `bin` and a one-track MODE2 `name.cue`
    /// for it into `dir`, returning the cue's path.
    pub fn write_disc(dir: &Path, name: &str, bin: &[u8]) -> PathBuf {
        std::fs::write(dir.join(format!("{name}.bin")), bin).expect("write bin");
        let cue = dir.join(format!("{name}.cue"));
        std::fs::write(
            &cue,
            format!(
                "FILE \"{name}.bin\" BINARY\r\n  TRACK 01 MODE2/2352\r\n    INDEX 01 00:00:00\r\n"
            ),
        )
        .expect("write cue");
        cue
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::{data_track_without_system_cnf, ps1_data_track, write_disc};
    use super::*;

    #[test]
    fn reads_the_disc_id_from_system_cnf() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 64));
        let disc = DiscImage::open(&cue, &cue).unwrap();
        assert_eq!(disc.sectors(), 64);
        assert_eq!(disc.image_size(), 64 * 2352);
        let id = disc.read_disc_id().unwrap().expect("disc id");
        assert_eq!(
            (id.sfo(), id.image()),
            ("SLUS99999".into(), "_SLUS_99999".into())
        );
    }

    #[test]
    fn a_boot_file_that_is_not_a_serial_has_no_disc_id() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("PSX.EXE", 64));
        assert_eq!(
            DiscImage::open(&cue, &cue).unwrap().read_disc_id().unwrap(),
            None
        );
    }

    #[test]
    fn a_disc_without_system_cnf_has_no_disc_id() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &data_track_without_system_cnf(64));
        assert_eq!(
            DiscImage::open(&cue, &cue).unwrap().read_disc_id().unwrap(),
            None
        );
    }

    #[test]
    fn places_tracks_of_separate_files_in_the_whole_image() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("t1.bin"), vec![0u8; 2352 * 100]).unwrap();
        std::fs::write(dir.path().join("t2.bin"), vec![0u8; 2352 * 200]).unwrap();
        let cue = dir.path().join("game.cue");
        std::fs::write(
            &cue,
            "FILE \"t1.bin\" BINARY\n TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\n\
             FILE \"t2.bin\" BINARY\n TRACK 02 AUDIO\n  INDEX 00 00:00:00\n  INDEX 01 00:02:00\n",
        )
        .unwrap();
        let disc = DiscImage::open(&cue, &cue).unwrap();
        assert_eq!(disc.sectors(), 300);
        assert_eq!(
            disc.tracks,
            vec![
                DiscTrack {
                    number: 1,
                    audio: false,
                    start: 0
                },
                // t1.bin's 100 sectors, then INDEX 01 two seconds into t2.bin.
                DiscTrack {
                    number: 2,
                    audio: true,
                    start: 100 + 150
                },
            ]
        );
    }

    #[test]
    fn streams_every_file_in_cue_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("t1.bin"), vec![1u8; 2352]).unwrap();
        std::fs::write(dir.path().join("t2.bin"), vec![2u8; 2352]).unwrap();
        let cue = dir.path().join("game.cue");
        std::fs::write(
            &cue,
            "FILE \"t1.bin\" BINARY\n TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\n\
             FILE \"t2.bin\" BINARY\n TRACK 02 AUDIO\n  INDEX 01 00:00:00\n",
        )
        .unwrap();
        let mut bytes = Vec::new();
        DiscImage::open(&cue, &cue)
            .unwrap()
            .reader()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes.len(), 2 * 2352);
        assert!(bytes[..2352].iter().all(|&b| b == 1));
        assert!(bytes[2352..].iter().all(|&b| b == 2));
    }

    #[test]
    fn refuses_what_an_eboot_cannot_carry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("game.bin"), vec![0u8; 2352 * 4]).unwrap();
        std::fs::write(dir.path().join("odd.bin"), vec![0u8; 2352 * 4 + 1]).unwrap();
        type MatchesError = fn(&PsxPbpError) -> bool;
        let hundred_tracks: String = (1..=100)
            .map(|n| format!(" TRACK {n:02} MODE2/2352\n  INDEX 01 00:00:00\n"))
            .collect();
        let hundred_tracks = format!("FILE \"game.bin\" BINARY\n{hundred_tracks}");
        let cases: [(&str, MatchesError); 10] = [
            (
                "FILE \"game.bin\" BINARY\n TRACK 01 MODE1/2048\n  INDEX 01 00:00:00\n",
                |e| matches!(e, PsxPbpError::UnsupportedTrackMode { .. }),
            ),
            (
                "FILE \"game.bin\" BINARY\n TRACK 01 MODE2/2352\n  PREGAP 00:02:00\n  INDEX 01 00:00:00\n",
                |e| matches!(e, PsxPbpError::PregapNotStored { .. }),
            ),
            (
                "FILE \"odd.bin\" BINARY\n TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\n",
                |e| matches!(e, PsxPbpError::PartialSector { .. }),
            ),
            (
                "FILE \"game.bin\" BINARY\n TRACK 01 AUDIO\n  INDEX 01 00:00:00\n",
                |e| matches!(e, PsxPbpError::NoDataTrack(_)),
            ),
            (
                "FILE \"game.bin\" BINARY\n TRACK 01 MODE2/2352\n  INDEX 01 00:00:10\n",
                |e| matches!(e, PsxPbpError::TrackPastEnd { .. }),
            ),
            (" TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\n", |e| {
                matches!(e, PsxPbpError::TrackWithoutFile { .. })
            }),
            (
                "FILE \"game.bin\" BINARY\n TRACK 01 MODE2/2352\n  INDEX 00 00:00:00\n  INDEX 01 00:00:02\n",
                |e| matches!(e, PsxPbpError::DataTrackNotAtStart { start: 2, .. }),
            ),
            (
                "FILE \"game.bin\" BINARY\n TRACK 00 MODE2/2352\n  INDEX 01 00:00:00\n",
                |e| matches!(e, PsxPbpError::TrackNumberOutOfRange { track: 0, .. }),
            ),
            (
                "FILE \"game.bin\" BINARY\n TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\n TRACK 100 AUDIO\n  INDEX 01 00:00:01\n",
                |e| matches!(e, PsxPbpError::TrackNumberOutOfRange { track: 100, .. }),
            ),
            (&hundred_tracks, |e| {
                matches!(e, PsxPbpError::TooManyTracks { count: 100, .. })
            }),
        ];
        for (sheet, expected) in cases {
            let cue = dir.path().join("game.cue");
            std::fs::write(&cue, sheet).unwrap();
            let err = DiscImage::open(&cue, &cue).unwrap_err();
            assert!(expected(&err), "{sheet}: {err}");
        }
    }

    /// Opens a cue sheet of `tracks` over one 40-sector bin.
    fn open_tracks(tracks: &str) -> PsxPbpResult<DiscImage> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("game.bin"), vec![0u8; 2352 * 40]).unwrap();
        let cue = dir.path().join("game.cue");
        std::fs::write(&cue, format!("FILE \"game.bin\" BINARY\n{tracks}")).unwrap();
        DiscImage::open(&cue, &cue)
    }

    #[test]
    fn refuses_a_repeated_track_number() {
        let err = open_tracks(
            " TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\n TRACK 01 MODE2/2352\n  INDEX 01 00:00:10\n",
        )
        .unwrap_err();
        assert!(
            matches!(err, PsxPbpError::TrackOutOfOrder { track: 1, .. }),
            "{err}"
        );
    }

    #[test]
    fn refuses_a_gap_in_track_numbers() {
        let err = open_tracks(
            " TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\n TRACK 03 AUDIO\n  INDEX 01 00:00:10\n",
        )
        .unwrap_err();
        assert!(
            matches!(err, PsxPbpError::TrackOutOfOrder { track: 3, .. }),
            "{err}"
        );
    }

    #[test]
    fn refuses_a_track_that_starts_before_the_one_before_it() {
        let err = open_tracks(
            " TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\n TRACK 02 AUDIO\n  INDEX 01 00:00:20\n\
             TRACK 03 AUDIO\n  INDEX 01 00:00:10\n",
        )
        .unwrap_err();
        assert!(
            matches!(err, PsxPbpError::TrackOutOfOrder { track: 3, .. }),
            "{err}"
        );
    }

    /// Extraction refuses a CHD whose track metadata claims more frames than
    /// its stream holds; the pre-check refuses it first, naming the CHD.
    #[test]
    fn refuses_a_chd_whose_tracks_run_past_its_stream() {
        let dir = tempfile::tempdir().unwrap();
        let chd = dir.path().join("game.chd");
        crate::disc::chd::test_fixtures::write_raw_cd_chd(
            &chd,
            *b"CHT2",
            "TRACK:1 TYPE:MODE2_RAW SUBTYPE:NONE FRAMES:80",
            &vec![0u8; FRAME_SIZE * 40],
            FRAME_SIZE as u32 * 4,
            FRAME_SIZE as u32,
        );
        let err = check_chd(&chd).unwrap_err();
        assert!(
            matches!(&err, PsxPbpError::Open { path, .. } if *path == chd),
            "{err}"
        );
    }

    #[test]
    fn a_missing_bin_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let cue = dir.path().join("game.cue");
        std::fs::write(
            &cue,
            "FILE \"gone.bin\" BINARY\n TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\n",
        )
        .unwrap();
        let err = DiscImage::open(&cue, &cue).unwrap_err();
        assert!(err.to_string().contains("gone.bin"), "{err}");
    }

    #[test]
    fn a_missing_playlist_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_playlist(&dir.path().join("game.m3u")).unwrap_err();
        assert!(err.to_string().contains("game.m3u"), "{err}");
    }

    #[test]
    fn refuses_a_disc_past_99_59_74() {
        let dir = tempfile::tempdir().unwrap();
        let bin = std::fs::File::create(dir.path().join("game.bin")).unwrap();
        bin.set_len((MAX_SECTORS + 1) * SECTOR_SIZE as u64).unwrap();
        let cue = dir.path().join("game.cue");
        std::fs::write(
            &cue,
            "FILE \"game.bin\" BINARY\n TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\n",
        )
        .unwrap();
        let err = DiscImage::open(&cue, &cue).unwrap_err();
        assert!(
            matches!(err, PsxPbpError::DiscTooLong { sectors, max: MAX_SECTORS, .. } if sectors == MAX_SECTORS + 1),
            "{err}"
        );

        let bin = std::fs::File::options()
            .write(true)
            .open(dir.path().join("game.bin"))
            .unwrap();
        bin.set_len(MAX_SECTORS * SECTOR_SIZE as u64).unwrap();
        assert_eq!(DiscImage::open(&cue, &cue).unwrap().sectors(), MAX_SECTORS);
    }

    #[test]
    fn parses_disc_ids_in_every_written_form() {
        for text in ["SLUS-99999", "slus_999.99", "SLUS99999", " SLUS 99999 "] {
            assert_eq!(DiscId::parse(text).unwrap().sfo(), "SLUS99999", "{text}");
        }
        for text in [
            "SLUS-9999",
            "SL1S-99999",
            "SLUS_9999.9",
            "SLUS",
            "",
            "SLUS-9999A",
        ] {
            assert!(DiscId::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn playlist_lists_discs_relative_to_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let m3u = dir.path().join("game.m3u");
        std::fs::write(
            &m3u,
            "\u{feff}# discs\r\ngame (Disc 1).cue\r\n\r\ngame (Disc 2).chd\r\n",
        )
        .unwrap();
        assert_eq!(
            read_playlist(&m3u).unwrap(),
            vec![
                dir.path().join("game (Disc 1).cue"),
                dir.path().join("game (Disc 2).chd")
            ]
        );

        let elsewhere = dir.path().join("elsewhere/game.cue");
        std::fs::write(&m3u, format!("../up.cue\n{}\n", elsewhere.display())).unwrap();
        assert_eq!(
            read_playlist(&m3u).unwrap(),
            vec![dir.path().join("../up.cue"), elsewhere]
        );
        std::fs::write(&m3u, "# nothing\n").unwrap();
        assert!(matches!(
            read_playlist(&m3u),
            Err(PsxPbpError::EmptyPlaylist(_))
        ));
    }
}
