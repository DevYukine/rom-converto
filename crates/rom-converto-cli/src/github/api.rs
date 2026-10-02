//! Rate-limited client for fetching the latest GitHub release of a repo
//! and downloading a named asset from it.

use crate::github::error::GithubError;
use crate::github::model::GithubReleaseResponse;
use crate::updater::release::{
    ReleaseAssetQuery, ReleaseVersion, parse_sha256_file, select_release_asset_name,
};
use crate::util::http::{CLIENT, USER_AGENT};
use bytes::Bytes;
use futures::Stream;
use lazy_static::lazy_static;
use log::debug;
use reqwest::{Client, Method};
use std::time::Duration;
use tower::limit::RateLimit;
use tower::{Service, ServiceBuilder, ServiceExt};

#[derive(Debug)]
pub struct GithubApi {
    client: Client,
    service: RateLimit<Client>,
    headers: reqwest::header::HeaderMap,
}

impl GithubApi {
    pub fn new() -> anyhow::Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("User-Agent", USER_AGENT.parse()?);

        let client = CLIENT.clone();

        let service = ServiceBuilder::new()
            .rate_limit(2, Duration::from_secs(1))
            .service(client.clone());

        Ok(Self {
            client,
            service,
            headers,
        })
    }

    /// Streams the latest release's asset matching `asset_query`, along with
    /// the SHA-256 its `<asset>.sha256` sibling publishes.
    pub async fn get_latest_release_file_by_asset_query(
        &mut self,
        user: &str,
        repo: &str,
        asset_query: &ReleaseAssetQuery,
    ) -> anyhow::Result<(impl Stream<Item = reqwest::Result<Bytes>>, [u8; 32])> {
        let response = self.get_latest_release(user, repo).await?;

        let asset_name = select_release_asset_name(
            response.assets.iter().map(|asset| asset.name.as_str()),
            asset_query,
        );

        let asset_name = asset_name
            .ok_or_else(|| GithubError::NoAssetFound(asset_query.expected_name.clone()))?;

        let asset = response
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| GithubError::NoAssetFound(asset_query.expected_name.clone()))?;

        debug!(
            "Selected GitHub release asset '{}' for expected asset '{}'",
            asset.name, asset_query.expected_name
        );

        let checksum_name = format!("{}.sha256", asset.name);
        let checksum_asset = response
            .assets
            .iter()
            .find(|candidate| candidate.name == checksum_name)
            .ok_or_else(|| GithubError::NoChecksumFound(checksum_name.clone()))?;
        let checksum_text = self
            .download(&checksum_asset.browser_download_url)
            .await?
            .text()
            .await?;
        let expected_sha256 = parse_sha256_file(&checksum_text, &asset.name)
            .ok_or_else(|| GithubError::NoChecksumFound(checksum_name.clone()))?;

        let res = self.download(&asset.browser_download_url).await?;

        Ok((res.bytes_stream(), expected_sha256))
    }

    async fn download(&mut self, url: &str) -> anyhow::Result<reqwest::Response> {
        let req = self
            .client
            .request(Method::GET, url)
            .headers(self.headers.clone())
            .build()?;

        let res = self.service.ready().await?.call(req).await?;

        if !res.status().is_success() {
            return Err(GithubError::NoSuccessStatusCode(res.status(), res.text().await?).into());
        }

        Ok(res)
    }

    pub async fn get_latest_release_version(
        &mut self,
        user: &str,
        repo: &str,
    ) -> anyhow::Result<ReleaseVersion> {
        let response = self.get_latest_release(user, repo).await?;

        lazy_static! {
            static ref RE: regex::Regex =
                regex::Regex::new(r#"(?P<major>\d+)\.(?P<minor>\d+)\.(?P<patch>\d+)"#)
                    .expect("static release tag pattern");
        }

        let tag_captures = RE
            .captures(&response.tag_name)
            .ok_or_else(|| GithubError::CannotParseReleaseVersion(response.tag_name.clone()))?;

        let major = tag_captures
            .name("major")
            .ok_or_else(|| GithubError::CannotParseReleaseVersion(response.tag_name.clone()))?
            .as_str()
            .parse::<u64>()?;

        let minor = tag_captures
            .name("minor")
            .ok_or_else(|| GithubError::CannotParseReleaseVersion(response.tag_name.clone()))?
            .as_str()
            .parse::<u64>()?;

        let patch = tag_captures
            .name("patch")
            .ok_or_else(|| GithubError::CannotParseReleaseVersion(response.tag_name.clone()))?
            .as_str()
            .parse::<u64>()?;

        Ok(ReleaseVersion {
            major,
            minor,
            patch,
        })
    }

    async fn get_latest_release(
        &mut self,
        user: &str,
        repo: &str,
    ) -> anyhow::Result<GithubReleaseResponse> {
        let res = self
            .download(&format!(
                "https://api.github.com/repos/{user}/{repo}/releases/latest"
            ))
            .await?;

        let parsed = res.json::<GithubReleaseResponse>().await?;

        Ok(parsed)
    }
}
