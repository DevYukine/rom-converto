use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscGroup {
    pub base_title: String,
    pub discs: Vec<PathBuf>,
    pub has_duplicate_numbers: bool,
}

impl DiscGroup {
    pub fn len(&self) -> usize {
        self.discs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.discs.is_empty()
    }
}

/// Parse a filename stem into a `(base_title, disc_number)` pair when it
/// carries a recognized disc token, otherwise return `None`.
///
/// The recognized forms are the Redump parenthesized "(Disc N)" /
/// "(Disc N of M)" and the TOSEC bare "Disc N" / "Disc N of M" at the end of
/// the stem. Matching is case-insensitive on the word "Disc". The token must
/// be preceded by '(' or whitespace so real titles like "Discworld" and
/// "Disco Elysium" never match. The last matching token wins, so a leading
/// region tag such as "(USA)" survives in the returned base title.
pub fn parse_disc_token(stem: &str) -> Option<(String, u32)> {
    if let Some(result) = parse_parenthesized(stem) {
        return Some(result);
    }
    parse_bare(stem)
}

fn parse_parenthesized(stem: &str) -> Option<(String, u32)> {
    let mut search_end = stem.len();
    while let Some(open_rel) = stem[..search_end].rfind('(') {
        let after_open = open_rel + 1;
        if let Some(close_rel) = stem[after_open..].find(')') {
            let close = after_open + close_rel;
            let body = &stem[after_open..close];
            if let Some(n) = parse_token_body(body) {
                let left = stem[..open_rel].trim_end();
                let right = stem[close + 1..].trim_start();
                let base = if left.is_empty() {
                    right.trim().to_string()
                } else if right.is_empty() {
                    left.trim().to_string()
                } else {
                    format!("{left} {right}")
                };
                return Some((base, n));
            }
        }
        if open_rel == 0 {
            break;
        }
        search_end = open_rel;
    }
    None
}

fn parse_bare(stem: &str) -> Option<(String, u32)> {
    let lower = stem.to_ascii_lowercase();
    let mut search_end = stem.len();
    while let Some(idx) = lower[..search_end].rfind("disc") {
        let preceded_ok = idx == 0
            || lower[..idx]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace());
        if preceded_ok {
            let after = &stem[idx + 4..];
            if let Some(n) = parse_token_tail(after) {
                let base = stem[..idx].trim_end().to_string();
                return Some((base, n));
            }
        }
        if idx == 0 {
            break;
        }
        search_end = idx;
    }
    None
}

/// Parse the inside of a parenthesized token, e.g. "Disc 2" or "Disc 2 of 3".
fn parse_token_body(body: &str) -> Option<u32> {
    let mut parts = body.split_ascii_whitespace();
    let head = parts.next()?;
    if !head.eq_ignore_ascii_case("disc") {
        return None;
    }
    let n: u32 = parts.next()?.parse().ok()?;
    match parts.next() {
        None => Some(n),
        Some(of) if of.eq_ignore_ascii_case("of") => {
            parts.next()?.parse::<u32>().ok()?;
            if parts.next().is_none() { Some(n) } else { None }
        }
        Some(_) => None,
    }
}

/// Parse a bare token tail that starts right after the word "disc" and must
/// run to the end of the stem, e.g. " 2" or " 2 of 3".
fn parse_token_tail(after: &str) -> Option<u32> {
    let trimmed = after.trim_start();
    if trimmed.len() == after.len() {
        return None;
    }
    let mut parts = trimmed.split_ascii_whitespace();
    let n: u32 = parts.next()?.parse().ok()?;
    match parts.next() {
        None => Some(n),
        Some(of) if of.eq_ignore_ascii_case("of") => {
            parts.next()?.parse::<u32>().ok()?;
            if parts.next().is_none() { Some(n) } else { None }
        }
        Some(_) => None,
    }
}

/// Group disc files by their parsed base title. Files without a disc token
/// form singleton groups keyed by their full stem. Within a group, discs are
/// ordered by parsed disc number ascending, with ties broken by path so the
/// output is deterministic. Grouping is case-sensitive on the base string,
/// since filenames are case-sensitive and distinct casings may be distinct
/// titles. Files that resolve to the same disc number are all kept and the
/// group is flagged via `has_duplicate_numbers`.
pub fn group_disc_files(files: &[PathBuf]) -> Vec<DiscGroup> {
    let mut groups: BTreeMap<String, Vec<(u32, PathBuf)>> = BTreeMap::new();
    for file in files {
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (key, number) = match parse_disc_token(&stem) {
            Some((base, n)) => (base, n),
            None => (stem, 0),
        };
        groups.entry(key).or_default().push((number, file.clone()));
    }

    groups
        .into_iter()
        .map(|(base_title, mut entries)| {
            entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            let has_duplicate_numbers = entries.windows(2).any(|w| w[0].0 == w[1].0);
            let discs = entries.into_iter().map(|(_, path)| path).collect();
            DiscGroup {
                base_title,
                discs,
                has_duplicate_numbers,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn redump_disc_n() {
        assert_eq!(
            parse_disc_token("Final Fantasy VII (Disc 1)"),
            Some(("Final Fantasy VII".to_string(), 1))
        );
    }

    #[test]
    fn redump_disc_n_of_m() {
        assert_eq!(
            parse_disc_token("Final Fantasy VII (Disc 2 of 3)"),
            Some(("Final Fantasy VII".to_string(), 2))
        );
    }

    #[test]
    fn tosec_bare_disc_n_of_m() {
        assert_eq!(
            parse_disc_token("Final Fantasy VII Disc 1 of 3"),
            Some(("Final Fantasy VII".to_string(), 1))
        );
    }

    #[test]
    fn tosec_bare_disc_n() {
        assert_eq!(
            parse_disc_token("Final Fantasy VII Disc 2"),
            Some(("Final Fantasy VII".to_string(), 2))
        );
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(
            parse_disc_token("Game (disc 1)"),
            Some(("Game".to_string(), 1))
        );
        assert_eq!(
            parse_disc_token("Game (DISC 1)"),
            Some(("Game".to_string(), 1))
        );
    }

    #[test]
    fn padded_equals_unpadded() {
        let padded = parse_disc_token("Game (Disc 01)").unwrap();
        let unpadded = parse_disc_token("Game (Disc 1)").unwrap();
        assert_eq!(padded.1, 1);
        assert_eq!(padded.1, unpadded.1);
    }

    #[test]
    fn region_tag_survives() {
        assert_eq!(
            parse_disc_token("Parasite Eve (USA) (Disc 2)"),
            Some(("Parasite Eve (USA)".to_string(), 2))
        );
    }

    #[test]
    fn discworld_not_matched() {
        assert_eq!(parse_disc_token("Discworld (1995)"), None);
    }

    #[test]
    fn disco_elysium_not_matched() {
        assert_eq!(parse_disc_token("Disco Elysium"), None);
    }

    #[test]
    fn title_word_disc_with_real_token() {
        assert_eq!(
            parse_disc_token("Disc World (Disc 1)"),
            Some(("Disc World".to_string(), 1))
        );
    }

    #[test]
    fn no_token_returns_none() {
        assert_eq!(parse_disc_token("Crash Bandicoot"), None);
    }

    #[test]
    fn groups_two_discs_in_order() {
        let groups = group_disc_files(&paths(&["Game (Disc 2).cue", "Game (Disc 1).cue"]));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].base_title, "Game");
        assert_eq!(
            groups[0].discs,
            paths(&["Game (Disc 1).cue", "Game (Disc 2).cue"])
        );
    }

    #[test]
    fn mixed_extensions_group_together() {
        let groups = group_disc_files(&paths(&["Game (Disc 1).cue", "Game (Disc 2).chd"]));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn numeric_sort_not_string() {
        let groups = group_disc_files(&paths(&["Game (Disc 10).cue", "Game (Disc 2).cue"]));
        assert_eq!(
            groups[0].discs,
            paths(&["Game (Disc 2).cue", "Game (Disc 10).cue"])
        );
    }

    #[test]
    fn untokened_is_singleton() {
        let groups = group_disc_files(&paths(&["Sonic.cue"]));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].base_title, "Sonic");
        assert_eq!(groups[0].len(), 1);
    }

    #[test]
    fn duplicate_numbers_flagged() {
        let groups = group_disc_files(&paths(&["Game (Disc 1).cue", "Game (Disc 1).chd"]));
        assert_eq!(groups.len(), 1);
        assert!(groups[0].has_duplicate_numbers);
        assert_eq!(
            groups[0].discs,
            paths(&["Game (Disc 1).chd", "Game (Disc 1).cue"])
        );
    }

    #[test]
    fn deterministic_group_order() {
        let groups = group_disc_files(&paths(&[
            "Zelda (Disc 1).cue",
            "Alpha (Disc 1).cue",
            "Mario (Disc 1).cue",
        ]));
        let titles: Vec<&str> = groups.iter().map(|g| g.base_title.as_str()).collect();
        assert_eq!(titles, vec!["Alpha", "Mario", "Zelda"]);
    }

    #[test]
    fn discworld_singletons_not_merged() {
        let groups = group_disc_files(&paths(&["Discworld (1995).cue"]));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].base_title, "Discworld (1995)");
        assert!(!groups[0].has_duplicate_numbers);
    }
}
