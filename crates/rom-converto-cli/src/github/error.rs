//! Error type for the GitHub Releases API client.

use reqwest::StatusCode;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum GithubError {
    /// The GitHub API returned a non-success HTTP status.
    #[error("GitHub API returned {0} with error message: {1}")]
    NoSuccessStatusCode(StatusCode, String),

    /// The release's tag name does not match the expected `major.minor.patch` shape.
    #[error("could not parse the release version tag: {0}")]
    CannotParseReleaseVersion(String),

    /// The requested asset name is not attached to the latest release.
    #[error("no asset with name {0} found in the release")]
    NoAssetFound(String),
}
