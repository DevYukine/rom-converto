use crate::github::error::GithubError;
use crate::github::model::GithubReleaseResponse;
use crate::updater::release::ReleaseVersion;
use crate::util::http::{CLIENT, USER_AGENT};
use bytes::Bytes;
use futures::Stream;
use lazy_static::lazy_static;
use reqwest::{Client, Method};
use std::time::Duration;
use tower::limit::RateLimit;
use tower::{Service, ServiceBuilder, ServiceExt};

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

    pub async fn get_latest_release_file_by_name(
        &mut self,
        user: &str,
        repo: &str,
        file_name: &str,
    ) -> anyhow::Result<impl Stream<Item = reqwest::Result<Bytes>>> {
        let response = self.get_latest_release(user, repo).await?;

        let asset = response.assets.iter().find(|asset| asset.name == file_name);

        let asset = match asset {
            Some(asset) => asset,
            None => {
                return Err(GithubError::NoAssetFound(file_name.to_string()).into());
            }
        };

        let req = self
            .client
            .request(Method::GET, asset.browser_download_url.clone())
            .headers(self.headers.clone())
            .build()?;

        let res = self.service.ready().await?.call(req).await?;

        if !res.status().is_success() {
            return Err(GithubError::NoSuccessStatusCode(res.status(), res.text().await?).into());
        }

        Ok(res.bytes_stream())
    }

    pub async fn get_latest_release_version(
        &mut self,
        user: &str,
        repo: &str,
    ) -> anyhow::Result<ReleaseVersion> {
        let response = self.get_latest_release(user, repo).await?;

        lazy_static! {
            static ref RE: regex::Regex =
                regex::Regex::new(r#"(?P<major>\d.*)\.(?P<minor>\d.*)\.(?P<patch>\d.*)"#).unwrap();
        }

        let tag_captures = RE.captures(&response.tag_name);

        let tag_captures = match tag_captures {
            Some(captures) => captures,
            None => {
                return Err(GithubError::CannotParseReleaseVersion(response.tag_name).into());
            }
        };

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
        let req = self
            .client
            .request(
                Method::GET,
                format!("https://api.github.com/repos/{user}/{repo}/releases/latest"),
            )
            .headers(self.headers.clone())
            .build()?;

        let res = self.service.ready().await?.call(req).await?;

        if !res.status().is_success() {
            return Err(GithubError::NoSuccessStatusCode(res.status(), res.text().await?).into());
        }

        let parsed = res.json::<GithubReleaseResponse>().await?;

        Ok(parsed)
    }
}
