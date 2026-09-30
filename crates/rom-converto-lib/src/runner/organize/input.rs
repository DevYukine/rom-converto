//! Input exclusion for organize: drop units matched by the
//! `input_exclude` globs before planning.

use super::clean::glob_matches;
use crate::dat::units::DatUnit;
use globset::GlobSet;
use std::path::Path;

/// Drops units matched by any exclude glob (relative to the input root or
/// absolute). A unit is excluded when any file of its set matches: a cue
/// sheet or any of its bins, or any part of a split WUD set as the
/// converter reads it (each part in its real on-disk spelling). A unit's
/// own scanned path is tested as well, so a part outside the consecutive
/// run (a gap, or a case twin the converter never reads) is excluded too.
/// A set holding an excluded member is dropped whole, so `move_source` can
/// never delete the excluded member together with the set. With no globs,
/// every unit is kept.
pub(super) fn retain_not_excluded(
    units: Vec<DatUnit>,
    root: &Path,
    excludes: Option<&GlobSet>,
) -> Vec<DatUnit> {
    let Some(excludes) = excludes else {
        return units;
    };
    units
        .into_iter()
        .filter(|unit| {
            let files = match unit {
                DatUnit::File(path) => super::split_set_files(path),
                DatUnit::CueSet { .. } => super::unit_source_files(unit),
            };
            !(glob_matches(excludes, root, unit.display_path())
                || files.iter().any(|file| glob_matches(excludes, root, file)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file(path: &str) -> DatUnit {
        DatUnit::File(PathBuf::from(path))
    }

    fn globs(patterns: &[&str]) -> Option<GlobSet> {
        let owned: Vec<String> = patterns.iter().map(|pattern| pattern.to_string()).collect();
        Some(super::super::clean::compile_globs(&owned, "input_exclude").unwrap())
    }

    #[test]
    fn no_globs_keeps_every_unit() {
        let units = vec![file("/lib/a.nes"), file("/lib/b.zip")];
        let kept = retain_not_excluded(units, Path::new("/lib"), None);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn relative_globs_exclude_units_below_the_root() {
        let kept = retain_not_excluded(
            vec![file("/lib/sub/Old Game.nes"), file("/lib/Keep.nes")],
            Path::new("/lib"),
            globs(&["sub/*.nes"]).as_ref(),
        );
        assert_eq!(kept.len(), 1);
        assert!(kept[0].display_path().ends_with("Keep.nes"));
    }

    /// `*` stays within one path segment: only the
    /// root-level .nes file is excluded.
    #[test]
    fn single_star_does_not_cross_directories() {
        let kept = retain_not_excluded(
            vec![file("/lib/sub/Old.nes"), file("/lib/Keep.nes")],
            Path::new("/lib"),
            globs(&["*.nes"]).as_ref(),
        );
        assert_eq!(kept.len(), 1);
        assert!(kept[0].display_path().ends_with("Old.nes"));
    }

    #[test]
    fn absolute_globs_exclude_too() {
        let kept = retain_not_excluded(
            vec![file("/lib/Dump.nes"), file("/elsewhere/Kept.nes")],
            Path::new("/lib"),
            globs(&["/elsewhere/**"]).as_ref(),
        );
        assert_eq!(kept.len(), 1);
        assert!(kept[0].display_path().ends_with("Dump.nes"));
    }

    /// A cue set is excluded through its cue, and through any of its bins:
    /// a set with an excluded member is dropped whole, so `move_source`
    /// never deletes the excluded bin together with the set.
    #[test]
    fn cue_sets_are_excluded_through_their_cue_or_any_bin() {
        let set = DatUnit::CueSet {
            cue: PathBuf::from("/lib/sets/Game.cue"),
            bins: vec![PathBuf::from("/lib/sets/Game.bin")],
        };
        let kept = retain_not_excluded(
            vec![set.clone()],
            Path::new("/lib"),
            globs(&["sets/*.cue"]).as_ref(),
        );
        assert!(kept.is_empty());

        let kept = retain_not_excluded(
            vec![set],
            Path::new("/lib"),
            globs(&["sets/*.bin"]).as_ref(),
        );
        assert!(kept.is_empty());
    }

    /// A split WUD set is excluded through any of its parts, for every
    /// unit the scan produced from it: excluding part2 drops the part1
    /// unit and the part3 continuation unit alike, so no orphan
    /// continuation survives to be reported as converted with a set that
    /// never runs.
    #[test]
    fn split_sets_are_excluded_through_any_part() {
        let dir = tempfile::TempDir::new().unwrap();
        let parts: Vec<DatUnit> = ["game_part1.wud", "game_part2.wud", "game_part3.wud"]
            .iter()
            .map(|name| {
                std::fs::write(dir.path().join(name), b"part").unwrap();
                DatUnit::File(dir.path().join(name))
            })
            .collect();
        let kept = retain_not_excluded(
            parts.clone(),
            dir.path(),
            globs(&["**/game_part2.wud"]).as_ref(),
        );
        assert!(
            kept.is_empty(),
            "an excluded part drops every unit of the set: {kept:?}"
        );
        // Excluding nothing from the set keeps all three units.
        let kept = retain_not_excluded(parts, dir.path(), globs(&["**/other.wud"]).as_ref());
        assert_eq!(kept.len(), 3);
    }
    /// A differently cased split set is matched in its on-disk spelling on
    /// a case-insensitive volume, where the converter's lowercase names
    /// reach those files: excluding `GAME_PART2.WUD` drops the units of all
    /// three parts, so no excluded member is left behind for `move_source`
    /// to delete. A case-sensitive volume has no such set (the converter
    /// reads only lowercase names), so the vector does not apply there.
    #[test]
    fn an_uppercase_split_set_is_excluded_through_its_real_spelling() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("Probe.tmp"), b"").unwrap();
        if !dir.path().join("probe.tmp").exists() {
            eprintln!("skipped: this volume is case-sensitive");
            return;
        }
        std::fs::remove_file(dir.path().join("Probe.tmp")).unwrap();
        let parts: Vec<DatUnit> = ["GAME_PART1.WUD", "GAME_PART2.WUD", "GAME_PART3.WUD"]
            .iter()
            .map(|name| {
                std::fs::write(dir.path().join(name), b"part").unwrap();
                DatUnit::File(dir.path().join(name))
            })
            .collect();
        let kept = retain_not_excluded(parts, dir.path(), globs(&["**/GAME_PART2.WUD"]).as_ref());
        assert!(kept.is_empty(), "the whole set is excluded: {kept:?}");
    }

    /// A part outside the consecutive run (a gap at part 4) is not in part
    /// 1's set, so it is excluded through its own scanned path.
    #[test]
    fn a_part_past_a_gap_is_excluded_through_its_own_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let parts: Vec<DatUnit> = [
            "game_part1.wud",
            "game_part2.wud",
            "game_part3.wud",
            "game_part5.wud",
        ]
        .iter()
        .map(|name| {
            std::fs::write(dir.path().join(name), b"part").unwrap();
            DatUnit::File(dir.path().join(name))
        })
        .collect();
        let kept = retain_not_excluded(parts, dir.path(), globs(&["**/game_part5.wud"]).as_ref());
        assert_eq!(kept.len(), 3, "only the excluded part5 unit goes: {kept:?}");
        assert!(
            kept.iter()
                .all(|unit| !unit.display_path().ends_with("game_part5.wud"))
        );
    }
}
