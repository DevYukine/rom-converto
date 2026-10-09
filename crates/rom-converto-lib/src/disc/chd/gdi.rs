//! GD-ROM track layout from a `.gdi` sheet: the per-track frame counts
//! and the zero padding between tracks that the reference `parse_gdi`
//! derives, plus the sector stream the CD writer ingests. The
//! [`TrackChain`] ingest reader also serves the `.cue` compression
//! path.

use crate::disc::cd::{IO_BUFFER_SIZE, MAX_TRACKS, SECTOR_SIZE};
use crate::disc::chd::error::{ChdError, ChdResult};
use crate::disc::chd::reader::cue_generator::MAX_TRACK_FRAMES;
use crate::disc::cue::models::TrackType;
use crate::sega::katana::{GdiIndex, GdiTrack};
use std::io::Read;
use std::path::PathBuf;

/// A GD-ROM ends near LBA 549,150; the cap keeps a bogus LBA from
/// turning into a huge padding run.
const MAX_GDI_LBA: u64 = 600_000;

/// Rejects a sheet whose track list is empty, declares more entries
/// than a disc can hold, or disagrees with its declared count, before
/// anything touches the track files.
pub(crate) fn check_track_count(index: &GdiIndex) -> ChdResult<()> {
    if index.tracks.is_empty() {
        return Err(ChdError::InvalidGdi("lists no tracks".to_string()));
    }
    if index.track_count > MAX_TRACKS {
        return Err(ChdError::TooManyGdiTracks {
            declared: index.track_count,
        });
    }
    if index.tracks.len() != index.track_count {
        return Err(ChdError::InvalidGdi(format!(
            "declares {} tracks but lists {}",
            index.track_count,
            index.tracks.len()
        )));
    }
    Ok(())
}

/// One track of a GD-ROM CHD as the writer lays it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GdTrack {
    pub number: u32,
    pub track_type: TrackType,
    /// Frames recorded in `FRAMES:`: the file's sectors plus `pad_frames`.
    pub frames: u32,
    /// Zero frames appended so the next track starts at its sheet LBA.
    pub pad_frames: u32,
}

impl GdTrack {
    /// Frames read from the track file.
    pub fn source_frames(&self) -> u32 {
        self.frames - self.pad_frames
    }
}

/// Lays out the sheet's tracks the way the reference `parse_gdi` does,
/// from one `(track line, file size)` pair per track so a layout
/// without a size for every line cannot be expressed. A track's frames
/// are its file's whole sectors; the gap to the next track's LBA (the
/// pregap, or the jump to the high-density area at LBA 45000) is
/// appended to the track as zero frames and recorded as `PAD:`, the
/// layout the reference implementation writes. Newer development
/// builds record the pregaps as virtual `PREGAP:` entries instead.
///
/// The slice must be non-empty and hold one pair per sheet line:
/// callers run [`check_track_count`] first, so the empty check here is
/// a contract for direct callers rather than a reachable error.
///
/// # Errors
/// [`ChdError::InvalidGdi`] when the pairs are empty, the tracks are
/// not numbered in order from 1, a track starts beyond the LBA cap, or
/// names a file with more sectors than a CHD track can hold;
/// [`ChdError::UnsupportedGdiTrack`] for a type and sector size the
/// format does not define; [`ChdError::EmptyGdiTrack`] for a file
/// shorter than one sector; [`ChdError::GdiTrackOverlap`] when a track
/// starts before the previous one ends.
pub(crate) fn gd_layout(files: &[(&GdiTrack, u64)]) -> ChdResult<Vec<GdTrack>> {
    if files.is_empty() {
        return Err(ChdError::InvalidGdi("lists no tracks".to_string()));
    }

    let mut tracks: Vec<GdTrack> = Vec::with_capacity(files.len());
    // Sheet LBA of the previous track.
    let mut prev_start = 0u64;
    for (i, (track, size)) in files.iter().enumerate() {
        if track.number as usize != i + 1 {
            return Err(ChdError::InvalidGdi(format!(
                "track line {} is numbered {}; tracks must be listed in order from 1",
                i + 1,
                track.number
            )));
        }
        let track_type = match (track.track_type, track.sector_size) {
            (4, 2352) => TrackType::Mode1_2352,
            (4, 2048) => TrackType::Mode1_2048,
            (0, 2352) => TrackType::Audio,
            _ => {
                return Err(ChdError::UnsupportedGdiTrack {
                    track: track.number,
                    track_type: track.track_type,
                    sector_size: track.sector_size,
                });
            }
        };
        let frames = size / u64::from(track.sector_size);
        if frames > u64::from(MAX_TRACK_FRAMES) {
            return Err(ChdError::InvalidGdi(format!(
                "track {} file holds {frames} sectors; a CHD track holds at most \
                 {MAX_TRACK_FRAMES}",
                track.number
            )));
        }
        // Bounded just above.
        let frames = frames as u32;
        if frames == 0 {
            return Err(ChdError::EmptyGdiTrack {
                track: track.number,
            });
        }
        if track.lba > MAX_GDI_LBA {
            return Err(ChdError::InvalidGdi(format!(
                "track {} starts at LBA {}, beyond any GD-ROM",
                track.number, track.lba
            )));
        }

        if let Some(prev) = tracks.last_mut() {
            let gap = track
                .lba
                .checked_sub(prev_start + u64::from(prev.frames))
                .ok_or(ChdError::GdiTrackOverlap {
                    track: track.number,
                })?;
            // Both bounded by the caps above, so the sum fits.
            let gap = gap as u32;
            prev.frames += gap;
            prev.pad_frames = gap;
        }
        prev_start = track.lba;
        tracks.push(GdTrack {
            number: track.number,
            track_type,
            frames,
            pad_frames: 0,
        });
    }
    Ok(tracks)
}

/// The track files read back to back, each cut at its whole-sector
/// length. A 2048-byte data sector is widened into a 2352-byte frame
/// with a zero tail, the slot the reference ingest leaves it in, so the
/// writer sees one uniform raw-sector stream.
pub(crate) struct TrackChain {
    pending: std::vec::IntoIter<TrackSource>,
    current: Option<(std::io::Take<std::io::BufReader<std::fs::File>>, usize)>,
    frame: [u8; SECTOR_SIZE],
    frame_pos: usize,
}

/// One track file: its path, the bytes to read, and its sector width.
pub(crate) struct TrackSource {
    pub path: PathBuf,
    pub bytes: u64,
    pub sector_width: usize,
}

impl TrackChain {
    pub fn new(sources: Vec<TrackSource>) -> Self {
        Self {
            pending: sources.into_iter(),
            current: None,
            frame: [0; SECTOR_SIZE],
            frame_pos: SECTOR_SIZE,
        }
    }
}

impl Read for TrackChain {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.frame_pos < SECTOR_SIZE {
                let n = buf.len().min(SECTOR_SIZE - self.frame_pos);
                buf[..n].copy_from_slice(&self.frame[self.frame_pos..self.frame_pos + n]);
                self.frame_pos += n;
                return Ok(n);
            }
            if let Some((reader, width)) = &mut self.current {
                if *width == SECTOR_SIZE {
                    let n = reader.read(buf)?;
                    if n > 0 || buf.is_empty() {
                        return Ok(n);
                    }
                } else {
                    let n = reader.read(&mut self.frame[..*width])?;
                    if n > 0 {
                        reader.read_exact(&mut self.frame[n..*width])?;
                        self.frame[*width..].fill(0);
                        self.frame_pos = 0;
                        continue;
                    }
                }
                self.current = None;
            }
            let Some(source) = self.pending.next() else {
                return Ok(0);
            };
            let file = std::fs::File::open(&source.path)?;
            self.current = Some((
                std::io::BufReader::with_capacity(IO_BUFFER_SIZE, file).take(source.bytes),
                source.sector_width,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(lines: &[(u32, u64, u32, u32)]) -> GdiIndex {
        GdiIndex {
            track_count: lines.len(),
            tracks: lines
                .iter()
                .map(|&(number, lba, track_type, sector_size)| GdiTrack {
                    number,
                    lba,
                    track_type,
                    sector_size,
                    filename: format!("track{number:02}.bin"),
                })
                .collect(),
        }
    }

    fn layout(lines: &[(u32, u64, u32, u32)], sizes: &[u64]) -> ChdResult<Vec<GdTrack>> {
        let index = index(lines);
        let files: Vec<(&GdiTrack, u64)> = index.tracks.iter().zip(sizes.iter().copied()).collect();
        gd_layout(&files)
    }

    /// Four-track set: the 150-frame gap ahead of the audio track, the
    /// jump to LBA 45000 and the 250-frame gap after the high-density
    /// start all pad the track before them.
    #[test]
    fn layout_follows_the_reference_gap_rules() {
        let lines = [
            (1, 0, 4, 2352),
            (2, 450, 0, 2352),
            (3, 45000, 4, 2352),
            (4, 45350, 0, 2352),
        ];
        let sizes = [300 * 2352, 200 * 2352, 100 * 2352, 50 * 2352];
        let tracks = layout(&lines, &sizes).unwrap();
        assert_eq!(
            tracks,
            vec![
                GdTrack {
                    number: 1,
                    track_type: TrackType::Mode1_2352,
                    frames: 450,
                    pad_frames: 150,
                },
                GdTrack {
                    number: 2,
                    track_type: TrackType::Audio,
                    frames: 200 + 44350,
                    pad_frames: 44350,
                },
                GdTrack {
                    number: 3,
                    track_type: TrackType::Mode1_2352,
                    frames: 350,
                    pad_frames: 250,
                },
                GdTrack {
                    number: 4,
                    track_type: TrackType::Audio,
                    frames: 50,
                    pad_frames: 0,
                },
            ]
        );
    }

    #[test]
    fn layout_accepts_2048_data_tracks_and_partial_trailing_sectors() {
        let tracks = layout(
            &[(1, 0, 4, 2048), (2, 45000, 4, 2352)],
            &[10 * 2048 + 7, 3 * 2352],
        )
        .unwrap();
        assert_eq!(tracks[0].track_type, TrackType::Mode1_2048);
        assert_eq!(tracks[0].frames, 10 + 44990);
        assert_eq!(tracks[0].pad_frames, 44990);
        assert_eq!(tracks[0].source_frames(), 10);
        assert_eq!(tracks[1].frames, 3);
    }

    #[test]
    fn track_count_checks_name_the_real_problem() {
        let short = GdiIndex {
            track_count: 3,
            ..index(&[(1, 0, 4, 2352), (2, 450, 0, 2352)])
        };
        assert!(matches!(
            check_track_count(&short),
            Err(ChdError::InvalidGdi(_))
        ));
        assert!(matches!(
            check_track_count(&index(&[])),
            Err(ChdError::InvalidGdi(_))
        ));
        // A sheet claiming more tracks than a disc can hold is named as
        // such, not as a mismatch against the lines it lists.
        let flooded = GdiIndex {
            track_count: 150,
            tracks: index(&[(1, 0, 4, 2352)]).tracks,
        };
        assert!(matches!(
            check_track_count(&flooded),
            Err(ChdError::TooManyGdiTracks { declared: 150 })
        ));
    }

    #[test]
    fn layout_rejects_malformed_sheets() {
        assert!(matches!(gd_layout(&[]), Err(ChdError::InvalidGdi(_))));
        assert!(matches!(
            layout(&[(2, 0, 4, 2352)], &[2352]),
            Err(ChdError::InvalidGdi(_))
        ));
        assert!(matches!(
            layout(&[(1, 0, 4, 2336)], &[2336]),
            Err(ChdError::UnsupportedGdiTrack {
                track: 1,
                track_type: 4,
                sector_size: 2336
            })
        ));
        assert!(matches!(
            layout(&[(1, 0, 0, 2048)], &[2048]),
            Err(ChdError::UnsupportedGdiTrack { .. })
        ));
        assert!(matches!(
            layout(&[(1, 0, 4, 2352)], &[2351]),
            Err(ChdError::EmptyGdiTrack { track: 1 })
        ));
        assert!(matches!(
            layout(&[(1, 0, 4, 2352), (2, 2, 0, 2352)], &[3 * 2352, 2352]),
            Err(ChdError::GdiTrackOverlap { track: 2 })
        ));
        assert!(matches!(
            layout(
                &[(1, 0, 4, 2352), (2, MAX_GDI_LBA + 1, 0, 2352)],
                &[2352, 2352]
            ),
            Err(ChdError::InvalidGdi(_))
        ));
        assert!(matches!(
            layout(
                &[(1, 0, 4, 2352)],
                &[(u64::from(MAX_TRACK_FRAMES) + 1) * 2352]
            ),
            Err(ChdError::InvalidGdi(_))
        ));
    }

    #[test]
    fn track_chain_widens_2048_sectors_and_cuts_partial_tails() {
        let dir = tempfile::tempdir().unwrap();
        let narrow = dir.path().join("a.bin");
        let raw = dir.path().join("b.bin");
        let narrow_bytes: Vec<u8> = (0..2 * 2048 + 5).map(|i| i as u8).collect();
        let raw_bytes: Vec<u8> = (0..2352 + 9).map(|i| (i * 3) as u8).collect();
        std::fs::write(&narrow, &narrow_bytes).unwrap();
        std::fs::write(&raw, &raw_bytes).unwrap();

        let mut chain = TrackChain::new(vec![
            TrackSource {
                path: narrow,
                bytes: 2 * 2048,
                sector_width: 2048,
            },
            TrackSource {
                path: raw,
                bytes: 2352,
                sector_width: 2352,
            },
        ]);
        let mut out = Vec::new();
        chain.read_to_end(&mut out).unwrap();

        let mut expected = Vec::new();
        for sector in narrow_bytes[..2 * 2048].chunks(2048) {
            expected.extend_from_slice(sector);
            expected.extend(std::iter::repeat_n(0u8, 2352 - 2048));
        }
        expected.extend_from_slice(&raw_bytes[..2352]);
        assert_eq!(out, expected);
    }
}
