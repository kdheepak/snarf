use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;

use color_eyre::eyre;
use reqwest::header::{
    ACCEPT, CONTENT_TYPE, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, USER_AGENT,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config;
use crate::extract;
use crate::types::Page;
use crate::urlrewrite::{self, Rewriter};

pub const MAX_BODY_BYTES: usize = 20 << 20;
pub const SOURCE_HTTP: &str = "http";
pub const SOURCE_HTTP_SHELL: &str = "http_shell";
pub const SOURCE_BROWSER: &str = "browser";

#[derive(Clone)]
pub struct Scraper {
    client: reqwest::Client,
    browser: String,
    rewriter: Option<Rewriter>,
}

#[derive(Debug, Clone, Default)]
pub struct FetchResult {
    pub page: Page,
    pub raw_html: String,
    pub not_modified: bool,
    pub js_detection: String,
    pub source: String,
}

impl Scraper {
    pub fn new(browser: String, rewriter: Option<Rewriter>) -> eyre::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("Mozilla/5.0 (compatible; snarf/1.0)")
            .build()?;
        Ok(Self {
            client,
            browser,
            rewriter,
        })
    }

    pub fn has_browser(&self) -> bool {
        !self.browser.is_empty()
    }

    pub fn rewrite(&self, raw_url: &str) -> String {
        urlrewrite::apply(&self.rewriter, raw_url)
    }

    pub async fn fetch(&self, raw_url: &str) -> eyre::Result<String> {
        let response = self
            .client
            .get(raw_url)
            .header(USER_AGENT, "Mozilla/5.0 (compatible; snarf/1.0)")
            .header(ACCEPT, "text/html,application/xhtml+xml")
            .send()
            .await
            .map_err(|err| eyre::eyre!("fetch failed: {err}"))?;

        let status = response.status();
        if !status.is_success() {
            eyre::bail!("HTTP {} for {}", status.as_u16(), raw_url);
        }

        read_limited_text(response).await
    }

    pub async fn fetch_with_browser_fallback(
        &self,
        raw_url: &str,
    ) -> eyre::Result<(String, String)> {
        let fetch_url = self.rewrite(raw_url);
        let body = self.fetch(&fetch_url).await?;
        let (body, _) = self.maybe_browser_fetch(&fetch_url, body).await;
        Ok((body, fetch_url))
    }

    pub async fn scrape(&self, raw_url: &str) -> eyre::Result<(Page, String)> {
        let fetch_url = self.rewrite(raw_url);
        let body = self.fetch(&fetch_url).await?;
        let (body, source) = self.maybe_browser_fetch(&fetch_url, body).await;
        let extracted = extract::extract(&fetch_url, &body)?;

        let mut page = Page {
            url: raw_url.to_string(),
            title: extracted.title,
            markdown: extracted.markdown,
            ..Page::default()
        };
        if fetch_url != raw_url {
            page.fetched_url = fetch_url;
        }
        Ok((page, source))
    }

    pub async fn scrape_conditional(
        &self,
        raw_url: &str,
        etag: &str,
        last_modified: &str,
    ) -> eyre::Result<FetchResult> {
        let fetch_url = self.rewrite(raw_url);
        let mut request = self
            .client
            .get(&fetch_url)
            .header(USER_AGENT, "Mozilla/5.0 (compatible; snarf/1.0)")
            .header(ACCEPT, "text/html,application/xhtml+xml");
        if !etag.is_empty() {
            request = request.header(IF_NONE_MATCH, etag);
        }
        if !last_modified.is_empty() {
            request = request.header(IF_MODIFIED_SINCE, last_modified);
        }

        let response = request
            .send()
            .await
            .map_err(|err| eyre::eyre!("fetch failed: {err}"))?;

        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(FetchResult {
                not_modified: true,
                ..FetchResult::default()
            });
        }
        let status = response.status();
        if !status.is_success() {
            eyre::bail!("HTTP {} for {}", status.as_u16(), fetch_url);
        }

        let etag_value = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let last_modified_value = response
            .headers()
            .get(LAST_MODIFIED)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let html = read_limited_text(response).await?;
        let detection = extract::detect_js_shell(&html).to_string();
        let (html, source) = if detection == "likely_shell" {
            self.browser_fetch_or_warn(&fetch_url, html).await
        } else {
            (html, SOURCE_HTTP.to_string())
        };
        let extracted = extract::extract(&fetch_url, &html)?;
        let mut page = Page {
            url: raw_url.to_string(),
            title: extracted.title,
            markdown: extracted.markdown,
            etag: etag_value,
            last_modified: last_modified_value,
            ..Page::default()
        };
        page.content_hash = content_hash(&page.markdown);
        if fetch_url != raw_url {
            page.fetched_url = fetch_url;
        }

        Ok(FetchResult {
            page,
            raw_html: html,
            not_modified: false,
            js_detection: detection,
            source,
        })
    }

    pub async fn browser_scrape(&self, raw_url: &str) -> eyre::Result<FetchResult> {
        if !self.has_browser() {
            eyre::bail!("browser is not configured");
        }

        let fetch_url = self.rewrite(raw_url);
        let html = browser_fetch(&self.browser, &fetch_url).await?;
        let extracted = extract::extract(&fetch_url, &html)?;
        let mut page = Page {
            url: raw_url.to_string(),
            title: extracted.title,
            markdown: extracted.markdown,
            ..Page::default()
        };
        page.content_hash = content_hash(&page.markdown);
        if fetch_url != raw_url {
            page.fetched_url = fetch_url;
        }

        Ok(FetchResult {
            page,
            raw_html: html,
            not_modified: false,
            js_detection: String::new(),
            source: SOURCE_BROWSER.to_string(),
        })
    }

    async fn maybe_browser_fetch(&self, raw_url: &str, html: String) -> (String, String) {
        if extract::detect_js_shell(&html) != "likely_shell" {
            return (html, SOURCE_HTTP.to_string());
        }
        self.browser_fetch_or_warn(raw_url, html).await
    }

    async fn browser_fetch_or_warn(&self, raw_url: &str, html: String) -> (String, String) {
        if self.has_browser() {
            match browser_fetch(&self.browser, raw_url).await {
                Ok(rendered) => return (rendered, SOURCE_BROWSER.to_string()),
                Err(err) => eprintln!("warn: browser fallback failed for {raw_url}: {err}"),
            }
        } else {
            eprintln!("warn: {raw_url} appears JS-rendered; configure browser for full content");
        }
        (html, SOURCE_HTTP_SHELL.to_string())
    }
}

async fn browser_fetch(browser_config: &str, raw_url: &str) -> eyre::Result<String> {
    let browser = resolve_browser_bin(browser_config)?;
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(browser)
            .arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--dump-dom")
            .arg(raw_url)
            .output(),
    )
    .await
    .map_err(|_| eyre::eyre!("browser render timed out"))??;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eyre::bail!(
            "browser exited with status {}: {}",
            output.status,
            stderr.trim()
        );
    }

    let html = String::from_utf8_lossy(&output.stdout).to_string();
    if html.trim().is_empty() {
        eyre::bail!("browser returned empty DOM");
    }
    Ok(html)
}

pub fn cache_stale_for_browser(source: &str, has_browser: bool) -> bool {
    source == SOURCE_HTTP_SHELL || (source.is_empty() && has_browser)
}

pub fn content_hash(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    hex::encode(&digest[..8])
}

pub fn resolve_browser_bin(config_value: &str) -> eyre::Result<String> {
    if config_value.contains(std::path::MAIN_SEPARATOR) {
        let path = std::path::Path::new(config_value);
        if path.exists() {
            return Ok(config_value.to_string());
        }
        eyre::bail!("browser path does not exist: {config_value}");
    }

    for candidate in match config_value {
        "chrome" => vec![
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ],
        "chromium" => vec!["chromium", "chromium-browser"],
        other => vec![other],
    } {
        if candidate.contains(std::path::MAIN_SEPARATOR) {
            if std::path::Path::new(candidate).exists() {
                return Ok(candidate.to_string());
            }
            continue;
        }
        if let Some(path) = find_on_path(candidate) {
            return Ok(path);
        }
    }

    eyre::bail!("browser executable not found for {config_value}");
}

pub async fn install_browser() -> eyre::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent("Mozilla/5.0 (compatible; snarf/1.0)")
        .build()?;
    let platform = chrome_for_testing_platform()?;
    let metadata: ChromeForTestingVersions = client
        .get("https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let stable = metadata
        .channels
        .stable
        .ok_or_else(|| eyre::eyre!("Chrome for Testing metadata did not include Stable"))?;
    let download = stable
        .downloads
        .chrome
        .iter()
        .find(|download| download.platform == platform)
        .ok_or_else(|| eyre::eyre!("Chrome for Testing has no chrome download for {platform}"))?;

    let install_dir = config::cache_dir()?
        .join("browser")
        .join(&stable.version)
        .join(platform);
    if let Some(existing) = find_browser_binary(&install_dir)? {
        return Ok(existing.display().to_string());
    }

    fs::create_dir_all(&install_dir)?;
    let archive = client
        .get(&download.url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    extract_zip(&archive, &install_dir)?;

    let browser = find_browser_binary(&install_dir)?.ok_or_else(|| {
        eyre::eyre!("downloaded Chrome for Testing archive did not contain a browser executable")
    })?;
    mark_executable(&browser)?;
    Ok(browser.display().to_string())
}

#[derive(Debug, Deserialize)]
struct ChromeForTestingVersions {
    channels: ChromeForTestingChannels,
}

#[derive(Debug, Deserialize)]
struct ChromeForTestingChannels {
    #[serde(rename = "Stable")]
    stable: Option<ChromeForTestingChannel>,
}

#[derive(Debug, Deserialize)]
struct ChromeForTestingChannel {
    version: String,
    downloads: ChromeForTestingDownloads,
}

#[derive(Debug, Deserialize)]
struct ChromeForTestingDownloads {
    chrome: Vec<ChromeForTestingDownload>,
}

#[derive(Debug, Deserialize)]
struct ChromeForTestingDownload {
    platform: String,
    url: String,
}

fn chrome_for_testing_platform() -> eyre::Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux64"),
        ("macos", "x86_64") => Ok("mac-x64"),
        ("macos", "aarch64") => Ok("mac-arm64"),
        ("windows", "x86") => Ok("win32"),
        ("windows", "x86_64" | "aarch64") => Ok("win64"),
        (os, arch) => eyre::bail!("unsupported browser install platform: {os}/{arch}"),
    }
}

fn extract_zip(bytes: &[u8], destination: &Path) -> eyre::Result<()> {
    let reader = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader)?;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let Some(path) = file.enclosed_name() else {
            continue;
        };
        let output_path = destination.join(path);
        if file.is_dir() {
            fs::create_dir_all(&output_path)?;
            continue;
        }
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = fs::File::create(&output_path)?;
        std::io::copy(&mut file, &mut output)?;
    }
    Ok(())
}

fn find_browser_binary(root: &Path) -> eyre::Result<Option<PathBuf>> {
    if !root.exists() {
        return Ok(None);
    }

    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }

            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if matches!(name, "chrome" | "chrome.exe" | "Google Chrome for Testing") {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

fn mark_executable(path: &Path) -> eyre::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

fn find_on_path(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate.display().to_string());
        }
    }
    None
}

async fn read_limited_text(response: reqwest::Response) -> eyre::Result<String> {
    read_limited_text_with_limit(response, MAX_BODY_BYTES).await
}

async fn read_limited_text_with_limit(
    mut response: reqwest::Response,
    limit: usize,
) -> eyre::Result<String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > limit {
            eyre::bail!("response body exceeded {limit} bytes");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

pub async fn fetch_llms_txt(client: &reqwest::Client, base_url: &str) -> Option<String> {
    let mut parsed = url::Url::parse(base_url).ok()?;
    if parsed.path() != "/" && !parsed.path().is_empty() {
        return None;
    }
    parsed.set_path("/llms.txt");
    parsed.set_query(None);
    parsed.set_fragment(None);
    let response = client
        .get(parsed)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if !content_type.contains("text/plain") {
        return None;
    }
    read_limited_text(response).await.ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use crate::urlrewrite::{Rewriter, Rule};

    use super::{Scraper, fetch_llms_txt, read_limited_text_with_limit};

    #[test]
    fn rewrite_without_rewriter_is_identity() {
        let scraper = Scraper::new(String::new(), None).unwrap();
        assert_eq!(
            scraper.rewrite("https://example.com/x"),
            "https://example.com/x"
        );
    }

    #[test]
    fn rewrite_applies_matching_rule() {
        let rewriter = Rewriter::new(&[Rule {
            r#match: r"^https?://www\.reddit\.com/(.*)$".to_string(),
            replace: "https://old.reddit.com/$1".to_string(),
        }])
        .unwrap();
        let scraper = Scraper::new(String::new(), rewriter).unwrap();

        assert_eq!(
            scraper.rewrite("https://www.reddit.com/r/rust"),
            "https://old.reddit.com/r/rust"
        );
    }

    #[test]
    fn rewrite_no_match_returns_original() {
        let rewriter = Rewriter::new(&[Rule {
            r#match: r"^https?://foo\.com/.*$".to_string(),
            replace: "https://bar.com/x".to_string(),
        }])
        .unwrap();
        let scraper = Scraper::new(String::new(), rewriter).unwrap();

        assert_eq!(
            scraper.rewrite("https://example.com/x"),
            "https://example.com/x"
        );
    }

    #[tokio::test]
    async fn scrape_applies_rewrite_and_sets_fetched_url() {
        let server = TestHttpServer::new(HashMap::from([(
            "/old".to_string(),
            "<html><head><title>Old</title></head><body><p>hello world from old</p></body></html>"
                .to_string(),
        )]));
        let original = format!("{}/new", server.url);
        let rewritten = format!("{}/old", server.url);
        let rewriter = Rewriter::new(&[Rule {
            r#match: format!("^{}{}$", regex::escape(&server.url), "/new"),
            replace: rewritten.clone(),
        }])
        .unwrap();
        let scraper = Scraper::new(String::new(), rewriter).unwrap();

        let (page, _) = scraper.scrape(&original).await.unwrap();

        assert_eq!(server.paths(), vec!["/old".to_string()]);
        assert_eq!(page.url, original);
        assert_eq!(page.fetched_url, rewritten);
    }

    #[tokio::test]
    async fn scrape_leaves_fetched_url_empty_without_rewrite() {
        let server = TestHttpServer::new(HashMap::from([(
            "/foo".to_string(),
            "<html><head><title>Foo</title></head><body><p>hello</p></body></html>".to_string(),
        )]));
        let scraper = Scraper::new(String::new(), None).unwrap();
        let original = format!("{}/foo", server.url);

        let (page, _) = scraper.scrape(&original).await.unwrap();

        assert_eq!(page.url, original);
        assert!(page.fetched_url.is_empty());
    }

    #[tokio::test]
    async fn scrape_conditional_applies_rewrite_and_sets_fetched_url() {
        let server = TestHttpServer::new(HashMap::from([(
            "/dst".to_string(),
            "<html><head><title>X</title></head><body><p>hi</p></body></html>".to_string(),
        )]));
        let original = format!("{}/src", server.url);
        let rewritten = format!("{}/dst", server.url);
        let rewriter = Rewriter::new(&[Rule {
            r#match: format!("^{}{}$", regex::escape(&server.url), "/src"),
            replace: rewritten.clone(),
        }])
        .unwrap();
        let scraper = Scraper::new(String::new(), rewriter).unwrap();

        let result = scraper.scrape_conditional(&original, "", "").await.unwrap();

        assert_eq!(server.paths(), vec!["/dst".to_string()]);
        assert_eq!(result.page.url, original);
        assert_eq!(result.page.fetched_url, rewritten);
    }

    #[tokio::test]
    async fn response_body_limit_is_enforced_while_reading() {
        let server = TestHttpServer::new(HashMap::from([(
            "/large".to_string(),
            "0123456789".to_string(),
        )]));
        let response = reqwest::get(format!("{}/large", server.url))
            .await
            .expect("request succeeds");

        let error = read_limited_text_with_limit(response, 8)
            .await
            .expect_err("response over limit is rejected");

        assert_eq!(error.to_string(), "response body exceeded 8 bytes");
    }

    #[tokio::test]
    async fn fetch_llms_txt_preserves_server_port() {
        let server = TestHttpServer::new(HashMap::from([(
            "/llms.txt".to_string(),
            "# Test llms\n".to_string(),
        )]));
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("test client builds");

        let content = fetch_llms_txt(&client, &server.url).await;

        assert_eq!(content.as_deref(), Some("# Test llms\n"));
        assert_eq!(server.paths(), vec!["/llms.txt".to_string()]);
    }

    struct TestHttpServer {
        url: String,
        paths: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl TestHttpServer {
        fn new(routes: HashMap<String, String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
            listener
                .set_nonblocking(true)
                .expect("test server can be nonblocking");
            let url = format!("http://{}", listener.local_addr().expect("server has addr"));
            let routes = Arc::new(routes);
            let paths = Arc::new(Mutex::new(Vec::new()));
            let server_paths = Arc::clone(&paths);
            let stop = Arc::new(AtomicBool::new(false));
            let server_stop = Arc::clone(&stop);

            let handle = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                while !server_stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                    let Ok((mut stream, _)) = listener.accept() else {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    stream
                        .set_nonblocking(false)
                        .expect("test connection can be blocking");

                    let mut request = [0u8; 4096];
                    let n = stream.read(&mut request).unwrap_or_default();
                    let request = String::from_utf8_lossy(&request[..n]);
                    let path = request
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_string();
                    server_paths
                        .lock()
                        .expect("paths mutex is not poisoned")
                        .push(path.clone());
                    let (status, body) = routes
                        .get(&path)
                        .map(|body| ("200 OK", body.as_str()))
                        .unwrap_or(("404 Not Found", "not found"));
                    let content_type = if path == "/llms.txt" {
                        "text/plain"
                    } else {
                        "text/html"
                    };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                }
            });

            Self {
                url,
                paths,
                stop,
                handle: Some(handle),
            }
        }

        fn paths(&self) -> Vec<String> {
            self.paths
                .lock()
                .expect("paths mutex is not poisoned")
                .clone()
        }
    }

    impl Drop for TestHttpServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                handle.join().expect("test server exits");
            }
        }
    }
}
