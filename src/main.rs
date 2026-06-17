mod cache;
mod code_search;
mod config;
mod crawl;
mod docs;
mod error;
mod extract;
mod fs_atomic;
mod headers;
mod http;
mod scrape;
mod search;
#[cfg(test)]
mod testserver;
mod types;
mod updatecheck;
mod urlrewrite;

use std::fs;
use std::io::{self, IsTerminal, Read};
use std::process::{Command as ProcessCommand, ExitCode, Stdio};
use std::time::Instant;

use chrono::Utc;
use clap::{Args, Parser, Subcommand};
use color_eyre::eyre;
use futures_util::StreamExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::cache::PageCache;
use crate::code_search::CodeQuery;
use crate::config::{AppConfig, CodeBackend, DocsBackend, SearchBackend};
use crate::crawl::CrawlOptions;
use crate::error::{AppError, AppResult};
use crate::scrape::Scraper;
use crate::types::{CodeResult, DocsResult, Page, SearchResult};

const EXIT_VALIDATION: u8 = 2;
const EXIT_NOT_FOUND: u8 = 3;
const EXIT_UPSTREAM: u8 = 4;
const EXIT_PRECONDITION: u8 = 5;
const EXIT_CANCELLED: u8 = 6;

#[derive(Debug)]
struct CliError {
    code: u8,
    message: String,
}

impl CliError {
    fn validation(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_VALIDATION,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_NOT_FOUND,
            message: message.into(),
        }
    }

    fn upstream(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_UPSTREAM,
            message: message.into(),
        }
    }

    fn precondition(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_PRECONDITION,
            message: message.into(),
        }
    }

    fn cancelled(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_CANCELLED,
            message: message.into(),
        }
    }
}

impl From<eyre::Report> for CliError {
    fn from(error: eyre::Report) -> Self {
        Self::upstream(error.to_string())
    }
}

impl From<AppError> for CliError {
    fn from(error: AppError) -> Self {
        match error {
            AppError::Validation(message) => Self::validation(message),
            AppError::NotFound(message) => Self::not_found(message),
            AppError::Upstream(message) => Self::upstream(message),
            AppError::Precondition(message) => Self::precondition(message),
            AppError::RegexUnsupported => Self::precondition("backend does not support --regex"),
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "snarf",
    version,
    about = "Fast web search and scrape for workflows",
    long_about = "snarf is a fast CLI for search and scrape workflows. Search the web, search code, search docs, scrape pages to clean markdown, or crawl a site."
)]
struct Cli {
    #[arg(long, global = true, help = "output as JSON")]
    json: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    #[command(about = "Search the web and return results")]
    Search(SearchArgs),
    #[command(about = "Search code across open-source repositories")]
    Code(CodeArgs),
    #[command(about = "Search library documentation")]
    Docs(DocsArgs),
    #[command(about = "Scrape URLs and extract clean markdown")]
    Scrape(ScrapeArgs),
    #[command(about = "Crawl a site and extract pages")]
    Crawl(CrawlArgs),
    #[command(about = "Manage browser for JS-rendered page support")]
    Browser(BrowserArgs),
    #[command(about = "Show or manage configuration")]
    Config(ConfigArgs),
    #[command(about = "Show cache stats")]
    Cache(CacheArgs),
    #[command(about = "Print snarf version information")]
    Version,
}

#[derive(Args, Debug)]
struct SearchArgs {
    #[arg(required = true, help = "search query")]
    query: Vec<String>,
    #[arg(short = 'b', long, value_enum, help = "search backend")]
    backend: Option<SearchBackend>,
    #[arg(short = 'l', long, help = "max number of results")]
    limit: Option<usize>,
    #[arg(long, help = "scrape full content from each result")]
    scrape: bool,
    #[arg(long, help = "SearXNG instance URL")]
    searxng_url: Option<String>,
    #[arg(
        long,
        default_value_t = 0,
        help = "truncate markdown output to N chars (0 = disabled)"
    )]
    max_chars: usize,
    #[arg(long, help = "strip markdown formatting, keep content text only")]
    trim: bool,
    #[arg(long, help = "one result per line, tab-separated (url/title/snippet)")]
    minimal: bool,
}

#[derive(Args, Debug)]
struct CodeArgs {
    #[arg(required = true, help = "code search query")]
    query: Vec<String>,
    #[arg(short = 'b', long, value_enum, help = "code search backend")]
    backend: Option<CodeBackend>,
    #[arg(long, default_value = "", help = "language filter (appended to query)")]
    lang: String,
    #[arg(
        long,
        help = "interpret query as a regular expression (grepapp, sourcegraph)"
    )]
    regex: bool,
    #[arg(short = 'l', long, help = "max number of results")]
    limit: Option<usize>,
    #[arg(long, help = "one result per line, tab-separated (url/repo/snippet)")]
    minimal: bool,
}

#[derive(Args, Debug)]
struct DocsArgs {
    #[arg(required = true, help = "documentation query")]
    query: Vec<String>,
    #[arg(short = 'b', long, value_enum, help = "docs backend")]
    backend: Option<DocsBackend>,
    #[arg(
        long,
        default_value = "",
        help = "Context7 library ID (skip resolve step)"
    )]
    library: String,
    #[arg(long, default_value_t = 4000, help = "Context7 token budget")]
    tokens: usize,
    #[arg(short = 'l', long, help = "max number of results")]
    limit: Option<usize>,
    #[arg(long, help = "resolve library name instead of searching")]
    resolve: bool,
    #[arg(
        long,
        help = "one result per line, tab-separated (url/library/snippet)"
    )]
    minimal: bool,
}

#[derive(Args, Debug)]
struct ScrapeArgs {
    #[arg(help = "URL, file path, or JSON array; with no args, reads piped stdin")]
    urls: Vec<String>,
    #[arg(long, help = "output raw HTML instead of markdown")]
    raw: bool,
    #[arg(long, help = "bypass the page cache")]
    no_cache: bool,
    #[arg(
        long,
        default_value_t = 0,
        help = "truncate markdown output to N chars (0 = disabled)"
    )]
    max_chars: usize,
    #[arg(long, help = "strip markdown formatting, keep content text only")]
    trim: bool,
    #[arg(
        long,
        default_value = "",
        help = "CSS selector to extract specific elements (skips readability)"
    )]
    select: String,
    #[arg(long, help = "disable automatic /llms.txt detection for bare domains")]
    no_llms_txt: bool,
    #[arg(
        long,
        default_value_t = 5,
        help = "max concurrent requests for multi-URL scraping"
    )]
    concurrency: usize,
}

#[derive(Debug, Clone)]
struct ScrapeOutputOptions {
    raw: bool,
    trim: bool,
    max_chars: usize,
    select: String,
    no_llms_txt: bool,
}

#[derive(Args, Debug)]
struct CrawlArgs {
    #[command(subcommand)]
    command: Option<CrawlCommand>,
    #[arg(help = "seed URL")]
    url: Option<String>,
    #[arg(long, default_value_t = 3, help = "max BFS depth")]
    depth: usize,
    #[arg(long, default_value_t = 8, help = "worker pool size")]
    concurrency: usize,
    #[arg(
        long,
        value_delimiter = ',',
        help = "path substring filters (any match passes)"
    )]
    allow: Vec<String>,
    #[arg(long, value_delimiter = ',', help = "regex deny patterns")]
    deny: Vec<String>,
    #[arg(long, help = "treat seed URL as sitemap")]
    sitemap: bool,
    #[arg(long, help = "bypass the page cache")]
    no_cache: bool,
    #[arg(
        long,
        help = "run crawl in background, return immediately with crawl ID"
    )]
    background: bool,
}

#[derive(Subcommand, Debug)]
enum CrawlCommand {
    #[command(about = "Show background crawl status")]
    Status { id: Option<String> },
    #[command(about = "Stop a running background crawl")]
    Stop { id: String },
}

#[derive(Args, Debug)]
struct BrowserArgs {
    #[command(subcommand)]
    command: Option<BrowserCommand>,
}

#[derive(Subcommand, Debug)]
enum BrowserCommand {
    #[command(about = "Download Chromium for headless rendering")]
    Install,
    #[command(about = "Check browser configuration and availability")]
    Status,
}

#[derive(Args, Debug)]
struct ConfigArgs {
    #[command(subcommand)]
    command: Option<ConfigCommand>,
}

#[derive(Subcommand, Debug)]
enum ConfigCommand {
    #[command(about = "Create a default config file")]
    Init,
    #[command(about = "Set a config value")]
    Set { key: String, value: String },
    #[command(about = "Print the config file path")]
    Path,
}

#[derive(Args, Debug)]
struct CacheArgs {
    #[command(subcommand)]
    command: Option<CacheCommand>,
}

#[derive(Subcommand, Debug)]
enum CacheCommand {
    #[command(about = "Remove all cached pages")]
    Clear,
}

#[derive(serde::Serialize)]
struct ConfigInfo {
    config_path: std::path::PathBuf,
    backend: SearchBackend,
    searxng_url: String,
    limit: usize,
    cache_ttl: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    browser: String,
    code_backend: CodeBackend,
    docs_backend: DocsBackend,
    sourcegraph_url: String,
    github_token_source: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    url_rewrites: Vec<urlrewrite::Rule>,
    available_backends: Vec<&'static str>,
    available_code_backends: Vec<&'static str>,
    available_doc_backends: Vec<&'static str>,
}

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(error) = color_eyre::install() {
        eprintln!("error: {error}");
        return ExitCode::from(EXIT_UPSTREAM);
    }

    match run_until_signal().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {}", error.message);
            ExitCode::from(error.code)
        }
    }
}

async fn run_until_signal() -> Result<(), CliError> {
    let cli = Cli::parse();
    if handles_shutdown_signal_in_command(&cli) {
        return run(cli).await;
    }

    tokio::select! {
        result = run(cli) => result,
        signal = shutdown_signal() => {
            mark_background_worker_stopped_on_cancel();
            Err(CliError::cancelled(format!("cancelled by {signal}")))
        }
    }
}

fn handles_shutdown_signal_in_command(cli: &Cli) -> bool {
    if std::env::var_os("SNARF_CRAWL_WORKER").is_some() {
        return true;
    }
    matches!(
        &cli.command,
        Some(Command::Crawl(CrawlArgs {
            command: None,
            background: false,
            ..
        }))
    )
}

async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate()).expect("SIGTERM handler installs");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => "SIGINT",
            _ = terminate.recv() => "SIGTERM",
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("Ctrl-C handler installs");
        "SIGINT"
    }
}

fn mark_background_worker_stopped_on_cancel() {
    let Ok(crawl_id) = std::env::var("SNARF_CRAWL_WORKER") else {
        return;
    };
    let Ok(mut status) = crawl::read_status(&crawl_id) else {
        return;
    };
    if status.status == "completed" || status.status == "failed" || status.status == "stopped" {
        return;
    }
    status.status = "stopped".to_string();
    let _ = crawl::write_status(&mut status);
}

async fn run(cli: Cli) -> Result<(), CliError> {
    let cfg = AppConfig::load();
    let as_json = cli.json;
    let passive_update = prepare_passive_update_notice(&cli, as_json).await;

    let result = match cli.command {
        Some(Command::Search(args)) => run_search(as_json, &cfg, args).await,
        Some(Command::Code(args)) => run_code(as_json, &cfg, args).await,
        Some(Command::Docs(args)) => run_docs(as_json, &cfg, args).await,
        Some(Command::Scrape(args)) => run_scrape(as_json, &cfg, args).await,
        Some(Command::Crawl(args)) => run_crawl(as_json, &cfg, args).await,
        Some(Command::Browser(args)) => run_browser(as_json, &cfg, args).await,
        Some(Command::Config(args)) => run_config(as_json, &cfg, args).await,
        Some(Command::Cache(args)) => run_cache(as_json, &cfg, args).await,
        Some(Command::Version) => run_version(as_json).await,
        None => {
            print_root(&cfg);
            Ok(())
        }
    };
    if result.is_ok() {
        emit_passive_update_notice(passive_update);
    }
    result
}

async fn prepare_passive_update_notice(
    cli: &Cli,
    as_json: bool,
) -> Option<updatecheck::UpdateStatus> {
    if should_skip_passive_update_notice(cli, as_json) {
        return None;
    }

    let status = updatecheck::get_status(updatecheck::Options {
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        allow_network: true,
        timeout: std::time::Duration::from_millis(400),
    })
    .await;
    if status.available && updatecheck::should_notify(&status) {
        Some(status)
    } else {
        None
    }
}

fn emit_passive_update_notice(status: Option<updatecheck::UpdateStatus>) {
    let Some(status) = status else {
        return;
    };
    let notice = updatecheck::format_notice(&status);
    if notice.is_empty() {
        return;
    }
    eprintln!("{notice}");
    let _ = updatecheck::mark_notified(&status);
}

fn should_skip_passive_update_notice(cli: &Cli, as_json: bool) -> bool {
    if as_json || updatecheck::disabled() || !io::stderr().is_terminal() {
        return true;
    }
    if command_name(&cli.command) == Some("version") {
        return true;
    }
    let ci = std::env::var("CI")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    ci == "1" || ci == "true"
}

fn command_name(command: &Option<Command>) -> Option<&'static str> {
    Some(match command {
        Some(Command::Search(_)) => "search",
        Some(Command::Code(_)) => "code",
        Some(Command::Docs(_)) => "docs",
        Some(Command::Scrape(_)) => "scrape",
        Some(Command::Crawl(_)) => "crawl",
        Some(Command::Browser(_)) => "browser",
        Some(Command::Config(_)) => "config",
        Some(Command::Cache(_)) => "cache",
        Some(Command::Version) => "version",
        None => "root",
    })
}

fn print_root(cfg: &AppConfig) {
    println!("snarf - web search, code search, docs, and scrape in one binary.\n");
    println!("Commands:");
    println!("  search      Search the web and return results");
    println!("  code        Search code across open-source repositories");
    println!("  docs        Search library documentation");
    println!("  scrape      Scrape URLs and extract clean markdown");
    println!("  crawl       Crawl a site and extract pages");
    println!("  browser     Manage browser for JS-rendered page support");
    println!("  cache       Show cache stats");
    println!("  config      Show or manage configuration");
    println!("  version     Print snarf version information");
    println!("\nBackends:");
    println!(
        "  search      {}",
        join_with_default(&config::available_backends(), cfg.backend)
    );
    println!(
        "  code        {}",
        join_with_default(&config::available_code_backends(), cfg.code_backend)
    );
    println!(
        "  docs        {}",
        join_with_default(&config::available_doc_backends(), cfg.docs_backend)
    );
    println!("\nRun 'snarf <command> --help' for flags and examples.");
}

fn join_with_default(backends: &[&str], active: impl std::fmt::Display) -> String {
    let active = active.to_string();
    backends
        .iter()
        .map(|backend| {
            if *backend == active.as_str() {
                format!("{backend} (default)")
            } else {
                (*backend).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

async fn run_search(as_json: bool, cfg: &AppConfig, args: SearchArgs) -> Result<(), CliError> {
    let query = args.query.join(" ");
    let backend = args.backend.unwrap_or(cfg.backend);
    let limit = args.limit.unwrap_or(cfg.limit);
    let searxng_url = args.searxng_url.unwrap_or_else(|| cfg.searxng_url.clone());
    let results = search::search(
        backend,
        &query,
        limit,
        &searxng_url,
        &cfg.brave_api_key,
        &cfg.exa_api_key,
    )
    .await
    .map_err(|err| CliError::from(err.with_upstream_context("search failed")))?;

    if args.scrape {
        return run_search_scrape(
            as_json,
            cfg,
            results,
            args.trim,
            args.max_chars,
            args.minimal,
        )
        .await;
    }

    if as_json {
        print_json(&results)?;
    } else if args.minimal {
        for result in results {
            println!("{}\t{}\t{}", result.url, result.title, result.description);
        }
    } else {
        println!("---");
        println!("query: {query}");
        println!("backend: {backend}");
        println!("result_count: {}", results.len());
        println!("---");
        for result in results {
            println!("{}\n  {}", result.title, result.url);
            if !result.description.is_empty() {
                println!("  {}", result.description);
            }
            println!();
        }
    }
    Ok(())
}

async fn run_search_scrape(
    as_json: bool,
    cfg: &AppConfig,
    mut results: Vec<SearchResult>,
    trim: bool,
    max_chars: usize,
    minimal: bool,
) -> Result<(), CliError> {
    let scraper = new_scraper(cfg)?;
    let cache = new_page_cache(cfg, false)?;

    if as_json {
        for result in &mut results {
            match cached_scrape(&scraper, cache.as_ref(), &result.url).await {
                Ok(mut page) => {
                    result.fetched_url = page.fetched_url.clone();
                    page.markdown = extract::post_process(&page.markdown, trim, max_chars);
                    result.content = page.markdown;
                }
                Err(err) => eprintln!("warn: failed to scrape {}: {err}", result.url),
            }
        }
        return print_json(&results);
    }

    for (index, result) in results.iter().enumerate() {
        match cached_scrape(&scraper, cache.as_ref(), &result.url).await {
            Ok(mut page) => {
                page.markdown = extract::post_process(&page.markdown, trim, max_chars);
                if minimal {
                    println!(
                        "{}\t{}\t{}",
                        result.url,
                        page.title,
                        first_line(&page.markdown)
                    );
                } else {
                    if index > 0 {
                        println!();
                    }
                    print_page(&page);
                }
            }
            Err(err) => eprintln!("warn: failed to scrape {}: {err}", result.url),
        }
    }
    Ok(())
}

async fn run_code(as_json: bool, cfg: &AppConfig, args: CodeArgs) -> Result<(), CliError> {
    let query = args.query.join(" ");
    let backend = args.backend.unwrap_or(cfg.code_backend);
    let limit = args.limit.unwrap_or(cfg.limit);
    let token = if backend == CodeBackend::Github {
        let (token, _) = cfg.resolve_github_token();
        if token.is_empty() {
            return Err(CliError::precondition(
                "github code search: no token found.\n  - explicit:   snarf config set github_token <token>\n  - env var:    export GITHUB_TOKEN=<token>\n  - or run:     gh auth login",
            ));
        }
        token
    } else {
        String::new()
    };

    let results = code_search::search(
        backend,
        CodeQuery {
            term: query.clone(),
            lang: args.lang.clone(),
            limit,
            regexp: args.regex,
        },
        &cfg.sourcegraph_url,
        &token,
    )
    .await
    .map_err(|err| {
        if matches!(err, AppError::RegexUnsupported) {
            return CliError::precondition(format!(
                "backend {backend} does not support --regex (try -b grepapp or -b sourcegraph)"
            ));
        }
        CliError::from(err.with_upstream_context("code search failed"))
    })?;

    if as_json {
        print_json(&results)?;
    } else if args.minimal {
        for result in results {
            println!(
                "{}\t{}\t{}",
                result.url,
                result.repo,
                first_line(&result.snippet)
            );
        }
    } else {
        println!("---");
        println!("query: {query}");
        if !args.lang.is_empty() {
            println!("lang: {}", args.lang);
        }
        println!("backend: {backend}");
        println!("result_count: {}", results.len());
        println!("---");
        for result in results {
            println!("{}", code_result_header(&result));
            if !result.snippet.is_empty() {
                println!("  {}", result.snippet);
            }
            println!("  {}\n", result.url);
        }
    }
    Ok(())
}

fn code_result_header(result: &CodeResult) -> String {
    let mut header = format!("{}  {}", result.repo, result.path);
    if result.line > 0 {
        header.push_str(&format!("  (line {})", result.line));
    }
    if result.stars > 0 {
        header.push_str(&format!("  \u{2605} {}", result.stars));
    }
    header
}

async fn run_docs(as_json: bool, cfg: &AppConfig, args: DocsArgs) -> Result<(), CliError> {
    let query = args.query.join(" ");
    let backend = args.backend.unwrap_or(cfg.docs_backend);
    let limit = args.limit.unwrap_or(cfg.limit);

    if args.resolve {
        let matches = docs::resolve(&query, &cfg.context7_api_key)
            .await
            .map_err(|err| CliError::from(err.with_upstream_context("resolve failed")))?;
        if as_json {
            print_json(&matches)?;
        } else {
            for item in matches {
                println!(
                    "{}  {}  (snippets: {}, trust: {:.1})",
                    item.id, item.title, item.total_snippets, item.trust_score
                );
            }
        }
        return Ok(());
    }

    let results = if !args.library.is_empty() && backend == DocsBackend::Context7 {
        docs::docs_for_library(&query, &args.library, args.tokens, &cfg.context7_api_key)
            .await
            .map_err(|err| CliError::from(err.with_upstream_context("docs fetch failed")))?
    } else {
        docs::search(backend, &query, limit, &cfg.context7_api_key)
            .await
            .map_err(|err| CliError::from(err.with_upstream_context("docs search failed")))?
    };
    let results = limit_docs_results(results, limit);

    if as_json {
        print_json(&results)?;
    } else {
        print_docs_results(&query, backend, &args.library, &results, args.minimal);
    }
    Ok(())
}

fn print_docs_results(
    query: &str,
    backend: DocsBackend,
    library: &str,
    results: &[DocsResult],
    minimal: bool,
) {
    if minimal {
        for result in results {
            println!(
                "{}\t{}\t{}",
                result.url,
                result.library,
                first_line(&result.snippet)
            );
        }
        return;
    }

    println!("---");
    println!("query: {query}");
    println!("backend: {backend}");
    if !library.is_empty() {
        println!("library: {library}");
    } else if let Some(first) = results.first()
        && !first.library.is_empty()
    {
        println!("library: {}", first.library);
    }
    println!("result_count: {}", results.len());
    println!("---");
    for result in results {
        let label = if result.breadcrumb.is_empty() {
            &result.title
        } else {
            &result.breadcrumb
        };
        println!("[{label}]");
        println!("  {}", result.snippet);
        println!("  source: {}\n", result.url);
    }
}

fn limit_docs_results(mut results: Vec<DocsResult>, limit: usize) -> Vec<DocsResult> {
    results.truncate(limit);
    results
}

async fn run_scrape(as_json: bool, cfg: &AppConfig, args: ScrapeArgs) -> Result<(), CliError> {
    let urls = resolve_urls(&args.urls)?;
    validate_at_least_one("concurrency", args.concurrency)?;
    let single_url = urls.len() == 1;
    let options = ScrapeOutputOptions {
        raw: args.raw,
        trim: args.trim,
        max_chars: args.max_chars,
        select: args.select,
        no_llms_txt: args.no_llms_txt,
    };
    let scraper = new_scraper(cfg)?;
    let cache = new_page_cache(
        cfg,
        args.no_cache || options.raw || !options.select.is_empty(),
    )?;
    let client =
        http::client(http::FETCH_TIMEOUT).map_err(|err| CliError::upstream(err.to_string()))?;

    let pages = if single_url {
        let raw_url = urls.first().expect("single URL exists");
        let page = scrape_one_url(&scraper, cache.clone(), &client, raw_url, &options)
            .await
            .map_err(|err| CliError::from(err.with_upstream_context("scrape failed")))?;
        vec![page]
    } else {
        scrape_many_urls(&scraper, cache, &client, urls, &options, args.concurrency).await?
    };

    if as_json {
        if single_url {
            print_json(&pages[0])?;
        } else {
            print_json(&pages)?;
        }
    } else {
        for (index, page) in pages.iter().enumerate() {
            if index > 0 {
                println!();
            }
            print_page(page);
        }
    }
    Ok(())
}

async fn scrape_one_url(
    scraper: &Scraper,
    cache: Option<PageCache>,
    client: &reqwest::Client,
    raw_url: &str,
    options: &ScrapeOutputOptions,
) -> AppResult<Page> {
    if options.raw {
        return scrape_raw_url(scraper, raw_url, options).await;
    }

    let mut page = scrape_page(scraper, cache.as_ref(), client, raw_url, options).await?;
    page.markdown = extract::post_process(&page.markdown, options.trim, options.max_chars);
    Ok(page)
}

async fn scrape_raw_url(
    scraper: &Scraper,
    raw_url: &str,
    options: &ScrapeOutputOptions,
) -> AppResult<Page> {
    let (html, fetched_url) = if options.select.is_empty() {
        let fetched_url = scraper.rewrite(raw_url);
        let html = scraper
            .fetch(&fetched_url)
            .await
            .map_err(|err| eyre::eyre!("raw fetch failed for {raw_url}: {err}"))?;
        (html, fetched_url)
    } else {
        scraper.fetch_with_browser_fallback(raw_url).await?
    };

    let markdown = if options.select.is_empty() {
        html.clone()
    } else {
        let selected = extract::extract_selector_html(&html, &options.select)?;
        if selected.is_empty() {
            return Err(AppError::not_found(format!(
                "no elements matched selector {:?}",
                options.select
            )));
        }
        selected
    };

    let mut page = Page {
        url: raw_url.to_string(),
        title: extract::title(&html),
        markdown,
        ..Page::default()
    };
    if fetched_url != raw_url {
        page.fetched_url = fetched_url;
    }
    Ok(page)
}

async fn scrape_page(
    scraper: &Scraper,
    cache: Option<&PageCache>,
    client: &reqwest::Client,
    raw_url: &str,
    options: &ScrapeOutputOptions,
) -> AppResult<Page> {
    if !options.select.is_empty() {
        let (html, fetched_url) = scraper.fetch_with_browser_fallback(raw_url).await?;
        let markdown = extract::extract_selector(&html, &options.select)?;
        if markdown.is_empty() {
            return Err(AppError::not_found(format!(
                "no elements matched selector {:?}",
                options.select
            )));
        }
        let mut page = Page {
            url: raw_url.to_string(),
            title: extract::title(&html),
            markdown,
            ..Page::default()
        };
        if fetched_url != raw_url {
            page.fetched_url = fetched_url;
        }
        return Ok(page);
    }
    if !options.no_llms_txt
        && let Some(content) = scrape::fetch_llms_txt(client, raw_url).await
    {
        return Ok(Page {
            url: raw_url.to_string(),
            title: "llms.txt".to_string(),
            markdown: content,
            ..Page::default()
        });
    }
    cached_scrape(scraper, cache, raw_url).await
}

async fn scrape_many_urls(
    scraper: &Scraper,
    cache: Option<PageCache>,
    client: &reqwest::Client,
    urls: Vec<String>,
    options: &ScrapeOutputOptions,
    concurrency: usize,
) -> Result<Vec<Page>, CliError> {
    let mut results: Vec<Option<AppResult<Page>>> = (0..urls.len()).map(|_| None).collect();
    let workers = futures_util::stream::iter(urls.into_iter().enumerate())
        .map(|(index, raw_url)| {
            let scraper = scraper.clone();
            let cache = cache.clone();
            let client = client.clone();
            let options = options.clone();

            tokio::spawn(async move {
                let result = scrape_one_url(&scraper, cache, &client, &raw_url, &options).await;
                (index, result)
            })
        })
        .buffer_unordered(concurrency.max(1));
    futures_util::pin_mut!(workers);

    while let Some(joined) = workers.next().await {
        let (index, result) =
            joined.map_err(|err| CliError::upstream(format!("scrape worker failed: {err}")))?;
        results[index] = Some(result);
    }

    let mut pages = Vec::new();
    for result in results {
        match result.expect("scrape result slot was filled") {
            Ok(page) => pages.push(page),
            Err(err) => eprintln!("warn: {err}"),
        }
    }
    Ok(pages)
}

async fn cached_scrape(
    scraper: &Scraper,
    cache: Option<&PageCache>,
    raw_url: &str,
) -> AppResult<Page> {
    let key = scraper.rewrite(raw_url);
    if let Some(cache) = cache
        && let Some((page, source)) = cache.get(&key)
        && !scrape::cache_stale_for_browser(&source, scraper.has_browser())
    {
        return Ok(page_for_request(page, raw_url, &key));
    }
    let (page, source) = scraper.scrape(raw_url).await?;
    if let Some(cache) = cache {
        cache.put(&key, &page, &source);
    }
    Ok(page)
}

fn page_for_request(mut page: Page, raw_url: &str, fetched_url: &str) -> Page {
    page.url = raw_url.to_string();
    page.fetched_url = if fetched_url == raw_url {
        String::new()
    } else {
        fetched_url.to_string()
    };
    page
}

async fn run_crawl(as_json: bool, cfg: &AppConfig, args: CrawlArgs) -> Result<(), CliError> {
    if let Ok(worker_id) = std::env::var("SNARF_CRAWL_WORKER") {
        return run_crawl_worker(cfg, args, &worker_id).await;
    }

    match args.command {
        Some(CrawlCommand::Status { id }) => return run_crawl_status(as_json, id),
        Some(CrawlCommand::Stop { id }) => {
            let status = crawl::read_status(&id)
                .map_err(|err| CliError::not_found(format!("crawl {id} not found: {err}")))?;
            if status.status != "running" {
                return Err(CliError::precondition(format!(
                    "crawl {id} is not running (status: {})",
                    status.status
                )));
            }
            send_stop_signal(status.pid)
                .map_err(|err| CliError::upstream(format!("failed to stop crawl: {err}")))?;
            eprintln!("Sent stop signal to crawl {id} (pid {})", status.pid);
            return Ok(());
        }
        None => {}
    }

    let seed = args
        .url
        .ok_or_else(|| CliError::validation("provide a URL for crawl"))?;
    validate_at_least_one("concurrency", args.concurrency)?;
    if args.background {
        return run_crawl_background(&seed);
    }

    let cache = new_page_cache(cfg, args.no_cache)?;
    let scraper = new_scraper(cfg)?;
    let start = Instant::now();
    let mut total: usize = 0;
    let mut new_count: usize = 0;
    let mut changed: usize = 0;
    let mut unchanged: usize = 0;
    let mut errors: usize = 0;
    let mut print_error = None;
    let mut first_page = true;

    let crawl_outcome = {
        let crawl = crawl::crawl_with_callback(
            &seed,
            &scraper,
            CrawlOptions {
                depth: args.depth,
                concurrency: args.concurrency,
                allow: args.allow,
                deny: args.deny,
            },
            cache.as_ref(),
            args.sitemap,
            |result| {
                total += 1;
                if !result.error.is_empty() {
                    errors += 1;
                    eprintln!("warn: {}: {}", result.url, result.error);
                    return;
                }
                match result.status.as_str() {
                    "new" => new_count += 1,
                    "changed" => changed += 1,
                    "unchanged" => unchanged += 1,
                    _ => {}
                }
                if let Some(page) = &result.page {
                    if as_json {
                        let mut value = serde_json::json!({
                            "url": page.url,
                            "title": page.title,
                            "words": page.markdown.split_whitespace().count(),
                            "status": result.status,
                            "source": result.source,
                            "body": page.markdown
                        });
                        if !page.fetched_url.is_empty() {
                            value["fetched_url"] = serde_json::json!(page.fetched_url);
                        }
                        if let Err(err) = print_json(&value) {
                            print_error = Some(err);
                        }
                    } else {
                        if !first_page {
                            println!();
                        }
                        first_page = false;
                        print_crawl_page(result, page);
                    }
                }
            },
        );
        tokio::pin!(crawl);
        tokio::select! {
            result = &mut crawl => CrawlOutcome::Finished(result),
            signal = shutdown_signal() => CrawlOutcome::Cancelled(signal),
        }
    };

    if let Some(err) = print_error {
        return Err(err);
    }
    let cancelled_by = match crawl_outcome {
        CrawlOutcome::Finished(result) => {
            result.map_err(|err| CliError::upstream(format!("crawl failed: {err}")))?;
            None
        }
        CrawlOutcome::Cancelled(signal) => Some(signal),
    };
    print_crawl_summary(
        &seed,
        total.saturating_sub(errors),
        new_count,
        changed,
        unchanged,
        errors,
        start.elapsed().as_secs_f64(),
    );
    if let Some(signal) = cancelled_by {
        return Err(CliError::cancelled(format!("cancelled by {signal}")));
    }
    Ok(())
}

enum CrawlOutcome<T> {
    Finished(T),
    Cancelled(&'static str),
}

fn run_crawl_background(seed: &str) -> Result<(), CliError> {
    let mut status = crawl::CrawlStatus {
        id: crawl::generate_crawl_id(),
        pid: 0,
        seed: seed.to_string(),
        status: "starting".to_string(),
        pages: 0,
        new: 0,
        changed: 0,
        unchanged: 0,
        errors: 0,
        started_at: Utc::now(),
        updated_at: Utc::now(),
        error: String::new(),
    };
    crawl::write_status(&mut status).map_err(|err| CliError::upstream(err.to_string()))?;

    let executable = std::env::current_exe()
        .map_err(|err| CliError::upstream(format!("cannot determine executable: {err}")))?;
    let dev_null = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(if cfg!(windows) { "NUL" } else { "/dev/null" })
        .map_err(|err| CliError::upstream(format!("open devnull: {err}")))?;

    let mut command = ProcessCommand::new(executable);
    command
        .args(std::env::args_os().skip(1))
        .env("SNARF_CRAWL_WORKER", &status.id);
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    match command
        .stdin(Stdio::from(
            dev_null
                .try_clone()
                .map_err(|err| CliError::upstream(err.to_string()))?,
        ))
        .stdout(Stdio::from(
            dev_null
                .try_clone()
                .map_err(|err| CliError::upstream(err.to_string()))?,
        ))
        .stderr(Stdio::from(dev_null))
        .spawn()
    {
        Ok(_child) => {}
        Err(err) => {
            mark_crawl_status_failed(&mut status, err.to_string());
            let _ = crawl::write_status(&mut status);
            return Err(CliError::upstream(format!(
                "failed to start background crawl: {err}"
            )));
        }
    }
    println!("crawl_id: {}", status.id);
    eprintln!(
        "Background crawl started. Check status with: snarf crawl status {}",
        status.id
    );
    Ok(())
}

async fn run_crawl_worker(
    cfg: &AppConfig,
    args: CrawlArgs,
    crawl_id: &str,
) -> Result<(), CliError> {
    let seed = args
        .url
        .ok_or_else(|| CliError::validation("provide a URL for crawl"))?;
    validate_at_least_one("concurrency", args.concurrency)?;
    let mut status = crawl::CrawlStatus {
        id: crawl_id.to_string(),
        pid: std::process::id(),
        seed: seed.clone(),
        status: "running".to_string(),
        pages: 0,
        new: 0,
        changed: 0,
        unchanged: 0,
        errors: 0,
        started_at: Utc::now(),
        updated_at: Utc::now(),
        error: String::new(),
    };
    let _ = crawl::write_status(&mut status);

    let run_result = {
        let crawl = async {
            let cache = new_page_cache(cfg, args.no_cache)?;
            let scraper = new_scraper(cfg)?;
            let mut status_error = None;
            let results = crawl::crawl_with_callback(
                &seed,
                &scraper,
                CrawlOptions {
                    depth: args.depth,
                    concurrency: args.concurrency,
                    allow: args.allow,
                    deny: args.deny,
                },
                cache.as_ref(),
                args.sitemap,
                |result| {
                    status.pages += 1;
                    if !result.error.is_empty() {
                        status.errors += 1;
                    } else {
                        match result.status.as_str() {
                            "new" => status.new += 1,
                            "changed" => status.changed += 1,
                            "unchanged" => status.unchanged += 1,
                            _ => {}
                        }
                    }
                    if status.pages.is_multiple_of(10)
                        && let Err(err) = crawl::write_status(&mut status)
                    {
                        status_error = Some(CliError::upstream(err.to_string()));
                    }
                },
            )
            .await
            .map_err(|err| CliError::upstream(format!("crawl failed: {err}")))?;
            if let Some(err) = status_error {
                Err(err)
            } else {
                Ok(results)
            }
        };
        tokio::pin!(crawl);
        tokio::select! {
            result = &mut crawl => CrawlOutcome::Finished(result),
            _signal = shutdown_signal() => CrawlOutcome::Cancelled("signal"),
        }
    };

    match run_result {
        CrawlOutcome::Finished(Ok(_)) => {
            status.status = "completed".to_string();
            crawl::write_status(&mut status).map_err(|err| CliError::upstream(err.to_string()))?;
            Ok(())
        }
        CrawlOutcome::Finished(Err(err)) => {
            mark_crawl_status_failed(&mut status, err.message.clone());
            let _ = crawl::write_status(&mut status);
            Err(err)
        }
        CrawlOutcome::Cancelled(_) => {
            status.status = "stopped".to_string();
            status.error.clear();
            crawl::write_status(&mut status).map_err(|err| CliError::upstream(err.to_string()))?;
            Ok(())
        }
    }
}

fn mark_crawl_status_failed(status: &mut crawl::CrawlStatus, error: String) {
    status.status = "failed".to_string();
    status.error = error;
}

fn send_stop_signal(pid: u32) -> eyre::Result<()> {
    if pid == 0 {
        eyre::bail!("crawl has no worker pid yet");
    }

    #[cfg(unix)]
    {
        let status = ProcessCommand::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status()?;
        if !status.success() {
            eyre::bail!("kill exited with status {status}");
        }
    }

    #[cfg(windows)]
    {
        let status = ProcessCommand::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()?;
        if !status.success() {
            eyre::bail!("taskkill exited with status {status}");
        }
    }

    Ok(())
}

fn run_crawl_status(as_json: bool, id: Option<String>) -> Result<(), CliError> {
    if let Some(id) = id {
        let status = crawl::read_status(&id)
            .map_err(|err| CliError::not_found(format!("crawl {id} not found: {err}")))?;
        if as_json {
            print_json(&status)?;
        } else {
            println!("---");
            println!("id: {}", status.id);
            println!("seed: {}", status.seed);
            println!("status: {}", status.status);
            println!("pages: {}", status.pages);
            println!("new: {}", status.new);
            println!("changed: {}", status.changed);
            println!("unchanged: {}", status.unchanged);
            println!("errors: {}", status.errors);
            println!("started_at: {}", status.started_at.to_rfc3339());
            println!("updated_at: {}", status.updated_at.to_rfc3339());
            if !status.error.is_empty() {
                println!("error: {}", status.error);
            }
            println!("---");
        }
        return Ok(());
    }

    let statuses = crawl::list_statuses().map_err(|err| CliError::upstream(err.to_string()))?;
    if statuses.is_empty() {
        eprintln!("No crawls found.");
        return Ok(());
    }
    if as_json {
        print_json(&statuses)?;
    } else {
        for status in statuses {
            println!(
                "{}  {:<9}  {:<40}  pages={:<5}  {}",
                status.id,
                status.status,
                status.seed,
                status.pages,
                status.updated_at.to_rfc3339()
            );
        }
    }
    Ok(())
}

async fn run_browser(as_json: bool, cfg: &AppConfig, args: BrowserArgs) -> Result<(), CliError> {
    match args.command {
        Some(BrowserCommand::Install) => {
            if !as_json {
                eprintln!("Downloading Chromium...");
            }
            let path = scrape::install_browser()
                .await
                .map_err(|err| CliError::upstream(format!("install failed: {err}")))?;
            if as_json {
                print_json(&serde_json::json!({
                    "status": "installed",
                    "browser_path": path,
                }))?;
            } else {
                eprintln!("Installed to: {path}");
                eprintln!("Configure with: snarf config set browser {path}");
            }
            Ok(())
        }
        Some(BrowserCommand::Status) => {
            if cfg.browser.is_empty() {
                if as_json {
                    print_json(&serde_json::json!({
                        "browser_config": "",
                        "status": "disabled",
                    }))?;
                } else {
                    println!("browser_config: (not set)");
                    println!("status: disabled");
                }
            } else {
                match scrape::resolve_browser_bin(&cfg.browser) {
                    Ok(path) => {
                        if as_json {
                            print_json(&serde_json::json!({
                                "browser_config": cfg.browser,
                                "browser_path": path,
                                "status": "ok",
                            }))?;
                        } else {
                            println!("browser_config: {}", cfg.browser);
                            println!("browser_path: {path}");
                            println!("status: ok");
                        }
                    }
                    Err(err) => {
                        if as_json {
                            print_json(&serde_json::json!({
                                "browser_config": cfg.browser,
                                "status": "error",
                                "error": err.to_string(),
                            }))?;
                        } else {
                            println!("browser_config: {}", cfg.browser);
                            println!("status: error ({err})");
                        }
                    }
                }
            }
            Ok(())
        }
        None => {
            print_browser_help();
            Ok(())
        }
    }
}

fn print_browser_help() {
    println!("Manage browser for JS-rendered page support\n");
    println!("Usage: snarf browser <COMMAND>\n");
    println!("Commands:");
    println!("  install  Download Chromium for headless rendering");
    println!("  status   Check browser configuration and availability");
    println!("\nRun 'snarf browser <command> --help' for flags and examples.");
}

async fn run_config(as_json: bool, cfg: &AppConfig, args: ConfigArgs) -> Result<(), CliError> {
    match args.command {
        None => {
            let (path, gh_source) = (
                config::config_path().map_err(|err| CliError::upstream(err.to_string()))?,
                cfg.resolve_github_token().1,
            );
            let info = ConfigInfo {
                config_path: path,
                backend: cfg.backend,
                searxng_url: cfg.searxng_url.clone(),
                limit: cfg.limit,
                cache_ttl: cfg.cache_ttl.clone(),
                browser: cfg.browser.clone(),
                code_backend: cfg.code_backend,
                docs_backend: cfg.docs_backend,
                sourcegraph_url: cfg.sourcegraph_url.clone(),
                github_token_source: gh_source,
                url_rewrites: cfg.url_rewrites.clone(),
                available_backends: config::available_backends(),
                available_code_backends: config::available_code_backends(),
                available_doc_backends: config::available_doc_backends(),
            };
            if as_json {
                print_json(&info)?;
            } else {
                print_json_pretty(&info)?;
            }
        }
        Some(ConfigCommand::Init) => {
            let path = config::config_path().map_err(|err| CliError::upstream(err.to_string()))?;
            if path.exists() {
                return Err(CliError::precondition(format!(
                    "config already exists: {}",
                    path.display()
                )));
            }
            AppConfig::default()
                .save()
                .map_err(|err| CliError::upstream(err.to_string()))?;
            if as_json {
                print_json(&serde_json::json!({
                    "created": true,
                    "path": path,
                }))?;
            } else {
                eprintln!("created {}", path.display());
            }
        }
        Some(ConfigCommand::Set { key, value }) => {
            let mut config = AppConfig::load();
            apply_config_set(&mut config, &key, &value)?;
            config
                .save()
                .map_err(|err| CliError::upstream(err.to_string()))?;
            if as_json {
                print_json(&serde_json::json!({
                    "set": true,
                    "key": key,
                    "value": value,
                }))?;
            } else {
                eprintln!("set {key} = {value}");
            }
        }
        Some(ConfigCommand::Path) => {
            let path = config::config_path().map_err(|err| CliError::upstream(err.to_string()))?;
            if as_json {
                print_json(&serde_json::json!({ "path": path }))?;
            } else {
                println!("{}", path.display());
            }
        }
    }
    Ok(())
}

fn apply_config_set(cfg: &mut AppConfig, key: &str, value: &str) -> Result<(), CliError> {
    match key {
        "backend" => {
            cfg.backend = SearchBackend::parse(value).map_err(CliError::validation)?;
        }
        "searxng_url" => cfg.searxng_url = value.to_string(),
        "brave_api_key" => cfg.brave_api_key = value.to_string(),
        "exa_api_key" => cfg.exa_api_key = value.to_string(),
        "limit" => {
            cfg.limit = value
                .parse()
                .map_err(|err| CliError::validation(format!("limit must be an integer: {err}")))?;
        }
        "cache_ttl" => {
            config::parse_duration(value).map_err(|err| {
                CliError::validation(format!(
                    "cache_ttl must be a duration (e.g. 1h, 30m): {err}"
                ))
            })?;
            cfg.cache_ttl = value.to_string();
        }
        "browser" => cfg.browser = value.to_string(),
        "code_backend" => {
            cfg.code_backend = CodeBackend::parse(value).map_err(CliError::validation)?;
        }
        "docs_backend" => {
            cfg.docs_backend = DocsBackend::parse(value).map_err(CliError::validation)?;
        }
        "context7_api_key" => cfg.context7_api_key = value.to_string(),
        "sourcegraph_url" => cfg.sourcegraph_url = value.to_string(),
        "github_token" => cfg.github_token = value.to_string(),
        "url_rewrites" => {
            let rules: Vec<urlrewrite::Rule> = serde_json::from_str(value).map_err(|err| {
                CliError::validation(format!(
                    "url_rewrites must be a JSON array of {{match, replace}}: {err}"
                ))
            })?;
            urlrewrite::Rewriter::new(&rules)
                .map_err(|err| CliError::validation(err.to_string()))?;
            cfg.url_rewrites = rules;
        }
        _ => {
            return Err(CliError::validation(format!(
                "unknown key: {key} (valid: backend, searxng_url, brave_api_key, exa_api_key, limit, cache_ttl, browser, code_backend, docs_backend, context7_api_key, sourcegraph_url, github_token, url_rewrites)"
            )));
        }
    }
    Ok(())
}

async fn run_cache(as_json: bool, cfg: &AppConfig, args: CacheArgs) -> Result<(), CliError> {
    match args.command {
        Some(CacheCommand::Clear) => {
            cache::clear().map_err(|err| CliError::upstream(err.to_string()))?;
            if as_json {
                print_json(&serde_json::json!({ "cleared": true }))?;
            } else {
                eprintln!("cache cleared");
            }
        }
        None => {
            let path = cache::db_path().map_err(|err| CliError::upstream(err.to_string()))?;
            let (entries, bytes) =
                cache::stats().map_err(|err| CliError::upstream(err.to_string()))?;
            let size = cache::format_bytes(bytes);
            if as_json {
                print_json(&serde_json::json!({
                    "path": path,
                    "entries": entries,
                    "size_bytes": bytes,
                    "size": size,
                    "ttl": cfg.cache_ttl,
                }))?;
            } else {
                println!("---");
                println!("path: {}", path.display());
                println!("entries: {entries}");
                println!("size: {size}");
                println!("ttl: {}", cfg.cache_ttl);
                println!("---");
            }
        }
    }
    Ok(())
}

async fn run_version(as_json: bool) -> Result<(), CliError> {
    let update = updatecheck::get_status(updatecheck::Options {
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        allow_network: true,
        timeout: std::time::Duration::from_secs(1),
    })
    .await;
    let commit = build_commit();
    let date = build_date();

    if as_json {
        print_json(&serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "commit": commit,
            "date": date,
            "rust": rust_version(),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "update": update
        }))?;
    } else {
        println!("snarf {}", env!("CARGO_PKG_VERSION"));
        if !commit.is_empty() {
            println!("  commit: {commit}");
        }
        if !date.is_empty() {
            println!("  built:  {date}");
        }
        println!(
            "  rust:   {} {}/{}",
            rust_version(),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        if update.available {
            println!();
            println!("Update available: {}", update.latest_version);
            if !update.command.is_empty() {
                println!("  command: {}", update.command);
            } else if !update.release_url.is_empty() {
                println!("  command: {}", update.release_url);
            }
        }
    }
    Ok(())
}

fn rust_version() -> &'static str {
    option_env!("RUSTC_VERSION").unwrap_or("unknown")
}

fn build_commit() -> &'static str {
    option_env!("SNARF_BUILD_COMMIT").unwrap_or("")
}

fn build_date() -> &'static str {
    option_env!("SNARF_BUILD_DATE").unwrap_or("")
}

fn resolve_urls(args: &[String]) -> Result<Vec<String>, CliError> {
    if args.len() > 1 {
        return Ok(args.to_vec());
    }
    if let Some(arg) = args.first() {
        if arg.trim_start().starts_with('[') {
            let urls: Vec<String> = serde_json::from_str(arg).map_err(|err| {
                CliError::validation(format!("failed to parse JSON array: {err}"))
            })?;
            if urls.is_empty() {
                return Err(CliError::validation("JSON array is empty"));
            }
            return Ok(urls);
        }
        if fs::metadata(arg).is_ok() {
            let urls = read_lines(
                &fs::read_to_string(arg)
                    .map_err(|err| CliError::validation(format!("failed to open {arg}: {err}")))?,
            );
            if urls.is_empty() {
                return Err(CliError::validation(format!(
                    "file {arg:?} contains no URLs"
                )));
            }
            return Ok(urls);
        }
        return Ok(vec![arg.clone()]);
    }

    if !io::stdin().is_terminal() {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .map_err(|err| CliError::validation(format!("failed to read stdin: {err}")))?;
        let urls = read_lines(&input);
        if !urls.is_empty() {
            return Ok(urls);
        }
    }

    Err(CliError::validation(
        "provide a URL, file path, JSON array, or pipe URLs via stdin",
    ))
}

fn read_lines(input: &str) -> Vec<String> {
    input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToString::to_string)
        .collect()
}

fn new_scraper(cfg: &AppConfig) -> Result<Scraper, CliError> {
    let rewriter = urlrewrite::Rewriter::new(&cfg.url_rewrites)
        .map_err(|err| CliError::precondition(format!("invalid url_rewrites: {err}")))?;
    Scraper::new(cfg.browser.clone(), rewriter).map_err(|err| CliError::upstream(err.to_string()))
}

fn new_page_cache(cfg: &AppConfig, no_cache: bool) -> Result<Option<PageCache>, CliError> {
    if no_cache {
        return Ok(None);
    }
    let ttl = config::parse_duration(&cfg.cache_ttl)
        .unwrap_or_else(|_| std::time::Duration::from_secs(60 * 60));
    PageCache::new(ttl)
        .map(Some)
        .map_err(|err| CliError::upstream(err.to_string()))
}

fn print_page(page: &Page) {
    println!("---");
    println!("url: {}", page.url);
    if !page.fetched_url.is_empty() {
        println!("fetched_url: {}", page.fetched_url);
    }
    println!("title: {}", page.title);
    println!("words: {}", page.markdown.split_whitespace().count());
    println!("---");
    println!("{}", page.markdown);
}

fn print_crawl_page(result: &crawl::CrawlResult, page: &Page) {
    println!("---");
    println!("url: {}", page.url);
    if !page.fetched_url.is_empty() {
        println!("fetched_url: {}", page.fetched_url);
    }
    println!("title: {}", page.title);
    println!("words: {}", page.markdown.split_whitespace().count());
    println!("status: {}", result.status);
    println!("source: {}", result.source);
    println!("---");
    println!("{}", page.markdown);
}

fn print_crawl_summary(
    seed: &str,
    pages: usize,
    new_count: usize,
    changed: usize,
    unchanged: usize,
    errors: usize,
    duration: f64,
) {
    eprintln!();
    eprintln!("---");
    eprintln!("seed: {seed}");
    eprintln!("pages: {pages}");
    eprintln!("new: {new_count}");
    eprintln!("changed: {changed}");
    eprintln!("unchanged: {unchanged}");
    eprintln!("errors: {errors}");
    eprintln!("duration: {duration:.1}s");
    eprintln!("---");
}

fn first_line(input: &str) -> String {
    input
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string()
}

fn validate_at_least_one(name: &str, value: usize) -> Result<(), CliError> {
    if value == 0 {
        return Err(CliError::validation(format!("{name} must be at least 1")));
    }
    Ok(())
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), CliError> {
    serde_json::to_writer(io::stdout(), value)
        .map_err(|err| CliError::upstream(err.to_string()))?;
    println!();
    Ok(())
}

fn print_json_pretty<T: serde::Serialize>(value: &T) -> Result<(), CliError> {
    serde_json::to_writer_pretty(io::stdout(), value)
        .map_err(|err| CliError::upstream(err.to_string()))?;
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::config::AppConfig;
    use crate::crawl::CrawlStatus;
    use crate::types::{CodeResult, DocsResult};

    use super::{
        apply_config_set, code_result_header, limit_docs_results, mark_crawl_status_failed,
        rust_version,
    };
    use clap::Parser;

    #[test]
    fn config_set_accepts_url_rewrites_json() {
        let mut config = AppConfig::default();
        apply_config_set(
            &mut config,
            "url_rewrites",
            r#"[{"match":"^https?://www\\.reddit\\.com/(.*)$","replace":"https://old.reddit.com/$1"}]"#,
        )
        .unwrap();

        assert_eq!(config.url_rewrites.len(), 1);
        assert_eq!(config.url_rewrites[0].replace, "https://old.reddit.com/$1");
    }

    #[test]
    fn config_set_rejects_invalid_url_rewrites_json() {
        let mut config = AppConfig::default();
        let error = apply_config_set(&mut config, "url_rewrites", "not json").unwrap_err();

        assert!(error.message.to_ascii_lowercase().contains("json"));
    }

    #[test]
    fn config_set_rejects_invalid_url_rewrite_regex() {
        let mut config = AppConfig::default();
        assert!(
            apply_config_set(
                &mut config,
                "url_rewrites",
                r#"[{"match":"[","replace":"x"}]"#,
            )
            .is_err()
        );
    }

    #[test]
    fn config_set_empty_url_rewrites_clears_rules() {
        let mut config = AppConfig::default();
        apply_config_set(
            &mut config,
            "url_rewrites",
            r#"[{"match":"^x$","replace":"y"}]"#,
        )
        .unwrap();
        assert_eq!(config.url_rewrites.len(), 1);

        apply_config_set(&mut config, "url_rewrites", "[]").unwrap();
        assert!(config.url_rewrites.is_empty());
    }

    #[test]
    fn config_set_accepts_combined_go_style_cache_ttl() {
        let mut config = AppConfig::default();

        apply_config_set(&mut config, "cache_ttl", "1h30m").unwrap();

        assert_eq!(config.cache_ttl, "1h30m");
    }

    #[test]
    fn config_set_rejects_invalid_backends() {
        let mut config = AppConfig::default();

        for key in ["backend", "code_backend", "docs_backend"] {
            let error = apply_config_set(&mut config, key, "missing").unwrap_err();

            assert!(
                error.message.contains("invalid value"),
                "{key} error was {:?}",
                error.message
            );
        }
    }

    #[test]
    fn crawl_filters_accept_comma_separated_values() {
        let cli = super::Cli::try_parse_from([
            "scrape",
            "crawl",
            "https://example.com",
            "--allow",
            "/docs,/api",
            "--deny",
            r"\.pdf$,/admin",
        ])
        .unwrap();

        let Some(super::Command::Crawl(args)) = cli.command else {
            panic!("expected crawl command");
        };

        assert_eq!(args.url.as_deref(), Some("https://example.com"));
        assert_eq!(args.allow, ["/docs", "/api"]);
        assert_eq!(args.deny, [r"\.pdf$", "/admin"]);
    }

    #[test]
    fn code_result_header_matches_upstream_star_format() {
        let result = CodeResult {
            repo: "owner/repo".to_string(),
            path: "src/lib.rs".to_string(),
            line: 42,
            stars: 123,
            ..CodeResult::default()
        };

        assert_eq!(
            code_result_header(&result),
            "owner/repo  src/lib.rs  (line 42)  \u{2605} 123"
        );
    }

    #[test]
    fn docs_limit_applies_after_direct_library_fetch() {
        let results = limit_docs_results(
            vec![
                DocsResult {
                    title: "first".to_string(),
                    ..DocsResult::default()
                },
                DocsResult {
                    title: "second".to_string(),
                    ..DocsResult::default()
                },
            ],
            1,
        );

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "first");
    }

    #[test]
    fn crawl_status_failure_records_error_message() {
        let now = chrono::Utc::now();
        let mut status = CrawlStatus {
            id: "c_deadbeef".to_string(),
            pid: 0,
            seed: "https://example.com".to_string(),
            status: "starting".to_string(),
            pages: 0,
            new: 0,
            changed: 0,
            unchanged: 0,
            errors: 0,
            started_at: now,
            updated_at: now,
            error: String::new(),
        };

        mark_crawl_status_failed(&mut status, "spawn failed".to_string());

        assert_eq!(status.status, "failed");
        assert_eq!(status.error, "spawn failed");
    }

    #[test]
    fn version_reports_build_rustc_version() {
        assert!(
            rust_version().starts_with("rustc "),
            "rust version was {:?}",
            rust_version()
        );
    }

    #[test]
    fn config_set_rejects_unknown_keys() {
        let mut config = AppConfig::default();
        assert!(apply_config_set(&mut config, "no_such_key", "x").is_err());
    }
}
