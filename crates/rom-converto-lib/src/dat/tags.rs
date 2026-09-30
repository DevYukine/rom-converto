//! Release-name tag parsing for the common DAT naming conventions.
//!
//! Two families of tags are recognized:
//!
//! * Parenthesised `(…)` tags in the No-Intro/Redump style: comma-separated
//!   region groups (`(USA, Europe)`), language groups (`(En,Fr)`),
//!   revisions (`(Rev 1)`, `(Rev A)`, `(v1.2)`, `(Version 1.02)`), disc
//!   numbers (`(Disc 2)`), and release-kind markers (`(Beta)`, `(Proto)`,
//!   `(Sample)`, `(Unl)`, …).
//! * Square-bracket `[…]` tags in the older TOSEC-style dump conventions:
//!   the classic markers `[!]` (verified dump), `[b]` (bad dump),
//!   `[c]`/`[x]` (bad checksum: a bad dump only when the name is not
//!   verified), `[h]` (hack), `[t]` (trained), `[f]` (fixed), `[o]`
//!   (overdump), `[p]` (pirated), `[T-]`/`[T+]` (translation), and
//!   `[cr]` (cracked).
//!
//! Region names map to short region codes; language groups map to
//! uppercase two-letter language codes.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;

/// Tag groups parsed out of a ROM file/DAT game name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GameTags {
    /// Region codes in tag order (e.g. `USA`, `EUR`, `JPN`).
    pub regions: Vec<String>,
    /// 2-letter language codes in tag order (e.g. `EN`, `JA`).
    pub languages: Vec<String>,
    /// Revision/version tag, when present.
    pub revision: Option<Revision>,
    /// Game kinds detected in the name (never contains [`GameKind::Unverified`]).
    pub kinds: BTreeSet<GameKind>,
    /// 1-based disc number from `(Disc N)`/`(Disk N)`/`(CD N)` tags.
    pub disc: Option<u32>,
    /// Whether the name carries the classic `[!]` verified-dump marker.
    pub verified: bool,
}

/// Kind of release a name belongs to; drives the `{type}` template token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GameKind {
    Bios,
    Device,
    Unlicensed,
    Debug,
    Demo,
    Beta,
    Sample,
    Prototype,
    Program,
    Aftermarket,
    Homebrew,
    Alpha,
    Bootleg,
    Cracked,
    Fixed,
    Hacked,
    Overdump,
    PendingDump,
    Pirated,
    Trained,
    Translated,
    Bad,
    Unverified,
}

/// Revision/version tag of a release name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Revision {
    /// `(Rev 1)` style numeric revision.
    Number(u32),
    /// `(Rev A)` style letter revision.
    Letter(char),
    /// `(v1.2.3)`/`(Version 1.02)` style dotted version.
    Version(Vec<u32>),
}

impl GameTags {
    /// Parses the `(…)`/`[…]` tags of a release name following the common
    /// DAT naming conventions.
    pub fn parse(name: &str) -> Self {
        let lower = name.to_lowercase();
        let mut tags = GameTags {
            kinds: detect_kinds(&lower),
            verified: lower.contains("[!]"),
            ..GameTags::default()
        };
        for group in PAREN_GROUP_RE.find_iter(name) {
            tags.absorb_paren_group(&name[group.start() + 1..group.end() - 1]);
        }
        tags
    }

    /// True when the name carries none of the non-retail markers;
    /// unlicensed releases still count as retail.
    pub fn is_retail(&self) -> bool {
        !self.kinds.iter().any(|kind| {
            matches!(
                kind,
                GameKind::Alpha
                    | GameKind::Aftermarket
                    | GameKind::Bad
                    | GameKind::Beta
                    | GameKind::Bios
                    | GameKind::Bootleg
                    | GameKind::Cracked
                    | GameKind::Debug
                    | GameKind::Demo
                    | GameKind::Device
                    | GameKind::Fixed
                    | GameKind::Hacked
                    | GameKind::Homebrew
                    | GameKind::Overdump
                    | GameKind::PendingDump
                    | GameKind::Pirated
                    | GameKind::Program
                    | GameKind::Prototype
                    | GameKind::Sample
                    | GameKind::Trained
                    | GameKind::Translated
            )
        })
    }

    /// `{type}` label, resolved in kind-precedence order (BIOS first,
    /// retail last).
    pub fn type_label(&self) -> &'static str {
        let has = |kind: GameKind| self.kinds.contains(&kind);
        if has(GameKind::Bios) {
            "BIOS"
        } else if has(GameKind::Device) {
            "Device"
        } else if has(GameKind::Aftermarket) {
            "Aftermarket"
        } else if has(GameKind::Homebrew) {
            "Homebrew"
        } else if has(GameKind::Unlicensed) {
            "Unlicensed"
        } else if has(GameKind::Bad) {
            "Bad"
        } else if has(GameKind::Alpha) {
            "Alpha"
        } else if has(GameKind::Beta) {
            "Beta"
        } else if has(GameKind::Prototype) {
            "Prototype"
        } else if has(GameKind::Sample) {
            "Sample"
        } else if has(GameKind::Demo) {
            "Demo"
        } else if has(GameKind::Debug) {
            "Debug"
        } else if has(GameKind::Program) {
            "Program"
        } else if has(GameKind::Cracked) {
            "Cracked"
        } else if has(GameKind::Bootleg) {
            "Bootleg"
        } else if has(GameKind::Fixed) {
            "Fixed"
        } else if has(GameKind::Hacked) {
            "Hacked"
        } else if has(GameKind::Overdump) {
            "Overdump"
        } else if has(GameKind::PendingDump) {
            "Pending Dump"
        } else if has(GameKind::Pirated) {
            "Pirated"
        } else if has(GameKind::Trained) {
            "Trained"
        } else if has(GameKind::Translated) {
            "Translated"
        } else {
            "Retail"
        }
    }

    /// First tagged region code, if any.
    pub fn primary_region(&self) -> Option<&str> {
        self.regions.first().map(String::as_str)
    }

    /// First tagged language code, falling back to the primary region's
    /// main language when the name tags none.
    pub fn primary_language(&self) -> Option<&str> {
        self.languages
            .first()
            .map(String::as_str)
            .or_else(|| self.primary_region().and_then(region_primary_language))
    }

    /// Title with every `(…)`/`[…]` tag stripped, whitespace collapsed,
    /// lowercased.
    pub fn normalized_title(name: &str) -> String {
        GROUP_RE
            .replace_all(name, " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }

    fn absorb_paren_group(&mut self, inner: &str) {
        let content = inner.trim();
        if content.is_empty() {
            return;
        }
        if let Some(revision) = parse_revision(content) {
            if self.revision.is_none() {
                self.revision = Some(revision);
            }
            return;
        }
        if let Some(disc) = parse_disc(content) {
            if self.disc.is_none() {
                self.disc = Some(disc);
            }
            return;
        }
        if let Some(codes) = parse_region_group(content) {
            push_unique(&mut self.regions, codes);
            return;
        }
        if let Some(codes) = parse_language_group(content) {
            push_unique(&mut self.languages, codes);
        }
    }
}

impl GameKind {
    /// Every kind, in contract order.
    const ALL: [GameKind; 23] = [
        GameKind::Bios,
        GameKind::Device,
        GameKind::Unlicensed,
        GameKind::Debug,
        GameKind::Demo,
        GameKind::Beta,
        GameKind::Sample,
        GameKind::Prototype,
        GameKind::Program,
        GameKind::Aftermarket,
        GameKind::Homebrew,
        GameKind::Alpha,
        GameKind::Bootleg,
        GameKind::Cracked,
        GameKind::Fixed,
        GameKind::Hacked,
        GameKind::Overdump,
        GameKind::PendingDump,
        GameKind::Pirated,
        GameKind::Trained,
        GameKind::Translated,
        GameKind::Bad,
        GameKind::Unverified,
    ];

    /// Parses a kind name (case-insensitive); `unverified` is accepted even
    /// though it is never stored in [`GameTags::kinds`].
    pub fn parse(s: &str) -> Option<Self> {
        let lower = s.to_lowercase();
        Self::ALL.into_iter().find(|kind| kind.name() == lower)
    }

    /// Lowercase name matching the CLI type list.
    pub fn name(self) -> &'static str {
        match self {
            GameKind::Bios => "bios",
            GameKind::Device => "device",
            GameKind::Unlicensed => "unlicensed",
            GameKind::Debug => "debug",
            GameKind::Demo => "demo",
            GameKind::Beta => "beta",
            GameKind::Sample => "sample",
            GameKind::Prototype => "prototype",
            GameKind::Program => "program",
            GameKind::Aftermarket => "aftermarket",
            GameKind::Homebrew => "homebrew",
            GameKind::Alpha => "alpha",
            GameKind::Bootleg => "bootleg",
            GameKind::Cracked => "cracked",
            GameKind::Fixed => "fixed",
            GameKind::Hacked => "hacked",
            GameKind::Overdump => "overdump",
            GameKind::PendingDump => "pendingdump",
            GameKind::Pirated => "pirated",
            GameKind::Trained => "trained",
            GameKind::Translated => "translated",
            GameKind::Bad => "bad",
            GameKind::Unverified => "unverified",
        }
    }
}

impl Revision {
    /// Compares revisions of the same variant numerically/lexically, older
    /// first; incomparable variants order equal.
    pub fn cmp_age(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Revision::Number(a), Revision::Number(b)) => a.cmp(b),
            (Revision::Letter(a), Revision::Letter(b)) => a.cmp(b),
            (Revision::Version(a), Revision::Version(b)) => a.cmp(b),
            _ => Ordering::Equal,
        }
    }
}

static PAREN_GROUP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\([^)]*\)").expect("valid regex"));
static GROUP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\([^)]*\)|\[[^\]]*\]").expect("valid regex"));
static REV_NUMBER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^rev\s+(\d+)$").expect("valid regex"));
static REV_LETTER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^rev\s+([a-z])$").expect("valid regex"));
static VERSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:v|version\s+)(\d+(?:\.\d+)*)$").expect("valid regex"));
static DISC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:disc|disk|cd)\s+(\d+)(?:\s+of\s+\d+)?$").expect("valid regex")
});
static DEMO_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\(demo[^)]*\)|@barai|\(kiosk[^)]*\)|\(preview\)|gamecube preview|kiosk demo disc|ps2 kiosk|psp system kiosk|taikenban|trial edition",
    )
    .expect("valid regex")
});
static BETA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\(beta[^)]*\)").expect("valid regex"));
static SAMPLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\(sample[^)]*\)").expect("valid regex"));
static PROTOTYPE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\(proto[^)]*\)").expect("valid regex"));
static ALPHA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\(alpha[^)]*\)").expect("valid regex"));
static PROGRAM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\([a-z0-9. ]*program\)").expect("valid regex"));
static CRACKED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\[cr( [^\]]*)?\]").expect("valid regex"));
static FIXED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[fF]\d*\]").expect("valid regex"));
static HACKED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\(hack\)|\[[h][^\]]*\]").expect("valid regex"));
static OVERDUMP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[oO]\d*\]").expect("valid regex"));
static PENDING_DUMP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[![pP]\]").expect("valid regex"));
static PIRATED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\(pirate\)|\[[p]\d*\]").expect("valid regex"));
static TRAINED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[tT]\d*\]").expect("valid regex"));
static TRANSLATED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[tT][+-][^\]]*\]").expect("valid regex"));
static BAD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[[bB]\d*\]").expect("valid regex"));

/// Detects release kinds from the lowercased name.
fn detect_kinds(lower: &str) -> BTreeSet<GameKind> {
    let mut kinds = BTreeSet::new();
    let mut flag = |present: bool, kind: GameKind| {
        if present {
            kinds.insert(kind);
        }
    };
    flag(
        lower.contains("[bios]") || lower.contains("(bios)"),
        GameKind::Bios,
    );
    flag(
        lower.contains("(unl)") || lower.contains("(unlicensed)"),
        GameKind::Unlicensed,
    );
    flag(lower.contains("(debug)"), GameKind::Debug);
    flag(DEMO_RE.is_match(lower), GameKind::Demo);
    flag(BETA_RE.is_match(lower), GameKind::Beta);
    flag(SAMPLE_RE.is_match(lower), GameKind::Sample);
    flag(PROTOTYPE_RE.is_match(lower), GameKind::Prototype);
    flag(
        PROGRAM_RE.is_match(lower)
            || lower.contains("check program")
            || lower.contains("sample program"),
        GameKind::Program,
    );
    flag(lower.contains("(aftermarket)"), GameKind::Aftermarket);
    flag(lower.contains("(homebrew)"), GameKind::Homebrew);
    flag(ALPHA_RE.is_match(lower), GameKind::Alpha);
    flag(lower.contains("(bootleg)"), GameKind::Bootleg);
    flag(CRACKED_RE.is_match(lower), GameKind::Cracked);
    flag(FIXED_RE.is_match(lower), GameKind::Fixed);
    flag(HACKED_RE.is_match(lower), GameKind::Hacked);
    flag(OVERDUMP_RE.is_match(lower), GameKind::Overdump);
    flag(PENDING_DUMP_RE.is_match(lower), GameKind::PendingDump);
    flag(PIRATED_RE.is_match(lower), GameKind::Pirated);
    flag(TRAINED_RE.is_match(lower), GameKind::Trained);
    flag(TRANSLATED_RE.is_match(lower), GameKind::Translated);
    // In the classic convention `[c]`/`[x]` mark a bad dump only without
    // the `[!]` verified marker.
    flag(
        BAD_RE.is_match(lower)
            || ((lower.contains("[c]") || lower.contains("[x]")) && !lower.contains("[!]")),
        GameKind::Bad,
    );
    kinds
}

/// `(Rev 1)`/`(Rev A)`/`(v1.2.3)`/`(Version 1.02)` revision tags.
fn parse_revision(content: &str) -> Option<Revision> {
    if let Some(caps) = REV_NUMBER_RE.captures(content) {
        return Some(Revision::Number(caps[1].parse().ok()?));
    }
    if let Some(caps) = REV_LETTER_RE.captures(content) {
        return Some(Revision::Letter(
            caps[1].chars().next()?.to_ascii_uppercase(),
        ));
    }
    let caps = VERSION_RE.captures(content)?;
    let parts = caps[1]
        .split('.')
        .map(|part| part.parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()?;
    Some(Revision::Version(parts))
}

/// `(Disc 2)`/`(Disc 2 of 3)`/`(Disk 2)`/`(CD 2)` tags; `(Side B)` is not one.
fn parse_disc(content: &str) -> Option<u32> {
    DISC_RE
        .captures(content)
        .and_then(|caps| caps[1].parse().ok())
}

/// Region tokens (lowercase) and the region codes each expands to: the one
/// mapping both the name parser and the code validator read.
const REGION_TOKENS: &[(&str, &[&str])] = &[
    ("usa", &["USA"]),
    ("europe", &["EUR"]),
    ("japan", &["JPN"]),
    ("world", &["WORLD"]),
    ("asia", &["ASI"]),
    ("australia", &["AUS"]),
    ("brazil", &["BRA"]),
    ("canada", &["CAN"]),
    ("china", &["CHN"]),
    ("denmark", &["DAN"]),
    ("france", &["FRA"]),
    ("finland", &["FYN"]),
    ("germany", &["GER"]),
    ("greece", &["GRE"]),
    ("hong kong", &["HK"]),
    ("netherlands", &["HOL"]),
    ("holland", &["HOL"]),
    ("italy", &["ITA"]),
    ("korea", &["KOR"]),
    ("mexico", &["MEX"]),
    ("norway", &["NOR"]),
    ("new zealand", &["NZ"]),
    ("portugal", &["POR"]),
    ("russia", &["RUS"]),
    ("spain", &["SPA"]),
    ("sweden", &["SWE"]),
    ("taiwan", &["TAI"]),
    ("uk", &["UK"]),
    ("united kingdom", &["UK"]),
    ("argentina", &["ARG"]),
    ("belgium", &["BEL"]),
    ("unknown", &["UNK"]),
    ("u", &["USA"]),
    ("e", &["EUR"]),
    ("j", &["JPN"]),
    ("w", &["WORLD"]),
    ("ju", &["JPN", "USA"]),
    ("ue", &["USA", "EUR"]),
    ("jue", &["JPN", "USA", "EUR"]),
    ("unk", &["UNK"]),
];

/// Maps a comma-separated region group to our region codes, or `None` when
/// any token is not a region.
fn parse_region_group(content: &str) -> Option<Vec<&'static str>> {
    let mut codes = Vec::new();
    for token in content.split(',') {
        let token = token.trim().to_lowercase();
        let (_, matched) = REGION_TOKENS.iter().find(|(name, _)| *name == token)?;
        codes.extend_from_slice(matched);
    }
    Some(codes)
}

/// True when `code` is a region code the parser can emit, case-insensitively.
pub fn is_region_code(code: &str) -> bool {
    let code = code.to_ascii_uppercase();
    REGION_TOKENS
        .iter()
        .any(|(_, codes)| codes.contains(&code.as_str()))
}

/// Language tokens (lowercase) and the uppercase code each maps to: the
/// one mapping both the name parser and the code validator read.
const LANGUAGES: [(&str, &str); 16] = [
    ("da", "DA"),
    ("de", "DE"),
    ("el", "EL"),
    ("en", "EN"),
    ("es", "ES"),
    ("fi", "FI"),
    ("fr", "FR"),
    ("it", "IT"),
    ("ja", "JA"),
    ("ko", "KO"),
    ("nl", "NL"),
    ("no", "NO"),
    ("pt", "PT"),
    ("ru", "RU"),
    ("sv", "SV"),
    ("zh", "ZH"),
];

/// The uppercase code of one language token, case-insensitively.
fn language_code(token: &str) -> Option<&'static str> {
    let token = token.trim().to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|(name, _)| *name == token)
        .map(|(_, code)| *code)
}

/// True when `code` names a supported language, case-insensitively.
pub fn is_language_code(code: &str) -> bool {
    language_code(code).is_some()
}

/// Maps a `En,Fr,De`/`En+Fr` group to uppercase language codes, or `None`
/// when any token is not a supported language.
fn parse_language_group(content: &str) -> Option<Vec<&'static str>> {
    content.split([',', '+']).map(language_code).collect()
}

/// The primary language fallback for a region code.
fn region_primary_language(region: &str) -> Option<&'static str> {
    match region {
        "USA" | "UK" | "AUS" | "CAN" | "NZ" | "WORLD" | "EUR" => Some("EN"),
        "JPN" => Some("JA"),
        "FRA" | "BEL" => Some("FR"),
        "GER" => Some("DE"),
        "ITA" => Some("IT"),
        "SPA" | "MEX" | "ARG" => Some("ES"),
        "POR" | "BRA" => Some("PT"),
        "HOL" => Some("NL"),
        "DAN" => Some("DA"),
        "FYN" => Some("FI"),
        "NOR" => Some("NO"),
        "SWE" => Some("SV"),
        "RUS" => Some("RU"),
        "KOR" => Some("KO"),
        "CHN" | "TAI" | "HK" => Some("ZH"),
        "GRE" => Some("EL"),
        _ => None,
    }
}

/// Appends codes not already present, preserving first-seen order.
fn push_unique(vec: &mut Vec<String>, codes: Vec<&'static str>) {
    for code in codes {
        if !vec.iter().any(|existing| existing == code) {
            vec.push(code.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_region_revision_and_retail() {
        let tags = GameTags::parse("Super Mario World (USA, Europe) (Rev 1)");
        assert_eq!(tags.regions, vec!["USA", "EUR"]);
        assert_eq!(tags.revision, Some(Revision::Number(1)));
        assert!(tags.is_retail());
        assert_eq!(tags.type_label(), "Retail");
    }

    #[test]
    fn parses_beta_bad_and_languages() {
        let tags = GameTags::parse("Game (Japan) (En,Ja) (Beta 2) [b]");
        assert_eq!(tags.regions, vec!["JPN"]);
        assert_eq!(tags.languages, vec!["EN", "JA"]);
        assert_eq!(tags.kinds, BTreeSet::from([GameKind::Beta, GameKind::Bad]));
        assert!(!tags.is_retail());
        assert_eq!(tags.type_label(), "Bad");
    }

    #[test]
    fn parses_bios_and_version() {
        let tags = GameTags::parse("[BIOS] PlayStation (USA) (v2.2)");
        assert!(tags.kinds.contains(&GameKind::Bios));
        assert_eq!(tags.revision, Some(Revision::Version(vec![2, 2])));
        assert_eq!(tags.type_label(), "BIOS");
        assert!(!tags.is_retail());
    }

    #[test]
    fn parses_disc_tags() {
        assert_eq!(
            GameTags::parse("Final Fantasy VII (USA) (Disc 2)").disc,
            Some(2)
        );
        assert_eq!(GameTags::parse("Game (Disc 2 of 3)").disc, Some(2));
        assert_eq!(GameTags::parse("Game (Disk 4)").disc, Some(4));
        assert_eq!(GameTags::parse("Game (CD 2)").disc, Some(2));
        assert_eq!(GameTags::parse("Game (Side B)").disc, None);
    }

    #[test]
    fn unlicensed_still_retail_with_language_fallback() {
        let tags = GameTags::parse("Game (Europe) (Unl)");
        assert!(tags.is_retail());
        assert_eq!(tags.type_label(), "Unlicensed");
        assert_eq!(tags.primary_language(), Some("EN"));
    }

    /// A bootleg is non-retail and labels as Bootleg, not Retail.
    #[test]
    fn bootleg_labels_as_bootleg() {
        let tags = GameTags::parse("Game (Asia) (Bootleg)");
        assert_eq!(tags.type_label(), "Bootleg");
        assert!(!tags.is_retail());
    }

    #[test]
    fn german_language_fallback() {
        assert_eq!(
            GameTags::parse("Game (Germany)").primary_language(),
            Some("DE")
        );
    }

    #[test]
    fn single_letter_region_code_and_verified_marker() {
        let tags = GameTags::parse("Game (U) [!]");
        assert_eq!(tags.regions, vec!["USA"]);
        assert!(tags.verified);
    }

    #[test]
    fn single_letter_codes_are_exact_groups() {
        assert_eq!(
            GameTags::parse("Game (JUE)").regions,
            vec!["JPN", "USA", "EUR"]
        );
        assert_eq!(GameTags::parse("Game (UE)").regions, vec!["USA", "EUR"]);
        assert_eq!(GameTags::parse("Game (Unk)").regions, vec!["UNK"]);
        // No-Intro side/version letters are not region codes.
        assert!(GameTags::parse("Game (C)").regions.is_empty());
        assert!(GameTags::parse("Game (F)").regions.is_empty());
        assert!(GameTags::parse("Game (G)").regions.is_empty());
        assert!(GameTags::parse("Game (I)").regions.is_empty());
        assert!(GameTags::parse("Game (S)").regions.is_empty());
        assert!(GameTags::parse("Game (K)").regions.is_empty());
        assert!(GameTags::parse("Game (A)").regions.is_empty());
        assert!(GameTags::parse("Game (B)").regions.is_empty());
    }

    #[test]
    fn bad_dump_markers_require_unverified_name() {
        assert!(GameTags::parse("Game [b]").kinds.contains(&GameKind::Bad));
        assert!(GameTags::parse("Game [c]").kinds.contains(&GameKind::Bad));
        assert!(GameTags::parse("Game [x]").kinds.contains(&GameKind::Bad));
        // The `[!]` verified marker overrides `[c]`/`[x]`.
        assert!(
            !GameTags::parse("Game [x] [!]")
                .kinds
                .contains(&GameKind::Bad)
        );
        assert!(
            !GameTags::parse("Game [c] [!]")
                .kinds
                .contains(&GameKind::Bad)
        );
    }

    #[test]
    fn normalized_titles_match_across_tagged_variants() {
        assert_eq!(
            GameTags::normalized_title("Super Mario World (USA) (Rev 1)"),
            GameTags::normalized_title("super mario   world")
        );
        assert_eq!(GameTags::normalized_title("Game [!]"), "game");
    }

    #[test]
    fn revision_age_orders_older_first() {
        use Ordering::*;
        assert_eq!(Revision::Number(1).cmp_age(&Revision::Number(2)), Less);
        assert_eq!(Revision::Letter('A').cmp_age(&Revision::Letter('B')), Less);
        assert_eq!(
            Revision::Version(vec![1, 9]).cmp_age(&Revision::Version(vec![1, 10])),
            Less
        );
        assert_eq!(Revision::Number(1).cmp_age(&Revision::Letter('A')), Equal);
    }

    /// The code validators agree with what the name parser emits: every
    /// code a parsed name carries validates, and codes the parser cannot
    /// produce do not.
    #[test]
    fn code_validators_accept_exactly_what_parsing_emits() {
        let tags = GameTags::parse("Game (Japan, USA, Europe) (En,Fr,Ja)");
        for region in &tags.regions {
            assert!(is_region_code(region), "{region}");
        }
        for language in &tags.languages {
            assert!(is_language_code(language), "{language}");
        }
        assert!(is_region_code("hk"));
        assert!(is_language_code("Zh"));
        assert!(!is_region_code("US"));
        assert!(!is_region_code("JU"));
        assert!(!is_language_code("en,fr"));
        assert!(!is_language_code("zz"));
    }

    /// Every kind's name parses back to the same kind.
    #[test]
    fn kind_names_round_trip() {
        assert_eq!(GameKind::parse("unverified"), Some(GameKind::Unverified));
        assert_eq!(GameKind::parse("BIOS"), Some(GameKind::Bios));
        assert_eq!(GameKind::PendingDump.name(), "pendingdump");
        for kind in GameKind::ALL {
            assert_eq!(GameKind::parse(kind.name()), Some(kind));
        }
    }
}
