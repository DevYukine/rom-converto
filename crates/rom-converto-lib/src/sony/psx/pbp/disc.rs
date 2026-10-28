//! A PS1 disc as the EBOOT builder reads it: every `.bin` of a cue sheet
//! back to back, the tracks laid over them, and the disc ID its
//! `SYSTEM.CNF` names.

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use super::error::{PsxPbpError, PsxPbpResult};
use crate::disc::cd::SECTOR_SIZE;
use crate::disc::cue::CueParser;
use crate::disc::cue::models::{FileType, TrackType};
use crate::disc::cue::to_iso::extract_user_data;
use crate::util::iso9660::{SectorSource, read_volume};
use crate::util::pread::file_read_exact_at;

/// One track of a disc, positioned in the whole image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiscTrack {
    pub number: u8,
    pub audio: bool,
    pub mode2: bool,
    /// The track's `INDEX 01`, in sectors from the start of the image.
    pub start: u32,
}

/// The disc a cue sheet describes.
#[derive(Debug, Clone)]
pub(crate) struct DiscImage {
    pub cue: PathBuf,
    files: Vec<(PathBuf, u64)>,
    pub tracks: Vec<DiscTrack>,
}

impl DiscImage {
    /// Reads the cue sheet at `cue`, refusing what a PS1 EBOOT cannot
    /// carry: non-binary files, 2048- or 2336-byte sectors, silence that is
    /// not stored, partial sectors.
    pub fn open(cue: &Path) -> PsxPbpResult<Self> {
        let sheet = CueParser::new(cue).parse_reader(BufReader::new(File::open(cue)?))?;
        let folder = cue.parent().unwrap_or(Path::new("."));

        let mut files = Vec::with_capacity(sheet.files.len());
        let mut file_starts = Vec::with_capacity(sheet.files.len());
        let mut sectors = 0u64;
        for file in &sheet.files {
            if !matches!(file.file_type, FileType::Binary) {
                return Err(PsxPbpError::NotBinary {
                    path: cue.to_path_buf(),
                    file: file.filename.clone(),
                });
            }
            let path = folder.join(&file.filename);
            let size = std::fs::metadata(&path)?.len();
            if size == 0 || !size.is_multiple_of(SECTOR_SIZE as u64) {
                return Err(PsxPbpError::PartialSector { path, size });
            }
            file_starts.push(sectors);
            sectors += size / SECTOR_SIZE as u64;
            files.push((path, size));
        }

        let mut tracks = Vec::with_capacity(sheet.tracks.len());
        for track in &sheet.tracks {
            let (audio, mode2) = match track.track_type {
                TrackType::Audio => (true, false),
                TrackType::Mode1_2352 => (false, false),
                TrackType::Mode2_2352 => (false, true),
                other => {
                    return Err(PsxPbpError::UnsupportedTrackMode {
                        path: cue.to_path_buf(),
                        track: track.number,
                        mode: other.cue_string(),
                    });
                }
            };
            let silence = |gap: Option<crate::disc::cue::models::Msf>| {
                gap.is_some_and(|msf| msf.to_lba() > 0)
            };
            if silence(track.pregap) || silence(track.postgap) {
                return Err(PsxPbpError::PregapNotStored {
                    path: cue.to_path_buf(),
                    track: track.number,
                });
            }
            let index =
                track
                    .primary_index_lba()
                    .ok_or_else(|| PsxPbpError::MissingTrackStart {
                        path: cue.to_path_buf(),
                        track: track.number,
                    })?;
            let file_start = file_starts[track.file_index];
            let file_sectors = files[track.file_index].1 / SECTOR_SIZE as u64;
            if u64::from(index) >= file_sectors {
                return Err(PsxPbpError::TrackPastEnd {
                    path: cue.to_path_buf(),
                    track: track.number,
                });
            }
            tracks.push(DiscTrack {
                number: track.number,
                audio,
                mode2,
                // A CD holds under 450,000 sectors, so the sum fits.
                start: (file_start + u64::from(index)) as u32,
            });
        }
        if tracks.first().is_none_or(|track| track.audio) {
            return Err(PsxPbpError::NoDataTrack(cue.to_path_buf()));
        }

        Ok(Self {
            cue: cue.to_path_buf(),
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
    pub fn reader(&self) -> io::Result<impl Read + use<>> {
        let mut chained: Box<dyn Read> = Box::new(io::empty());
        for (path, size) in &self.files {
            // Never read past the size the layout was planned from.
            chained = Box::new(chained.chain(File::open(path)?.take(*size)));
        }
        Ok(chained)
    }

    /// Reads the disc ID from the boot executable `SYSTEM.CNF` names.
    /// `Ok(None)` when the data track has no ISO9660 file system or its
    /// boot file is not named like a serial.
    pub fn read_disc_id(&self) -> PsxPbpResult<Option<DiscId>> {
        let first = self.tracks[0];
        let mut src = DataTrack {
            files: self
                .files
                .iter()
                .map(|(path, size)| Ok((File::open(path)?, size / SECTOR_SIZE as u64)))
                .collect::<io::Result<_>>()?,
            track_type: if first.mode2 {
                TrackType::Mode2_2352
            } else {
                TrackType::Mode1_2352
            },
            start: u64::from(first.start),
            sectors: self.sectors() - u64::from(first.start),
            raw: vec![0u8; SECTOR_SIZE],
        };
        let Some(volume) = read_volume(&mut src)? else {
            return Ok(None);
        };
        let info = crate::sony::psx::read(&mut src, &volume)?;
        info.title_id.as_deref().map(DiscId::parse).transpose()
    }
}

/// The data track of a [`DiscImage`] as 2048-byte user sectors.
struct DataTrack {
    files: Vec<(File, u64)>,
    track_type: TrackType,
    start: u64,
    sectors: u64,
    raw: Vec<u8>,
}

impl SectorSource for DataTrack {
    fn read_sector(&mut self, lba: u32, buf: &mut [u8; 2048]) -> io::Result<()> {
        if u64::from(lba) >= self.sectors {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "sector past end of image",
            ));
        }
        let mut sector = self.start + u64::from(lba);
        for (file, file_sectors) in &self.files {
            if sector < *file_sectors {
                file_read_exact_at(file, &mut self.raw, sector * SECTOR_SIZE as u64)?;
                let (user, _) =
                    extract_user_data(self.track_type, &self.raw).map_err(io::Error::other)?;
                buf.copy_from_slice(user);
                return Ok(());
            }
            sector -= file_sectors;
        }
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "sector past end of image",
        ))
    }

    fn total_sectors(&self) -> u64 {
        self.sectors
    }
}

/// A PS1 disc ID: four letters and five digits, such as `SLUS00001`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscId {
    prefix: String,
    number: String,
}

impl DiscId {
    /// Parses `SLUS-00001`, `SLUS_000.01` or `SLUS00001`, in any case.
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

    /// The `PARAM.SFO` form, `SLUS00001`.
    pub fn sfo(&self) -> String {
        format!("{}{}", self.prefix, self.number)
    }

    /// The PSISOIMG and PSTITLEIMG form, `_SLUS_00001`.
    pub fn image(&self) -> String {
        format!("_{}_{}", self.prefix, self.number)
    }
}

/// Reads the discs an `.m3u` playlist lists, in order, relative to its
/// folder.
pub(crate) fn read_playlist(path: &Path) -> PsxPbpResult<Vec<PathBuf>> {
    let text = std::fs::read_to_string(path)?;
    let folder = path.parent().unwrap_or(Path::new("."));
    let mut discs = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let entry = line.trim();
        if entry.is_empty() || entry.starts_with('#') {
            continue;
        }
        if !crate::util::is_safe_relative_path(entry) {
            return Err(PsxPbpError::UnsafePlaylistEntry {
                path: path.to_path_buf(),
                entry: entry.to_string(),
            });
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
        let iso = make_iso(&IsoSpec {
            system_id: b"PLAYSTATION",
            volume_sectors: sectors,
            root_entries: &[(b"SYSTEM.CNF;1", false)],
            file_content: cnf.as_bytes(),
        });
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
    use super::test_fixtures::{ps1_data_track, write_disc};
    use super::*;

    #[test]
    fn reads_the_disc_id_from_system_cnf() {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", 64));
        let disc = DiscImage::open(&cue).unwrap();
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
        assert_eq!(DiscImage::open(&cue).unwrap().read_disc_id().unwrap(), None);
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
        let disc = DiscImage::open(&cue).unwrap();
        assert_eq!(disc.sectors(), 300);
        assert_eq!(
            disc.tracks,
            vec![
                DiscTrack {
                    number: 1,
                    audio: false,
                    mode2: true,
                    start: 0
                },
                // t1.bin's 100 sectors, then INDEX 01 two seconds into t2.bin.
                DiscTrack {
                    number: 2,
                    audio: true,
                    mode2: false,
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
        DiscImage::open(&cue)
            .unwrap()
            .reader()
            .unwrap()
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
        let cases: [(&str, fn(&PsxPbpError) -> bool); 5] = [
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
        ];
        for (sheet, expected) in cases {
            let cue = dir.path().join("game.cue");
            std::fs::write(&cue, sheet).unwrap();
            let err = DiscImage::open(&cue).unwrap_err();
            assert!(expected(&err), "{sheet}: {err}");
        }
    }

    #[test]
    fn parses_disc_ids_in_every_written_form() {
        for text in ["SLUS-00001", "slus_000.01", "SLUS00001", " SLUS 00001 "] {
            assert_eq!(DiscId::parse(text).unwrap().sfo(), "SLUS00001", "{text}");
        }
        for text in [
            "SLUS-0001",
            "SL1S-00001",
            "SLUS_0000.1",
            "SLUS",
            "",
            "SLUS-0000A",
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

        std::fs::write(&m3u, "../elsewhere.cue\n").unwrap();
        assert!(matches!(
            read_playlist(&m3u),
            Err(PsxPbpError::UnsafePlaylistEntry { .. })
        ));
        std::fs::write(&m3u, "# nothing\n").unwrap();
        assert!(matches!(
            read_playlist(&m3u),
            Err(PsxPbpError::EmptyPlaylist(_))
        ));
    }
}
