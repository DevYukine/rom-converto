//! The organize layout passes that add a directory level below the
//! template-level folder: letter directories (`dir_letter*` options) named
//! after the first characters of each file stem, and multi-disc directories
//! (`multi_disc_dirs`) that gather one game's discs in a folder named after
//! the game.

use super::plan::{Decision, UnitPlan};
use crate::playlist::parse_disc_token;
use crate::runner::invalid_arg;
use crate::runner::models::RunOptions;
use anyhow::Result;
use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
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
    /// Never split a multi-disc set across numbered folders (the
    /// `multi_disc_dirs` pass runs next and needs the set in one parent).
    pub keep_sets: bool,
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
            keep_sets: options.multi_disc_dirs == Some(true),
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
/// `limit`. With `keep_sets`, a multi-disc set sorts as one block and
/// never splits across numbered folders, so `limit` is a soft cap there.
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

        for (folder, members) in folders {
            // A set member sorts under its base title so the set is one
            // contiguous block; everything else sorts under its own file
            // name, which keeps the plain file-name order.
            let mut members: Vec<Member> = members
                .into_iter()
                .map(|i| {
                    let set = layout.keep_sets.then(|| disc_set_of(plans, i)).flatten();
                    (set, output_file_name(plans, i), i)
                })
                .collect();
            members.sort_by(|a, b| member_order(a).cmp(&member_order(b)));
            let Some(limit) = layout.limit.filter(|&limit| members.len() > limit) else {
                for &(_, _, i) in &members {
                    add_dir(&mut plans[i], &folder);
                }
                continue;
            };
            // Over-limit folders split, in file-name order, into numbered
            // folders `<letter>1`, `<letter>2`, ...; a chunk only ends
            // between two different sets. A set that pulls every member
            // into the first chunk keeps the plain folder.
            let mut start = 0;
            let mut n = 0;
            while start < members.len() {
                let mut end = (start + limit).min(members.len());
                while end < members.len()
                    && members[end].0.is_some()
                    && members[end].0 == members[end - 1].0
                {
                    end += 1;
                }
                n += 1;
                let dirname = if start == 0 && end == members.len() {
                    folder.clone()
                } else {
                    format!("{folder}{n}")
                };
                for &(_, _, i) in &members[start..end] {
                    add_dir(&mut plans[i], &dirname);
                }
                start = end;
            }
        }
    }
}

/// One letter-folder member: its disc set (with `keep_sets`), output file
/// name, and plan index.
type Member = (Option<OsString>, OsString, usize);

/// Letter-folder members sort by set (a lone file is its own set), then by
/// file name.
fn member_order(member: &Member) -> (&OsStr, &OsStr) {
    let (set, file, _) = member;
    (set.as_deref().unwrap_or(file), file)
}

/// Inserts a game folder before the file name of every Keep plan that
/// belongs to a multi-disc set: two or more Keep plans in the same parent
/// directory whose output stems carry a disc token and share a base title.
/// The folder is named after that base title, so with playlists on the
/// `.m3u` a set of disc images derives lands inside the folder. A plan
/// without a disc token never joins a set, even when its stem equals a
/// set's base title, and a set whose parent already is its game folder (a
/// re-run over an organized tree, or an `{input_dir}` template mirroring
/// per-game input folders) is left alone; `fold_case` compares that name
/// the way the output volume does. Only Keep plans count and move: a
/// dedupe loser shares its winner's path and must stay on it, since the
/// rejoin in `organize` maps it through the winner's flat path. Runs after
/// the letter pass, so the game folder sits below the letter folder.
pub(super) fn apply_multi_disc_dirs(plans: &mut [UnitPlan], fold_case: bool) {
    let mut sets: HashMap<(PathBuf, String), Vec<usize>> = HashMap::new();
    for (i, plan) in plans.iter().enumerate() {
        if plan.decision != Decision::Keep {
            continue;
        }
        let Some((parent, base_title)) = disc_set_key(plans, i) else {
            continue;
        };
        sets.entry((parent.to_path_buf(), base_title))
            .or_default()
            .push(i);
    }
    for ((parent, base_title), members) in sets {
        // `add_dir` sanitizes the title itself; `folder` is the name it
        // will create, for the guard.
        let folder = crate::util::template::sanitize_file_stem(&base_title);
        let nested = parent.file_name().is_some_and(|name| {
            super::select::path_key(Path::new(name), fold_case)
                == super::select::path_key(Path::new(&folder), fold_case)
        });
        if folder.is_empty() || members.len() < 2 || nested {
            continue;
        }
        for i in members {
            add_dir(&mut plans[i], &base_title);
        }
    }
}

/// The (parent dir, base title) a plan's output groups under when its stem
/// carries a disc token.
fn disc_set_key(plans: &[UnitPlan], i: usize) -> Option<(&Path, String)> {
    let desired = plans[i].desired.as_ref()?;
    let stem = desired.file_stem()?.to_str()?;
    let (base_title, _) = parse_disc_token(stem)?;
    Some((
        desired.parent().unwrap_or_else(|| Path::new("")),
        base_title,
    ))
}

/// The base title a plan's output belongs to, as a sort key for
/// `apply_letter_dirs`; `None` when the stem carries no disc token.
fn disc_set_of(plans: &[UnitPlan], i: usize) -> Option<OsString> {
    disc_set_key(plans, i).map(|(_, base_title)| base_title.into())
}

/// The plan's output file name; an over-limit folder splits its members in
/// this order.
fn output_file_name(plans: &[UnitPlan], i: usize) -> OsString {
    plans[i]
        .desired
        .as_ref()
        .and_then(|p| p.file_name())
        .map(OsStr::to_os_string)
        .unwrap_or_default()
}

/// Splices `dirname` between `plan.desired`'s parent and file name. A
/// folder spelling a Windows device name (`CON`, `COM1`, ...) gets the same
/// `_` suffix as every other output path component.
fn add_dir(plan: &mut UnitPlan, dirname: &str) {
    let Some(desired) = plan.desired.take() else {
        return;
    };
    let parent = desired.parent().map(Path::to_path_buf).unwrap_or_default();
    let file = desired
        .file_name()
        .map(OsStr::to_os_string)
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
            keep_sets: false,
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
            keep_sets: false,
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
            keep_sets: false,
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
            keep_sets: false,
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
            keep_sets: false,
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
            keep_sets: false,
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
            keep_sets: false,
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
            keep_sets: false,
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
            keep_sets: false,
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
            keep_sets: false,
        };
        apply_letter_dirs(&mut plans, &layout);
        assert_eq!(desired_of(&plans, 0), Path::new("/out/NES/A/Apple.rom"));
        assert_eq!(desired_of(&plans, 1), Path::new("/out/SNES/A/Apple.rom"));
    }

    /// Two or more discs sharing a base title in one parent move into a
    /// folder named after the title; a lone disc, a tokenless file whose
    /// stem equals the title, and a Skip plan (a dedupe loser, parked on
    /// its winner's flat path) stay where they are, and the same title in
    /// another parent is its own set.
    #[test]
    fn multi_disc_sets_get_a_game_folder() {
        let mut plans = vec![
            keep_plan(0, "/out/PS1/Game (USA) (Disc 2).chd"),
            keep_plan(1, "/out/PS1/Game (USA) (Disc 1).chd"),
            keep_plan(2, "/out/PS1/Game (USA).chd"),
            keep_plan(3, "/out/PS1/Solo (USA) (Disc 1).chd"),
            keep_plan(4, "/out/PS1/Game (USA) (Disc 1).chd"),
            keep_plan(5, "/out/Saturn/Game (USA) (Disc 1).chd"),
            keep_plan(6, "/out/Saturn/Game (USA) (Disc 2).chd"),
        ];
        plans[4].decision = Decision::Skip(String::from("duplicate"));
        apply_multi_disc_dirs(&mut plans, false);
        assert_eq!(
            desired_of(&plans, 0),
            Path::new("/out/PS1/Game (USA)/Game (USA) (Disc 2).chd")
        );
        assert_eq!(
            desired_of(&plans, 1),
            Path::new("/out/PS1/Game (USA)/Game (USA) (Disc 1).chd")
        );
        assert_eq!(desired_of(&plans, 2), Path::new("/out/PS1/Game (USA).chd"));
        assert_eq!(
            desired_of(&plans, 3),
            Path::new("/out/PS1/Solo (USA) (Disc 1).chd")
        );
        assert_eq!(
            desired_of(&plans, 4),
            Path::new("/out/PS1/Game (USA) (Disc 1).chd")
        );
        assert_eq!(
            desired_of(&plans, 5),
            Path::new("/out/Saturn/Game (USA)/Game (USA) (Disc 1).chd")
        );
        assert_eq!(
            desired_of(&plans, 6),
            Path::new("/out/Saturn/Game (USA)/Game (USA) (Disc 2).chd")
        );
    }

    /// A set is counted on Keep plans only: a disc whose only sibling is a
    /// Skip plan stays flat.
    #[test]
    fn a_skipped_sibling_does_not_make_a_set() {
        let mut plans = vec![
            keep_plan(0, "/out/PS1/Game (Disc 1).chd"),
            keep_plan(1, "/out/PS1/Game (Disc 2).chd"),
        ];
        plans[1].decision = Decision::Skip(String::from("duplicate"));
        apply_multi_disc_dirs(&mut plans, false);
        assert_eq!(
            desired_of(&plans, 0),
            Path::new("/out/PS1/Game (Disc 1).chd")
        );
    }

    /// Mixed formats and the TOSEC bare token form one set, and the game
    /// folder sits below the letter folder when both passes run.
    #[test]
    fn game_folder_sits_below_the_letter_folder() {
        let mut plans = vec![
            keep_plan(0, "/out/PS1/Game Disc 1 of 2.cue"),
            keep_plan(1, "/out/PS1/Game Disc 2 of 2.chd"),
        ];
        let layout = LetterLayout {
            count: 1,
            limit: None,
            group: false,
            keep_sets: true,
        };
        apply_letter_dirs(&mut plans, &layout);
        apply_multi_disc_dirs(&mut plans, false);
        assert_eq!(
            desired_of(&plans, 0),
            Path::new("/out/PS1/G/Game/Game Disc 1 of 2.cue")
        );
        assert_eq!(
            desired_of(&plans, 1),
            Path::new("/out/PS1/G/Game/Game Disc 2 of 2.chd")
        );
    }

    /// A set whose parent already is its game folder (a re-run over an
    /// organized tree, or an `{input_dir}` template) does not nest again.
    #[test]
    fn a_set_already_in_its_game_folder_stays_put() {
        let mut plans = vec![
            keep_plan(0, "/out/PS1/Game (USA)/Game (USA) (Disc 1).chd"),
            keep_plan(1, "/out/PS1/Game (USA)/Game (USA) (Disc 2).chd"),
        ];
        apply_multi_disc_dirs(&mut plans, false);
        assert_eq!(
            desired_of(&plans, 0),
            Path::new("/out/PS1/Game (USA)/Game (USA) (Disc 1).chd")
        );
        assert_eq!(
            desired_of(&plans, 1),
            Path::new("/out/PS1/Game (USA)/Game (USA) (Disc 2).chd")
        );
    }

    /// On a case-insensitive output volume the nesting guard folds case,
    /// so a `game/` input folder mirrored by `{input_dir}` is not nested
    /// again under `Game/`; on a case-sensitive one the names differ.
    #[test]
    fn nesting_guard_follows_the_volume_case_rule() {
        let make = || {
            vec![
                keep_plan(0, "/out/PS1/game/Game (Disc 1).chd"),
                keep_plan(1, "/out/PS1/game/Game (Disc 2).chd"),
            ]
        };
        let mut plans = make();
        apply_multi_disc_dirs(&mut plans, true);
        assert_eq!(
            desired_of(&plans, 0),
            Path::new("/out/PS1/game/Game (Disc 1).chd")
        );

        let mut plans = make();
        apply_multi_disc_dirs(&mut plans, false);
        assert_eq!(
            desired_of(&plans, 0),
            Path::new("/out/PS1/game/Game/Game (Disc 1).chd")
        );
    }

    /// With `keep_sets`, an over-limit letter folder never splits a set:
    /// the set sorts as one block and the chunk holding it grows past the
    /// limit, so the game folder pass finds the whole set in one parent.
    #[test]
    fn keep_sets_holds_a_set_in_one_numbered_folder() {
        let mut plans = vec![
            keep_plan(0, "/out/PS1/Gadget.chd"),
            keep_plan(1, "/out/PS1/Game (Disc 1).chd"),
            keep_plan(2, "/out/PS1/Game (Disc 2).chd"),
            keep_plan(3, "/out/PS1/Game (Disc 3).chd"),
            keep_plan(4, "/out/PS1/Gex.chd"),
        ];
        let layout = LetterLayout {
            count: 1,
            limit: Some(2),
            group: false,
            keep_sets: true,
        };
        apply_letter_dirs(&mut plans, &layout);
        apply_multi_disc_dirs(&mut plans, false);
        assert_eq!(desired_of(&plans, 0), Path::new("/out/PS1/G1/Gadget.chd"));
        for (i, n) in [(1, 1), (2, 2), (3, 3)] {
            assert_eq!(
                desired_of(&plans, i),
                Path::new(&format!("/out/PS1/G1/Game/Game (Disc {n}).chd"))
            );
        }
        assert_eq!(desired_of(&plans, 4), Path::new("/out/PS1/G2/Gex.chd"));
    }

    /// A set that alone exceeds the limit fills the whole folder, which
    /// then keeps its plain letter name instead of a lone `G1`.
    #[test]
    fn keep_sets_leaves_a_single_chunk_unnumbered() {
        let mut plans = vec![
            keep_plan(0, "/out/PS1/Game (Disc 1).chd"),
            keep_plan(1, "/out/PS1/Game (Disc 2).chd"),
            keep_plan(2, "/out/PS1/Game (Disc 3).chd"),
        ];
        let layout = LetterLayout {
            count: 1,
            limit: Some(2),
            group: false,
            keep_sets: true,
        };
        apply_letter_dirs(&mut plans, &layout);
        assert_eq!(
            desired_of(&plans, 0),
            Path::new("/out/PS1/G/Game (Disc 1).chd")
        );
        assert_eq!(
            desired_of(&plans, 2),
            Path::new("/out/PS1/G/Game (Disc 3).chd")
        );
    }
}
