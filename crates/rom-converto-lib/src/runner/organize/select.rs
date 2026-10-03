//! The organize selection pass: include/exclude filters and preference
//! ordering across the whole library. Option parsing and validation live
//! here alongside the filter, best-release, and dedupe passes.

use super::plan::{Action, Decision, GameRef, UnitPlan};
use crate::dat::tags::{GameKind, GameTags, Revision};
use crate::runner::invalid_arg;
use crate::runner::models::RunOptions;
use anyhow::Result;
use regex::{Regex, RegexBuilder};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Library-wide include/exclude filters, parsed from the run options.
pub(super) struct Filters {
    /// `filter_regex` patterns a game name (DAT) or filename stem must match.
    pub include: Vec<Regex>,
    /// `filter_regex_exclude` patterns that reject a match.
    pub exclude: Vec<Regex>,
    /// 2-letter language codes (see `dat::tags`).
    pub languages: Vec<String>,
    /// Region codes (see `dat::tags`).
    pub regions: Vec<String>,
    /// Game kinds to exclude.
    pub no_type: Vec<GameKind>,
    /// Game kinds to restrict to.
    pub only_type: Vec<GameKind>,
    /// Restrict to retail releases.
    pub only_retail: bool,
}

impl Filters {
    pub(super) fn from_options(options: &RunOptions) -> Result<Self> {
        Ok(Self {
            include: compile_all(
                options.filter_regex.as_deref().unwrap_or_default(),
                "filter_regex",
            )?,
            exclude: compile_all(
                options.filter_regex_exclude.as_deref().unwrap_or_default(),
                "filter_regex_exclude",
            )?,
            languages: validate_codes(
                options.filter_language.as_deref().unwrap_or_default(),
                "filter_language",
                crate::dat::tags::is_language_code,
            )?,
            regions: validate_codes(
                options.filter_region.as_deref().unwrap_or_default(),
                "filter_region",
                crate::dat::tags::is_region_code,
            )?,
            no_type: parse_kinds(options.no_type.as_deref().unwrap_or_default(), "no_type")?,
            only_type: parse_kinds(
                options.only_type.as_deref().unwrap_or_default(),
                "only_type",
            )?,
            only_retail: options.only_retail == Some(true),
        })
    }
}

/// Checks every code against its tags table after trimming, and collects
/// the trimmed codes: an untrimmed ` fr` would pass lookup tables that
/// trim and then silently never match a unit's `FR`.
fn validate_codes(codes: &[String], option: &str, known: fn(&str) -> bool) -> Result<Vec<String>> {
    let mut trimmed = Vec::with_capacity(codes.len());
    for code in codes {
        let code = code.trim();
        if !known(code) {
            return Err(invalid_arg(format!(
                "invalid {option} code {code:?}: not a known code"
            )));
        }
        trimmed.push(code.to_string());
    }
    Ok(trimmed)
}

/// Preference ordering for the single-copy and dedupe passes, parsed from
/// the run options.
pub(super) struct Preferences {
    /// Keep exactly one game per parent/disc bucket.
    pub single: bool,
    /// `prefer_game_regex` patterns preferred when picking a winner.
    pub game_regex: Vec<Regex>,
    /// `prefer_filename_regex` patterns preferred when picking a winner.
    pub filename_regex: Vec<Regex>,
    pub prefer_verified: bool,
    pub prefer_good: bool,
    pub prefer_retail: bool,
    pub prefer_parent: bool,
    /// Preferred languages, in priority order.
    pub languages: Vec<String>,
    /// Preferred regions, in priority order.
    pub regions: Vec<String>,
    /// Prefer older or newer revisions.
    pub revision: Option<RevisionOrder>,
}

/// Revision preference direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RevisionOrder {
    Older,
    Newer,
}

impl Preferences {
    pub(super) fn from_options(options: &RunOptions) -> Result<Self> {
        let revision = match options.prefer_revision.as_deref() {
            None => None,
            // The explicit "no preference": it overrides a config default
            // without enabling a revision ordering.
            Some("any") => None,
            Some("older") => Some(RevisionOrder::Older),
            Some("newer") => Some(RevisionOrder::Newer),
            Some(other) => {
                return Err(invalid_arg(format!(
                    "invalid prefer_revision {other:?}; expected \"any\", \"older\" or \"newer\""
                )));
            }
        };
        Ok(Self {
            single: options.single == Some(true),
            game_regex: compile_all(
                options.prefer_game_regex.as_deref().unwrap_or_default(),
                "prefer_game_regex",
            )?,
            filename_regex: compile_all(
                options.prefer_filename_regex.as_deref().unwrap_or_default(),
                "prefer_filename_regex",
            )?,
            prefer_verified: options.prefer_verified == Some(true),
            prefer_good: options.prefer_good == Some(true),
            prefer_retail: options.prefer_retail == Some(true),
            prefer_parent: options.prefer_parent == Some(true),
            languages: validate_codes(
                options.prefer_language.as_deref().unwrap_or_default(),
                "prefer_language",
                crate::dat::tags::is_language_code,
            )?,
            regions: validate_codes(
                options.prefer_region.as_deref().unwrap_or_default(),
                "prefer_region",
                crate::dat::tags::is_region_code,
            )?,
            revision,
        })
    }
}

fn compile_all(patterns: &[String], option: &str) -> Result<Vec<Regex>> {
    patterns
        .iter()
        .map(|pattern| compile_regex(pattern, option))
        .collect()
}

/// Compiles one pattern, accepting the JavaScript-style `/pattern/flags`
/// form in addition to a raw pattern.
fn compile_regex(raw: &str, option: &str) -> Result<Regex> {
    let (pattern, flags) = match raw.rfind('/') {
        // A leading and a later slash delimit the pattern; what follows are
        // the flags.
        Some(last) if raw.starts_with('/') && last > 0 => (&raw[1..last], &raw[last + 1..]),
        _ => (raw, ""),
    };
    let mut builder = RegexBuilder::new(pattern);
    for flag in flags.chars() {
        match flag {
            'i' => builder.case_insensitive(true),
            's' => builder.dot_matches_new_line(true),
            'm' => builder.multi_line(true),
            'x' => builder.ignore_whitespace(true),
            _ => {
                return Err(invalid_arg(format!(
                    "invalid {option} regex {raw:?}: unknown flag {flag:?}"
                )));
            }
        };
    }
    builder
        .build()
        .map_err(|err| invalid_arg(format!("invalid {option} regex {raw:?}: {err}")))
}

fn parse_kinds(names: &[String], option: &str) -> Result<Vec<GameKind>> {
    names
        .iter()
        .map(|name| {
            GameKind::parse(name)
                .ok_or_else(|| invalid_arg(format!("invalid {option} kind {name:?}")))
        })
        .collect()
}

/// Marks plans that fail the include/exclude filters as skipped.
///
/// Every specified filter is a logical AND. Without a DAT match the tags
/// come from the source file stem.
pub(super) fn apply_filters(plans: &mut [UnitPlan], filters: &Filters) {
    for plan in plans.iter_mut() {
        if !matches!(plan.decision, Decision::Keep) {
            continue;
        }
        let fallback;
        let (name, tags) = match &plan.game {
            Some(game) => (game.name.as_str(), &game.tags),
            None => {
                fallback = GameTags::parse(&plan.tokens.basename);
                (plan.tokens.basename.as_str(), &fallback)
            }
        };
        if let Some(reason) = filter_skip_reason(name, tags, filters) {
            plan.decision = Decision::Skip(reason);
        }
    }
}

/// The first filter the name/tags fail, as a skip reason.
fn filter_skip_reason(name: &str, tags: &GameTags, filters: &Filters) -> Option<String> {
    if !filters.include.is_empty() && !filters.include.iter().any(|re| re.is_match(name)) {
        return Some("filtered: no filter_regex match".to_string());
    }
    if filters.exclude.iter().any(|re| re.is_match(name)) {
        return Some("filtered: filter_regex_exclude match".to_string());
    }
    if !filters.languages.is_empty() {
        let tagged = tags
            .languages
            .iter()
            .any(|lang| code_in(&filters.languages, lang));
        let fallback = tags
            .primary_language()
            .is_some_and(|lang| code_in(&filters.languages, lang));
        if !tagged && !fallback {
            return Some("filtered: no filter_language match".to_string());
        }
    }
    if !filters.regions.is_empty()
        && !tags
            .regions
            .iter()
            .any(|region| code_in(&filters.regions, region))
    {
        return Some("filtered: no filter_region match".to_string());
    }
    if let Some(kind) = filters.no_type.iter().find(|kind| kind_matches(kind, tags)) {
        return Some(format!("filtered: no_type {}", kind.name()));
    }
    // `only_type` is a list: any kind in it matching the name satisfies
    // the filter (OR, not AND).
    if !filters.only_type.is_empty()
        && !filters
            .only_type
            .iter()
            .any(|kind| kind_matches(kind, tags))
    {
        return Some("filtered: only_type unmatched".to_string());
    }
    if filters.only_retail && !tags.is_retail() {
        return Some("filtered: only_retail".to_string());
    }
    None
}

/// Case-insensitive membership of `code` in an option's code list.
fn code_in(codes: &[String], code: &str) -> bool {
    codes
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(code))
}

/// Whether a `--no-*`/`--only-*` kind matches a name's tags. `device` is
/// never name-detectable and `unverified` maps to the classic `[!]`
/// verified marker, so neither can live in [`GameTags::kinds`].
fn kind_matches(kind: &GameKind, tags: &GameTags) -> bool {
    match kind {
        GameKind::Device => false,
        GameKind::Unverified => !tags.verified,
        _ => tags.kinds.contains(kind),
    }
}

/// Keeps one game per parent/disc bucket under `single`.
pub(super) fn apply_single(plans: &mut [UnitPlan], preferences: &Preferences) {
    if !preferences.single {
        return;
    }
    // Ids referenced as a parent: every game carrying one belongs to that
    // parent's bucket rather than the title fallback.
    let parents: HashSet<String> = plans
        .iter()
        .filter(|plan| matches!(plan.decision, Decision::Keep))
        .filter_map(|plan| plan.game.as_ref().and_then(|game| game.parent_id.clone()))
        .collect();
    // Parent/clone inference is all-or-nothing per platform: the title
    // fallback applies only when no game on the platform carries a parent.
    // Filters run before apply_single and only set Decision::Skip; the
    // GameRef survives on skipped plans, so this checks every plan
    // carrying a GameRef rather than only the surviving Keep plans;
    // dropping all clones must not flip a platform into title fallback.
    let pc_platforms: HashSet<Option<String>> = plans
        .iter()
        .filter_map(|plan| {
            plan.game
                .as_ref()
                .filter(|game| game.parent_id.is_some())
                .map(|game| game.platform.clone())
        })
        .collect();
    let mut buckets: HashMap<BucketKey, Vec<usize>> = HashMap::new();
    for (index, plan) in plans.iter().enumerate() {
        if !matches!(plan.decision, Decision::Keep) {
            continue;
        }
        let Some(game) = &plan.game else {
            continue;
        };
        let title_fallback = !pc_platforms.contains(&game.platform);
        buckets
            .entry(bucket_key(game, &parents, title_fallback))
            .or_default()
            .push(index);
    }
    for indices in buckets.values() {
        let Some(winner) = indices.iter().copied().min_by(|&a, &b| {
            single_prefer(
                plans[a].game.as_ref().expect("bucketed plans have a game"),
                plans[b].game.as_ref().expect("bucketed plans have a game"),
                preferences,
            )
        }) else {
            continue;
        };
        let detail = format!("duplicate of {}", input_display(&plans[winner]));
        for &index in indices {
            if index != winner {
                plans[index].decision = Decision::Skip(detail.clone());
            }
        }
    }
}

/// Best-release bucket: a DAT parent and its clones share one family bucket keyed
/// by the parent's id; games without P/C information fall back to the
/// normalized title plus platform only when no game
/// on their platform carries a parent, otherwise they keep their own family
/// bucket; disc number splits multi-disc sets into one bucket each.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum BucketKey {
    /// A DAT parent/clone family, keyed by the parent's game id.
    Family { key: String, disc: Option<u32> },
    /// A game on a platform without P/C information, keyed by normalized
    /// title plus platform.
    Title {
        title: String,
        platform: Option<String>,
        disc: Option<u32>,
    },
}

fn bucket_key(game: &GameRef, parents: &HashSet<String>, title_fallback: bool) -> BucketKey {
    match &game.parent_id {
        Some(parent_id) => BucketKey::Family {
            key: parent_id.clone(),
            disc: game.tags.disc,
        },
        None if parents.contains(&game.id) => BucketKey::Family {
            key: game.id.clone(),
            disc: game.tags.disc,
        },
        None if title_fallback => BucketKey::Title {
            title: GameTags::normalized_title(&game.name),
            platform: game.platform.clone(),
            disc: game.tags.disc,
        },
        // The platform carries P/C information, so an unreferenced game is
        // its own family rather than a title twin of the parent.
        None => BucketKey::Family {
            key: game.id.clone(),
            disc: game.tags.disc,
        },
    }
}

/// The best-release preference chain in priority order; `Less` means `a`
/// wins. Inactive preferences and ties fall through; the caller keeps the
/// earlier plan on a full tie.
fn single_prefer(a: &GameRef, b: &GameRef, preferences: &Preferences) -> Ordering {
    if !preferences.game_regex.is_empty() {
        let order = regex_rank(&preferences.game_regex, &a.name)
            .cmp(&regex_rank(&preferences.game_regex, &b.name));
        if order != Ordering::Equal {
            return order;
        }
    }
    if preferences.prefer_verified {
        let order = a.tags.verified.cmp(&b.tags.verified).reverse();
        if order != Ordering::Equal {
            return order;
        }
    }
    if preferences.prefer_good {
        let order = a
            .tags
            .kinds
            .contains(&GameKind::Bad)
            .cmp(&b.tags.kinds.contains(&GameKind::Bad));
        if order != Ordering::Equal {
            return order;
        }
    }
    if !preferences.languages.is_empty() {
        let order = priority_rank(&a.tags.languages, &preferences.languages)
            .cmp(&priority_rank(&b.tags.languages, &preferences.languages));
        if order != Ordering::Equal {
            return order;
        }
    }
    if !preferences.regions.is_empty() {
        let order = priority_rank(&a.tags.regions, &preferences.regions)
            .cmp(&priority_rank(&b.tags.regions, &preferences.regions));
        if order != Ordering::Equal {
            return order;
        }
    }
    if let Some(order) = preferences.revision {
        let order = revision_prefer(&a.tags.revision, &b.tags.revision, order);
        if order != Ordering::Equal {
            return order;
        }
    }
    if preferences.prefer_retail {
        let order = a.tags.is_retail().cmp(&b.tags.is_retail()).reverse();
        if order != Ordering::Equal {
            return order;
        }
    }
    if preferences.prefer_parent {
        let order = a.parent_id.is_none().cmp(&b.parent_id.is_none()).reverse();
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

/// Position of the highest-priority code a game carries in the preference
/// list; unlisted games rank last.
fn priority_rank(codes: &[String], priority: &[String]) -> usize {
    codes
        .iter()
        .filter_map(|code| {
            priority
                .iter()
                .position(|wanted| wanted.eq_ignore_ascii_case(code))
        })
        .min()
        .unwrap_or(usize::MAX)
}

/// Index of the first regex matching `name`, or [`usize::MAX`]; earlier
/// patterns outrank later ones.
fn regex_rank(regexes: &[Regex], name: &str) -> usize {
    regexes
        .iter()
        .position(|re| re.is_match(name))
        .unwrap_or(usize::MAX)
}

/// Revision preference; a missing revision counts as the oldest.
fn revision_prefer(a: &Option<Revision>, b: &Option<Revision>, order: RevisionOrder) -> Ordering {
    let by_age = match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => a.cmp_age(b),
    };
    if order == RevisionOrder::Newer {
        by_age.reverse()
    } else {
        by_age
    }
}

/// The input's full source path: the archive or plain file the plan came
/// from.
fn input_display(plan: &UnitPlan) -> String {
    plan.source.display().to_string()
}

/// The input file's own name, for filename-based preferences.
fn source_name(plan: &UnitPlan) -> &str {
    plan.source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
}

/// Reduces plans for the same game id to one winner.
pub(super) fn dedupe_by_game(plans: &mut [UnitPlan], preferences: &Preferences) {
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, plan) in plans.iter().enumerate() {
        if !matches!(plan.decision, Decision::Keep) {
            continue;
        }
        let Some(game) = &plan.game else {
            continue;
        };
        groups.entry(game.id.clone()).or_default().push(index);
    }
    reduce_duplicates(plans, groups.into_values(), preferences);
}

/// Deepest already-existing ancestor of the absolutized `dir`; `None` when
/// no ancestor exists (absolutizing first means a relative path resolves
/// against the process's current directory, which always exists).
fn probe_base(dir: &Path) -> Option<std::path::PathBuf> {
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    dir.ancestors()
        .find(|ancestor| ancestor.exists())
        .map(Path::to_path_buf)
}

/// Whether `dir` sits on a case-insensitive file system: an existing
/// entry's name also resolves under its case-flipped spelling. The probe
/// only stats, so it never writes and a dry run previews exactly what the
/// run decides. The probe looks at the deepest existing ancestor of `dir`
/// (`dir` itself may not exist yet); an entry whose name carries no ASCII
/// letters cannot discriminate, and an empty, unreadable, or missing base
/// falls back to the platform default: case-insensitive on macOS and
/// Windows, sensitive elsewhere.
pub(super) fn probe_case_insensitive(dir: &Path) -> bool {
    let default = cfg!(any(target_os = "macos", windows));
    let Some(existing) = probe_base(dir) else {
        return default;
    };
    let entries = match std::fs::read_dir(&existing) {
        Ok(entries) => entries.flatten(),
        Err(_) => return default,
    };
    probe_from_entries(&existing, entries, default, same_dir_entry)
}

/// The flip-and-compare core: the first entry whose name carries an ASCII
/// letter decides by whether its case-flipped spelling points at the same
/// directory entry.
fn probe_from_entries(
    base: &Path,
    entries: impl Iterator<Item = std::fs::DirEntry>,
    default: bool,
    same: impl Fn(&Path, &Path) -> bool,
) -> bool {
    for entry in entries {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let has_lower = name.chars().any(|c| c.is_ascii_lowercase());
        let has_upper = name.chars().any(|c| c.is_ascii_uppercase());
        if !has_lower && !has_upper {
            continue;
        }
        let flipped = if has_lower {
            name.to_ascii_uppercase()
        } else {
            name.to_ascii_lowercase()
        };
        return same(&base.join(&name), &base.join(flipped));
    }
    default
}

/// True when both spellings exist and resolve to the same entry: by
/// (dev, ino) identity on unix, by matching lengths elsewhere.
/// `symlink_metadata` never follows a link, so a link's own spelling is
/// what gets compared.
fn same_dir_entry(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
            (Ok(meta_a), Ok(meta_b)) => {
                meta_a.dev() == meta_b.dev() && meta_a.ino() == meta_b.ino()
            }
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
            (Ok(meta_a), Ok(meta_b)) => meta_a.len() == meta_b.len(),
            _ => false,
        }
    }
}

/// The map key two output paths group under: the path's lossy string,
/// case-folded when the file system is case-insensitive. `dedupe_by_path`
/// groups by it, and the layout rejoin looks groups up by it, so a
/// duplicate whose spelling only folds to its group's finds the group's
/// laid-out path.
pub(super) fn path_key(path: &Path, fold_case: bool) -> String {
    let path = path.to_string_lossy();
    if fold_case {
        path.to_lowercase()
    } else {
        path.into_owned()
    }
}

/// Reduces plans whose resolved output path collides to one winner. On
/// case-insensitive file systems paths group case-folded (`Game.zip` and
/// `game.zip` are the same name); elsewhere grouping is exact. Folding is a
/// plain ASCII/Unicode lowercase (`str::to_lowercase`), not Unicode
/// normalization: NFC/NFD variants of the same visual name still group
/// separately.
pub(super) fn dedupe_by_path(plans: &mut [UnitPlan], preferences: &Preferences, fold_case: bool) {
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, plan) in plans.iter().enumerate() {
        if !matches!(plan.decision, Decision::Keep) {
            continue;
        }
        if let Some(desired) = &plan.desired {
            let key = path_key(desired, fold_case);
            groups.entry(key).or_default().push(index);
        }
    }
    for (_, indices) in groups {
        if indices.len() < 2 {
            continue;
        }
        let winner = pick_winner(plans, &indices, preferences);
        // Report the winner's original path, not the folded group key.
        let detail = format!(
            "duplicate output {}",
            plans[winner]
                .desired
                .as_deref()
                .expect("grouped plans have a desired path")
                .display()
        );
        for index in indices {
            if index != winner {
                plans[index].decision = Decision::Skip(detail.clone());
            }
        }
    }
}

/// Skips every loser of each group, keeping the plan
/// [`dedupe_prefer`](fn@dedupe_prefer) ranks best.
fn reduce_duplicates(
    plans: &mut [UnitPlan],
    groups: impl IntoIterator<Item = Vec<usize>>,
    preferences: &Preferences,
) {
    for indices in groups {
        if indices.len() < 2 {
            continue;
        }
        let winner = pick_winner(plans, &indices, preferences);
        let detail = format!("duplicate of {}", input_display(&plans[winner]));
        for index in indices {
            if index != winner {
                plans[index].decision = Decision::Skip(detail.clone());
            }
        }
    }
}

fn pick_winner(plans: &[UnitPlan], indices: &[usize], preferences: &Preferences) -> usize {
    let mut best = indices[0];
    for &index in &indices[1..] {
        if dedupe_prefer(&plans[index], &plans[best], preferences) == Ordering::Less {
            best = index;
        }
    }
    best
}

/// True when the plan's desired output is one of its own unit's source
/// files (a split WUD first part owns its continuations, matching
/// execute_unit's rule): the unit is already in place, so it must win a
/// dedupe group; letting a sibling take the path would split one correctly
/// placed file into a stale leftover plus a rewritten copy. A hardlink
/// twin elsewhere is another location and does not count.
fn in_place(plan: &UnitPlan) -> bool {
    plan.desired.as_ref().is_some_and(|desired| {
        super::unit_source_files(&crate::dat::units::DatUnit::File(plan.source.clone()))
            .iter()
            .any(|path| super::same_location(path, desired))
    })
}

/// Tie-break between inputs competing for the same game id or output path:
/// an already-placed unit first, then best format (copy/link beats re-zip
/// beats convert), then a preferred filename match, then the plain-file
/// preference (a plain file beats an archive input), then
/// input path order. The earlier plan wins a full tie.
fn dedupe_prefer(a: &UnitPlan, b: &UnitPlan, preferences: &Preferences) -> Ordering {
    let order = in_place(b).cmp(&in_place(a));
    if order != Ordering::Equal {
        return order;
    }
    let order = action_rank(&a.action).cmp(&action_rank(&b.action));
    if order != Ordering::Equal {
        return order;
    }
    if !preferences.filename_regex.is_empty() {
        // The input's own file name, not the (shared) output stem: two
        // plans colliding on one output path usually share that stem, so
        // only the input name can break the tie.
        let order = regex_rank(&preferences.filename_regex, source_name(a))
            .cmp(&regex_rank(&preferences.filename_regex, source_name(b)));
        if order != Ordering::Equal {
            return order;
        }
    }
    let order =
        crate::util::is_archive_path(&a.source).cmp(&crate::util::is_archive_path(&b.source));
    if order != Ordering::Equal {
        return order;
    }
    input_display(a).cmp(&input_display(b))
}

/// Archive inputs need one more conversion step than plain files, so plain
/// files rank first; copies/links are already in their best shape.
fn action_rank(action: &Action) -> u8 {
    match action {
        Action::Copy | Action::Link => 0,
        Action::Zip => 1,
        Action::Convert { .. } => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::template::TemplateTokens;
    use std::path::{Path, PathBuf};

    fn game(id: &str, parent_id: Option<&str>, name: &str) -> GameRef {
        GameRef {
            id: id.to_string(),
            parent_id: parent_id.map(str::to_string),
            name: name.to_string(),
            dat_name: None,
            platform: Some("Nintendo SNES".to_string()),
            tags: GameTags::parse(name),
        }
    }

    fn plan(index: usize, file: &str, game: Option<GameRef>, action: Action) -> UnitPlan {
        UnitPlan {
            index,
            source: PathBuf::from(file),
            source_ext: String::new(),
            action,
            tokens: TemplateTokens::new(None, Path::new(file), "zip"),
            input_subdir: PathBuf::new(),
            game,
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
            desired: None,
            decision: Decision::Keep,
        }
    }

    fn filters() -> Filters {
        Filters {
            include: vec![],
            exclude: vec![],
            languages: vec![],
            regions: vec![],
            no_type: vec![],
            only_type: vec![],
            only_retail: false,
        }
    }

    fn prefs() -> Preferences {
        Preferences {
            single: true,
            game_regex: vec![],
            filename_regex: vec![],
            prefer_verified: false,
            prefer_good: false,
            prefer_retail: false,
            prefer_parent: false,
            languages: vec![],
            regions: vec![],
            revision: None,
        }
    }

    fn decisions(plans: &[UnitPlan]) -> Vec<&Decision> {
        plans.iter().map(|plan| &plan.decision).collect()
    }

    #[test]
    fn filter_language_falls_back_to_region_primary_language() {
        let mut filters = filters();
        filters.languages = vec!["FR".to_string()];
        let mut plans = vec![
            plan(
                0,
                "Game (France).zip",
                Some(game("g1", None, "Game (France)")),
                Action::Copy,
            ),
            plan(
                1,
                "Game (USA).zip",
                Some(game("g2", None, "Game (USA)")),
                Action::Copy,
            ),
        ];
        apply_filters(&mut plans, &filters);
        assert_eq!(plans[0].decision, Decision::Keep);
        assert_eq!(
            plans[1].decision,
            Decision::Skip("filtered: no filter_language match".to_string())
        );
    }

    #[test]
    fn no_type_excludes_bios_names() {
        let mut filters = filters();
        filters.no_type = vec![GameKind::Bios];
        let mut plans = vec![
            plan(0, "[BIOS] Nintendo (USA).zip", None, Action::Copy),
            plan(1, "Super Game (USA).zip", None, Action::Copy),
        ];
        apply_filters(&mut plans, &filters);
        assert_eq!(
            plans[0].decision,
            Decision::Skip("filtered: no_type bios".to_string())
        );
        assert_eq!(plans[1].decision, Decision::Keep);
    }

    #[test]
    fn only_retail_excludes_hacks_and_betas() {
        let mut filters = filters();
        filters.only_retail = true;
        let mut plans = vec![
            plan(0, "Game (Hack).zip", None, Action::Copy),
            plan(1, "Game (Beta).zip", None, Action::Copy),
            plan(2, "Game (Europe).zip", None, Action::Copy),
        ];
        apply_filters(&mut plans, &filters);
        assert!(matches!(plans[0].decision, Decision::Skip(_)));
        assert!(matches!(plans[1].decision, Decision::Skip(_)));
        assert_eq!(plans[2].decision, Decision::Keep);
    }

    #[test]
    fn single_prefers_higher_priority_region() {
        let mut preferences = prefs();
        preferences.regions = vec!["USA".to_string(), "EUR".to_string()];
        let mut plans = vec![
            plan(
                0,
                "Game (Europe).zip",
                Some(game("g-eur", Some("p1"), "Game (Europe)")),
                Action::Copy,
            ),
            plan(
                1,
                "Game (USA).zip",
                Some(game("g-usa", Some("p1"), "Game (USA)")),
                Action::Copy,
            ),
        ];
        apply_single(&mut plans, &preferences);
        assert_eq!(
            plans[0].decision,
            Decision::Skip("duplicate of Game (USA).zip".to_string())
        );
        assert_eq!(plans[1].decision, Decision::Keep);
    }

    #[test]
    fn single_keeps_every_disc_of_a_set() {
        let mut plans = vec![
            plan(
                0,
                "Game (Disc 1).zip",
                Some(game("g1", Some("p1"), "Game (Disc 1)")),
                Action::Copy,
            ),
            plan(
                1,
                "Game (Disc 2).zip",
                Some(game("g2", Some("p1"), "Game (Disc 2)")),
                Action::Copy,
            ),
        ];
        apply_single(&mut plans, &prefs());
        assert_eq!(decisions(&plans), vec![&Decision::Keep, &Decision::Keep]);
    }

    #[test]
    fn single_prefers_newer_revision() {
        let mut preferences = prefs();
        preferences.revision = Some(RevisionOrder::Newer);
        let mut plans = vec![
            plan(
                0,
                "Game (Rev 1).zip",
                Some(game("g1", Some("p1"), "Game (Rev 1)")),
                Action::Copy,
            ),
            plan(
                1,
                "Game (Rev 2).zip",
                Some(game("g2", Some("p1"), "Game (Rev 2)")),
                Action::Copy,
            ),
        ];
        apply_single(&mut plans, &preferences);
        assert_eq!(
            plans[0].decision,
            Decision::Skip("duplicate of Game (Rev 2).zip".to_string())
        );
        assert_eq!(plans[1].decision, Decision::Keep);
    }

    #[test]
    fn single_collapses_parentless_games_by_title() {
        let mut plans = vec![
            plan(
                0,
                "Game (Europe).zip",
                Some(game("g1", None, "Game (Europe)")),
                Action::Copy,
            ),
            plan(
                1,
                "Game (USA).zip",
                Some(game("g2", None, "Game (USA)")),
                Action::Copy,
            ),
        ];
        apply_single(&mut plans, &prefs());
        assert_eq!(plans[0].decision, Decision::Keep);
        assert_eq!(
            plans[1].decision,
            Decision::Skip("duplicate of Game (Europe).zip".to_string())
        );
    }

    #[test]
    fn dedupe_prefers_copy_over_convert() {
        let convert = Action::Convert {
            op: "chd.compress",
            ext: "chd".to_string(),
            format: None,
        };
        let mut plans = vec![
            plan(
                0,
                "Game (USA).iso",
                Some(game("g1", None, "Game (USA)")),
                convert,
            ),
            plan(
                1,
                "Game (USA).zip",
                Some(game("g1", None, "Game (USA)")),
                Action::Copy,
            ),
            plan(
                2,
                "Game (USA).zip",
                Some(game("g1", None, "Game (USA)")),
                Action::Copy,
            ),
        ];
        plans[2].patch = Some(PathBuf::from("patches/Game.ips"));
        dedupe_by_game(&mut plans, &prefs());
        assert_eq!(
            plans[0].decision,
            Decision::Skip("duplicate of Game (USA).zip".to_string())
        );
        assert_eq!(plans[1].decision, Decision::Keep);
        // Patched variants stay in their base plan's dedupe group: patch
        // expansion runs after dedupe.
        assert_eq!(
            plans[2].decision,
            Decision::Skip("duplicate of Game (USA).zip".to_string())
        );
    }

    /// The plain-file preference: for the same game id a plain file
    /// ranks ahead of an archive input when formats tie.
    #[test]
    fn dedupe_prefers_plain_file_over_archive_input() {
        let mut plans = vec![
            plan(
                0,
                "Game.zip",
                Some(game("g1", None, "Game (USA)")),
                Action::Zip,
            ),
            plan(
                1,
                "Game.gba",
                Some(game("g1", None, "Game (USA)")),
                Action::Zip,
            ),
        ];
        dedupe_by_game(&mut plans, &prefs());
        assert_eq!(
            plans[0].decision,
            Decision::Skip("duplicate of Game.gba".to_string())
        );
        assert_eq!(plans[1].decision, Decision::Keep);
    }

    /// In-place means the same directory entry, not the same content: a
    /// hardlink twin at the desired path in another directory does not
    /// make the unit in place, while the unit's own path does. A split
    /// WUD first part is in place at any of its continuations' paths.
    #[cfg(unix)]
    #[test]
    fn in_place_requires_the_same_location() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("Game.zip");
        std::fs::write(&a, b"zip").unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let twin = elsewhere.join("Game.zip");
        std::fs::hard_link(&a, &twin).unwrap();

        let mut twin_plan = plan(0, a.to_str().unwrap(), None, Action::Copy);
        twin_plan.desired = Some(twin);
        assert!(!in_place(&twin_plan), "a hardlink twin is another location");

        let mut own = plan(1, a.to_str().unwrap(), None, Action::Copy);
        own.desired = Some(a);
        assert!(in_place(&own));

        let wud = dir.path().join("wud");
        std::fs::create_dir(&wud).unwrap();
        for n in 1..=2 {
            std::fs::write(wud.join(format!("game_part{n}.wud")), vec![0u8; 64]).unwrap();
        }
        let mut part1 = plan(
            2,
            wud.join("game_part1.wud").to_str().unwrap(),
            None,
            Action::Copy,
        );
        part1.desired = Some(wud.join("game_part2.wud"));
        assert!(in_place(&part1), "part1 owns its continuations' paths");
    }

    #[test]
    fn dedupe_by_path_keeps_one_winner() {
        let mut plans = vec![
            plan(0, "Game (Europe).zip", None, Action::Copy),
            plan(1, "Game (USA).zip", None, Action::Copy),
        ];
        plans[0].desired = Some(PathBuf::from("out/Game.zip"));
        plans[1].desired = Some(PathBuf::from("out/Game.zip"));
        dedupe_by_path(&mut plans, &prefs(), true);
        assert_eq!(plans[0].decision, Decision::Keep);
        assert_eq!(
            plans[1].decision,
            Decision::Skip("duplicate output out/Game.zip".to_string())
        );
    }

    #[test]
    fn single_buckets_parent_and_clone_together() {
        let mut preferences = prefs();
        preferences.regions = vec!["EUR".to_string(), "USA".to_string()];
        let mut plans = vec![
            plan(
                0,
                "Game (USA).zip",
                Some(game("p1", None, "Game (USA)")),
                Action::Copy,
            ),
            plan(
                1,
                "Game (Europe).zip",
                Some(game("g-eur", Some("p1"), "Game (Europe)")),
                Action::Copy,
            ),
        ];
        apply_single(&mut plans, &preferences);
        assert_eq!(
            plans[0].decision,
            Decision::Skip("duplicate of Game (Europe).zip".to_string())
        );
        assert_eq!(plans[1].decision, Decision::Keep);
    }

    /// The Title fallback is all-or-nothing per platform: once a platform
    /// carries parent/clone information, cloneless games keep their own
    /// family bucket instead of collapsing by title.
    #[test]
    fn single_keeps_cloneless_games_separate_on_pc_platforms() {
        let mut plans = vec![
            plan(
                0,
                "Game (USA).zip",
                Some(game("p1", None, "Game (USA)")),
                Action::Copy,
            ),
            plan(
                1,
                "Game (Europe).zip",
                Some(game("g-eur", Some("p1"), "Game (Europe)")),
                Action::Copy,
            ),
            plan(
                2,
                "Game (Brazil).zip",
                Some(game("g-bra", None, "Game (Brazil)")),
                Action::Copy,
            ),
        ];
        apply_single(&mut plans, &prefs());
        assert_eq!(plans[0].decision, Decision::Keep);
        assert_eq!(
            plans[1].decision,
            Decision::Skip("duplicate of Game (USA).zip".to_string())
        );
        assert_eq!(plans[2].decision, Decision::Keep);
    }

    /// Filters run before `apply_single` and only set `Decision::Skip`; the
    /// `GameRef` survives on skipped plans. A platform's P/C presence must
    /// come from every plan carrying a `GameRef`, not just the surviving
    /// `Keep` plans, so dropping every clone (e.g. via a filter) must not
    /// flip the platform into title fallback and collapse unrelated
    /// cloneless games that merely share a normalized title.
    #[test]
    fn single_pc_platform_survives_when_every_clone_is_filtered_out() {
        let mut plans = vec![
            plan(
                0,
                "Other Game (Europe).zip",
                Some(game("clone1", Some("p1"), "Other Game (Europe)")),
                Action::Copy,
            ),
            plan(
                1,
                "Twin Game (USA).zip",
                Some(game("twin-usa", None, "Twin Game (USA)")),
                Action::Copy,
            ),
            plan(
                2,
                "Twin Game (Europe).zip",
                Some(game("twin-eur", None, "Twin Game (Europe)")),
                Action::Copy,
            ),
        ];
        plans[0].decision = Decision::Skip("filtered".to_string());
        apply_single(&mut plans, &prefs());
        assert_eq!(plans[0].decision, Decision::Skip("filtered".to_string()));
        assert_eq!(plans[1].decision, Decision::Keep);
        assert_eq!(plans[2].decision, Decision::Keep);
    }

    #[test]
    fn dedupe_by_path_folds_path_case() {
        let mut plans = vec![
            plan(0, "Game (USA).zip", None, Action::Copy),
            plan(1, "game (usa).zip", None, Action::Copy),
        ];
        plans[0].desired = Some(PathBuf::from("out/Game.zip"));
        plans[1].desired = Some(PathBuf::from("out/game.zip"));
        dedupe_by_path(&mut plans, &prefs(), true);
        assert_eq!(plans[0].decision, Decision::Keep);
        assert_eq!(
            plans[1].decision,
            Decision::Skip("duplicate output out/Game.zip".to_string())
        );
    }

    /// On case-sensitive file systems grouping is exact: `Game.zip` and
    /// `game.zip` are distinct output names.
    #[test]
    fn dedupe_by_path_keeps_case_distinct_paths_without_folding() {
        let mut plans = vec![
            plan(0, "Game (USA).zip", None, Action::Copy),
            plan(1, "game (usa).zip", None, Action::Copy),
        ];
        plans[0].desired = Some(PathBuf::from("out/Game.zip"));
        plans[1].desired = Some(PathBuf::from("out/game.zip"));
        dedupe_by_path(&mut plans, &prefs(), false);
        assert_eq!(decisions(&plans), vec![&Decision::Keep, &Decision::Keep]);
    }

    /// The probe only stats: it answers from an existing entry's
    /// case-flipped name and never creates or removes anything, so a dry
    /// run previews exactly what the run decides.
    #[test]
    fn probe_answers_from_entries_without_writing() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Probe Entry.bin"), b"").unwrap();
        let expected = root.path().join("PROBE ENTRY.BIN").exists();
        assert_eq!(probe_case_insensitive(root.path()), expected);
        let count = std::fs::read_dir(root.path()).unwrap().count();
        assert_eq!(count, 1, "the probe writes nothing");

        let nested = root.path().join("output").join("roms").join("snes");
        probe_case_insensitive(&nested);
        assert!(!nested.exists());
        assert!(!root.path().join("output").exists());
    }

    /// An entry whose name carries no ASCII letters cannot discriminate,
    /// and an empty base has nothing to compare: both fall back to the
    /// platform default.
    #[test]
    fn probe_without_lettered_entries_takes_the_default() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            probe_case_insensitive(root.path()),
            cfg!(any(target_os = "macos", windows))
        );
        std::fs::write(root.path().join("0123"), b"").unwrap();
        assert_eq!(
            probe_case_insensitive(root.path()),
            cfg!(any(target_os = "macos", windows))
        );
    }

    /// The flip-and-compare core against a stubbed lookup, so the answer
    /// never depends on the host volume: a flipped spelling the lookup
    /// rejects means case-sensitive even on a case-insensitive host, an
    /// upper-only name flips down, and a non-lettered name defers to the
    /// given default.
    #[test]
    fn probe_core_decides_from_the_flipped_lookup() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Ab.txt"), b"").unwrap();
        let entries = || std::fs::read_dir(root.path()).unwrap().flatten();

        // The lookup accepts the flipped spelling: insensitive.
        let accepted = probe_from_entries(root.path(), entries(), true, |_, flipped| {
            flipped.file_name().and_then(|name| name.to_str()) == Some("AB.TXT")
        });
        assert!(accepted);

        // The lookup rejects it: case-sensitive, whatever the host default.
        let rejected = probe_from_entries(root.path(), entries(), true, |_, _| false);
        assert!(!rejected);

        // An upper-only name flips down.
        let upper = tempfile::tempdir().unwrap();
        std::fs::write(upper.path().join("README"), b"").unwrap();
        let upper_entries = std::fs::read_dir(upper.path()).unwrap().flatten();
        assert!(probe_from_entries(
            upper.path(),
            upper_entries,
            false,
            |original, flipped| {
                flipped.file_name().and_then(|name| name.to_str()) == Some("readme")
                    && original.file_name().and_then(|name| name.to_str()) == Some("README")
            }
        ));

        // A non-lettered name cannot decide: the default stands.
        let digits = tempfile::tempdir().unwrap();
        std::fs::write(digits.path().join("0123"), b"").unwrap();
        let digit_entries = std::fs::read_dir(digits.path()).unwrap().flatten();
        assert!(probe_from_entries(
            digits.path(),
            digit_entries,
            true,
            |_, _| false
        ));
        let digit_entries = std::fs::read_dir(digits.path()).unwrap().flatten();
        assert!(!probe_from_entries(
            digits.path(),
            digit_entries,
            false,
            |_, _| false
        ));
    }

    /// A relative `dir` that does not exist anywhere still resolves through
    /// the real filesystem: absolutizing first means the ancestors walk
    /// reaches the process's current directory (which always exists)
    /// instead of exhausting the relative path's own ancestors and
    /// silently falling back to the platform default.
    #[test]
    fn probe_case_insensitive_absolutizes_a_relative_missing_dir() {
        let cwd = std::env::current_dir().unwrap();
        let relative = Path::new("rc-relative-probe-test")
            .join("nested")
            .join("dir");
        let absolute = cwd.join(&relative);

        assert_eq!(
            probe_base(&relative).as_deref(),
            Some(cwd.as_path()),
            "a relative missing dir's deepest existing ancestor is the current dir"
        );
        assert_eq!(probe_base(&absolute), probe_base(&relative));
    }

    #[test]
    fn compile_all_accepts_slash_delimited_regex_flags() {
        let regexes = compile_all(&["/mario/i".to_string()], "filter_regex").unwrap();
        assert!(regexes[0].is_match("Super MARIO"));
        assert!(!regexes[0].is_match("super luigi"));

        let err = compile_all(&["/mario/q".to_string()], "filter_regex")
            .expect_err("unknown regex flags are invalid");
        assert!(err.to_string().contains("unknown flag"));

        // Without a closing slash the string stays a raw pattern.
        let regexes = compile_all(&["/mario".to_string()], "filter_regex").unwrap();
        assert!(regexes[0].is_match("/mario"));
        assert!(!regexes[0].is_match("Super MARIO"));
    }

    /// `prefer_filename_regex` ranks by the input's own file name: two
    /// plans colliding on one output path share the output stem, so only
    /// the input name (here the archive's `.zip`) can break the tie.
    #[test]
    fn dedupe_by_path_filename_regex_matches_the_input_name() {
        let mut preferences = prefs();
        preferences.filename_regex =
            vec![compile_regex("\\.zip$", "prefer_filename_regex").expect("valid regex")];
        let mut plans = vec![
            plan(0, "Game.gba", None, Action::Copy),
            plan(1, "Game.zip", None, Action::Copy),
        ];
        plans[0].desired = Some(PathBuf::from("out/Game.zip"));
        plans[1].desired = Some(PathBuf::from("out/Game.zip"));
        dedupe_by_path(&mut plans, &preferences, true);
        assert_eq!(plans[1].decision, Decision::Keep);
        assert_eq!(
            plans[0].decision,
            Decision::Skip("duplicate output out/Game.zip".to_string())
        );
    }

    /// Filter language/region codes must name a table code: a typo such as
    /// `US` (the code is `USA`) would otherwise silently filter out every
    /// unit. The same table validates the prefer codes.
    #[test]
    fn unknown_language_and_region_codes_are_rejected() {
        let mut options = RunOptions {
            filter_language: Some(vec!["US".to_string()]),
            ..RunOptions::default()
        };
        let err = Filters::from_options(&options)
            .err()
            .expect("unknown language code");
        assert!(err.to_string().contains("filter_language"), "{err}");

        options.filter_language = Some(vec!["en".to_string(), "JA".to_string()]);
        options.filter_region = Some(vec!["USA".to_string()]);
        assert!(Filters::from_options(&options).is_ok());

        // Surrounding whitespace is trimmed before validation and in the
        // collected codes, so a `en, fr` list still matches FR units.
        options.filter_language = Some(vec!["en".to_string(), " fr ".to_string()]);
        let filters = Filters::from_options(&options).unwrap();
        assert_eq!(filters.languages, vec!["en", "fr"]);

        options.filter_language = None;
        options.filter_region = Some(vec!["US".to_string()]);
        let err = Filters::from_options(&options)
            .err()
            .expect("unknown region code");
        assert!(err.to_string().contains("filter_region"), "{err}");

        let mut prefer = RunOptions {
            prefer_language: Some(vec!["zz".to_string()]),
            ..RunOptions::default()
        };
        assert!(Preferences::from_options(&prefer).is_err());
        prefer.prefer_language = Some(vec!["ja".to_string()]);
        prefer.prefer_region = Some(vec!["EUR".to_string()]);
        assert!(Preferences::from_options(&prefer).is_ok());
    }

    /// `prefer_revision "any"` is the explicit no-preference: it parses,
    /// leaves the revision ordering unset, and unknown values stay
    /// invalid.
    #[test]
    fn prefer_revision_any_means_no_preference() {
        let mut options = RunOptions {
            prefer_revision: Some("any".to_string()),
            ..RunOptions::default()
        };
        let preferences = Preferences::from_options(&options).unwrap();
        assert_eq!(preferences.revision, None);

        options.prefer_revision = Some("sometimes".to_string());
        assert!(Preferences::from_options(&options).is_err());

        options.prefer_revision = Some("older".to_string());
        assert_eq!(
            Preferences::from_options(&options).unwrap().revision,
            Some(RevisionOrder::Older)
        );
    }
}
