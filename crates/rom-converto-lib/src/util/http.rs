//! Shared HTTP client for outbound requests (DAT downloads, update checks).

use std::sync::LazyLock;
use std::time::Duration;

// CARGO_PKG_NAME here would expand to "rom-converto-lib" and the workspace
// sets no `homepage`, so the product name is spelled out and no homepage
// segment is emitted.
pub const USER_AGENT: &str = concat!("rom-converto/", env!("CARGO_PKG_VERSION"));

/// Shared `reqwest` client: the crate's user agent, a 30s request timeout,
/// and a 10s connect timeout.
pub static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .expect("default reqwest client")
});
