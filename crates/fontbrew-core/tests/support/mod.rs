#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::Duration,
};

use fontbrew_core::fetch::{NetworkClient, NetworkEndpoints};

const PROBE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordedRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct RouteResponse {
    status: u16,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
    content_length: Option<u64>,
    chunk_size: Option<usize>,
    cancel_after_chunks: Option<(usize, Arc<std::sync::atomic::AtomicBool>)>,
    probe: Option<Arc<ServerConcurrencyProbe>>,
    gate: Option<Arc<ResponseGate>>,
}

#[derive(Debug)]
struct ServerState {
    base_url: String,
    routes: Mutex<BTreeMap<String, RouteResponse>>,
    requests: Mutex<Vec<RecordedRequest>>,
}

#[derive(Debug)]
pub(crate) struct LocalHttpServer {
    state: Arc<ServerState>,
}

impl LocalHttpServer {
    pub(crate) fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP server");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("local server address")
        );
        let state = Arc::new(ServerState {
            base_url,
            routes: Mutex::new(BTreeMap::new()),
            requests: Mutex::new(Vec::new()),
        });
        let server_state = state.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let stream = stream.expect("accept local HTTP connection");
                let connection_state = server_state.clone();
                thread::spawn(move || handle_connection(stream, connection_state));
            }
        });

        Self { state }
    }

    pub(crate) fn network_client(&self) -> NetworkClient {
        NetworkClient::with_client_and_endpoints(
            reqwest::Client::new(),
            NetworkEndpoints {
                github_api_base_url: self.base_url(),
                fontsource_api_base_url: self.base_url(),
            },
        )
    }

    pub(crate) fn base_url(&self) -> String {
        self.state.base_url.clone()
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{}", self.state.base_url, normalize_path(path))
    }

    pub(crate) fn respond_text(&self, path: &str, body: impl Into<String>) {
        self.respond(path, 200, body.into().into_bytes());
    }

    pub(crate) fn respond_text_with_gate(
        &self,
        path: &str,
        body: impl Into<String>,
        gate: Arc<ResponseGate>,
    ) {
        let mut response = self.response(200, body.into().into_bytes());
        response.gate = Some(gate);
        self.insert_route(path, response);
    }

    pub(crate) fn respond_bytes(&self, path: &str, body: Vec<u8>) {
        self.respond(path, 200, body);
    }

    pub(crate) fn respond_bytes_with_gate(
        &self,
        path: &str,
        body: Vec<u8>,
        gate: Arc<ResponseGate>,
    ) {
        let mut response = self.response(200, body);
        response.gate = Some(gate);
        self.insert_route(path, response);
    }

    pub(crate) fn respond_status(&self, path: &str, status: u16, body: impl Into<Vec<u8>>) {
        self.respond(path, status, body.into());
    }

    pub(crate) fn respond_content_length(&self, path: &str, content_length: u64) {
        self.insert_route(
            path,
            RouteResponse {
                status: 200,
                body: Vec::new(),
                headers: Vec::new(),
                content_length: Some(content_length),
                chunk_size: None,
                cancel_after_chunks: None,
                probe: None,
                gate: None,
            },
        );
    }

    pub(crate) fn respond_bytes_with_cancellation(
        &self,
        path: &str,
        body: Vec<u8>,
        chunk_size: usize,
        cancel_after_chunks: usize,
        cancel_flag: Arc<std::sync::atomic::AtomicBool>,
    ) {
        self.insert_route(
            path,
            RouteResponse {
                status: 200,
                content_length: Some(body.len() as u64),
                body,
                headers: Vec::new(),
                chunk_size: Some(chunk_size),
                cancel_after_chunks: Some((cancel_after_chunks, cancel_flag)),
                probe: None,
                gate: None,
            },
        );
    }

    pub(crate) fn respond_with_probe(
        &self,
        path: &str,
        body: impl Into<Vec<u8>>,
        probe: Arc<ServerConcurrencyProbe>,
    ) {
        self.insert_route(
            path,
            RouteResponse {
                status: 200,
                body: body.into(),
                headers: Vec::new(),
                content_length: None,
                chunk_size: None,
                cancel_after_chunks: None,
                probe: Some(probe),
                gate: None,
            },
        );
    }

    pub(crate) fn request_urls(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .map(|request| request.url)
            .collect()
    }

    pub(crate) fn requests(&self) -> Vec<RecordedRequest> {
        self.state.requests.lock().expect("requests lock").clone()
    }

    fn respond(&self, path: &str, status: u16, body: Vec<u8>) {
        self.insert_route(path, self.response(status, body));
    }

    fn response(&self, status: u16, body: Vec<u8>) -> RouteResponse {
        RouteResponse {
            status,
            body,
            headers: Vec::new(),
            content_length: None,
            chunk_size: None,
            cancel_after_chunks: None,
            probe: None,
            gate: None,
        }
    }

    fn insert_route(&self, path: &str, response: RouteResponse) {
        self.state
            .routes
            .lock()
            .expect("routes lock")
            .insert(normalize_path(path), response);
    }
}

#[derive(Debug)]
pub(crate) struct ResponseGate {
    state: Mutex<ResponseGateState>,
    state_changed: Condvar,
}

#[derive(Debug)]
struct ResponseGateState {
    arrived: bool,
    released: bool,
    completed: bool,
}

impl ResponseGate {
    pub(crate) fn blocked() -> Self {
        Self::new(false)
    }

    pub(crate) fn open() -> Self {
        Self::new(true)
    }

    pub(crate) fn wait_for_arrival(&self) {
        self.wait_until("response arrival", |state| state.arrived);
    }

    pub(crate) fn wait_for_completion(&self) {
        self.wait_until("response completion", |state| state.completed);
    }

    pub(crate) fn release(&self) {
        let mut state = self.state.lock().expect("response gate lock");
        state.released = true;
        self.state_changed.notify_all();
    }

    fn new(released: bool) -> Self {
        Self {
            state: Mutex::new(ResponseGateState {
                arrived: false,
                released,
                completed: false,
            }),
            state_changed: Condvar::new(),
        }
    }

    fn arrive_and_wait_for_release(&self) {
        let mut state = self.state.lock().expect("response gate lock");
        state.arrived = true;
        self.state_changed.notify_all();
        while !state.released {
            let (next_state, wait_result) = self
                .state_changed
                .wait_timeout(state, PROBE_WAIT_TIMEOUT)
                .expect("response gate wait");
            state = next_state;
            if wait_result.timed_out() && !state.released {
                panic!("timed out waiting for gated response release");
            }
        }
    }

    fn complete(&self) {
        let mut state = self.state.lock().expect("response gate lock");
        state.completed = true;
        self.state_changed.notify_all();
    }

    fn wait_until(&self, description: &str, is_ready: impl Fn(&ResponseGateState) -> bool) {
        let mut state = self.state.lock().expect("response gate lock");
        while !is_ready(&state) {
            let (next_state, wait_result) = self
                .state_changed
                .wait_timeout(state, PROBE_WAIT_TIMEOUT)
                .expect("response gate wait");
            state = next_state;
            if wait_result.timed_out() && !is_ready(&state) {
                panic!("timed out waiting for {description}");
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct ServerConcurrencyProbe {
    active: AtomicUsize,
    max_active: AtomicUsize,
    release_entries: Mutex<usize>,
    release_gate: Condvar,
    wait_for_first_entries: usize,
}

impl ServerConcurrencyProbe {
    pub(crate) fn new(wait_for_first_entries: usize) -> Self {
        Self {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            release_entries: Mutex::new(0),
            release_gate: Condvar::new(),
            wait_for_first_entries,
        }
    }

    fn enter_release_request(&self) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);

        if self.wait_for_first_entries > 1 {
            let mut entries = self.release_entries.lock().expect("release entries lock");
            if *entries < self.wait_for_first_entries {
                *entries += 1;
                self.release_gate.notify_all();
                while *entries < self.wait_for_first_entries {
                    let (next_entries, wait_result) = self
                        .release_gate
                        .wait_timeout(entries, PROBE_WAIT_TIMEOUT)
                        .expect("release gate wait");
                    entries = next_entries;
                    if wait_result.timed_out() && *entries < self.wait_for_first_entries {
                        self.active.fetch_sub(1, Ordering::SeqCst);
                        panic!(
                            "timed out waiting for {expected} concurrent release requests; observed {observed}",
                            expected = self.wait_for_first_entries,
                            observed = *entries
                        );
                    }
                }
            }
        }

        self.active.fetch_sub(1, Ordering::SeqCst);
    }

    pub(crate) fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }
}

fn handle_connection(mut stream: TcpStream, state: Arc<ServerState>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone local HTTP stream"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() || request_line.trim().is_empty() {
        return;
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let request_target = parts.next().unwrap_or_default().to_string();
    let path = request_path(&request_target);
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    state
        .requests
        .lock()
        .expect("requests lock")
        .push(RecordedRequest {
            method,
            path: path.clone(),
            url: format!("{}{}", state.base_url, path),
            headers,
        });

    let response = state
        .routes
        .lock()
        .expect("routes lock")
        .get(&path)
        .cloned()
        .unwrap_or_else(not_found_response);
    if let Some(probe) = &response.probe {
        probe.enter_release_request();
    }
    let gate = response.gate.clone();
    if let Some(gate) = &gate {
        gate.arrive_and_wait_for_release();
    }
    write_response(&mut stream, response);
    if let Some(gate) = &gate {
        gate.complete();
    }
}

fn write_response(stream: &mut TcpStream, response: RouteResponse) {
    let reason = match response.status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let mut headers = response.headers;
    headers.push((
        "Content-Length".to_string(),
        response
            .content_length
            .unwrap_or(response.body.len() as u64)
            .to_string(),
    ));
    headers.push(("Connection".to_string(), "close".to_string()));

    write!(stream, "HTTP/1.1 {} {}\r\n", response.status, reason).expect("write status line");
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n").expect("write header");
    }
    write!(stream, "\r\n").expect("write header terminator");
    if let Some(chunk_size) = response.chunk_size {
        for (index, chunk) in response.body.chunks(chunk_size).enumerate() {
            stream.write_all(chunk).expect("write body chunk");
            stream.flush().expect("flush body chunk");
            if let Some((cancel_after_chunks, cancel_flag)) = &response.cancel_after_chunks {
                if index + 1 == *cancel_after_chunks {
                    cancel_flag.store(true, Ordering::SeqCst);
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
    } else {
        stream.write_all(&response.body).expect("write body");
    }
    stream.flush().expect("flush response body");
    stream
        .shutdown(Shutdown::Write)
        .expect("shutdown response body");
}

fn not_found_response() -> RouteResponse {
    RouteResponse {
        status: 404,
        body: b"not found".to_vec(),
        headers: Vec::new(),
        content_length: None,
        chunk_size: None,
        cancel_after_chunks: None,
        probe: None,
        gate: None,
    }
}

fn normalize_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

fn request_path(request_target: &str) -> String {
    if let Some(rest) = request_target.strip_prefix("http://") {
        if let Some(path_start) = rest.find('/') {
            return normalize_path(&rest[path_start..]);
        }
        return "/".to_string();
    }
    normalize_path(request_target)
}
