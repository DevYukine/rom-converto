use crate::playlist::{group_disc_files, parse_disc_token};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RenameCandidate {
    pub path: PathBuf,
    pub game_id: Option<String>,
    pub game_name: Option<String>,
    pub file_name: Option<String>,
    pub verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameAction {
    Rename,
    AlreadyCanonical,
    SkipUnmatched,
    SkipWeakMatch,
    SkipCollision,
    SkipDiscSetConflict,
}

#[derive(Debug, Clone)]
pub struct RenamePlan {
    pub from: PathBuf,
    pub to: Option<PathBuf>,
    pub action: RenameAction,
    pub detail: Option<String>,
}

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Sanitize a target stem for the filesystem. Illegal characters become `_`,
/// trailing dots and spaces are trimmed, and Windows reserved device names get
/// a `_` suffix. Returns the cleaned stem plus a note when anything changed.
fn sanitize_stem(stem: &str) -> (String, Option<String>) {
    let mut changed = false;
    let mut out: String = stem
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                changed = true;
                '_'
            } else {
                c
            }
        })
        .collect();

    let trimmed = out.trim_end_matches([' ', '.']);
    if trimmed.len() != out.len() {
        changed = true;
        out = trimmed.to_string();
    }

    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(&out)) {
        changed = true;
        out.push('_');
    }

    let note = changed.then(|| format!("sanitized to \"{out}\""));
    (out, note)
}

/// Lowercased extension of a path, without the dot. Empty when absent.
fn ext_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Stem without the disc token, for comparing set members' base titles.
fn base_title(name: &str) -> String {
    match parse_disc_token(name) {
        Some((base, _)) => base,
        None => name.to_string(),
    }
}

// Target stem for a single candidate. Prefers the canonical fileName stem when
// the local file is a raw single-file rom whose extension equals the canonical
// extension; otherwise the game name.
fn target_stem(c: &RenameCandidate, local_ext: &str) -> Option<String> {
    if let Some(file_name) = &c.file_name {
        let canon = Path::new(file_name);
        if ext_of(canon) == local_ext
            && let Some(stem) = canon.file_stem()
        {
            return Some(stem.to_string_lossy().into_owned());
        }
    }
    c.game_name.clone()
}

fn target_path(from: &Path, stem: &str) -> PathBuf {
    let ext = ext_of(from);
    let file_name = if ext.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}.{ext}")
    };
    from.with_file_name(file_name)
}

/// Plan renames with collision and multi-disc-set guards. Pure, no filesystem
/// access.
pub fn plan_renames(candidates: &[RenameCandidate]) -> Vec<RenamePlan> {
    let mut plans: Vec<RenamePlan> = Vec::with_capacity(candidates.len());
    let disc_conflict = disc_set_conflicts(candidates);

    for c in candidates {
        plans.push(plan_one(c, &disc_conflict));
    }

    apply_collision_guard(&mut plans);
    plans
}

// Group candidate paths into disc sets; a set of 2+ that is not all-verified or
// whose base titles diverge marks every member as conflicting.
fn disc_set_conflicts(candidates: &[RenameCandidate]) -> HashMap<PathBuf, &'static str> {
    let by_path: HashMap<&Path, &RenameCandidate> =
        candidates.iter().map(|c| (c.path.as_path(), c)).collect();
    let paths: Vec<PathBuf> = candidates.iter().map(|c| c.path.clone()).collect();
    let mut conflict: HashMap<PathBuf, &'static str> = HashMap::new();

    for group in group_disc_files(&paths) {
        if group.len() < 2 {
            continue;
        }
        let members: Vec<&RenameCandidate> = group
            .discs
            .iter()
            .filter_map(|p| by_path.get(p.as_path()).copied())
            .collect();

        let all_verified = members.iter().all(|m| m.verified && m.game_name.is_some());
        let titles: Vec<String> = members
            .iter()
            .filter_map(|m| m.game_name.as_deref().map(base_title))
            .collect();
        let one_title = titles.len() == members.len()
            && titles.windows(2).all(|w| w[0].eq_ignore_ascii_case(&w[1]));

        if !all_verified || !one_title {
            let reason = if !all_verified {
                "disc set has an unmatched or weak member"
            } else {
                "disc set resolves to different games"
            };
            for m in &members {
                conflict.insert(m.path.clone(), reason);
            }
        }
    }
    conflict
}

fn plan_one(c: &RenameCandidate, disc_conflict: &HashMap<PathBuf, &'static str>) -> RenamePlan {
    if let Some(reason) = disc_conflict.get(&c.path) {
        return RenamePlan {
            from: c.path.clone(),
            to: None,
            action: RenameAction::SkipDiscSetConflict,
            detail: Some((*reason).to_string()),
        };
    }
    if c.game_id.is_none() || c.game_name.is_none() {
        return RenamePlan {
            from: c.path.clone(),
            to: None,
            action: RenameAction::SkipUnmatched,
            detail: None,
        };
    }
    if !c.verified {
        return RenamePlan {
            from: c.path.clone(),
            to: None,
            action: RenameAction::SkipWeakMatch,
            detail: Some("name+size hint only".to_string()),
        };
    }

    let local_ext = ext_of(&c.path);
    let Some(raw_stem) = target_stem(c, &local_ext) else {
        return RenamePlan {
            from: c.path.clone(),
            to: None,
            action: RenameAction::SkipUnmatched,
            detail: None,
        };
    };

    let (stem, sanitize_note) = sanitize_stem(&raw_stem);
    if stem.is_empty() {
        return RenamePlan {
            from: c.path.clone(),
            to: None,
            action: RenameAction::SkipUnmatched,
            detail: Some("canonical name is empty after sanitization".to_string()),
        };
    }
    let to = target_path(&c.path, &stem);

    if paths_eq(&to, &c.path) {
        return RenamePlan {
            from: c.path.clone(),
            to: None,
            action: RenameAction::AlreadyCanonical,
            detail: sanitize_note,
        };
    }

    RenamePlan {
        from: c.path.clone(),
        to: Some(to),
        action: RenameAction::Rename,
        detail: sanitize_note,
    }
}

// Any two Rename plans with the same target (case-insensitive) both become
// SkipCollision.
fn apply_collision_guard(plans: &mut [RenamePlan]) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for p in plans.iter() {
        if p.action == RenameAction::Rename
            && let Some(to) = &p.to
        {
            *counts.entry(collision_key(to)).or_insert(0) += 1;
        }
    }
    for p in plans.iter_mut() {
        if p.action != RenameAction::Rename {
            continue;
        }
        let Some(to) = &p.to else { continue };
        if counts.get(&collision_key(to)).copied().unwrap_or(0) > 1 {
            p.action = RenameAction::SkipCollision;
            p.detail = Some("two files map to the same target".to_string());
            p.to = None;
        }
    }
}

// Separator- and case-insensitive key so `/` vs `\` and letter case never
// split a real collision on Windows.
fn collision_key(p: &Path) -> String {
    use std::path::Component;
    let mut key = String::new();
    for comp in p.components() {
        match comp {
            Component::RootDir => key.push('/'),
            Component::CurDir => continue,
            other => {
                if !key.is_empty() && !key.ends_with('/') {
                    key.push('/');
                }
                key.push_str(&other.as_os_str().to_string_lossy().to_ascii_lowercase());
            }
        }
    }
    key
}

fn paths_eq(a: &Path, b: &Path) -> bool {
    collision_key(a) == collision_key(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(path: &str, game: Option<&str>, file: Option<&str>, verified: bool) -> RenameCandidate {
        RenameCandidate {
            path: PathBuf::from(path),
            game_id: game.map(|_| "gid".to_string()),
            game_name: game.map(str::to_string),
            file_name: file.map(str::to_string),
            verified,
        }
    }

    fn plan_of<'a>(plans: &'a [RenamePlan], from: &str) -> &'a RenamePlan {
        plans.iter().find(|p| p.from == Path::new(from)).unwrap()
    }

    #[test]
    fn renames_to_game_name_preserving_extension() {
        let plans = plan_renames(&[cand("dir/sg.chd", Some("Some Game (USA)"), None, true)]);
        assert_eq!(plans[0].action, RenameAction::Rename);
        assert_eq!(
            plans[0].to.as_ref().unwrap(),
            &PathBuf::from("dir/Some Game (USA).chd")
        );
    }

    #[test]
    fn uses_file_name_stem_when_extension_matches() {
        let plans = plan_renames(&[cand(
            "dir/x.iso",
            Some("Some Game"),
            Some("Some Game (USA).iso"),
            true,
        )]);
        assert_eq!(
            plans[0].to.as_ref().unwrap(),
            &PathBuf::from("dir/Some Game (USA).iso")
        );
    }

    #[test]
    fn file_name_ignored_when_extension_differs() {
        // Local is .chd but canonical file is a .bin; fall back to game name.
        let plans = plan_renames(&[cand(
            "dir/x.chd",
            Some("Some Game"),
            Some("Some Game (Track 01).bin"),
            true,
        )]);
        assert_eq!(
            plans[0].to.as_ref().unwrap(),
            &PathBuf::from("dir/Some Game.chd")
        );
    }

    #[test]
    fn already_canonical_is_noop() {
        let plans = plan_renames(&[cand("dir/Some Game.rvz", Some("Some Game"), None, true)]);
        assert_eq!(plans[0].action, RenameAction::AlreadyCanonical);
        assert!(plans[0].to.is_none());
    }

    #[test]
    fn unmatched_is_skipped() {
        let plans = plan_renames(&[cand("dir/x.iso", None, None, false)]);
        assert_eq!(plans[0].action, RenameAction::SkipUnmatched);
    }

    #[test]
    fn weak_match_is_skipped() {
        let plans = plan_renames(&[cand("dir/x.iso", Some("Some Game"), None, false)]);
        assert_eq!(plans[0].action, RenameAction::SkipWeakMatch);
    }

    #[test]
    fn collision_marks_both() {
        let plans = plan_renames(&[
            cand("dir/a.chd", Some("Same Name"), None, true),
            cand("dir/b.chd", Some("Same Name"), None, true),
        ]);
        assert_eq!(
            plan_of(&plans, "dir/a.chd").action,
            RenameAction::SkipCollision
        );
        assert_eq!(
            plan_of(&plans, "dir/b.chd").action,
            RenameAction::SkipCollision
        );
    }

    #[test]
    fn collision_is_case_insensitive() {
        let plans = plan_renames(&[
            cand("dir/a.chd", Some("Some Game"), None, true),
            cand("dir/b.chd", Some("SOME GAME"), None, true),
        ]);
        assert_eq!(
            plan_of(&plans, "dir/a.chd").action,
            RenameAction::SkipCollision
        );
        assert_eq!(
            plan_of(&plans, "dir/b.chd").action,
            RenameAction::SkipCollision
        );
    }

    #[test]
    fn illegal_chars_sanitized() {
        let plans = plan_renames(&[cand("dir/x.chd", Some("A/B:C?"), None, true)]);
        assert_eq!(plans[0].action, RenameAction::Rename);
        assert_eq!(
            plans[0].to.as_ref().unwrap(),
            &PathBuf::from("dir/A_B_C_.chd")
        );
        assert!(plans[0].detail.is_some());
    }

    #[test]
    fn reserved_name_gets_suffix() {
        let plans = plan_renames(&[cand("dir/x.iso", Some("CON"), None, true)]);
        assert_eq!(
            plans[0].to.as_ref().unwrap(),
            &PathBuf::from("dir/CON_.iso")
        );
    }

    #[test]
    fn trailing_dot_trimmed() {
        let plans = plan_renames(&[cand("dir/x.iso", Some("Game."), None, true)]);
        assert_eq!(
            plans[0].to.as_ref().unwrap(),
            &PathBuf::from("dir/Game.iso")
        );
    }

    #[test]
    fn consistent_disc_set_renames_all() {
        let plans = plan_renames(&[
            cand("dir/d1.chd", Some("Some Game (USA) (Disc 1)"), None, true),
            cand("dir/d2.chd", Some("Some Game (USA) (Disc 2)"), None, true),
        ]);
        assert_eq!(plan_of(&plans, "dir/d1.chd").action, RenameAction::Rename);
        assert_eq!(plan_of(&plans, "dir/d2.chd").action, RenameAction::Rename);
        assert_eq!(
            plan_of(&plans, "dir/d1.chd").to.as_ref().unwrap(),
            &PathBuf::from("dir/Some Game (USA) (Disc 1).chd")
        );
    }

    #[test]
    fn disc_set_with_divergent_titles_skips_all() {
        let plans = plan_renames(&[
            cand("dir/Game (Disc 1).chd", Some("Alpha (Disc 1)"), None, true),
            cand("dir/Game (Disc 2).chd", Some("Beta (Disc 2)"), None, true),
        ]);
        assert_eq!(
            plan_of(&plans, "dir/Game (Disc 1).chd").action,
            RenameAction::SkipDiscSetConflict
        );
        assert_eq!(
            plan_of(&plans, "dir/Game (Disc 2).chd").action,
            RenameAction::SkipDiscSetConflict
        );
    }

    #[test]
    fn disc_set_with_weak_member_skips_all() {
        let plans = plan_renames(&[
            cand(
                "dir/Game (Disc 1).chd",
                Some("Some Game (Disc 1)"),
                None,
                true,
            ),
            cand(
                "dir/Game (Disc 2).chd",
                Some("Some Game (Disc 2)"),
                None,
                false,
            ),
        ]);
        assert_eq!(
            plan_of(&plans, "dir/Game (Disc 1).chd").action,
            RenameAction::SkipDiscSetConflict
        );
        assert_eq!(
            plan_of(&plans, "dir/Game (Disc 2).chd").action,
            RenameAction::SkipDiscSetConflict
        );
    }
}
