use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use color_eyre::eyre;
use futures_util::StreamExt;
use regex::Regex;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::cache::{CachedPage, PageCache};
use crate::config;
use crate::http;
use crate::scrape::{self, Scraper};
use crate::types::Page;

#[derive(Debug, Clone, Default)]
pub struct CrawlOptions {
    pub depth: usize,
    pub concurrency: usize,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CrawlResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<Page>,
    pub depth: usize,
    pub status: String,
    pub source: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub error: String,
    pub url: String,
}

#[derive(Debug, Clone)]
struct QueueItem {
    url: String,
    fetch_url: String,
    depth: usize,
    source: String,
}

type SharedHostJsStats = Arc<Mutex<HashMap<String, HostJsStats>>>;

#[derive(Debug, Default)]
struct HostJsStats {
    total: usize,
    shells: usize,
}

pub async fn crawl_with_callback<F>(
    seed: &str,
    scraper: &Scraper,
    options: CrawlOptions,
    cache: Option<&PageCache>,
    sitemap: bool,
    mut on_result: F,
) -> eyre::Result<Vec<CrawlResult>>
where
    F: FnMut(&CrawlResult),
{
    let seed_url = Url::parse(seed).map_err(|err| eyre::eyre!("invalid seed URL: {err}"))?;
    let seed_fetch_url = normalize_url(&scraper.rewrite(seed));
    let seed_host = if seed_fetch_url.is_empty() {
        seed_url.host_str().unwrap_or_default().to_string()
    } else {
        Url::parse(&seed_fetch_url)
            .ok()
            .and_then(|url| url.host_str().map(ToString::to_string))
            .unwrap_or_else(|| seed_url.host_str().unwrap_or_default().to_string())
    };
    let deny = compile_deny(&options.deny)?;
    let mut visited = HashSet::new();
    let mut current = VecDeque::new();
    let cache = cache.cloned();
    let host_stats = Arc::new(Mutex::new(HashMap::new()));
    let concurrency = options.concurrency.max(1);

    if sitemap {
        for url in fetch_sitemap(seed).await? {
            enqueue(
                &mut current,
                &mut visited,
                scraper,
                &url,
                0,
                "sitemap",
                &seed_host,
                &options.allow,
                &deny,
            );
        }
    } else {
        enqueue(
            &mut current,
            &mut visited,
            scraper,
            seed,
            0,
            "seed",
            &seed_host,
            &options.allow,
            &deny,
        );
    }

    let mut results = Vec::new();
    while !current.is_empty() {
        let mut next = VecDeque::new();

        {
            let workers = futures_util::stream::iter(current.drain(..))
                .map(|item| {
                    let scraper = scraper.clone();
                    let cache = cache.clone();
                    let host_stats = Arc::clone(&host_stats);
                    let max_depth = options.depth;

                    tokio::spawn(async move {
                        process_item(item, scraper, cache, host_stats, max_depth).await
                    })
                })
                .buffer_unordered(concurrency);
            futures_util::pin_mut!(workers);

            while let Some(joined) = workers.next().await {
                let processed = joined.map_err(|err| eyre::eyre!("crawl worker failed: {err}"))?;
                for link in processed.links {
                    enqueue(
                        &mut next,
                        &mut visited,
                        scraper,
                        &link,
                        processed.depth + 1,
                        "link",
                        &seed_host,
                        &options.allow,
                        &deny,
                    );
                }
                if let Some(result) = processed.result {
                    on_result(&result);
                    results.push(result);
                }
            }
        }

        current = next;
    }

    Ok(results)
}

#[derive(Debug)]
struct ProcessedItem {
    depth: usize,
    links: Vec<String>,
    result: Option<CrawlResult>,
}

async fn process_item(
    item: QueueItem,
    scraper: Scraper,
    cache: Option<PageCache>,
    host_stats: SharedHostJsStats,
    max_depth: usize,
) -> ProcessedItem {
    let cached = cache
        .as_ref()
        .and_then(|cache| cache.get_any(&item.fetch_url));
    if let Some(cached) = cached.as_ref()
        && !cached.expired
        && !scrape::cache_stale_for_browser(&cached.source, scraper.has_browser())
    {
        let page = page_with_crawl_urls(cached.page.clone(), &item.url, &item.fetch_url);
        return ProcessedItem {
            depth: item.depth,
            links: Vec::new(),
            result: Some(CrawlResult {
                page: Some(page),
                depth: item.depth,
                status: "unchanged".to_string(),
                source: item.source,
                url: item.url,
                ..CrawlResult::default()
            }),
        };
    }

    let fetch_result =
        if should_force_browser(&host_stats, &item.fetch_url) && scraper.has_browser() {
            scraper.browser_scrape(&item.url).await
        } else {
            let etag = cached
                .as_ref()
                .filter(|cached| cached.expired)
                .map(|cached| cached.page.etag.as_str())
                .unwrap_or_default();
            let last_modified = cached
                .as_ref()
                .filter(|cached| cached.expired)
                .map(|cached| cached.page.last_modified.as_str())
                .unwrap_or_default();
            let fetch_result = scraper
                .scrape_conditional(&item.url, etag, last_modified)
                .await;
            if let Ok(fetch) = &fetch_result
                && !fetch.not_modified
            {
                record_js_detection(&host_stats, &item.fetch_url, &fetch.js_detection);
            }
            fetch_result
        };

    match fetch_result {
        Ok(fetch) => {
            if fetch.not_modified {
                if let Some(cached) = cached {
                    if let Some(cache) = cache.as_ref() {
                        cache.put(&item.fetch_url, &cached.page, &cached.source);
                    }
                    let page = page_with_crawl_urls(cached.page, &item.url, &item.fetch_url);
                    return ProcessedItem {
                        depth: item.depth,
                        links: Vec::new(),
                        result: Some(CrawlResult {
                            page: Some(page),
                            depth: item.depth,
                            status: "unchanged".to_string(),
                            source: item.source,
                            url: item.url,
                            ..CrawlResult::default()
                        }),
                    };
                }
                return ProcessedItem {
                    depth: item.depth,
                    links: Vec::new(),
                    result: None,
                };
            }

            let page = page_with_crawl_urls(fetch.page, &item.url, &item.fetch_url);
            if let Some(cache) = cache.as_ref() {
                cache.put(&item.fetch_url, &page, &fetch.source);
            }

            let links = if item.depth < max_depth {
                extract_links(&item.fetch_url, &fetch.raw_html)
            } else {
                Vec::new()
            };
            let status = crawl_status(cached.as_ref(), &page);
            ProcessedItem {
                depth: item.depth,
                links,
                result: Some(CrawlResult {
                    page: Some(page),
                    depth: item.depth,
                    status: status.to_string(),
                    source: item.source,
                    url: item.url,
                    ..CrawlResult::default()
                }),
            }
        },
        Err(err) => ProcessedItem {
            depth: item.depth,
            links: Vec::new(),
            result: Some(CrawlResult {
                page: None,
                depth: item.depth,
                status: String::new(),
                source: item.source,
                error: err.to_string(),
                url: item.url,
            }),
        },
    }
}

fn page_with_crawl_urls(mut page: Page, url: &str, fetch_url: &str) -> Page {
    page.url = url.to_string();
    page.fetched_url = if fetch_url == url {
        String::new()
    } else {
        fetch_url.to_string()
    };
    page
}

fn crawl_status(cached: Option<&CachedPage>, fetched: &Page) -> &'static str {
    let Some(cached) = cached else {
        return "new";
    };
    if pages_have_same_content(&cached.page, fetched) {
        "unchanged"
    } else {
        "changed"
    }
}

fn pages_have_same_content(left: &Page, right: &Page) -> bool {
    if !left.content_hash.is_empty() && !right.content_hash.is_empty() {
        return left.content_hash == right.content_hash;
    }
    left.title == right.title && left.markdown == right.markdown
}

fn record_js_detection(stats: &SharedHostJsStats, raw_url: &str, detection: &str) {
    let Ok(url) = Url::parse(raw_url) else {
        return;
    };
    let Some(host) = url.host_str() else {
        return;
    };

    let mut stats = stats.lock().expect("host stats mutex is not poisoned");
    let entry = stats.entry(host.to_string()).or_default();
    entry.total += 1;
    if detection == "likely_shell" {
        entry.shells += 1;
    }
}

fn should_force_browser(stats: &SharedHostJsStats, raw_url: &str) -> bool {
    let Ok(url) = Url::parse(raw_url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };

    let stats = stats.lock().expect("host stats mutex is not poisoned");
    let Some(stats) = stats.get(host) else {
        return false;
    };
    stats.total >= 10 && (stats.shells as f64 / stats.total as f64) > 0.8
}

#[allow(clippy::too_many_arguments)]
fn enqueue(
    queue: &mut VecDeque<QueueItem>,
    visited: &mut HashSet<String>,
    scraper: &Scraper,
    raw_url: &str,
    depth: usize,
    source: &str,
    seed_host: &str,
    allow: &[String],
    deny: &[Regex],
) {
    let fetch_url = normalize_url(&scraper.rewrite(raw_url));
    if fetch_url.is_empty() || visited.contains(&fetch_url) {
        return;
    }
    if !passes_filters(&fetch_url, seed_host, allow, deny) {
        return;
    }
    let display_url = normalize_url(raw_url);
    let display_url = if display_url.is_empty() {
        fetch_url.clone()
    } else {
        display_url
    };
    visited.insert(fetch_url.clone());
    queue.push_back(QueueItem {
        url: display_url,
        fetch_url,
        depth,
        source: source.to_string(),
    });
}

pub fn normalize_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return String::new();
    };
    url.set_fragment(None);

    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| !key.starts_with("utm_"))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    pairs.sort();
    url.set_query(None);
    if !pairs.is_empty() {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in pairs {
            serializer.append_pair(&key, &value);
        }
        url.set_query(Some(&serializer.finish()));
    }

    url.to_string().trim_end_matches('/').to_string()
}

pub fn passes_filters(raw_url: &str, seed_host: &str, allow: &[String], deny: &[Regex]) -> bool {
    let Ok(url) = Url::parse(raw_url) else {
        return false;
    };
    if url.host_str().unwrap_or_default() != seed_host {
        return false;
    }
    if deny.iter().any(|regex| regex.is_match(raw_url)) {
        return false;
    }
    if allow.is_empty() {
        return true;
    }
    allow.iter().any(|substring| url.path().contains(substring))
}

fn compile_deny(patterns: &[String]) -> eyre::Result<Vec<Regex>> {
    patterns
        .iter()
        .map(|pattern| {
            Regex::new(pattern)
                .map_err(|err| eyre::eyre!("invalid deny pattern {pattern:?}: {err}"))
        })
        .collect()
}

pub fn extract_links(page_url: &str, html: &str) -> Vec<String> {
    let Ok(base) = Url::parse(page_url) else {
        return Vec::new();
    };
    let document = Html::parse_document(html);
    let selector = Selector::parse("a[href]").expect("static selector is valid");
    let mut links = Vec::new();
    for element in document.select(&selector) {
        let Some(href) = element.value().attr("href") else {
            continue;
        };
        if href.is_empty()
            || href.starts_with("javascript:")
            || href.starts_with("mailto:")
            || href.starts_with("tel:")
            || href.starts_with('#')
        {
            continue;
        }
        if let Ok(resolved) = base.join(href)
            && matches!(resolved.scheme(), "http" | "https")
        {
            links.push(resolved.to_string());
        }
    }
    links
}

pub async fn fetch_sitemap(sitemap_url: &str) -> eyre::Result<Vec<String>> {
    let client = http::client(http::FETCH_TIMEOUT)?;
    let candidates = sitemap_candidates(sitemap_url, &client).await?;
    let mut failures = Vec::new();

    for candidate in &candidates {
        let body = match client.get(candidate).send().await {
            Ok(response) => match response.error_for_status() {
                Ok(response) => match response.text().await {
                    Ok(body) => body,
                    Err(err) => {
                        failures.push(format!("{candidate}: failed to read response body: {err}"));
                        continue;
                    },
                },
                Err(err) => {
                    failures.push(format!("{candidate}: {err}"));
                    continue;
                },
            },
            Err(err) => {
                failures.push(format!("{candidate}: {err}"));
                continue;
            },
        };

        match parse_sitemap(&body, &client).await {
            Ok(urls) => return Ok(urls),
            Err(err) => failures.push(format!("{candidate}: {err}")),
        }
    }

    let tried = candidates.join(", ");
    let detail = failures
        .last()
        .map(|failure| format!("; last error: {failure}"))
        .unwrap_or_default();
    eyre::bail!("failed to find a sitemap for {sitemap_url}; tried {tried}{detail}")
}

async fn sitemap_candidates(
    sitemap_url: &str,
    client: &reqwest::Client,
) -> eyre::Result<Vec<String>> {
    let seed = Url::parse(sitemap_url).map_err(|err| eyre::eyre!("invalid sitemap URL: {err}"))?;
    let mut candidates = Vec::new();

    if seed.path() != "/" {
        push_unique(&mut candidates, seed.to_string());
    }

    for sitemap in fetch_robots_sitemaps(&seed, client).await {
        push_unique(&mut candidates, sitemap);
    }

    for path in ["/sitemap.xml", "/sitemap-index.xml", "/sitemap_index.xml"] {
        if let Ok(candidate) = seed.join(path) {
            push_unique(&mut candidates, candidate.to_string());
        }
    }

    push_unique(&mut candidates, seed.to_string());
    Ok(candidates)
}

async fn fetch_robots_sitemaps(seed: &Url, client: &reqwest::Client) -> Vec<String> {
    let Ok(robots_url) = seed.join("/robots.txt") else {
        return Vec::new();
    };
    let Ok(response) = client.get(robots_url).send().await else {
        return Vec::new();
    };
    let Ok(response) = response.error_for_status() else {
        return Vec::new();
    };
    let Ok(body) = response.text().await else {
        return Vec::new();
    };
    parse_robots_sitemaps(&body)
}

fn parse_robots_sitemaps(robots: &str) -> Vec<String> {
    robots
        .lines()
        .filter_map(|line| {
            let line = line.split_once('#').map_or(line, |(value, _)| value).trim();
            let (field, value) = line.split_once(':')?;
            if !field.trim().eq_ignore_ascii_case("sitemap") {
                return None;
            }
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        })
        .collect()
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

async fn parse_sitemap(xml: &str, client: &reqwest::Client) -> eyre::Result<Vec<String>> {
    match first_xml_element(xml)? {
        Some(root) if root == "sitemapindex" => {
            let index: SitemapIndex = quick_xml::de::from_str(xml)
                .map_err(|err| eyre::eyre!("failed to parse sitemap XML: {err}"))?;
            let mut urls = Vec::new();
            for sitemap in index.sitemaps {
                let Ok(response) = client.get(&sitemap.loc).send().await else {
                    continue;
                };
                let Ok(body) = response.error_for_status()?.text().await else {
                    continue;
                };
                if let Ok(mut child_urls) = Box::pin(parse_sitemap(&body, client)).await {
                    urls.append(&mut child_urls);
                }
            }
            Ok(urls)
        },
        Some(root) if root == "urlset" => {
            let url_set: UrlSet = quick_xml::de::from_str(xml)
                .map_err(|err| eyre::eyre!("failed to parse sitemap XML: {err}"))?;
            Ok(url_set
                .urls
                .into_iter()
                .filter_map(|entry| {
                    let loc = entry.loc.trim().to_string();
                    if loc.is_empty() { None } else { Some(loc) }
                })
                .collect())
        },
        Some(root) => {
            eyre::bail!("expected sitemap XML root <urlset> or <sitemapindex>, found <{root}>")
        },
        None => eyre::bail!("empty sitemap XML document"),
    }
}

fn first_xml_element(xml: &str) -> eyre::Result<Option<String>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(element))
            | Ok(quick_xml::events::Event::Empty(element)) => {
                let name = element.name();
                return Ok(Some(String::from_utf8_lossy(name.as_ref()).into_owned()));
            },
            Ok(quick_xml::events::Event::Eof) => return Ok(None),
            Ok(_) => {},
            Err(err) => return Err(eyre::eyre!("failed to parse sitemap XML: {err}")),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename = "sitemapindex")]
struct SitemapIndex {
    #[serde(rename = "sitemap", default)]
    sitemaps: Vec<SitemapLoc>,
}

#[derive(Debug, Deserialize)]
struct SitemapLoc {
    loc: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "urlset")]
struct UrlSet {
    #[serde(rename = "url", default)]
    urls: Vec<UrlLoc>,
}

#[derive(Debug, Deserialize)]
struct UrlLoc {
    loc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrawlStatus {
    pub id: String,
    pub pid: u32,
    pub seed: String,
    pub status: String,
    pub pages: usize,
    pub new: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub errors: usize,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub error: String,
}

pub fn generate_crawl_id() -> String {
    let value: u32 = rand::random();
    format!("c_{value:08x}")
}

pub fn status_dir() -> eyre::Result<PathBuf> {
    Ok(config::cache_dir()?.join("crawls"))
}

pub fn status_path(id: &str) -> eyre::Result<PathBuf> {
    Ok(status_dir()?.join(format!("{id}.json")))
}

pub fn write_status(status: &mut CrawlStatus) -> eyre::Result<()> {
    let path = status_path(&status.id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    status.updated_at = Utc::now();
    let data = serde_json::to_string_pretty(status)?;
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, format!("{data}\n"))?;
    Ok(fs::rename(tmp_path, path)?)
}

pub fn read_status(id: &str) -> eyre::Result<CrawlStatus> {
    Ok(serde_json::from_str(&fs::read_to_string(status_path(
        id,
    )?)?)?)
}

pub fn list_statuses() -> eyre::Result<Vec<CrawlStatus>> {
    let dir = status_dir()?;
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };

    let mut statuses = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Ok(status) = serde_json::from_str::<CrawlStatus>(&fs::read_to_string(entry.path())?)
        {
            statuses.push(status);
        }
    }
    statuses.sort_by_key(|status| std::cmp::Reverse(status.updated_at));
    Ok(statuses)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use regex::Regex;

    use crate::cache::PageCache;
    use crate::scrape::{SOURCE_HTTP, Scraper};
    use crate::testserver::TestHttpServer;
    use crate::types::Page;
    use crate::urlrewrite::{Rewriter, Rule};

    use super::{
        CrawlOptions, HostJsStats, compile_deny, crawl_with_callback, extract_links, fetch_sitemap,
        normalize_url, passes_filters, record_js_detection, should_force_browser,
    };

    #[test]
    fn normalizes_urls() {
        for (input, expected) in [
            (
                "https://example.com/page#section",
                "https://example.com/page",
            ),
            (
                "https://example.com/page?utm_source=twitter&id=1",
                "https://example.com/page?id=1",
            ),
            (
                "https://example.com/?utm_medium=email",
                "https://example.com",
            ),
            (
                "https://example.com/p?utm_source=a&utm_medium=b&utm_campaign=c&keep=1",
                "https://example.com/p?keep=1",
            ),
            ("https://example.com/page/", "https://example.com/page"),
            ("https://example.com/page/#top", "https://example.com/page"),
            (
                "https://example.com/search?q=test&page=2",
                "https://example.com/search?page=2&q=test",
            ),
            ("", ""),
        ] {
            assert_eq!(normalize_url(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn filters_same_host_allow_and_deny() {
        let deny = vec![
            Regex::new("/admin").unwrap(),
            Regex::new(r"\.pdf$").unwrap(),
        ];
        assert!(passes_filters(
            "https://example.com/docs",
            "example.com",
            &["/docs".to_string()],
            &deny
        ));
        assert!(!passes_filters(
            "https://example.com/admin",
            "example.com",
            &[],
            &deny
        ));
        assert!(!passes_filters(
            "https://other.com/docs",
            "example.com",
            &[],
            &[]
        ));
        assert!(!passes_filters(
            "https://sub.example.com/docs",
            "example.com",
            &[],
            &[]
        ));
        assert!(passes_filters(
            "https://example.com/api/v1/users",
            "example.com",
            &["/docs".to_string(), "/api".to_string()],
            &[]
        ));
        assert!(!passes_filters(
            "https://example.com/blog/post",
            "example.com",
            &["/docs".to_string(), "/api".to_string()],
            &[]
        ));
        assert!(!passes_filters(
            "https://example.com/doc.pdf",
            "example.com",
            &[],
            &deny
        ));
    }

    #[test]
    fn compiles_deny_patterns() {
        assert_eq!(
            compile_deny(&["/admin".to_string(), r"\.pdf$".to_string()])
                .unwrap()
                .len(),
            2
        );
        assert!(compile_deny(&["[invalid".to_string()]).is_err());
    }

    #[test]
    fn extracts_resolved_links() {
        let links = extract_links(
            "https://example.com/docs/page",
            r##"
            <a href="/about">About</a>
            <a href="https://example.com/contact">Contact</a>
            <a href="../other">Other</a>
            <a href="javascript:void(0)">JS</a>
            <a href="mailto:a@b">Email</a>
            <a href="tel:+1234567890">Phone</a>
            <a href="#section">Anchor</a>
            <a href="https://external.com/page">External</a>
            "##,
        );
        assert!(links.contains(&"https://example.com/about".to_string()));
        assert!(links.contains(&"https://example.com/contact".to_string()));
        assert!(links.contains(&"https://example.com/other".to_string()));
        assert!(links.contains(&"https://external.com/page".to_string()));
        assert_eq!(links.len(), 4);
    }

    #[tokio::test]
    async fn fetches_urlset_sitemaps() {
        let server = TestHttpServer::routes_with_root_fallback(HashMap::from([(
            "/sitemap.xml".to_string(),
            r#"
            <urlset>
                <url><loc>https://example.com/page1</loc></url>
                <url><loc>https://example.com/page2</loc></url>
                <url><loc>https://example.com/page3</loc></url>
            </urlset>
            "#
            .to_string(),
        )]));

        let mut urls = fetch_sitemap(&format!("{}/sitemap.xml", server.url))
            .await
            .unwrap();
        urls.sort();
        assert_eq!(
            urls,
            vec![
                "https://example.com/page1".to_string(),
                "https://example.com/page2".to_string(),
                "https://example.com/page3".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn fetches_default_sitemap_from_site_root() {
        let server = TestHttpServer::routes(HashMap::from([(
            "/sitemap.xml".to_string(),
            r#"<urlset><url><loc>https://example.com/from-root</loc></url></urlset>"#.to_string(),
        )]));

        let urls = fetch_sitemap(&server.url).await.unwrap();
        assert_eq!(urls, vec!["https://example.com/from-root".to_string()]);
        assert_eq!(server.paths(), vec!["/robots.txt", "/sitemap.xml"]);
    }

    #[tokio::test]
    async fn fetches_robots_sitemap_locations() {
        let server = TestHttpServer::routes(HashMap::from([
            (
                "/robots.txt".to_string(),
                "User-agent: *\nSitemap: SERVER/custom-sitemap.xml # primary sitemap\n".to_string(),
            ),
            (
                "/custom-sitemap.xml".to_string(),
                r#"<urlset><url><loc>https://example.com/from-robots</loc></url></urlset>"#
                    .to_string(),
            ),
        ]));
        server.replace_route_token("SERVER");

        let urls = fetch_sitemap(&server.url).await.unwrap();
        assert_eq!(urls, vec!["https://example.com/from-robots".to_string()]);
        assert_eq!(server.paths(), vec!["/robots.txt", "/custom-sitemap.xml"]);
    }

    #[tokio::test]
    async fn fetches_sitemap_indexes() {
        let server = TestHttpServer::routes_with_root_fallback(HashMap::from([
            (
                "/sitemap.xml".to_string(),
                r#"<sitemapindex><sitemap><loc>SERVER/sitemap-pages.xml</loc></sitemap></sitemapindex>"#.to_string(),
            ),
            (
                "/sitemap-pages.xml".to_string(),
                r#"<urlset><url><loc>https://example.com/from-index</loc></url></urlset>"#.to_string(),
            ),
        ]));
        server.replace_route_token("SERVER");

        let urls = fetch_sitemap(&format!("{}/sitemap.xml", server.url))
            .await
            .unwrap();
        assert_eq!(urls, vec!["https://example.com/from-index".to_string()]);
    }

    #[tokio::test]
    async fn fetches_sitemap_index_from_site_root() {
        let server = TestHttpServer::routes(HashMap::from([
            (
                "/sitemap-index.xml".to_string(),
                r#"<sitemapindex><sitemap><loc>SERVER/sitemap-0.xml</loc></sitemap></sitemapindex>"#.to_string(),
            ),
            (
                "/sitemap-0.xml".to_string(),
                r#"<urlset><url><loc>https://example.com/from-index-root</loc></url></urlset>"#
                    .to_string(),
            ),
        ]));
        server.replace_route_token("SERVER");

        let urls = fetch_sitemap(&server.url).await.unwrap();
        assert_eq!(
            urls,
            vec!["https://example.com/from-index-root".to_string()]
        );
        assert_eq!(
            server.paths(),
            vec![
                "/robots.txt",
                "/sitemap.xml",
                "/sitemap-index.xml",
                "/sitemap-0.xml",
            ]
        );
    }

    #[tokio::test]
    async fn reports_html_documents_as_non_sitemaps() {
        let server = TestHttpServer::routes_with_root_fallback(HashMap::from([(
            "/".to_string(),
            r#"<!doctype html><html><head><style>@media(min-width:32.5rem){.rail[data-astro-cid-rff6ndou]}</style></head><body></body></html>"#
                .to_string(),
        )]));

        let err = fetch_sitemap(&server.url).await.unwrap_err().to_string();
        assert!(err.contains("expected sitemap XML root <urlset> or <sitemapindex>"));
        assert!(err.contains("found <html>"));
    }

    #[tokio::test]
    async fn crawl_scheduler_visits_each_page_once() {
        let pages = ["/", "/a", "/b", "/c", "/d"];
        let mut routes = HashMap::new();
        for path in pages {
            let mut body =
                String::from("<!doctype html><html><body><main><article><h1>Page</h1><p>");
            body.push_str(&"content ".repeat(40));
            body.push_str("</p>");
            for link in pages {
                body.push_str(&format!(r#"<a href="{link}">link</a>"#));
            }
            body.push_str("</article></main></body></html>");
            routes.insert(path.to_string(), body);
        }
        let server = TestHttpServer::routes_with_root_fallback(routes);
        let scraper = Scraper::new(String::new(), None).unwrap();
        let mut callback_urls = Vec::new();

        let results = crawl_with_callback(
            &server.url,
            &scraper,
            CrawlOptions {
                depth: 5,
                concurrency: 4,
                allow: Vec::new(),
                deny: Vec::new(),
            },
            None,
            false,
            |result| callback_urls.push(result.url.clone()),
        )
        .await
        .unwrap();

        assert_eq!(callback_urls.len(), results.len());
        let mut seen = HashMap::new();
        for result in results {
            assert!(result.error.is_empty(), "{}", result.error);
            *seen.entry(result.url).or_insert(0usize) += 1;
        }
        assert_eq!(seen.len(), pages.len());
        assert!(seen.contains_key(&server.url));
        for count in seen.values() {
            assert_eq!(*count, 1);
        }
    }

    #[tokio::test]
    async fn crawl_host_rewrite_preserves_original_url_and_records_fetched_url() {
        let server = TestHttpServer::routes_with_root_fallback(HashMap::from([(
            "/start".to_string(),
            format!(
                "<!doctype html><html><head><title>Rewritten</title></head><body><main><p>{}</p></main></body></html>",
                "rewritten content ".repeat(30)
            ),
        )]));
        let original = "http://seed.test/start";
        let fetched = format!("{}/start", server.url);
        let rewriter = Rewriter::new(&[Rule {
            r#match: "^http://seed\\.test/start$".to_string(),
            replace: fetched.clone(),
        }])
        .unwrap();
        let scraper = Scraper::new(String::new(), rewriter).unwrap();

        let results = crawl_with_callback(
            original,
            &scraper,
            CrawlOptions {
                depth: 0,
                concurrency: 1,
                allow: Vec::new(),
                deny: Vec::new(),
            },
            None,
            false,
            |_| {},
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, original);
        let page = results[0].page.as_ref().expect("rewritten crawl has page");
        assert_eq!(page.url, original);
        assert_eq!(page.fetched_url, fetched);
        assert_eq!(page.title, "Rewritten");
    }

    #[tokio::test]
    async fn stale_cache_refetch_reports_changed_content() {
        let server = TestHttpServer::routes_with_root_fallback(HashMap::from([(
            "/".to_string(),
            "<!doctype html><html><head><title>New</title></head><body><main><p>new crawled content with enough words for extraction to keep this page body</p></main></body></html>".to_string(),
        )]));
        let cache = test_cache(Duration::from_nanos(1), "changed");
        cache.put(
            &server.url,
            &Page {
                url: server.url.clone(),
                title: "Old".to_string(),
                markdown: "old cached content".to_string(),
                ..Page::default()
            },
            SOURCE_HTTP,
        );
        std::thread::sleep(Duration::from_millis(10));

        let scraper = Scraper::new(String::new(), None).unwrap();
        let results = crawl_with_callback(
            &server.url,
            &scraper,
            CrawlOptions {
                depth: 0,
                concurrency: 1,
                allow: Vec::new(),
                deny: Vec::new(),
            },
            Some(&cache),
            false,
            |_| {},
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "changed");
        let page = results[0].page.as_ref().expect("changed result has page");
        assert_eq!(page.title, "New");
        assert!(page.markdown.contains("new crawled content"));
    }

    #[test]
    fn forces_browser_for_shell_heavy_hosts_after_enough_samples() {
        let stats = Arc::new(Mutex::new(HashMap::<String, HostJsStats>::new()));
        for _ in 0..9 {
            record_js_detection(&stats, "https://example.com/page", "likely_shell");
        }
        record_js_detection(&stats, "https://example.com/other", "static");

        assert!(should_force_browser(&stats, "https://example.com/new"));
        assert!(!should_force_browser(&stats, "https://other.example/new"));
    }

    #[test]
    fn does_not_force_browser_before_threshold_or_at_exact_eighty_percent() {
        let under_sampled = Arc::new(Mutex::new(HashMap::<String, HostJsStats>::new()));
        for _ in 0..9 {
            record_js_detection(&under_sampled, "https://example.com/page", "likely_shell");
        }
        assert!(!should_force_browser(
            &under_sampled,
            "https://example.com/new"
        ));

        let exact_threshold = Arc::new(Mutex::new(HashMap::<String, HostJsStats>::new()));
        for _ in 0..8 {
            record_js_detection(&exact_threshold, "https://example.com/page", "likely_shell");
        }
        for _ in 0..2 {
            record_js_detection(&exact_threshold, "https://example.com/page", "static");
        }
        assert!(!should_force_browser(
            &exact_threshold,
            "https://example.com/new"
        ));
    }

    fn test_cache(ttl: Duration, name: &str) -> PageCache {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("crawl-tests")
            .join(format!("{name}-{}.json", std::process::id()));
        PageCache::for_path(path, ttl)
    }
}
