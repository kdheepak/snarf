use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};

pub fn ua_headers(value: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(value));
    headers
}
