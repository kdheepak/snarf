use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

static NEXT_ENV_ID: AtomicUsize = AtomicUsize::new(0);

#[test]
fn root_summary_lists_version_command() {
    let output = snarf_command().output().expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("  version     Print snarf version information"),
        "stdout: {stdout}"
    );
}

#[test]
fn browser_namespace_prints_help_without_error() {
    let output = snarf_command().arg("browser").output().expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Manage browser for JS-rendered page support"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("  install  Download Chromium for headless rendering"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("  status   Check browser configuration and availability"),
        "stdout: {stdout}"
    );
}

#[test]
fn command_help_includes_flag_descriptions() {
    let cases = [
        (
            ["search", "--help"].as_slice(),
            [
                "search backend: brave, ddg, searxng, exa",
                "scrape full content from each result",
            ]
            .as_slice(),
        ),
        (
            ["scrape", "--help"].as_slice(),
            [
                "CSS selector to extract specific elements",
                "disable automatic /llms.txt detection",
            ]
            .as_slice(),
        ),
        (
            ["crawl", "--help"].as_slice(),
            ["path substring filters", "run crawl in background"].as_slice(),
        ),
    ];

    for (args, expected) in cases {
        let output = snarf_command().args(args).output().expect("snarf runs");
        assert_eq!(
            output.status.code(),
            Some(0),
            "args: {args:?}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        for needle in expected {
            assert!(
                stdout.contains(needle),
                "args: {args:?}\nmissing {needle:?}\nstdout: {stdout}"
            );
        }
    }
}

#[test]
fn version_prints_rust_toolchain() {
    let output = snarf_command().arg("version").output().expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("  rust:   rustc "), "stdout: {stdout}");
}

#[test]
fn version_json_reports_cached_update_when_notifier_disabled() {
    let root = isolated_root();
    let cache_dir = root.join("cache").join("snarf");
    fs::create_dir_all(&cache_dir).expect("cache dir exists");
    fs::write(
        cache_dir.join("update-check.json"),
        r#"{
  "schema_version": 1,
  "current_version": "0.1.0",
  "last_checked_at": "2999-01-01T00:00:00Z",
  "latest_version": "1.2.3",
  "release_url": "https://example.com/snarf/releases/latest",
  "update_available": true,
  "install_type": "cargo",
  "update_command": "cargo install --git https://example.com/snarf --force"
}
"#,
    )
    .expect("cached update status is written");

    let output = command_for_root(root)
        .args(["--json", "version"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("version output is json");
    assert_eq!(payload["update"]["available"], true);
    assert_eq!(payload["update"]["latest_version"], "1.2.3");
}

#[test]
fn select_without_matches_exits_not_found() {
    let server = TestHttpServer::new(
        "<html><head><title>T</title></head><body><main><p>hello</p></main></body></html>",
    );
    let output = snarf_command()
        .args(["scrape", &server.url, "--select", ".missing"])
        .output()
        .expect("snarf runs");

    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no elements matched selector"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn invalid_selectors_exit_validation() {
    let server = TestHttpServer::new(
        "<html><head><title>T</title></head><body><main><p>hello</p></main></body></html>",
    );
    let output = snarf_command()
        .args(["scrape", &server.url, "--select", "["])
        .output()
        .expect("snarf runs");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid selector"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn scrape_http_errors_exit_upstream() {
    let server = TestHttpServer::with_response(
        "500 Internal Server Error",
        "text/html",
        "<html><head><title>Error</title></head><body>boom</body></html>",
    );
    let output = snarf_command()
        .args(["scrape", &server.url, "--no-llms-txt"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(4),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("HTTP 500"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn raw_scrape_fetches_source_without_llms_probe() {
    let server = TestHttpServer::new(
        "<html><head><title>Raw</title></head><body><main><p>raw body</p></main></body></html>",
    );
    let output = snarf_command()
        .args(["scrape", &server.url, "--raw"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("<main><p>raw body</p></main>"),
        "stdout: {stdout}"
    );
    assert_eq!(server.paths(), ["/"]);
}

#[test]
fn raw_scrape_does_not_initialize_page_cache() {
    let server = TestHttpServer::new(
        "<html><head><title>Raw</title></head><body><main><p>raw body</p></main></body></html>",
    );
    let root = isolated_root();
    let output = command_for_root(root.clone())
        .args(["scrape", &server.url, "--raw"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !root.join("cache").join("snarf").exists(),
        "raw scrape should not create the page cache namespace"
    );
}

#[test]
fn raw_select_outputs_selected_html() {
    let server = TestHttpServer::new(
        r#"<html><head><title>Raw Select</title></head><body><div class="keep"><strong>raw text</strong></div><div class="drop">drop me</div></body></html>"#,
    );
    let output = snarf_command()
        .args(["scrape", &server.url, "--raw", "--select", ".keep"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("<strong>raw text</strong>"),
        "stdout: {stdout}"
    );
    assert!(!stdout.contains("drop me"), "stdout: {stdout}");
}

#[test]
fn cached_rewrite_aliases_are_relabelled_for_current_request() {
    let server = TestHttpServer::new(
        "<html><head><title>Alias Cache</title></head><body><main><p>cached alias content with enough words for the extractor to keep this page body</p></main></body></html>",
    );
    let root = isolated_root();
    let config_dir = root.join("config").join("snarf");
    fs::create_dir_all(&config_dir).expect("config dir exists");
    let original = "http://alias.test/page";
    let fetched = format!("{}/page", server.url);
    let config = serde_json::json!({
        "url_rewrites": [
            {
                "match": "^http://alias\\.test/page$",
                "replace": fetched,
            }
        ]
    });
    fs::write(
        config_dir.join("config.json"),
        serde_json::to_string(&config).expect("config json"),
    )
    .expect("config is written");

    let first = command_for_root(root.clone())
        .args(["scrape", original])
        .output()
        .expect("snarf runs");
    assert_eq!(
        first.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first_stdout.contains(&format!("url: {original}")));
    assert!(first_stdout.contains(&format!(
        "fetched_url: {}",
        config["url_rewrites"][0]["replace"]
            .as_str()
            .expect("rewrite replace is string")
    )));

    let second = command_for_root(root)
        .args([
            "scrape",
            config["url_rewrites"][0]["replace"]
                .as_str()
                .expect("rewrite replace is string"),
        ])
        .output()
        .expect("snarf runs");
    assert_eq!(
        second.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(second_stdout.contains(&format!(
        "url: {}",
        config["url_rewrites"][0]["replace"]
            .as_str()
            .expect("rewrite replace is string")
    )));
    assert!(
        !second_stdout.contains(&format!("url: {original}")),
        "stdout: {second_stdout}"
    );
    assert!(
        !second_stdout.contains("fetched_url:"),
        "stdout: {second_stdout}"
    );
    assert_eq!(server.paths(), ["/page"]);
}

#[test]
fn docs_local_stub_exits_upstream() {
    let output = snarf_command()
        .args(["docs", "anything", "--backend", "local"])
        .output()
        .expect("snarf runs");

    assert_eq!(output.status.code(), Some(4));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("local fts5 backend not yet implemented"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn docs_context7_without_api_key_exits_precondition() {
    let output = snarf_command()
        .args(["docs", "anything", "--backend", "context7"])
        .output()
        .expect("snarf runs");

    assert_eq!(output.status.code(), Some(5));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("context7: API key not set"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn search_brave_without_api_key_exits_precondition() {
    let output = snarf_command()
        .args(["search", "anything", "--backend", "brave"])
        .output()
        .expect("snarf runs");

    assert_eq!(output.status.code(), Some(5));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("brave: API key not set"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unknown_code_backend_exits_validation() {
    let output = snarf_command()
        .args(["code", "anything", "--backend", "unknown"])
        .output()
        .expect("snarf runs");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown code backend"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn config_omits_empty_optional_fields() {
    let output = snarf_command().arg("config").output().expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("config output is json");
    assert!(
        !value
            .as_object()
            .expect("config is object")
            .contains_key("browser")
    );
    assert!(
        !value
            .as_object()
            .expect("config is object")
            .contains_key("url_rewrites")
    );
}

#[test]
fn config_path_honors_json_output() {
    let output = snarf_command()
        .args(["--json", "config", "path"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("config path output is json");
    assert!(
        value["path"]
            .as_str()
            .unwrap_or_default()
            .ends_with("snarf/config.json")
    );
}

#[test]
fn config_init_honors_json_output() {
    let output = snarf_command()
        .args(["--json", "config", "init"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("config init output is json");
    assert_eq!(value["created"], true);
    assert!(
        value["path"]
            .as_str()
            .unwrap_or_default()
            .ends_with("snarf/config.json")
    );
}

#[test]
fn config_init_existing_file_exits_precondition() {
    let root = isolated_root();
    let first = command_for_root(root.clone())
        .args(["config", "init"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        first.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );

    let second = command_for_root(root)
        .args(["config", "init"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        second.status.code(),
        Some(5),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("config already exists"),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
}

#[test]
fn config_set_honors_json_output() {
    let output = snarf_command()
        .args(["--json", "config", "set", "limit", "9"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("config set output is json");
    assert_eq!(value["set"], true);
    assert_eq!(value["key"], "limit");
    assert_eq!(value["value"], "9");
}

#[test]
fn browser_status_honors_json_output() {
    let output = snarf_command()
        .args(["--json", "browser", "status"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("browser status output is json");
    assert_eq!(value["browser_config"], "");
    assert_eq!(value["status"], "disabled");
}

#[test]
fn cache_stats_honors_json_output() {
    let output = snarf_command()
        .args(["--json", "cache"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cache output is json");
    assert!(
        value["path"]
            .as_str()
            .unwrap_or_default()
            .ends_with("cache.json")
    );
    assert_eq!(value["entries"], 0);
    assert_eq!(value["size"], "0 B");
    assert_eq!(value["size_bytes"], 0);
    assert_eq!(value["ttl"], "72h");
}

#[test]
fn cache_clear_honors_json_output() {
    let output = snarf_command()
        .args(["--json", "cache", "clear"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cache clear output is json");
    assert_eq!(value["cleared"], true);
}

#[test]
fn crawl_json_includes_fetched_url_for_rewrites() {
    let server = TestHttpServer::new(
        "<html><head><title>Crawl Rewrite</title></head><body><main><p>rewritten crawl content with enough words to extract and print as markdown output from the page body</p></main></body></html>",
    );
    let original = "http://seed.test/page";
    let fetched = format!("{}/rewritten", server.url);
    let config = serde_json::json!({
        "url_rewrites": [
            {
                "match": "^http://seed\\.test/page$",
                "replace": fetched,
            }
        ]
    });

    let output = snarf_command_with_config(&serde_json::to_string(&config).expect("config json"))
        .args(["--json", "crawl", original, "--depth", "0"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(
        stdout
            .lines()
            .next()
            .expect("crawl emits one json object line"),
    )
    .expect("crawl json parses");
    assert_eq!(value["url"], original);
    assert_eq!(value["fetched_url"], config["url_rewrites"][0]["replace"]);
    assert_eq!(server.paths(), ["/rewritten"]);
}

#[cfg(unix)]
#[test]
fn select_uses_browser_fallback_for_js_shells() {
    let server = TestHttpServer::new(
        r#"<html><head><title>Shell</title></head><body><div id="__next"></div><noscript>Please enable JavaScript</noscript></body></html>"#,
    );
    let browser = fake_browser(
        "selector-fallback",
        r#"<html><head><title>Rendered</title></head><body><main><p class="rendered">browser text</p></main></body></html>"#,
    );
    let config = serde_json::json!({
        "browser": browser.display().to_string(),
    });

    let output = snarf_command_with_config(&serde_json::to_string(&config).expect("config json"))
        .args(["scrape", &server.url, "--select", ".rendered"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("title: Rendered"), "stdout: {stdout}");
    assert!(stdout.contains("browser text"), "stdout: {stdout}");
}

fn snarf_command() -> Command {
    command_for_root(isolated_root())
}

fn snarf_command_with_config(config_json: &str) -> Command {
    let root = isolated_root();
    let config_dir = root.join("config").join("snarf");
    fs::create_dir_all(&config_dir).expect("config dir exists");
    fs::write(config_dir.join("config.json"), config_json).expect("config is written");
    command_for_root(root)
}

fn command_for_root(root: PathBuf) -> Command {
    let mut command = Command::new(snarf_binary());
    command
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("SNARF_NO_UPDATE_NOTIFIER", "1");
    command
}

fn snarf_binary() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_snarf")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/snarf"))
}

fn isolated_root() -> PathBuf {
    let id = NEXT_ENV_ID.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("snarf-cli-tests")
        .join(format!("{}-{id}", std::process::id()));
    fs::create_dir_all(root.join("config")).expect("config dir exists");
    fs::create_dir_all(root.join("cache")).expect("cache dir exists");
    root
}

#[cfg(unix)]
fn fake_browser(name: &str, html: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let root = isolated_root().join("browser").join(name);
    fs::create_dir_all(&root).expect("browser dir exists");
    let path = root.join("fake-browser");
    fs::write(
        &path,
        format!("#!/bin/sh\ncat <<'SNARF_FAKE_BROWSER_HTML'\n{html}\nSNARF_FAKE_BROWSER_HTML\n"),
    )
    .expect("fake browser is written");

    let mut permissions = fs::metadata(&path)
        .expect("fake browser metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("fake browser is executable");
    path
}

struct TestHttpServer {
    url: String,
    paths: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TestHttpServer {
    fn new(body: &'static str) -> Self {
        Self::with_response("200 OK", "text/html", body)
    }

    fn with_response(status: &'static str, content_type: &'static str, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
        listener
            .set_nonblocking(true)
            .expect("test server can be nonblocking");
        let url = format!("http://{}", listener.local_addr().expect("server has addr"));
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
                    .push(path);
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
