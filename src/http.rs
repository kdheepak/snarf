use std::time::Duration;

use color_eyre::eyre;

use crate::headers;

pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
pub const CODE_SEARCH_TIMEOUT: Duration = Duration::from_secs(120);
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
pub const BROWSER_TIMEOUT: Duration = FETCH_TIMEOUT;
pub const LLMS_TXT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
pub const TEST_TIMEOUT: Duration = Duration::from_secs(5);

const BROWSER_UA: &str = "Mozilla/5.0 (compatible; snarf/1.0)";
const SNARF_UA: &str = "snarf";

pub fn client(timeout: Duration) -> eyre::Result<reqwest::Client> {
    build(timeout, BROWSER_UA)
}

pub fn snarf_client(timeout: Duration) -> eyre::Result<reqwest::Client> {
    build(timeout, SNARF_UA)
}

fn build(timeout: Duration, user_agent: &'static str) -> eyre::Result<reqwest::Client> {
    let builder = reqwest::Client::builder()
        .timeout(timeout)
        .default_headers(headers::ua_headers(user_agent));
    #[cfg(test)]
    let builder = builder.no_proxy();

    Ok(builder.build()?)
}
