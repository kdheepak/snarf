use std::time::Duration;

use color_eyre::eyre;
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use scraper::{Html, Selector};
use serde::Deserialize;
use serde_json::json;

use crate::config::SearchBackend;
use crate::error::{AppError, AppResult};
use crate::http;
use crate::types::SearchResult;

pub async fn search(
    backend: SearchBackend,
    query: &str,
    limit: usize,
    searxng_url: &str,
    brave_api_key: &str,
    exa_api_key: &str,
) -> AppResult<Vec<SearchResult>> {
    let client =
        http::client(http::FETCH_TIMEOUT).map_err(|err| AppError::upstream(err.to_string()))?;

    match backend {
        SearchBackend::Brave => brave(&client, query, limit, brave_api_key).await,
        SearchBackend::Searxng => searxng(&client, query, limit, searxng_url)
            .await
            .map_err(AppError::from),
        SearchBackend::Ddg => ddg(&client, query, limit).await.map_err(AppError::from),
        SearchBackend::Exa => exa(&client, query, limit, exa_api_key)
            .await
            .map_err(AppError::from),
    }
}

async fn brave(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
    api_key: &str,
) -> AppResult<Vec<SearchResult>> {
    brave_from_url(
        client,
        "https://api.search.brave.com/res/v1/web/search",
        query,
        limit,
        api_key,
    )
    .await
}

async fn brave_from_url(
    client: &reqwest::Client,
    url: &str,
    query: &str,
    limit: usize,
    api_key: &str,
) -> AppResult<Vec<SearchResult>> {
    if api_key.is_empty() {
        return Err(AppError::precondition(
            "brave: API key not set (get one free at https://brave.com/search/api/ then: snarf config set brave_api_key <key>)",
        ));
    }

    let response = client
        .get(url)
        .query(&[
            ("q", query),
            ("count", &limit.to_string()),
            ("text_decorations", "false"),
            ("result_filter", "web"),
        ])
        .header(ACCEPT, "application/json")
        .header("X-Subscription-Token", api_key)
        .send()
        .await
        .map_err(|err| eyre::eyre!("brave request failed: {err}"))?;

    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(AppError::upstream(
            "brave: invalid API key (set via: snarf config set brave_api_key <key>)",
        ));
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(AppError::upstream("brave: rate limited"));
    }
    if status != reqwest::StatusCode::OK {
        return Err(AppError::upstream(format!(
            "brave returned status {}",
            status.as_u16()
        )));
    }

    let body: BraveResponse = response
        .json()
        .await
        .map_err(|err| eyre::eyre!("failed to decode brave response: {err}"))?;
    Ok(brave_results(body, limit))
}

#[derive(Deserialize)]
struct BraveResponse {
    web: Option<BraveWeb>,
}

#[derive(Deserialize)]
struct BraveWeb {
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    title: String,
    url: String,
    #[serde(default)]
    description: String,
}

fn brave_results(body: BraveResponse, limit: usize) -> Vec<SearchResult> {
    body.web
        .map(|web| web.results)
        .unwrap_or_default()
        .into_iter()
        .take(limit)
        .map(|result| SearchResult {
            title: result.title,
            url: result.url,
            description: result.description,
            ..SearchResult::default()
        })
        .collect()
}

async fn searxng(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
    base_url: &str,
) -> eyre::Result<Vec<SearchResult>> {
    let url = format!("{}/search", base_url.trim_end_matches('/'));
    let response = client
        .get(url)
        .query(&[("q", query), ("format", "json"), ("pageno", "1")])
        .send()
        .await
        .map_err(|err| eyre::eyre!("searxng request failed: {err}"))?;

    let status = response.status();
    if status != reqwest::StatusCode::OK {
        eyre::bail!("searxng returned status {}", status.as_u16());
    }

    let body: SearxngResponse = response
        .json()
        .await
        .map_err(|err| eyre::eyre!("failed to decode searxng response: {err}"))?;
    Ok(searxng_results(body, limit))
}

#[derive(Deserialize)]
struct SearxngResponse {
    results: Vec<SearxngResult>,
}

#[derive(Deserialize)]
struct SearxngResult {
    title: String,
    url: String,
    #[serde(default)]
    content: String,
}

fn searxng_results(body: SearxngResponse, limit: usize) -> Vec<SearchResult> {
    body.results
        .into_iter()
        .take(limit)
        .map(|result| SearchResult {
            title: result.title,
            url: result.url,
            description: result.content,
            ..SearchResult::default()
        })
        .collect()
}

async fn ddg(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> eyre::Result<Vec<SearchResult>> {
    let response = fetch_ddg(client, query).await?;
    let body = response
        .text()
        .await
        .map_err(|err| eyre::eyre!("failed to read ddg response: {err}"))?;
    Ok(parse_ddg_results(&body, limit))
}

fn parse_ddg_results(body: &str, limit: usize) -> Vec<SearchResult> {
    let document = Html::parse_document(body);
    let result_selector = Selector::parse(".result").expect("static selector is valid");
    let title_selector =
        Selector::parse(".result__title .result__a").expect("static selector is valid");
    let snippet_selector = Selector::parse(".result__snippet").expect("static selector is valid");

    let mut results = Vec::new();
    for result in document.select(&result_selector) {
        if results.len() >= limit {
            break;
        }
        let Some(title_element) = result.select(&title_selector).next() else {
            continue;
        };
        let title = title_element
            .text()
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let href = title_element.value().attr("href").unwrap_or_default();
        if title.is_empty() || href.is_empty() {
            continue;
        }
        let snippet = result
            .select(&snippet_selector)
            .next()
            .map(|element| element.text().collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        results.push(SearchResult {
            title,
            url: extract_ddg_url(href),
            description: snippet,
            ..SearchResult::default()
        });
    }
    results
}

async fn fetch_ddg(client: &reqwest::Client, query: &str) -> eyre::Result<reqwest::Response> {
    fetch_ddg_from_url(client, "https://html.duckduckgo.com/html/", query).await
}

async fn fetch_ddg_from_url(
    client: &reqwest::Client,
    url: &str,
    query: &str,
) -> eyre::Result<reqwest::Response> {
    for _ in 0..3 {
        let response = client
            .get(url)
            .query(&[("q", query)])
            .header(
                USER_AGENT,
                "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0",
            )
            .send()
            .await
            .map_err(|err| eyre::eyre!("ddg request failed: {err}"))?;
        let status = response.status();
        if status == reqwest::StatusCode::OK {
            return Ok(response);
        }
        if status != reqwest::StatusCode::ACCEPTED {
            eyre::bail!("ddg returned status {}", status.as_u16());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    eyre::bail!("ddg rate limited after retries");
}

fn extract_ddg_url(href: &str) -> String {
    let query = href
        .split_once('?')
        .map(|(_, query)| query.split_once('#').map_or(query, |(query, _)| query))
        .unwrap_or_default();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if key == "uddg" {
            return if value.is_empty() {
                href.to_string()
            } else {
                value.into_owned()
            };
        }
    }
    href.to_string()
}

async fn exa(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
    api_key: &str,
) -> eyre::Result<Vec<SearchResult>> {
    exa_from_url(client, "https://mcp.exa.ai/mcp", query, limit, api_key).await
}

async fn exa_from_url(
    client: &reqwest::Client,
    endpoint: &str,
    query: &str,
    limit: usize,
    api_key: &str,
) -> eyre::Result<Vec<SearchResult>> {
    let body = exa_request_body(query, limit);
    let endpoint = exa_endpoint(endpoint, api_key)?;

    let response = client
        .post(endpoint)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json, text/event-stream")
        .json(&body)
        .send()
        .await
        .map_err(|err| eyre::eyre!("exa request failed: {err}"))?;
    let status = response.status();
    if status != reqwest::StatusCode::OK {
        eyre::bail!("exa returned status {}", status.as_u16());
    }
    let raw = response
        .text()
        .await
        .map_err(|err| eyre::eyre!("failed to read exa response: {err}"))?;
    let payload = extract_sse_payload(&raw)?;

    #[derive(Deserialize)]
    struct ExaPayload {
        result: ExaResult,
    }
    #[derive(Deserialize)]
    struct ExaResult {
        content: Vec<ExaContent>,
    }
    #[derive(Deserialize)]
    struct ExaContent {
        #[serde(rename = "type")]
        kind: String,
        text: String,
    }

    let parsed: ExaPayload = serde_json::from_str(&payload)
        .map_err(|err| eyre::eyre!("failed to decode exa response: {err}"))?;

    let mut results = Vec::new();
    for content in parsed.result.content {
        if content.kind != "text" || results.len() >= limit {
            continue;
        }
        let remaining = limit - results.len();
        results.extend(parse_exa_content(&content.text, remaining));
    }
    Ok(results)
}

fn exa_request_body(query: &str, limit: usize) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "web_search_exa",
            "arguments": {
                "query": query,
                "numResults": limit,
                "type": "auto",
                "livecrawl": "fallback",
                "contextMaxCharacters": 3000
            }
        }
    })
}

fn exa_endpoint(endpoint: &str, api_key: &str) -> eyre::Result<url::Url> {
    let mut endpoint =
        url::Url::parse(endpoint).map_err(|err| eyre::eyre!("invalid exa endpoint: {err}"))?;
    if !api_key.trim().is_empty() {
        endpoint
            .query_pairs_mut()
            .append_pair("exaApiKey", api_key.trim());
    }
    Ok(endpoint)
}

fn extract_sse_payload(raw: &str) -> eyre::Result<String> {
    let payload = raw
        .lines()
        .filter_map(|line| line.trim().strip_prefix("data:"))
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string();
    if payload.is_empty() {
        eyre::bail!("exa response contained no data payload");
    }
    Ok(payload)
}

fn parse_exa_content(raw: &str, limit: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    for block in raw.split("\n---\n") {
        if results.len() >= limit {
            break;
        }
        let mut result = SearchResult::default();
        let mut content_lines = Vec::new();
        for line in block.lines().map(str::trim) {
            if let Some(title) = line.strip_prefix("Title:") {
                result.title = title.trim().to_string();
            } else if let Some(url) = line.strip_prefix("URL:") {
                result.url = url.trim().to_string();
            } else if !line.is_empty() && !known_exa_prefix(line) {
                if result.description.is_empty() {
                    result.description = line.to_string();
                }
                content_lines.push(line.to_string());
            }
        }
        result.content = content_lines.join("\n");
        if !result.title.is_empty() && !result.url.is_empty() {
            results.push(result);
        }
    }
    results
}

fn known_exa_prefix(line: &str) -> bool {
    [
        "Title:",
        "URL:",
        "Highlights:",
        "Published date:",
        "Author:",
        "Score:",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::{
        BraveResponse, SearxngResponse, brave_from_url, brave_results, exa_endpoint, exa_from_url,
        exa_request_body, extract_ddg_url, extract_sse_payload, fetch_ddg_from_url,
        parse_ddg_results, parse_exa_content, searxng, searxng_results,
    };
    use crate::http;
    use crate::testserver::{TestHttpServer, TestResponse};

    #[test]
    fn parses_brave_results() {
        let body: BraveResponse = serde_json::from_str(
            r#"{
                "web": {
                    "results": [
                        {"title": "Go Docs", "url": "https://golang.org/doc/", "description": "Go documentation"},
                        {"title": "Go Blog", "url": "https://blog.golang.org/", "description": "The Go Blog"},
                        {"title": "Go Playground", "url": "https://play.golang.org/", "description": "Run Go online"}
                    ]
                }
            }"#,
        )
        .expect("brave response parses");

        let results = brave_results(body, 3);

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].title, "Go Docs");
        assert_eq!(results[0].url, "https://golang.org/doc/");
        assert_eq!(results[0].description, "Go documentation");
    }

    #[test]
    fn respects_brave_result_limit_and_empty_results() {
        let body: BraveResponse = serde_json::from_str(
            r#"{
                "web": {
                    "results": [
                        {"title": "A", "url": "https://a.com", "description": "a"},
                        {"title": "B", "url": "https://b.com", "description": "b"},
                        {"title": "C", "url": "https://c.com", "description": "c"}
                    ]
                }
            }"#,
        )
        .expect("brave response parses");
        let empty_body: BraveResponse =
            serde_json::from_str(r#"{"web": {"results": []}}"#).expect("empty response parses");

        assert_eq!(brave_results(body, 2).len(), 2);
        assert!(brave_results(empty_body, 5).is_empty());
    }

    #[tokio::test]
    async fn rejects_brave_non_ok_success_status() {
        let server = TestHttpServer::responses(vec![TestResponse::new(
            "201 Created",
            "application/json",
            r#"{"web": {"results": []}}"#,
        )]);
        let client = http::client(http::TEST_TIMEOUT).expect("test client builds");

        let err = brave_from_url(&client, &server.url, "test", 5, "key")
            .await
            .expect_err("brave rejects non-200 success status");

        assert!(err.to_string().contains("201"));
        assert!(
            server.requests()[0]
                .to_ascii_lowercase()
                .contains("x-subscription-token: key")
        );
    }

    #[test]
    fn parses_searxng_results() {
        let body: SearxngResponse = serde_json::from_str(
            r#"{
                "results": [
                    {"title": "SearX Result 1", "url": "https://example.com/1", "content": "First result content"},
                    {"title": "SearX Result 2", "url": "https://example.com/2", "content": "Second result content"}
                ]
            }"#,
        )
        .expect("searxng response parses");

        let results = searxng_results(body, 10);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "SearX Result 1");
        assert_eq!(results[0].url, "https://example.com/1");
        assert_eq!(results[0].description, "First result content");
    }

    #[test]
    fn respects_searxng_result_limit_and_empty_results() {
        let body: SearxngResponse = serde_json::from_str(
            r#"{
                "results": [
                    {"title": "A", "url": "https://a.com", "content": "a"},
                    {"title": "B", "url": "https://b.com", "content": "b"},
                    {"title": "C", "url": "https://c.com", "content": "c"}
                ]
            }"#,
        )
        .expect("searxng response parses");
        let empty_body: SearxngResponse =
            serde_json::from_str(r#"{"results": []}"#).expect("empty response parses");

        assert_eq!(searxng_results(body, 2).len(), 2);
        assert!(searxng_results(empty_body, 5).is_empty());
    }

    #[tokio::test]
    async fn rejects_searxng_non_ok_success_status() {
        let server = TestHttpServer::responses(vec![TestResponse::new(
            "201 Created",
            "application/json",
            r#"{"results": []}"#,
        )]);
        let client = http::client(http::TEST_TIMEOUT).expect("test client builds");

        let err = searxng(&client, "test", 5, &server.url)
            .await
            .expect_err("searxng rejects non-200 success status");

        assert!(err.to_string().contains("201"));
    }

    #[test]
    fn builds_exa_request_body() {
        let body = exa_request_body("rust release date", 5);

        assert_eq!(body["method"], "tools/call");
        assert_eq!(body["params"]["name"], "web_search_exa");
        assert_eq!(body["params"]["arguments"]["query"], "rust release date");
        assert_eq!(
            body["params"]["arguments"]["numResults"],
            serde_json::json!(5)
        );
        assert_eq!(body["params"]["arguments"]["livecrawl"], "fallback");
        assert_eq!(
            body["params"]["arguments"]["contextMaxCharacters"],
            serde_json::json!(3000)
        );
    }

    #[test]
    fn builds_exa_endpoint_with_trimmed_api_key() {
        let endpoint =
            exa_endpoint("https://mcp.exa.ai/mcp", " test-key ").expect("exa endpoint builds");
        let api_key = endpoint
            .query_pairs()
            .find(|(key, _)| key == "exaApiKey")
            .map(|(_, value)| value.into_owned());
        let without_key =
            exa_endpoint("https://mcp.exa.ai/mcp", "   ").expect("exa endpoint builds");

        assert_eq!(api_key.as_deref(), Some("test-key"));
        assert_eq!(without_key.query(), None);
    }

    #[test]
    fn extracts_last_exa_sse_payload() {
        let raw = r#"event: message
data: {"ignored":true}
data: {"result":{"content":[]}}
"#;

        assert_eq!(
            extract_sse_payload(raw).expect("sse payload exists"),
            r#"{"result":{"content":[]}}"#
        );
        assert!(extract_sse_payload("event: ping\n").is_err());
    }

    #[test]
    fn parses_exa_results_and_respects_limit() {
        let results = parse_exa_content(
            "Title: Rust Programming Language\nURL: https://www.rust-lang.org/\nHighlights:\nRust is a programming language.\n\n---\n\nTitle: Rust Releases\nURL: https://blog.rust-lang.org/\nHighlights:\nRelease notes for Rust.",
            1,
        );

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(results[0].description, "Rust is a programming language.");
        assert!(
            results[0]
                .content
                .contains("Rust is a programming language.")
        );
        assert!(parse_exa_content("", 5).is_empty());
    }

    #[tokio::test]
    async fn rejects_exa_non_ok_success_status() {
        let server = TestHttpServer::responses(vec![TestResponse::new(
            "201 Created",
            "text/event-stream",
            r#"data: {"result":{"content":[{"type":"text","text":"Title: Rust\nURL: https://www.rust-lang.org/\nHighlights:\nRust"}]}}"#,
        )]);
        let client = http::client(http::TEST_TIMEOUT).expect("test client builds");

        let err = exa_from_url(&client, &server.url, "rust", 5, "")
            .await
            .expect_err("exa rejects non-200 success status");

        assert!(err.to_string().contains("201"));
    }

    #[tokio::test]
    async fn retries_ddg_accepted_responses() {
        let body = r#"<html><body>
            <div class="result">
                <h2 class="result__title"><a class="result__a" href="https://example.com">Example</a></h2>
                <div class="result__snippet">After retries</div>
            </div>
        </body></html>"#;
        let server = TestHttpServer::responses(vec![
            TestResponse::new("202 Accepted", "text/plain", ""),
            TestResponse::new("202 Accepted", "text/plain", ""),
            TestResponse::new("200 OK", "text/html", body),
        ]);
        let client = http::client(http::TEST_TIMEOUT).expect("test client builds");

        let response = fetch_ddg_from_url(&client, &format!("{}/html/", server.url), "test")
            .await
            .expect("ddg fetch retries accepted responses");
        let body = response.text().await.expect("response body reads");
        let results = parse_ddg_results(&body, 5);

        assert_eq!(server.requests().len(), 3);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example");
        assert_eq!(results[0].description, "After retries");
    }

    #[test]
    fn parses_ddg_results() {
        let body = r#"<html><body>
            <div class="result">
                <h2 class="result__title"><a class="result__a" href="https://golang.org/">Go Language</a></h2>
                <div class="result__snippet">The Go programming language</div>
            </div>
            <div class="result">
                <h2 class="result__title"><a class="result__a" href="https://go.dev/">Go Dev</a></h2>
                <div class="result__snippet">Go developer portal</div>
            </div>
        </body></html>"#;

        let results = parse_ddg_results(body, 10);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Go Language");
        assert_eq!(results[0].url, "https://golang.org/");
        assert_eq!(results[0].description, "The Go programming language");
        assert_eq!(results[1].title, "Go Dev");
        assert_eq!(results[1].url, "https://go.dev/");
        assert_eq!(results[1].description, "Go developer portal");
    }

    #[test]
    fn parses_ddg_protocol_relative_redirects_from_html() {
        let body = r#"<html><body>
            <div class="result">
                <h2 class="result__title">
                    <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpath%3Fq%3Drust&amp;rut=abc">Example</a>
                </h2>
                <div class="result__snippet">Example snippet</div>
            </div>
        </body></html>"#;

        let results = parse_ddg_results(body, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/path?q=rust");
    }

    #[test]
    fn respects_ddg_result_limit() {
        let body = r#"<html><body>
            <div class="result"><h2 class="result__title"><a class="result__a" href="https://a.com">A</a></h2><div class="result__snippet">a</div></div>
            <div class="result"><h2 class="result__title"><a class="result__a" href="https://b.com">B</a></h2><div class="result__snippet">b</div></div>
            <div class="result"><h2 class="result__title"><a class="result__a" href="https://c.com">C</a></h2><div class="result__snippet">c</div></div>
        </body></html>"#;

        let results = parse_ddg_results(body, 1);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "A");
        assert_eq!(results[0].url, "https://a.com");
    }

    #[test]
    fn skips_ddg_results_without_title_or_href() {
        let body = r#"<html><body>
            <div class="result">
                <h2 class="result__title"><a class="result__a" href="">No href</a></h2>
                <div class="result__snippet">skip me</div>
            </div>
            <div class="result">
                <h2 class="result__title"><a class="result__a" href="https://empty-title.example"></a></h2>
                <div class="result__snippet">skip me too</div>
            </div>
            <div class="result">
                <h2 class="result__title"><a class="result__a" href="https://ok.example">OK</a></h2>
                <div class="result__snippet">keep me</div>
            </div>
        </body></html>"#;

        let results = parse_ddg_results(body, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "OK");
        assert_eq!(results[0].url, "https://ok.example");
    }

    #[test]
    fn extracts_ddg_redirect_targets() {
        let cases = [
            (
                "https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com",
                "https://example.com",
            ),
            (
                "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com",
                "https://example.com",
            ),
            (
                "/l/?uddg=https%3A%2F%2Fexample.com%2Fa%3Fb%3D1%26c%3Dtwo",
                "https://example.com/a?b=1&c=two",
            ),
            (
                "//duckduckgo.com/l/?rut=abc&uddg=https%3A%2F%2Fexample.com%2Fwith-rut",
                "https://example.com/with-rut",
            ),
        ];

        for (href, expected) in cases {
            assert_eq!(extract_ddg_url(href), expected);
        }
    }

    #[test]
    fn preserves_non_ddg_and_empty_ddg_urls() {
        let direct_url = "https://example.com/search?q=rust";
        let empty_ddg = "//duckduckgo.com/l/?uddg=&rut=abc";

        assert_eq!(extract_ddg_url(direct_url), direct_url);
        assert_eq!(extract_ddg_url(empty_ddg), empty_ddg);
    }

    #[test]
    fn parses_exa_blocks() {
        let results = parse_exa_content(
            "Title: Example\nURL: https://example.com\nHighlights:\nA useful result",
            5,
        );
        assert_eq!(results[0].title, "Example");
        assert_eq!(results[0].description, "A useful result");
    }
}
