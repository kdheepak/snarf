use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

struct Response {
    body: &'static str,
}

fn serve_searxng(
    responses: Vec<Response>,
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
    listener
        .set_nonblocking(true)
        .expect("test server can be nonblocking");
    let url = format!("http://{}", listener.local_addr().expect("server has addr"));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server_requests = Arc::clone(&requests);
    let server = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut index = 0;
        while index < responses.len() && std::time::Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(10));
                continue;
            };
            stream
                .set_nonblocking(false)
                .expect("test connection can be blocking");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("test server sets read timeout");
            let mut request = [0; 8192];
            let n = stream.read(&mut request).unwrap_or_default();
            server_requests
                .lock()
                .expect("request lock")
                .push(String::from_utf8_lossy(&request[..n]).into_owned());
            let response = &responses[index];
            index += 1;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.body.len(),
                response.body
            )
            .expect("test server writes response");
        }
    });
    (url, requests, server)
}

fn serve_searxng_with_page() -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
    listener
        .set_nonblocking(true)
        .expect("test server can be nonblocking");
    let url = format!("http://{}", listener.local_addr().expect("server has addr"));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server_requests = Arc::clone(&requests);
    let server_url = url.clone();
    let server = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut count = 0;
        while count < 4 && std::time::Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(10));
                continue;
            };
            stream
                .set_nonblocking(false)
                .expect("test connection can be blocking");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("test server sets read timeout");
            let mut request = [0; 8192];
            let n = stream.read(&mut request).unwrap_or_default();
            let request = String::from_utf8_lossy(&request[..n]).into_owned();
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            server_requests.lock().expect("request lock").push(request);
            count += 1;

            let (status, content_type, body) = if path.starts_with("/search?") {
                (
                    "200 OK",
                    "application/json",
                    format!(
                        r#"{{
                            "results": [
                                {{"title": "Scraped Result", "url": "{}/page", "content": "search snippet"}}
                            ]
                        }}"#,
                        server_url
                    ),
                )
            } else if path == "/page" {
                (
                    "200 OK",
                    "text/html",
                    r#"<html>
                        <head><title>Scraped Result</title></head>
                        <body><main><p>Alpha scraped content with enough words for both extractors to keep the body.</p></main></body>
                    </html>"#
                        .to_string(),
                )
            } else {
                ("404 Not Found", "text/plain", "not found".to_string())
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("test server writes response");
        }
    });
    (url, requests, server)
}

fn serve_sourcegraph() -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
    listener
        .set_nonblocking(true)
        .expect("test server can be nonblocking");
    let url = format!("http://{}", listener.local_addr().expect("server has addr"));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server_requests = Arc::clone(&requests);
    let server = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut count = 0;
        while count < 2 && std::time::Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(10));
                continue;
            };
            stream
                .set_nonblocking(false)
                .expect("test connection can be blocking");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("test server sets read timeout");
            let mut request = [0; 8192];
            let n = stream.read(&mut request).unwrap_or_default();
            server_requests
                .lock()
                .expect("request lock")
                .push(String::from_utf8_lossy(&request[..n]).into_owned());
            count += 1;

            let body = r#"event: matches
data: [{"type":"content","repository":"example/repo","path":"src/lib.rs","language":"Rust","repoStars":42,"lineMatches":[{"line":"fn searched_symbol() {}","lineNumber":7}]}]

"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("test server writes response");
        }
    });
    (url, requests, server)
}

fn command_output(mut command: Command) -> String {
    let output = command.output().expect("command runs");
    assert!(
        output.status.success(),
        "command failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is utf-8")
}

fn rust_command(args: &[&str]) -> Command {
    let mut command = Command::new(rust_binary());
    command.args(args);
    command_env(&mut command);
    command
}

fn go_command(args: &[&str]) -> Command {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut command = Command::new("go");
    command
        .arg("run")
        .arg(".")
        .args(args)
        .current_dir(manifest_dir.join("tmp"));
    command_env(&mut command);
    command
}

fn reference_cli_available() -> bool {
    if Command::new("go").arg("version").output().is_err() {
        eprintln!("skipping reference CLI check: go is not available");
        return false;
    }
    true
}

fn rust_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_snarf"))
}

fn command_env(command: &mut Command) {
    let root = reference_root();
    fs::create_dir_all(root.join("config")).expect("config dir exists");
    fs::create_dir_all(root.join("cache")).expect("cache dir exists");
    command
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("SNARF_NO_UPDATE_NOTIFIER", "1")
        .env("CI", "1");
}

fn reference_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/snarf-reference")
}

fn write_snarf_config(value: &serde_json::Value) {
    let dir = reference_root().join("config").join("snarf");
    fs::create_dir_all(&dir).expect("config dir exists");
    fs::write(
        dir.join("config.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(value).expect("config serializes")
        ),
    )
    .expect("config writes");
}

fn searxng_body() -> &'static str {
    r#"{
        "results": [
            {"title": "Result A", "url": "https://example.com/a", "content": "first result"},
            {"title": "Result B", "url": "https://example.com/b", "content": "second result"}
        ]
    }"#
}

fn assert_searxng_requests_match(requests: &[String]) {
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert!(request.starts_with("GET /search?"), "{request}");
        assert!(request.contains("q=rust+reference"), "{request}");
        assert!(request.contains("format=json"), "{request}");
        assert!(request.contains("pageno=1"), "{request}");
    }
}

#[test]
#[ignore = "requires Go and compares snarf against the reference CLI in ./tmp"]
fn searxng_default_output_matches_reference_cli() {
    if !reference_cli_available() {
        return;
    }

    let (url, requests, server) = serve_searxng(vec![
        Response {
            body: searxng_body(),
        },
        Response {
            body: searxng_body(),
        },
    ]);
    let args = [
        "search",
        "rust reference",
        "--backend",
        "searxng",
        "--searxng-url",
        &url,
        "--limit",
        "2",
    ];

    let rust_stdout = command_output(rust_command(&args));
    let go_stdout = command_output(go_command(&args));

    server.join().expect("test server exits");
    assert_eq!(rust_stdout, go_stdout);
    assert_searxng_requests_match(&requests.lock().expect("request lock"));
}

#[test]
#[ignore = "requires Go and compares snarf against the reference CLI in ./tmp"]
fn searxng_minimal_output_matches_reference_cli() {
    if !reference_cli_available() {
        return;
    }

    let (url, requests, server) = serve_searxng(vec![
        Response {
            body: searxng_body(),
        },
        Response {
            body: searxng_body(),
        },
    ]);
    let args = [
        "search",
        "rust reference",
        "--backend",
        "searxng",
        "--searxng-url",
        &url,
        "--limit",
        "2",
        "--minimal",
    ];

    let rust_stdout = command_output(rust_command(&args));
    let go_stdout = command_output(go_command(&args));

    server.join().expect("test server exits");
    assert_eq!(rust_stdout, go_stdout);
    assert_searxng_requests_match(&requests.lock().expect("request lock"));
}

#[test]
#[ignore = "requires Go and compares snarf against the reference CLI in ./tmp"]
fn searxng_scrape_minimal_output_matches_reference_cli() {
    if !reference_cli_available() {
        return;
    }

    let (url, requests, server) = serve_searxng_with_page();
    let args = [
        "search",
        "rust reference",
        "--backend",
        "searxng",
        "--searxng-url",
        &url,
        "--limit",
        "1",
        "--scrape",
        "--minimal",
    ];

    let rust_stdout = command_output(rust_command(&args));
    let go_stdout = command_output(go_command(&args));

    server.join().expect("test server exits");
    assert_eq!(rust_stdout, go_stdout);

    let requests = requests.lock().expect("request lock");
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /search?"))
            .count(),
        2
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /page "))
            .count(),
        2
    );
}

#[test]
#[ignore = "requires Go and compares snarf against the reference CLI in ./tmp"]
fn searxng_json_output_matches_reference_cli() {
    if !reference_cli_available() {
        return;
    }

    let (url, requests, server) = serve_searxng(vec![
        Response {
            body: searxng_body(),
        },
        Response {
            body: searxng_body(),
        },
    ]);
    let rust_args = [
        "--json",
        "search",
        "rust reference",
        "--backend",
        "searxng",
        "--searxng-url",
        &url,
        "--limit",
        "2",
    ];
    let go_args = [
        "--json",
        "search",
        "rust reference",
        "--backend",
        "searxng",
        "--searxng-url",
        &url,
        "--limit",
        "2",
    ];

    let rust_json: serde_json::Value =
        serde_json::from_str(&command_output(rust_command(&rust_args))).expect("rust json parses");
    let go_json: serde_json::Value =
        serde_json::from_str(&command_output(go_command(&go_args))).expect("go json parses");

    server.join().expect("test server exits");
    assert_eq!(rust_json, go_json);
    assert_searxng_requests_match(&requests.lock().expect("request lock"));
}

#[test]
#[ignore = "requires Go and compares snarf against the reference CLI in ./tmp"]
fn sourcegraph_code_minimal_output_matches_reference_cli() {
    if !reference_cli_available() {
        return;
    }

    let (url, requests, server) = serve_sourcegraph();
    write_snarf_config(&serde_json::json!({
        "sourcegraph_url": url,
    }));
    let _ = command_output(go_command(&["config", "set", "sourcegraph_url", &url]));
    let args = [
        "code",
        "searched_symbol",
        "--backend",
        "sourcegraph",
        "--limit",
        "1",
        "--minimal",
    ];

    let rust_stdout = command_output(rust_command(&args));
    let go_stdout = command_output(go_command(&args));

    server.join().expect("test server exits");
    assert_eq!(rust_stdout, go_stdout);

    let requests = requests.lock().expect("request lock");
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert!(request.starts_with("GET /.api/search/stream?"), "{request}");
        assert!(request.contains("q=searched_symbol"), "{request}");
        assert!(request.contains("archived%3Ano"), "{request}");
        assert!(request.contains("fork%3Ano"), "{request}");
        assert!(request.contains("display=1"), "{request}");
    }
}
