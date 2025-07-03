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

pub fn get_filename_for_current_target_triple() -> anyhow::Result<String> {
    match built_info::TARGET {
        "x86_64-unknown-freebsd" => Ok("rom-converto-freebsd-x64".to_string()),
        "x86_64-unknown-linux-gnu" => Ok("rom-converto-linux-x64".to_string()),
        "x86_64-unknown-linux-musl" => Ok("rom-converto-linux-x64-musl".to_string()),
        "aarch64-unknown-linux-gnu" => Ok("rom-converto-linux-arm64".to_string()),
        "armv7-unknown-linux-gnueabihf" => Ok("rom-converto-linux-arm7".to_string()),
        "x86_64-pc-windows-msvc" => Ok("rom-converto-windows-x64.exe".to_string()),
        "x86_64-apple-darwin" => Ok("rom-converto-macos-x64".to_string()),
        "aarch64-apple-darwin" => Ok("rom-converto-macos-arm64".to_string()),
        _ => Err(NoPrebuildFoundError.into()),
    }
}

pub fn compare_latest_release_to_current_version(
    latest: &ReleaseVersion,
    current: &ReleaseVersion,
) -> ReleaseVersionCompareResult {
    if latest.major > current.major {
        return ReleaseVersionCompareResult::OutdatedMajor;
    }

    if latest.minor > current.minor && latest.major == current.major {
        return ReleaseVersionCompareResult::OutdatedMinor;
    }

    if latest.patch > current.patch
        && latest.minor == current.minor
        && latest.major == current.major
    {
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
        // latest has both minor and patch higher, but we should get OutdatedMinor
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
}
