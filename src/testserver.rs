use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct TestResponse {
    pub status: String,
    pub content_type: String,
    pub body: String,
}

impl TestResponse {
    pub fn new(status: &str, content_type: &str, body: &str) -> Self {
        Self {
            status: status.to_string(),
            content_type: content_type.to_string(),
            body: body.to_string(),
        }
    }
}

pub struct TestHttpServer {
    pub url: String,
    routes: Option<Arc<Mutex<HashMap<String, String>>>>,
    paths: Arc<Mutex<Vec<String>>>,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TestHttpServer {
    pub fn routes(routes: HashMap<String, String>) -> Self {
        Self::routes_with_fallback(routes, false)
    }

    pub fn routes_with_root_fallback(routes: HashMap<String, String>) -> Self {
        Self::routes_with_fallback(routes, true)
    }

    pub fn responses(responses: Vec<TestResponse>) -> Self {
        let listener = bind_listener();
        let url = listener_url(&listener);
        let paths = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let server_paths = Arc::clone(&paths);
        let server_requests = Arc::clone(&requests);
        let server_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut index = 0;
            while index < responses.len()
                && !server_stop.load(Ordering::Relaxed)
                && Instant::now() < deadline
            {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream
                    .set_nonblocking(false)
                    .expect("test connection can be blocking");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("test server sets read timeout");
                let request = read_http_request(&mut stream);
                record_request(&server_paths, &server_requests, request);

                let response = &responses[index];
                index += 1;
                write_response(
                    &mut stream,
                    &response.status,
                    &response.content_type,
                    &response.body,
                );
            }
        });

        Self {
            url,
            routes: None,
            paths,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    pub fn paths(&self) -> Vec<String> {
        self.paths
            .lock()
            .expect("paths mutex is not poisoned")
            .clone()
    }

    pub fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("requests mutex is not poisoned")
            .clone()
    }

    pub fn replace_route_token(&self, token: &str) {
        let routes = self
            .routes
            .as_ref()
            .expect("route replacement requires route-backed server");
        let mut routes = routes.lock().expect("routes mutex is not poisoned");
        for body in routes.values_mut() {
            *body = body.replace(token, &self.url);
        }
    }

    fn routes_with_fallback(routes: HashMap<String, String>, root_fallback: bool) -> Self {
        let listener = bind_listener();
        let url = listener_url(&listener);
        let routes = Arc::new(Mutex::new(routes));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let server_routes = Arc::clone(&routes);
        let server_paths = Arc::clone(&paths);
        let server_requests = Arc::clone(&requests);
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
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("test server sets read timeout");
                let request = read_http_request(&mut stream);
                let path = request_path(&request);
                record_request(&server_paths, &server_requests, request);

                let routes = server_routes.lock().expect("routes mutex is not poisoned");
                let body = routes
                    .get(&path)
                    .or_else(|| root_fallback.then(|| routes.get("/")).flatten());
                let (status, body) = body
                    .map(|body| ("200 OK", body.as_str()))
                    .unwrap_or(("404 Not Found", "not found"));
                let content_type = if path == "/llms.txt" {
                    "text/plain"
                } else {
                    "text/html"
                };
                write_response(&mut stream, status, content_type, body);
            }
        });

        Self {
            url,
            routes: Some(routes),
            paths,
            requests,
            stop,
            handle: Some(handle),
        }
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

fn bind_listener() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server binds");
    listener
        .set_nonblocking(true)
        .expect("test server can be nonblocking");
    listener
}

fn listener_url(listener: &TcpListener) -> String {
    format!("http://{}", listener.local_addr().expect("server has addr"))
}

fn record_request(
    paths: &Arc<Mutex<Vec<String>>>,
    requests: &Arc<Mutex<Vec<String>>>,
    request: String,
) {
    paths
        .lock()
        .expect("paths mutex is not poisoned")
        .push(request_path(&request));
    requests
        .lock()
        .expect("requests mutex is not poisoned")
        .push(request);
}

fn request_path(request: &str) -> String {
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    if let Ok(url) = url::Url::parse(target) {
        let mut path = url.path().to_string();
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        return path;
    }
    target.to_string()
}

fn write_response(stream: &mut std::net::TcpStream, status: &str, content_type: &str, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .expect("test server writes response");
}

// Read a full HTTP request (headers + any Content-Length body) so the socket
// has no unread inbound bytes when we respond and close. Closing a socket with
// unread data can trigger a TCP RST on Linux loopback.
fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Some(header_end) = find_header_end(&buf) {
            let content_length = parse_content_length(&buf[..header_end]);
            if buf.len() >= header_end + content_length {
                break;
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn parse_content_length(headers: &[u8]) -> usize {
    String::from_utf8_lossy(headers)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0)
}
