use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
            ["search backend", "scrape full content from each result"].as_slice(),
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
    assert_eq!(payload["version"], env!("CARGO_PKG_VERSION"));
    assert!(payload["commit"].is_string());
    assert!(payload["date"].is_string());
    assert!(
        payload["rust"]
            .as_str()
            .expect("rust version is a string")
            .starts_with("rustc ")
    );
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
fn scrape_rejects_zero_concurrency() {
    let output = snarf_command()
        .args(["scrape", "https://example.com", "--concurrency", "0"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("concurrency must be at least 1"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn crawl_rejects_zero_concurrency() {
    let output = snarf_command()
        .args(["crawl", "https://example.com", "--concurrency", "0"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("concurrency must be at least 1"),
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
fn scrape_existing_directory_exits_validation() {
    let root = isolated_root();
    let input_dir = root.join("url-input-dir");
    fs::create_dir_all(&input_dir).expect("input directory exists");
    let input_dir_arg = input_dir.display().to_string();

    let output = command_for_root(root)
        .args(["scrape", &input_dir_arg])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(2),
        "bin: {}\nstdout: {}\nstderr: {}",
        snarf_binary().display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("failed to open"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn scrape_json_array_input_returns_pages_in_order() {
    let server = TestHttpServer::new(
        "<html><head><title>Array Input</title></head><body><main><p>array input body with enough words for extraction to keep this page content</p></main></body></html>",
    );
    let first = format!("{}/one", server.url);
    let second = format!("{}/two", server.url);
    let input = serde_json::json!([first, second]).to_string();

    let output = snarf_command()
        .args(["--json", "scrape", &input, "--concurrency", "1"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let pages: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("scrape output is json");
    assert_eq!(
        pages.as_array().expect("multi scrape returns array").len(),
        2
    );
    assert_eq!(pages[0]["url"], format!("{}/one", server.url));
    assert_eq!(pages[1]["url"], format!("{}/two", server.url));
    assert_eq!(server.paths(), ["/one", "/two"]);
}

#[test]
fn scrape_file_input_ignores_blank_and_comment_lines() {
    let server = TestHttpServer::new(
        "<html><head><title>File Input</title></head><body><main><p>file input body with enough words for extraction to keep this page content</p></main></body></html>",
    );
    let root = isolated_root();
    let input_file = root.join("urls.txt");
    let input_file_arg = input_file.display().to_string();
    fs::write(
        &input_file,
        format!(
            "\n# skip me\n{}/file-one\n\n{}/file-two\n",
            server.url, server.url
        ),
    )
    .expect("url file is written");

    let output = command_for_root(root)
        .args(["--json", "scrape", &input_file_arg, "--concurrency", "1"])
        .output()
        .expect("snarf runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let pages: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("scrape output is json");
    assert_eq!(
        pages.as_array().expect("multi scrape returns array").len(),
        2
    );
    assert_eq!(pages[0]["url"], format!("{}/file-one", server.url));
    assert_eq!(pages[1]["url"], format!("{}/file-two", server.url));
    assert_eq!(server.paths(), ["/file-one", "/file-two"]);
}

#[test]
fn scrape_reads_urls_from_stdin_when_no_args_are_provided() {
    let server = TestHttpServer::new(
        "<html><head><title>Stdin Input</title></head><body><main><p>stdin input body with enough words for extraction to keep this page content</p></main></body></html>",
    );
    let mut child = snarf_command()
        .args(["--json", "scrape", "--concurrency", "1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("snarf starts");
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin
        .write_all(format!("{}\n# ignored\n{}\n", server.url, server.url).as_bytes())
        .expect("stdin writes");
    drop(stdin);

    let output = child.wait_with_output().expect("snarf exits");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let pages: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("scrape output is json");
    assert_eq!(
        pages.as_array().expect("multi scrape returns array").len(),
        2
    );
}

#[test]
fn scrape_explicit_args_take_priority_over_stdin() {
    let server = TestHttpServer::new(
        "<html><head><title>Arg Input</title></head><body><main><p>arg input body with enough words for extraction to keep this page content</p></main></body></html>",
    );
    let explicit = format!("{}/explicit", server.url);
    let ignored = format!("{}/ignored", server.url);
    let mut child = snarf_command()
        .args(["--json", "scrape", &explicit])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("snarf starts");
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin
        .write_all(format!("{ignored}\n").as_bytes())
        .expect("stdin writes");
    drop(stdin);

    let output = child.wait_with_output().expect("snarf exits");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let page: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("scrape output is json");
    assert_eq!(page["url"], explicit);
    assert_eq!(server.paths(), ["/explicit"]);
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
fn invalid_search_backend_exits_validation() {
    let output = snarf_command()
        .args(["search", "anything", "--backend", "missing"])
        .output()
        .expect("snarf runs");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid value 'missing'"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn invalid_code_backend_exits_validation() {
    let output = snarf_command()
        .args(["code", "anything", "--backend", "unknown"])
        .output()
        .expect("snarf runs");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid value 'unknown'"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn github_code_regex_exits_precondition() {
    let output = snarf_command()
        .env("GITHUB_TOKEN", "dummy-token")
        .args(["code", "anything", "--backend", "github", "--regex"])
        .output()
        .expect("snarf runs");

    assert_eq!(output.status.code(), Some(5));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not support --regex"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn sourcegraph_code_search_does_not_probe_github_token() {
    let sourcegraph = TestHttpServer::with_response(
        "200 OK",
        "text/event-stream",
        r#"event: matches
data: [{"type":"content","repository":"example/repo","path":"src/lib.rs","language":"Rust","repoStars":0,"lineMatches":[{"line":"fn searched_symbol() {}","lineNumber":7}]}]

"#,
    );
    let root = isolated_root();
    let config_dir = root.join("config").join("snarf");
    fs::create_dir_all(&config_dir).expect("config dir exists");
    fs::write(
        config_dir.join("config.json"),
        serde_json::to_string(&serde_json::json!({
            "sourcegraph_url": sourcegraph.url,
        }))
        .expect("config json"),
    )
    .expect("config is written");
    let marker = root.join("gh-called");
    let fake_bin = fake_gh_bin(&root, &marker);

    let output = command_for_root(root.clone())
        .env("PATH", fake_bin)
        .args([
            "code",
            "searched_symbol",
            "--backend",
            "sourcegraph",
            "--minimal",
        ])
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
        String::from_utf8_lossy(&output.stdout).contains("searched_symbol"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !marker.exists(),
        "non-GitHub code search should not invoke gh"
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
    assert!(json_path_ends_with(
        &value["path"],
        ["snarf", "config.json"]
    ));
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
    assert!(json_path_ends_with(
        &value["path"],
        ["snarf", "config.json"]
    ));
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
fn background_crawl_stop_preserves_final_counts() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
    listener
        .set_nonblocking(true)
        .expect("test server can be nonblocking");
    let url = format!("http://{}", listener.local_addr().expect("server has addr"));
    let stop = Arc::new(AtomicBool::new(false));
    let slow_seen = Arc::new(AtomicBool::new(false));
    let server_stop = Arc::clone(&stop);
    let server_slow_seen = Arc::clone(&slow_seen);
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !server_stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(10));
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
                .unwrap_or("/");

            match path {
                "/" => {
                    let body = r#"
                    <html>
                      <head><title>Seed</title></head>
                      <body>
                        <main>
                          <p>Background crawl seed content with enough words to extract.</p>
                          <a href="/slow">Slow page</a>
                        </main>
                      </body>
                    </html>
                    "#;
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                }
                "/slow" => {
                    server_slow_seen.store(true, Ordering::Relaxed);
                    while !server_stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(10));
                    }
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    );
                }
                _ => {
                    let _ = stream.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            }
        }
    });

    let root = isolated_root();
    let output = command_for_root(root.clone())
        .args([
            "crawl",
            &url,
            "--background",
            "--depth",
            "1",
            "--concurrency",
            "1",
            "--no-cache",
        ])
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
    let crawl_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("crawl_id: "))
        .expect("background crawl prints id")
        .to_string();

    assert!(wait_for_flag(&slow_seen), "crawl reached slow page");

    let stop_output = command_for_root(root.clone())
        .args(["crawl", "stop", &crawl_id])
        .output()
        .expect("snarf runs");
    assert_eq!(
        stop_output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&stop_output.stdout),
        String::from_utf8_lossy(&stop_output.stderr)
    );

    let status = wait_for_crawl_status(&root, &crawl_id, |value| {
        value["status"] == "stopped" && value["pages"].as_u64() == Some(1)
    });
    stop.store(true, Ordering::Relaxed);
    server.join().expect("test server exits");

    let status = status.expect("crawl worker writes stopped status with final count");
    assert_eq!(status["new"].as_u64(), Some(1), "status: {status}");
    assert_eq!(status["errors"].as_u64(), Some(0), "status: {status}");
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

fn json_path_ends_with<const N: usize>(value: &serde_json::Value, components: [&str; N]) -> bool {
    let Some(path) = value.as_str() else {
        return false;
    };
    let mut expected = PathBuf::new();
    for component in components {
        expected.push(component);
    }
    Path::new(path).ends_with(expected)
}

fn snarf_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_snarf"))
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

fn wait_for_flag(flag: &AtomicBool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if flag.load(Ordering::Relaxed) {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn wait_for_crawl_status(
    root: &Path,
    crawl_id: &str,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> Option<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let output = command_for_root(root.to_path_buf())
            .args(["--json", "crawl", "status", crawl_id])
            .output()
            .expect("snarf runs");
        if output.status.success()
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
            && predicate(&value)
        {
            return Some(value);
        }
        thread::sleep(Duration::from_millis(25));
    }
    None
}

#[cfg(unix)]
fn fake_gh_bin(root: &Path, marker: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let dir = root.join("fake-gh-bin");
    fs::create_dir_all(&dir).expect("fake gh dir exists");
    let path = dir.join("gh");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf called > '{}'\nprintf '%s\\n' fake-token\n",
            marker.display()
        ),
    )
    .expect("fake gh is written");
    let mut permissions = fs::metadata(&path).expect("fake gh metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("fake gh is executable");
    dir
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
