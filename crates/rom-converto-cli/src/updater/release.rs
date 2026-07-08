use crate::built_info;
use crate::updater::error::UpdaterError::NoPrebuildFoundError;
use std::fmt::Display;

#[derive(Debug)]
pub struct ReleaseVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Display for ReleaseVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum ReleaseVersionCompareResult {
    OutdatedMajor,
    OutdatedMinor,
    OutdatedPatch,
    EqualOrNewer,
}

pub fn get_current_release_version() -> ReleaseVersion {
    ReleaseVersion {
        major: built_info::PKG_VERSION_MAJOR.parse().unwrap(),
        minor: built_info::PKG_VERSION_MINOR.parse().unwrap(),
        patch: built_info::PKG_VERSION_PATCH.parse().unwrap(),
    }
}

#[derive(Debug, Clone)]
pub struct ReleaseAssetQuery {
    pub expected_name: String,
    token_groups: Vec<Vec<&'static str>>,
    forbidden_tokens: Vec<&'static str>,
    required_suffix: Option<&'static str>,
}

impl ReleaseAssetQuery {
    fn new(
        expected_name: &str,
        token_groups: Vec<Vec<&'static str>>,
        forbidden_tokens: Vec<&'static str>,
        required_suffix: Option<&'static str>,
    ) -> Self {
        Self {
            expected_name: expected_name.to_string(),
            token_groups,
            forbidden_tokens,
            required_suffix,
        }
    }
}

pub fn get_release_asset_query_for_current_target() -> anyhow::Result<ReleaseAssetQuery> {
    get_release_asset_query_for_target(built_info::TARGET)
}

pub fn get_release_asset_query_for_target(target: &str) -> anyhow::Result<ReleaseAssetQuery> {
    match target {
        "x86_64-unknown-freebsd" => Ok(ReleaseAssetQuery::new(
            "rom-converto-cli-freebsd-x64",
            vec![vec!["freebsd"], vec!["x64", "x86_64", "x86-64", "amd64"]],
            vec!["gui"],
            None,
        )),
        "x86_64-unknown-linux-gnu" => Ok(ReleaseAssetQuery::new(
            "rom-converto-cli-linux-x64",
            vec![vec!["linux"], vec!["x64", "x86_64", "x86-64", "amd64"]],
            vec!["gui", "musl"],
            None,
        )),
        "x86_64-unknown-linux-musl" => Ok(ReleaseAssetQuery::new(
            "rom-converto-cli-linux-x64-musl",
            vec![
                vec!["linux"],
                vec!["x64", "x86_64", "x86-64", "amd64"],
                vec!["musl"],
            ],
            vec!["gui"],
            None,
        )),
        "aarch64-unknown-linux-gnu" => Ok(ReleaseAssetQuery::new(
            "rom-converto-cli-linux-arm64",
            vec![vec!["linux"], vec!["arm64", "aarch64"]],
            vec!["gui", "musl"],
            None,
        )),
        "armv7-unknown-linux-gnueabihf" => Ok(ReleaseAssetQuery::new(
            "rom-converto-cli-linux-arm7",
            vec![vec!["linux"], vec!["arm7", "armv7"]],
            vec!["gui", "musl"],
            None,
        )),
        "x86_64-pc-windows-msvc" => Ok(ReleaseAssetQuery::new(
            "rom-converto-cli-windows-x64.exe",
            vec![
                vec!["windows", "win"],
                vec!["x64", "x86_64", "x86-64", "amd64"],
            ],
            vec!["gui", "setup"],
            Some(".exe"),
        )),
        "x86_64-apple-darwin" => Ok(ReleaseAssetQuery::new(
            "rom-converto-cli-macos-x64",
            vec![
                vec!["macos", "mac", "darwin", "apple"],
                vec!["x64", "x86_64", "x86-64", "amd64"],
            ],
            vec!["gui"],
            None,
        )),
        "aarch64-apple-darwin" => Ok(ReleaseAssetQuery::new(
            "rom-converto-cli-macos-arm64",
            vec![
                vec!["macos", "mac", "darwin", "apple"],
                vec!["arm64", "aarch64"],
            ],
            vec!["gui"],
            None,
        )),
        _ => Err(NoPrebuildFoundError.into()),
    }
}

pub fn select_release_asset_name<'a, I>(
    asset_names: I,
    query: &ReleaseAssetQuery,
) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    asset_names
        .into_iter()
        .filter_map(|asset_name| {
            score_release_asset_name(asset_name, query).map(|score| (score, asset_name))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, asset_name)| asset_name)
}

fn score_release_asset_name(asset_name: &str, query: &ReleaseAssetQuery) -> Option<u32> {
    if asset_name.eq_ignore_ascii_case(&query.expected_name) {
        return Some(10_000);
    }

    let normalized_name = normalize_name(asset_name);

    if !normalized_name.contains("romconverto") {
        return None;
    }

    if is_packaged_or_sidecar_asset(asset_name) {
        return None;
    }

    if let Some(required_suffix) = query.required_suffix
        && !asset_name
            .to_ascii_lowercase()
            .ends_with(&required_suffix.to_ascii_lowercase())
    {
        return None;
    }

    if query
        .forbidden_tokens
        .iter()
        .any(|token| normalized_name.contains(&normalize_name(token)))
    {
        return None;
    }

    if !query.token_groups.iter().all(|group| {
        group
            .iter()
            .any(|token| normalized_name.contains(&normalize_name(token)))
    }) {
        return None;
    }

    let mut score = 100;

    if normalized_name.contains("cli") {
        score += 25;
    }

    if normalized_name.starts_with("romconverto") {
        score += 10;
    }

    let expected_normalized = normalize_name(&query.expected_name);
    let matching_prefix = normalized_name
        .chars()
        .zip(expected_normalized.chars())
        .take_while(|(left, right)| left == right)
        .count() as u32;

    Some(score + matching_prefix)
}

fn normalize_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn is_packaged_or_sidecar_asset(asset_name: &str) -> bool {
    let lower_name = asset_name.to_ascii_lowercase();

    [
        ".appimage",
        ".app.zip",
        ".asc",
        ".deb",
        ".dmg",
        ".msi",
        ".pkg",
        ".rpm",
        ".sha256",
        ".sig",
        ".tar.gz",
        ".tgz",
        ".zip",
    ]
    .iter()
    .any(|suffix| lower_name.ends_with(suffix))
}

pub fn compare_latest_release_to_current_version(
    latest: &ReleaseVersion,
    current: &ReleaseVersion,
) -> ReleaseVersionCompareResult {
    if latest.major > current.major {
        return ReleaseVersionCompareResult::OutdatedMajor;
    }

    if latest.major < current.major {
        return ReleaseVersionCompareResult::EqualOrNewer;
    }

    // Same major version from here
    if latest.minor > current.minor {
        return ReleaseVersionCompareResult::OutdatedMinor;
    }

    if latest.minor < current.minor {
        return ReleaseVersionCompareResult::EqualOrNewer;
    }

    // Same major and minor version from here
    if latest.patch > current.patch {
        return ReleaseVersionCompareResult::OutdatedPatch;
    }

    ReleaseVersionCompareResult::EqualOrNewer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u64, minor: u64, patch: u64) -> ReleaseVersion {
        ReleaseVersion {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn outdated_major_when_latest_major_is_higher() {
        let current = v(1, 9, 9);
        let latest = v(2, 0, 0);
        assert_eq!(
            compare_latest_release_to_current_version(&latest, &current),
            ReleaseVersionCompareResult::OutdatedMajor
        );
    }

    #[test]
    fn equal_or_newer_when_latest_major_is_lower() {
        let current = v(2, 0, 0);
        let latest = v(1, 9, 9);
        assert_eq!(
            compare_latest_release_to_current_version(&latest, &current),
            ReleaseVersionCompareResult::EqualOrNewer
        );
    }

    #[test]
    fn outdated_minor_when_same_major_and_latest_minor_is_higher() {
        let current = v(1, 2, 3);
        let latest = v(1, 3, 0);
        assert_eq!(
            compare_latest_release_to_current_version(&latest, &current),
            ReleaseVersionCompareResult::OutdatedMinor
        );
    }

    #[test]
    fn equal_or_newer_when_same_major_and_latest_minor_is_lower() {
        let current = v(1, 3, 0);
        let latest = v(1, 2, 9);
        assert_eq!(
            compare_latest_release_to_current_version(&latest, &current),
            ReleaseVersionCompareResult::EqualOrNewer
        );
    }

    #[test]
    fn outdated_patch_when_same_major_minor_and_latest_patch_is_higher() {
        let current = v(1, 2, 3);
        let latest = v(1, 2, 4);
        assert_eq!(
            compare_latest_release_to_current_version(&latest, &current),
            ReleaseVersionCompareResult::OutdatedPatch
        );
    }

    #[test]
    fn equal_or_newer_when_same_major_minor_and_latest_patch_is_lower() {
        let current = v(1, 2, 4);
        let latest = v(1, 2, 3);
        assert_eq!(
            compare_latest_release_to_current_version(&latest, &current),
            ReleaseVersionCompareResult::EqualOrNewer
        );
    }

    #[test]
    fn equal_or_newer_when_versions_are_exactly_equal() {
        let current = v(1, 2, 3);
        let latest = v(1, 2, 3);
        assert_eq!(
            compare_latest_release_to_current_version(&latest, &current),
            ReleaseVersionCompareResult::EqualOrNewer
        );
    }

    #[test]
    fn minor_branch_takes_precedence_over_patch() {
        // latest has both minor and patch higher, but this should report OutdatedMinor
        let current = v(1, 2, 3);
        let latest = v(1, 3, 4);
        assert_eq!(
            compare_latest_release_to_current_version(&latest, &current),
            ReleaseVersionCompareResult::OutdatedMinor
        );
    }

    #[test]
    fn major_branch_takes_precedence_over_everything_else() {
        // even if minor/patch are lower, OutdatedMajor should win
        let current = v(1, 9, 9);
        let latest = v(2, 0, 0);
        // sanity check duplicate of the first test
        assert_eq!(
            compare_latest_release_to_current_version(&latest, &current),
            ReleaseVersionCompareResult::OutdatedMajor
        );
    }

    #[test]
    fn works_at_u64_boundaries() {
        let max = u64::MAX;
        // equal at max
        assert_eq!(
            compare_latest_release_to_current_version(&v(max, max, max), &v(max, max, max)),
            ReleaseVersionCompareResult::EqualOrNewer
        );
        // current at max, latest one less
        assert_eq!(
            compare_latest_release_to_current_version(&v(max, max, max - 1), &v(max, max, max)),
            ReleaseVersionCompareResult::EqualOrNewer
        );
    }

    #[test]
    fn target_query_uses_current_cli_release_names() {
        assert_eq!(
            get_release_asset_query_for_target("aarch64-apple-darwin")
                .unwrap()
                .expected_name,
            "rom-converto-cli-macos-arm64"
        );
        assert_eq!(
            get_release_asset_query_for_target("x86_64-pc-windows-msvc")
                .unwrap()
                .expected_name,
            "rom-converto-cli-windows-x64.exe"
        );
    }

    #[test]
    fn asset_selector_prefers_exact_current_name() {
        let query = get_release_asset_query_for_target("x86_64-unknown-linux-gnu").unwrap();
        let assets = [
            "rom-converto-linux-x64",
            "rom-converto-cli-linux-x64",
            "rom-converto-gui-linux-x64.AppImage",
        ];

        assert_eq!(
            select_release_asset_name(assets, &query),
            Some("rom-converto-cli-linux-x64")
        );
    }

    #[test]
    fn asset_selector_accepts_legacy_cli_name_without_cli_token() {
        let query = get_release_asset_query_for_target("x86_64-apple-darwin").unwrap();
        let assets = ["rom-converto-macos-x64"];

        assert_eq!(
            select_release_asset_name(assets, &query),
            Some("rom-converto-macos-x64")
        );
    }

    #[test]
    fn asset_selector_accepts_minor_separator_and_arch_name_changes() {
        let query = get_release_asset_query_for_target("aarch64-apple-darwin").unwrap();
        let assets = ["rom_converto_cli_darwin_aarch64"];

        assert_eq!(
            select_release_asset_name(assets, &query),
            Some("rom_converto_cli_darwin_aarch64")
        );
    }

    #[test]
    fn asset_selector_ignores_gui_bundles_and_installers() {
        let query = get_release_asset_query_for_target("aarch64-apple-darwin").unwrap();
        let assets = [
            "rom-converto-gui-macos-arm64.dmg",
            "rom-converto-gui-macos-arm64.app.zip",
            "rom-converto-cli-macos-arm64",
        ];

        assert_eq!(
            select_release_asset_name(assets, &query),
            Some("rom-converto-cli-macos-arm64")
        );
    }

    #[test]
    fn asset_selector_does_not_pick_musl_for_linux_gnu() {
        let query = get_release_asset_query_for_target("x86_64-unknown-linux-gnu").unwrap();
        let assets = ["rom-converto-cli-linux-x64-musl"];

        assert_eq!(select_release_asset_name(assets, &query), None);
    }

    #[test]
    fn asset_selector_requires_exe_for_windows() {
        let query = get_release_asset_query_for_target("x86_64-pc-windows-msvc").unwrap();
        let assets = ["rom-converto-cli-windows-x64"];

        assert_eq!(select_release_asset_name(assets, &query), None);
    }

    #[test]
    fn asset_selector_does_not_confuse_arm64_and_armv7() {
        let query = get_release_asset_query_for_target("armv7-unknown-linux-gnueabihf").unwrap();
        let assets = ["rom-converto-cli-linux-arm64"];

        assert_eq!(select_release_asset_name(assets, &query), None);
    }
}
