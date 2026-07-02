use crate::dat::digest::TrackDigests;
use crate::dat::model::{GameMatchType, PlaymatchGameFile};
use crate::util::HashAlgo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchStrength {
    Verified(HashAlgo),
    NameSizeHint,
    NoMatch,
}

impl MatchStrength {
    pub fn is_verified(self) -> bool {
        matches!(self, MatchStrength::Verified(_))
    }
}

pub fn match_strength(t: GameMatchType) -> MatchStrength {
    match t {
        GameMatchType::Sha256 => MatchStrength::Verified(HashAlgo::Sha256),
        GameMatchType::Sha1 => MatchStrength::Verified(HashAlgo::Sha1),
        GameMatchType::Md5 => MatchStrength::Verified(HashAlgo::Md5),
        GameMatchType::Crc => MatchStrength::Verified(HashAlgo::Crc32),
        GameMatchType::FileNameAndSize => MatchStrength::NameSizeHint,
        GameMatchType::NoMatch => MatchStrength::NoMatch,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatVerdict {
    Verified,
    Hint,
    Unknown,
    Misnamed,
    Renamed,
    Skipped,
    Unsupported,
    Failed,
}

impl DatVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            DatVerdict::Verified => "verified",
            DatVerdict::Hint => "hint",
            DatVerdict::Unknown => "unknown",
            DatVerdict::Misnamed => "misnamed",
            DatVerdict::Renamed => "renamed",
            DatVerdict::Skipped => "skipped",
            DatVerdict::Unsupported => "unsupported",
            DatVerdict::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TrackCheck {
    pub track_number: u32,
    pub matched_file: Option<String>,
    pub algo: Option<HashAlgo>,
    pub ok: bool,
}

#[derive(Debug, Clone)]
pub struct SetReconciliation {
    pub tracks: Vec<TrackCheck>,
    pub missing_remote: Vec<String>,
    pub all_ok: bool,
}

/// Case-insensitive hex compare after trimming; crc padded to 8 chars.
pub fn hashes_equal(local: &str, remote: &str) -> bool {
    let l = local.trim();
    let r = remote.trim();
    if l.is_empty() || r.is_empty() {
        return false;
    }
    l.eq_ignore_ascii_case(r)
}

fn crc_equal(local: &str, remote: &str) -> bool {
    let pad = |s: &str| format!("{:0>8}", s.trim());
    pad(local).eq_ignore_ascii_case(&pad(remote))
}

// Strongest-hash-first probe of one remote file against one local track.
// Returns the algo that reconciled, or None. crc requires equal fileSize.
fn track_reconciles(local: &TrackDigests, remote: &PlaymatchGameFile) -> Option<HashAlgo> {
    let d = &local.digests;
    if let (Some(l), Some(r)) = (&d.sha256, &remote.sha256)
        && hashes_equal(l, r)
    {
        return Some(HashAlgo::Sha256);
    }
    if let (Some(l), Some(r)) = (&d.sha1, &remote.sha1)
        && hashes_equal(l, r)
    {
        return Some(HashAlgo::Sha1);
    }
    if let (Some(l), Some(r)) = (&d.md5, &remote.md5)
        && hashes_equal(l, r)
    {
        return Some(HashAlgo::Md5);
    }
    if let (Some(l), Some(r)) = (&d.crc32, &remote.crc)
        && remote.file_size_in_bytes == Some(d.size_bytes)
        && crc_equal(l, r)
    {
        return Some(HashAlgo::Crc32);
    }
    None
}

/// Match each local track digest against gameFiles[] by strongest available
/// hash. Each remote file may reconcile at most one local track (greedy,
/// strongest-hash-first). crc only counts together with equal fileSize.
pub fn reconcile_tracks(local: &[TrackDigests], remote: &[PlaymatchGameFile]) -> SetReconciliation {
    let mut used = vec![false; remote.len()];
    let mut tracks = Vec::with_capacity(local.len());

    for lt in local {
        let mut best: Option<(usize, HashAlgo)> = None;
        for (ri, rf) in remote.iter().enumerate() {
            if used[ri] {
                continue;
            }
            if let Some(algo) = track_reconciles(lt, rf) {
                let stronger = match best {
                    None => true,
                    Some((_, cur)) => algo_rank(algo) < algo_rank(cur),
                };
                if stronger {
                    best = Some((ri, algo));
                }
            }
        }
        match best {
            Some((ri, algo)) => {
                used[ri] = true;
                tracks.push(TrackCheck {
                    track_number: lt.track_number,
                    matched_file: Some(remote[ri].file_name.clone()),
                    algo: Some(algo),
                    ok: true,
                });
            }
            None => tracks.push(TrackCheck {
                track_number: lt.track_number,
                matched_file: None,
                algo: None,
                ok: false,
            }),
        }
    }

    let missing_remote: Vec<String> = remote
        .iter()
        .zip(used.iter())
        .filter(|(_, u)| !**u)
        .map(|(rf, _)| rf.file_name.clone())
        .collect();

    let all_ok = tracks.iter().all(|t| t.ok) && missing_remote.is_empty();
    SetReconciliation {
        tracks,
        missing_remote,
        all_ok,
    }
}

fn algo_rank(a: HashAlgo) -> u8 {
    match a {
        HashAlgo::Sha256 => 0,
        HashAlgo::Sha1 => 1,
        HashAlgo::Md5 => 2,
        HashAlgo::Crc32 => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::FileDigests;

    fn track(n: u32, sha1: Option<&str>, crc: Option<&str>, size: u64) -> TrackDigests {
        TrackDigests {
            track_number: n,
            track_type: "MODE1".to_string(),
            digests: FileDigests {
                crc32: crc.map(str::to_string),
                sha1: sha1.map(str::to_string),
                md5: None,
                sha256: None,
                size_bytes: size,
            },
        }
    }

    fn remote(
        name: &str,
        sha1: Option<&str>,
        crc: Option<&str>,
        size: Option<u64>,
    ) -> PlaymatchGameFile {
        PlaymatchGameFile {
            id: "id".to_string(),
            game_id: "g".to_string(),
            file_name: name.to_string(),
            file_size_in_bytes: size,
            crc: crc.map(str::to_string),
            md5: None,
            sha1: sha1.map(str::to_string),
            sha256: None,
            current_in_latest_dat: true,
            last_seen_dat_version: None,
        }
    }

    #[test]
    fn match_strength_only_hash_rungs_verified() {
        assert!(match_strength(GameMatchType::Sha256).is_verified());
        assert!(match_strength(GameMatchType::Sha1).is_verified());
        assert!(match_strength(GameMatchType::Md5).is_verified());
        assert!(match_strength(GameMatchType::Crc).is_verified());
        assert_eq!(
            match_strength(GameMatchType::FileNameAndSize),
            MatchStrength::NameSizeHint
        );
        assert!(!match_strength(GameMatchType::FileNameAndSize).is_verified());
        assert_eq!(
            match_strength(GameMatchType::NoMatch),
            MatchStrength::NoMatch
        );
    }

    #[test]
    fn match_strength_reports_which_algo() {
        assert_eq!(
            match_strength(GameMatchType::Sha1),
            MatchStrength::Verified(HashAlgo::Sha1)
        );
        assert_eq!(
            match_strength(GameMatchType::Crc),
            MatchStrength::Verified(HashAlgo::Crc32)
        );
    }

    #[test]
    fn reconcile_all_ok() {
        let local = vec![
            track(1, Some("aaaa"), None, 100),
            track(2, Some("bbbb"), None, 200),
        ];
        let remote = vec![
            remote("t1.bin", Some("AAAA"), None, Some(100)),
            remote("t2.bin", Some("bbbb"), None, Some(200)),
        ];
        let r = reconcile_tracks(&local, &remote);
        assert!(r.all_ok);
        assert!(r.tracks.iter().all(|t| t.ok));
        assert_eq!(r.tracks[0].algo, Some(HashAlgo::Sha1));
        assert!(r.missing_remote.is_empty());
    }

    #[test]
    fn reconcile_missing_local_track() {
        let local = vec![track(1, Some("aaaa"), None, 100)];
        let remote = vec![
            remote("t1.bin", Some("aaaa"), None, Some(100)),
            remote("t2.bin", Some("bbbb"), None, Some(200)),
        ];
        let r = reconcile_tracks(&local, &remote);
        assert!(!r.all_ok);
        assert_eq!(r.missing_remote, vec!["t2.bin".to_string()]);
    }

    #[test]
    fn reconcile_missing_remote_match() {
        let local = vec![
            track(1, Some("aaaa"), None, 100),
            track(2, Some("cccc"), None, 200),
        ];
        let remote = vec![remote("t1.bin", Some("aaaa"), None, Some(100))];
        let r = reconcile_tracks(&local, &remote);
        assert!(!r.all_ok);
        assert!(r.tracks[0].ok);
        assert!(!r.tracks[1].ok);
        assert!(r.tracks[1].matched_file.is_none());
    }

    #[test]
    fn crc_needs_matching_size() {
        let local = vec![track(1, None, Some("1234abcd"), 100)];
        let wrong_size = vec![remote("t1.bin", None, Some("1234abcd"), Some(999))];
        assert!(!reconcile_tracks(&local, &wrong_size).all_ok);

        let right_size = vec![remote("t1.bin", None, Some("1234abcd"), Some(100))];
        let r = reconcile_tracks(&local, &right_size);
        assert!(r.all_ok);
        assert_eq!(r.tracks[0].algo, Some(HashAlgo::Crc32));
    }

    #[test]
    fn crc_padding_compares_equal() {
        let local = vec![track(1, None, Some("abcd"), 100)];
        let remote = vec![remote("t1.bin", None, Some("0000abcd"), Some(100))];
        assert!(reconcile_tracks(&local, &remote).all_ok);
    }

    #[test]
    fn each_remote_used_once() {
        let local = vec![
            track(1, Some("aaaa"), None, 100),
            track(2, Some("aaaa"), None, 100),
        ];
        let remote = vec![remote("t1.bin", Some("aaaa"), None, Some(100))];
        let r = reconcile_tracks(&local, &remote);
        assert!(r.tracks[0].ok);
        assert!(
            !r.tracks[1].ok,
            "one remote file reconciles at most one local track"
        );
    }

    #[test]
    fn verdict_strings_are_lowercase_names() {
        assert_eq!(DatVerdict::Verified.as_str(), "verified");
        assert_eq!(DatVerdict::Hint.as_str(), "hint");
        assert_eq!(DatVerdict::Misnamed.as_str(), "misnamed");
        assert_eq!(DatVerdict::Unsupported.as_str(), "unsupported");
    }
}
