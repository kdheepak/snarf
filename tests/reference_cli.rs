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
    std::env::var_os("CARGO_BIN_EXE_snarf")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/snarf"))
}

fn command_env(command: &mut Command) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/snarf-reference");
    fs::create_dir_all(root.join("config")).expect("config dir exists");
    fs::create_dir_all(root.join("cache")).expect("cache dir exists");
    command
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("SNARF_NO_UPDATE_NOTIFIER", "1")
        .env("CI", "1");
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
