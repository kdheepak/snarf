#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

#[test]
fn sigint_exits_with_cancelled_code() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
    listener
        .set_nonblocking(true)
        .expect("test server can be nonblocking");
    let url = format!("http://{}", listener.local_addr().expect("server has addr"));
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = Arc::clone(&stop);
    let server = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !server_stop.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(10));
                continue;
            };
            stream
                .set_nonblocking(false)
                .expect("test connection can be blocking");
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            while !server_stop.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
            return;
        }
    });

    let child = Command::new(snarf_binary())
        .args(["scrape", &url, "--no-cache"])
        .env("XDG_CONFIG_HOME", isolated_dir("config"))
        .env("XDG_CACHE_HOME", isolated_dir("cache"))
        .env("SNARF_NO_UPDATE_NOTIFIER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("snarf starts");

    thread::sleep(Duration::from_secs(1));
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(status.success());

    let output = child.wait_with_output().expect("snarf exits");
    stop.store(true, Ordering::Relaxed);
    server.join().expect("test server exits");
    assert_eq!(output.status.code(), Some(6));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cancelled by SIGINT"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sigint_during_crawl_prints_summary_and_exits_cancelled() {
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
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !server_stop.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(10));
                continue;
            };
            stream
                .set_nonblocking(false)
                .expect("test connection can be blocking");
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let request = String::from_utf8_lossy(&request);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            match path {
                "/" => {
                    let body = r#"
                    <html>
                      <head><title>Root</title></head>
                      <body>
                        <main>
                          <p>Hello from the crawl seed.</p>
                          <a href="/slow">Slow page</a>
                        </main>
                      </body>
                    </html>
                    "#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
                "/slow" => {
                    server_slow_seen.store(true, Ordering::Relaxed);
                    while !server_stop.load(Ordering::Relaxed)
                        && std::time::Instant::now() < deadline
                    {
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

    let child = Command::new(snarf_binary())
        .args([
            "crawl",
            &url,
            "--no-cache",
            "--depth",
            "1",
            "--concurrency",
            "1",
        ])
        .env("XDG_CONFIG_HOME", isolated_dir("crawl-config"))
        .env("XDG_CACHE_HOME", isolated_dir("crawl-cache"))
        .env("SNARF_NO_UPDATE_NOTIFIER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("snarf starts");

    assert!(wait_for(&slow_seen), "crawl reached the slow page");
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(status.success());

    let output = child.wait_with_output().expect("snarf exits");
    stop.store(true, Ordering::Relaxed);
    server.join().expect("test server exits");

    assert_eq!(output.status.code(), Some(6));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("seed: "), "stderr: {stderr}");
    assert!(stderr.contains("pages: 1"), "stderr: {stderr}");
    assert!(stderr.contains("new: 1"), "stderr: {stderr}");
    assert!(
        stderr.contains("error: cancelled by SIGINT"),
        "stderr: {stderr}"
    );
}

fn snarf_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_snarf"))
}

fn isolated_dir(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/cancel-signal");
    let dir = root.join(name);
    fs::create_dir_all(&dir).expect("isolated dir exists");
    dir
}

fn wait_for(flag: &AtomicBool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if flag.load(Ordering::Relaxed) {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}
