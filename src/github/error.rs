use reqwest::StatusCode;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum GithubError {
    #[error("Github API returned {0} with error message: {1}")]
    NoSuccessStatusCode(StatusCode, String),

    #[error("Couldn't parse the release version tag: {0}")]
    CannotParseReleaseVersion(String),

    #[error("No asset with name {0} found in the release.")]
    NoAssetFound(String),
}
