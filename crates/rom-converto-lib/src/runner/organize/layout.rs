//! The organize letter-directory layout pass (`dir_letter*` options): each
//! output gains a leading directory named after the first characters of its
//! file stem.

use super::plan::{Decision, UnitPlan};
use crate::runner::invalid_arg;
use crate::runner::models::RunOptions;
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Letter-directory layout for organized outputs.
#[derive(Debug)]
pub(super) struct LetterLayout {
    /// Number of leading stem characters forming the letter folder name.
    pub count: usize,
    /// A folder holding more than this many plans splits into numbered
    /// folders (`A1`, `A2`, ...).
    pub limit: Option<usize>,
    /// Merge adjacent letter folders into range folders (e.g. `A-K`) that
    /// stay within `limit`.
    pub group: bool,
}

impl LetterLayout {
    /// `Some` only when `dir_letter` is explicitly on. The sibling options
    /// matter only then: with `dir_letter` off or unset they are ignored,
    /// so a GUI (or config) that always sends them cannot fail the run.
    pub(super) fn from_options(options: &RunOptions) -> Result<Option<Self>> {
        if options.dir_letter != Some(true) {
            return Ok(None);
        }
        let count = options.dir_letter_count.unwrap_or(1);
        if count < 1 {
            return Err(invalid_arg("dir_letter_count must be at least 1"));
        }
        if count > 26 {
            return Err(invalid_arg("dir_letter_count must be at most 26"));
        }
        let limit = options.dir_letter_limit;
        if limit == Some(0) {
            return Err(invalid_arg("dir_letter_limit must be at least 1"));
        }
        let group = options.dir_letter_group == Some(true);
        if group && limit.is_none() {
            return Err(invalid_arg("dir_letter_group requires dir_letter_limit"));
        }
        Ok(Some(Self {
            count,
            limit,
            group,
        }))
    }
}

/// Inserts the letter folder before the file name of every Keep plan's
/// desired path. A plan's folder name is the first `count` characters of
/// its output file stem, uppercased, with characters outside `A-Z0-9`
/// turned into `#` (a shorter stem pads with `#`). Plans are bucketed by
/// (parent dir, folder) so each console/DAT subtree gets its own sequence.
/// A folder holding more than `limit` plans splits, in file-name order,
/// into numbered folders (`A1`, `A2`, ...), and `group` first merges
/// adjacent letters into range folders (e.g. `A-K`) that stay within
/// `limit`.
pub(super) fn apply_letter_dirs(plans: &mut [UnitPlan], layout: &LetterLayout) {
    // One folder sequence per parent directory.
    let mut by_parent: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (i, plan) in plans.iter().enumerate() {
        if plan.decision != Decision::Keep {
            continue;
        }
        let Some(desired) = plan.desired.as_ref() else {
            continue;
        };
        let parent = desired
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        by_parent.entry(parent).or_default().push(i);
    }

    for indices in by_parent.into_values() {
        // Bucket the parent's plans by their letter folder.
        let mut buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for i in indices {
            let stem = plans[i]
                .desired
                .as_ref()
                .and_then(|p| p.file_stem())
                .and_then(|s| s.to_str())
                .unwrap_or("");
            buckets
                .entry(letter_of(stem, layout.count))
                .or_default()
                .push(i);
        }

        // `group` merges adjacent letters into range folders; without it
        // every letter keeps its own folder.
        let folders: Vec<(String, Vec<usize>)> = if layout.group {
            let limit = layout
                .limit
                .expect("from_options rejects group without limit");
            merge_ranges(buckets, limit)
        } else {
            buckets.into_iter().collect()
        };

        for (folder, mut members) in folders {
            members.sort_by_cached_key(|&i| output_file_name(plans, i));
            let Some(limit) = layout.limit.filter(|&limit| members.len() > limit) else {
                for &i in &members {
                    add_letter_dir(&mut plans[i], &folder);
                }
                continue;
            };
            // Over-limit folders split, in file-name order, into numbered
            // folders `<letter>1`, `<letter>2`, ...
            for (n, chunk) in members.chunks(limit).enumerate() {
                let dirname = format!("{folder}{}", n + 1);
                for &i in chunk {
                    add_letter_dir(&mut plans[i], &dirname);
                }
            }
        }
    }
}

/// The plan's output file name; an over-limit folder splits its members in
/// this order.
fn output_file_name(plans: &[UnitPlan], i: usize) -> std::ffi::OsString {
    plans[i]
        .desired
        .as_ref()
        .and_then(|p| p.file_name())
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default()
}

/// Splices `dirname` between `plan.desired`'s parent and file name. A
/// folder spelling a Windows device name (`CON`, `COM1`, ...) gets the same
/// `_` suffix as every other output path component.
fn add_letter_dir(plan: &mut UnitPlan, dirname: &str) {
    let Some(desired) = plan.desired.take() else {
        return;
    };
    let parent = desired.parent().map(Path::to_path_buf).unwrap_or_default();
    let file = desired
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    let dirname = crate::util::template::sanitize_file_stem(dirname);
    plan.desired = Some(parent.join(dirname).join(file));
}

/// The letter folder for one plan: the first `count` characters of `stem`,
/// uppercased; characters outside `A-Z0-9` become `#`, and a stem shorter
/// than `count` pads with `#`.
fn letter_of(stem: &str, count: usize) -> String {
    let mut letters: Vec<char> = stem
        .chars()
        .take(count)
        .map(|c| {
            let upper = c.to_ascii_uppercase();
            if upper.is_ascii_alphanumeric() {
                upper
            } else {
                '#'
            }
        })
        .collect();
    letters.resize(count, '#');
    letters.into_iter().collect()
}

/// Merges adjacent letters (in letter order) into range folders
/// `first-last`, as long as the merged folder stays within `limit`. A
/// folder that would overflow closes before the letter, and a letter whose
/// own bucket exceeds `limit` stands alone; the numeric-split pass in
/// `apply_letter_dirs` divides such folders. `first` and `last` collapse
/// to the bare letter for a single-letter range.
fn merge_ranges(buckets: BTreeMap<String, Vec<usize>>, limit: usize) -> Vec<(String, Vec<usize>)> {
    let mut folders = Vec::new();
    // Open range: its first letter, its last letter, and its members.
    let mut open: Option<(String, String, Vec<usize>)> = None;
    for (letter, mut members) in buckets {
        let fits = open
            .as_ref()
            .is_some_and(|(_, _, so_far)| so_far.len() + members.len() <= limit);
        if fits {
            let (_, last, so_far) = open.as_mut().expect("fits implies an open range");
            *last = letter;
            so_far.append(&mut members);
            continue;
        }
        if let Some((first, last, so_far)) = open.take() {
            folders.push((range_name(first, last), so_far));
        }
        open = Some((letter.clone(), letter, members));
    }
    if let Some((first, last, so_far)) = open {
        folders.push((range_name(first, last), so_far));
    }
    folders
}

/// `first` alone when the range spans a single letter, else `first-last`.
fn range_name(first: String, last: String) -> String {
    if first == last {
        first
    } else {
        format!("{first}-{last}")
    }
}

#[cfg(test)]
mod tests {
    use super::super::plan::Action;
    use super::*;
    use crate::util::TemplateTokens;

    fn keep_plan(index: usize, desired: &str) -> UnitPlan {
        UnitPlan {
            index,
            source: PathBuf::from(desired),
            source_ext: String::new(),
            action: Action::Copy,
            tokens: TemplateTokens::new(None, Path::new(desired), "rom"),
            input_subdir: PathBuf::new(),
            game: None,
            header: None,
            strip: 0,
            trim: None,
            pad: 0,
            pad_fill: 0x00,
            patch: None,
            staging_error: None,
            output_error: None,
            match_error: None,
            member: None,
            desired: Some(PathBuf::from(desired)),
            decision: Decision::Keep,
        }
    }

    fn desired_of(plans: &[UnitPlan], index: usize) -> &Path {
        plans[index].desired.as_deref().unwrap()
    }

    #[test]
    fn count_one_uses_first_letter() {
        let mut plans = vec![
            keep_plan(0, "/out/NES/Apple.rom"),
            keep_plan(1, "/out/NES/Banana.rom"),
            keep_plan(2, "/out/NES/Zelda.rom"),
        ];
        let layout = LetterLayout {
            count: 1,
            limit: None,
            group: false,
        };
        apply_letter_dirs(&mut plans, &layout);
        assert_eq!(desired_of(&plans, 0), Path::new("/out/NES/A/Apple.rom"));
        assert_eq!(desired_of(&plans, 1), Path::new("/out/NES/B/Banana.rom"));
        assert_eq!(desired_of(&plans, 2), Path::new("/out/NES/Z/Zelda.rom"));
    }

    #[test]
    fn count_two_uses_first_two_letters() {
        let mut plans = vec![
            keep_plan(0, "/out/NES/Ant.rom"),
            keep_plan(1, "/out/NES/Axe.rom"),
            keep_plan(2, "/out/NES/Bat.rom"),
        ];
        let layout = LetterLayout {
            count: 2,
            limit: None,
            group: false,
        };
        apply_letter_dirs(&mut plans, &layout);
        assert_eq!(desired_of(&plans, 0), Path::new("/out/NES/AN/Ant.rom"));
        assert_eq!(desired_of(&plans, 1), Path::new("/out/NES/AX/Axe.rom"));
        assert_eq!(desired_of(&plans, 2), Path::new("/out/NES/BA/Bat.rom"));
    }

    #[test]
    fn non_alnum_stem_characters_become_hash_and_short_stems_pad() {
        let mut plans = vec![
            keep_plan(0, "/out/NES/1994.rom"),
            keep_plan(1, "/out/NES/éclair.rom"),
            keep_plan(2, "/out/NES/A.rom"),
        ];
        let layout = LetterLayout {
            count: 2,
            limit: None,
            group: false,
        };
        apply_letter_dirs(&mut plans, &layout);
        assert_eq!(desired_of(&plans, 0), Path::new("/out/NES/19/1994.rom"));
        assert_eq!(desired_of(&plans, 1), Path::new("/out/NES/#C/éclair.rom"));
        assert_eq!(desired_of(&plans, 2), Path::new("/out/NES/A#/A.rom"));
    }

    #[test]
    fn windows_device_folder_names_get_a_suffix() {
        let mut plans = vec![
            keep_plan(0, "/out/NES/Contra.rom"),
            keep_plan(1, "/out/NES/Comet.rom"),
            keep_plan(2, "/out/NES/Comic.rom"),
        ];
        let layout = LetterLayout {
            count: 3,
            limit: Some(1),
            group: false,
        };
        apply_letter_dirs(&mut plans, &layout);
        assert_eq!(desired_of(&plans, 0), Path::new("/out/NES/CON_/Contra.rom"));
        assert_eq!(desired_of(&plans, 1), Path::new("/out/NES/COM1_/Comet.rom"));
        assert_eq!(desired_of(&plans, 2), Path::new("/out/NES/COM2_/Comic.rom"));
    }

    #[test]
    fn limit_splits_oversized_letter_into_numbered_chunks() {
        let mut plans = vec![
            keep_plan(0, "/out/NES/Apple.rom"),
            keep_plan(1, "/out/NES/Ant.rom"),
            keep_plan(2, "/out/NES/Air.rom"),
            keep_plan(3, "/out/NES/Arc.rom"),
            keep_plan(4, "/out/NES/Axe.rom"),
        ];
        let layout = LetterLayout {
            count: 1,
            limit: Some(2),
            group: false,
        };
        apply_letter_dirs(&mut plans, &layout);
        // Sorted by filename: Air, Ant, Apple, Arc, Axe -> chunks of 2: A1, A1, A2, A2, A3.
        assert_eq!(desired_of(&plans, 2), Path::new("/out/NES/A1/Air.rom"));
        assert_eq!(desired_of(&plans, 1), Path::new("/out/NES/A1/Ant.rom"));
        assert_eq!(desired_of(&plans, 0), Path::new("/out/NES/A2/Apple.rom"));
        assert_eq!(desired_of(&plans, 3), Path::new("/out/NES/A2/Arc.rom"));
        assert_eq!(desired_of(&plans, 4), Path::new("/out/NES/A3/Axe.rom"));
    }

    #[test]
    fn group_merges_adjacent_letters_into_ranges() {
        let mut plans = vec![
            keep_plan(0, "/out/NES/Apple.rom"),
            keep_plan(1, "/out/NES/Bat.rom"),
            keep_plan(2, "/out/NES/Cat.rom"),
            keep_plan(3, "/out/NES/Dog.rom"),
        ];
        let layout = LetterLayout {
            count: 1,
            limit: Some(2),
            group: true,
        };
        apply_letter_dirs(&mut plans, &layout);
        assert_eq!(desired_of(&plans, 0), Path::new("/out/NES/A-B/Apple.rom"));
        assert_eq!(desired_of(&plans, 1), Path::new("/out/NES/A-B/Bat.rom"));
        assert_eq!(desired_of(&plans, 2), Path::new("/out/NES/C-D/Cat.rom"));
        assert_eq!(desired_of(&plans, 3), Path::new("/out/NES/C-D/Dog.rom"));
    }

    #[test]
    fn group_still_splits_a_single_letter_over_limit() {
        let mut plans = vec![
            keep_plan(0, "/out/NES/Air.rom"),
            keep_plan(1, "/out/NES/Ant.rom"),
            keep_plan(2, "/out/NES/Apple.rom"),
        ];
        let layout = LetterLayout {
            count: 1,
            limit: Some(2),
            group: true,
        };
        apply_letter_dirs(&mut plans, &layout);
        assert_eq!(desired_of(&plans, 0), Path::new("/out/NES/A1/Air.rom"));
        assert_eq!(desired_of(&plans, 1), Path::new("/out/NES/A1/Ant.rom"));
        assert_eq!(desired_of(&plans, 2), Path::new("/out/NES/A2/Apple.rom"));
    }

    #[test]
    fn group_keeps_an_over_limit_letter_alone_and_splits_it() {
        let mut plans = vec![
            keep_plan(0, "/out/NES/Ape.rom"),
            keep_plan(1, "/out/NES/Bat.rom"),
            keep_plan(2, "/out/NES/Bee.rom"),
            keep_plan(3, "/out/NES/Boa.rom"),
            keep_plan(4, "/out/NES/Cow.rom"),
        ];
        let layout = LetterLayout {
            count: 1,
            limit: Some(2),
            group: true,
        };
        apply_letter_dirs(&mut plans, &layout);
        assert_eq!(desired_of(&plans, 0), Path::new("/out/NES/A/Ape.rom"));
        // B's three files exceed the limit: B stands alone, splits
        // numerically in file-name order (Bat, Bee, Boa), and neither
        // neighbour merges with it.
        assert_eq!(desired_of(&plans, 1), Path::new("/out/NES/B1/Bat.rom"));
        assert_eq!(desired_of(&plans, 2), Path::new("/out/NES/B1/Bee.rom"));
        assert_eq!(desired_of(&plans, 3), Path::new("/out/NES/B2/Boa.rom"));
        assert_eq!(desired_of(&plans, 4), Path::new("/out/NES/C/Cow.rom"));
    }

    #[test]
    fn skip_plans_are_untouched() {
        let mut plans = vec![keep_plan(0, "/out/NES/Apple.rom")];
        plans[0].decision = Decision::Skip("filtered".to_string());
        let layout = LetterLayout {
            count: 1,
            limit: None,
            group: false,
        };
        apply_letter_dirs(&mut plans, &layout);
        assert_eq!(desired_of(&plans, 0), Path::new("/out/NES/Apple.rom"));
    }

    #[test]
    fn zero_limit_is_rejected() {
        let options = RunOptions {
            dir_letter: Some(true),
            dir_letter_limit: Some(0),
            ..RunOptions::default()
        };
        let err = LetterLayout::from_options(&options).expect_err("limit 0 is invalid");
        assert!(err.to_string().contains("dir_letter_limit"));
    }

    /// The GUI always sends `dir_letter: false` (plus group: false) even
    /// when letter folders are off, and a config may fill count and limit
    /// besides: with `dir_letter` off or unset the sibling options are
    /// ignored, not rejected, so such a run never fails.
    #[test]
    fn siblings_are_ignored_without_dir_letter() {
        let mut options = RunOptions {
            dir_letter: Some(false),
            dir_letter_group: Some(false),
            dir_letter_count: Some(2),
            dir_letter_limit: Some(10),
            ..RunOptions::default()
        };
        assert!(LetterLayout::from_options(&options).unwrap().is_none());

        options.dir_letter = None;
        assert!(LetterLayout::from_options(&options).unwrap().is_none());

        // Letters on: the options validate again, and only an explicit
        // `true` counts as grouping.
        options.dir_letter = Some(true);
        let layout = LetterLayout::from_options(&options).unwrap().unwrap();
        assert_eq!(layout.count, 2);
        assert_eq!(layout.limit, Some(10));
        assert!(!layout.group);
    }

    /// A dir_letter_count past 26 is rejected up front.
    #[test]
    fn count_over_26_is_rejected() {
        let mut options = RunOptions {
            dir_letter: Some(true),
            dir_letter_count: Some(27),
            ..RunOptions::default()
        };
        let err = LetterLayout::from_options(&options).expect_err("count 27 is invalid");
        assert!(err.to_string().contains("dir_letter_count"));

        options.dir_letter_count = Some(26);
        assert!(LetterLayout::from_options(&options).is_ok());
    }

    #[test]
    fn parent_dirs_get_independent_letter_sequences() {
        let mut plans = vec![
            keep_plan(0, "/out/NES/Apple.rom"),
            keep_plan(1, "/out/SNES/Apple.rom"),
        ];
        let layout = LetterLayout {
            count: 1,
            limit: None,
            group: false,
        };
        apply_letter_dirs(&mut plans, &layout);
        assert_eq!(desired_of(&plans, 0), Path::new("/out/NES/A/Apple.rom"));
        assert_eq!(desired_of(&plans, 1), Path::new("/out/SNES/A/Apple.rom"));
    }
}
