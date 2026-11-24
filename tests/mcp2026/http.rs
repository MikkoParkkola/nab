//! Raw HTTP/1.1 client for `/mcp`.
//!
//! `reqwest` hides empty 202 bodies, SSE framing, and a budget that expires
//! before the body. This client stops at the budget. A `text/event-stream`
//! body yields the first non-empty `data:` event that a blank line has ended.
//! A line that starts with `:` is a comment, not that event. An empty line is
//! a keep-alive, not the message.
//! Status 403 is a harness failure.
//! A timeout with headers and no JSON-RPC object is an observation.

use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::MutexGuard;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use crate::harness::{nab_lock, server_bin};
use crate::wire;

#[derive(Clone, Copy)]
pub enum Accept {
    JsonAndSse,
    Omit,
    EventStream,
}

#[derive(Clone, Copy)]
pub enum Proto<'a> {
    Omit,
    Empty,
    Value(&'a str),
}

pub enum Observed {
    Rpc {
        status: u16,
        headers: Vec<(String, String)>,
        body: Value,
    },
    Other {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    Empty {
        status: u16,
        headers: Vec<(String, String)>,
    },
}

pub struct HttpChild {
    child: Child,
    addr: SocketAddr,
    stderr_path: std::path::PathBuf,
    _home: TempDir,
    _lock: Option<MutexGuard<'static, ()>>,
}

impl HttpChild {
    pub fn start() -> Self {
        Self::start_allow(None)
    }

    pub fn start_allow(allowlist: Option<&str>) -> Self {
        let lock = nab_lock();
        let probe = TcpListener::bind("127.0.0.1:0").expect("probe bind");
        let port = probe.local_addr().expect("probe addr").port();
        drop(probe);
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let home = TempDir::new().expect("temp home");
        let stderr_path = home.path().join("stderr.txt");
        let stderr = File::create(&stderr_path).expect("stderr");
        let mut command = Command::new(server_bin());
        command
            .arg("--http")
            .arg(format!("127.0.0.1:{port}"))
            .env("HOME", home.path())
            .env("XDG_DATA_HOME", home.path())
            .env_remove("NAB_SSRF_ALLOWLIST")
            .env_remove("NAB_SSRF_ALLOW_PRIVATE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr));
        if let Some(allowlist) = allowlist {
            command.env("NAB_SSRF_ALLOWLIST", allowlist);
        }
        let mut child = command.spawn().expect("spawn nab-mcp");
        wait_accept(&mut child, addr, &stderr_path);
        Self {
            child,
            addr,
            stderr_path,
            _home: home,
            _lock: Some(lock),
        }
    }

    fn ensure_alive(&mut self) {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let stderr = std::fs::read_to_string(&self.stderr_path).unwrap_or_default();
                panic!("harness: nab-mcp exited {status}\n{stderr}");
            }
            Ok(None) => {}
            Err(error) => panic!("harness: wait failed: {error}"),
        }
    }

    pub fn post(
        &mut self,
        body: &Value,
        header: Proto<'_>,
        session: Option<&str>,
        accept: Accept,
        budget: Duration,
    ) -> Observed {
        let bytes = serde_json::to_vec(body).expect("body");
        let mut request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {}\r\nOrigin: http://127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
            self.addr,
            bytes.len()
        );
        request.push_str(&accept_line(accept));
        request.push_str(&proto_line(header));
        if let Some(session) = session {
            request.push_str("mcp-session-id: ");
            request.push_str(session);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        self.round_trip(request.as_bytes(), &bytes, budget)
    }

    /// Reads the POST, then keeps that same stream for two more seconds.
    /// Later JSON `data:` events are returned beside the first observation.
    /// EOF ends the wait. This does not open a GET stream.
    pub fn post_drain(
        &mut self,
        body: &Value,
        header: Proto<'_>,
        session: Option<&str>,
        accept: Accept,
        budget: Duration,
    ) -> (Observed, Vec<Value>) {
        let bytes = serde_json::to_vec(body).expect("body");
        let mut request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {}\r\nOrigin: http://127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
            self.addr,
            bytes.len()
        );
        request.push_str(&accept_line(accept));
        request.push_str(&proto_line(header));
        if let Some(session) = session {
            request.push_str("mcp-session-id: ");
            request.push_str(session);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        let mut stream = TcpStream::connect_timeout(&self.addr, Duration::from_secs(2))
            .unwrap_or_else(|error| panic!("harness: connect {}: {error}", self.addr));
        let _ = stream.set_nodelay(true);
        stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.write_all(&bytes))
            .unwrap_or_else(|error| panic!("harness: HTTP write failed: {error}"));
        let raw = read_response(&mut stream, budget);
        assert!(raw.status != 403, "harness: HTTP 403 from {}", self.addr);
        let mut combined = raw.body.clone();
        combined.extend_from_slice(&read_more(&mut stream, wire::TWO));
        let view = event_bytes(&raw.headers, &combined);
        let mut events = all_json(&view);
        let observed = classify(raw);
        if let Observed::Rpc { body, .. } = &observed
            && events.first() == Some(body)
        {
            events.remove(0);
        }
        self.ensure_alive();
        (observed, events)
    }

    pub fn get(
        &mut self,
        header: Proto<'_>,
        session: Option<&str>,
        accept: Accept,
        budget: Duration,
    ) -> Observed {
        self.no_body("GET", header, session, accept, budget)
    }

    pub fn delete(
        &mut self,
        header: Proto<'_>,
        session: Option<&str>,
        accept: Accept,
        budget: Duration,
    ) -> Observed {
        self.no_body("DELETE", header, session, accept, budget)
    }

    pub fn session_of(observed: &Observed) -> Option<String> {
        header_value(headers_of(observed), "mcp-session-id")
    }

    fn no_body(
        &mut self,
        method: &str,
        header: Proto<'_>,
        session: Option<&str>,
        accept: Accept,
        budget: Duration,
    ) -> Observed {
        let mut request = format!(
            "{method} /mcp HTTP/1.1\r\nHost: {}\r\nOrigin: http://127.0.0.1\r\n",
            self.addr
        );
        request.push_str(&accept_line(accept));
        request.push_str(&proto_line(header));
        if let Some(session) = session {
            request.push_str("mcp-session-id: ");
            request.push_str(session);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        self.round_trip(request.as_bytes(), &[], budget)
    }

    fn round_trip(&mut self, head: &[u8], body: &[u8], budget: Duration) -> Observed {
        let mut stream = TcpStream::connect_timeout(&self.addr, Duration::from_secs(2))
            .unwrap_or_else(|error| panic!("harness: connect {}: {error}", self.addr));
        let _ = stream.set_nodelay(true);
        stream
            .write_all(head)
            .and_then(|()| stream.write_all(body))
            .unwrap_or_else(|error| {
                panic!("harness: HTTP write failed: {error}");
            });
        let raw = read_response(&mut stream, budget);
        assert!(raw.status != 403, "harness: HTTP 403 from {}", self.addr);
        let observed = classify(raw);
        self.ensure_alive();
        observed
    }
}

impl Drop for HttpChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Raw {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn wait_accept(child: &mut Child, addr: SocketAddr, stderr_path: &std::path::Path) {
    let deadline = Instant::now() + wire::READY_CAP;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            let stderr = std::fs::read_to_string(stderr_path).unwrap_or_default();
            panic!("harness: nab-mcp exited {status}\n{stderr}");
        }
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let stderr = std::fs::read_to_string(stderr_path).unwrap_or_default();
    panic!("harness: {addr} did not accept\n{stderr}");
}

fn accept_line(accept: Accept) -> String {
    match accept {
        Accept::JsonAndSse => "Accept: application/json, text/event-stream\r\n".into(),
        Accept::EventStream => "Accept: text/event-stream\r\n".into(),
        Accept::Omit => String::new(),
    }
}

fn proto_line(header: Proto<'_>) -> String {
    match header {
        Proto::Omit => String::new(),
        Proto::Empty => "mcp-protocol-version:\r\n".into(),
        Proto::Value(value) => format!("mcp-protocol-version: {value}\r\n"),
    }
}

fn read_response(stream: &mut TcpStream, budget: Duration) -> Raw {
    let deadline = Instant::now() + budget;
    let mut buf = Vec::new();
    loop {
        if let Some(raw) = try_complete(&buf) {
            return raw;
        }
        if Instant::now() >= deadline {
            return partial(&buf);
        }
        let remain = deadline.saturating_duration_since(Instant::now());
        let _ = stream.set_read_timeout(Some(remain.min(Duration::from_millis(200))));
        let mut tmp = [0u8; 4096];
        match stream.read(&mut tmp) {
            Ok(0) => return partial(&buf),
            Ok(count) => buf.extend_from_slice(&tmp[..count]),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => return partial(&buf),
        }
    }
}

fn try_complete(buf: &[u8]) -> Option<Raw> {
    let split = find_header_end(buf)?;
    let (status, headers) = parse_head(&buf[..split])?;
    let body = &buf[split + 4..];
    if content_type(&headers).contains("event-stream") {
        // SSE stays open, so a complete data event returns before the terminating chunk.
        let decoded = if chunked(&headers) {
            decode_available(body)
        } else {
            None
        };
        let scan = decoded.as_ref().map_or(body, |view| view.bytes.as_slice());
        if terminated_data(scan).is_some_and(|data| serde_json::from_str::<Value>(&data).is_ok())
            || decoded.as_ref().is_some_and(|view| view.finished)
        {
            return Some(Raw {
                status,
                headers,
                body: body.to_vec(),
            });
        }
        if header_value(&headers, "content-length").is_some_and(|value| value == "0") {
            return Some(Raw {
                status,
                headers,
                body: Vec::new(),
            });
        }
        return None;
    }
    if let Some(length) = content_length(&headers) {
        if body.len() >= length {
            return Some(Raw {
                status,
                headers,
                body: body[..length].to_vec(),
            });
        }
        return None;
    }
    if chunked(&headers) {
        return decode_chunks(body).map(|decoded| Raw {
            status,
            headers,
            body: decoded,
        });
    }
    if status == 202 || status == 204 {
        return Some(Raw {
            status,
            headers,
            body: body.to_vec(),
        });
    }
    None
}

fn partial(buf: &[u8]) -> Raw {
    if let Some(split) = find_header_end(buf)
        && let Some((status, headers)) = parse_head(&buf[..split])
    {
        return Raw {
            status,
            headers,
            body: buf[split + 4..].to_vec(),
        };
    }
    Raw {
        status: 0,
        headers: Vec::new(),
        body: buf.to_vec(),
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_head(head: &[u8]) -> Option<(u16, Vec<(String, String)>)> {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let status = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    Some((status, headers))
}

fn content_length(headers: &[(String, String)]) -> Option<usize> {
    header_value(headers, "content-length")?.parse().ok()
}

fn content_type(headers: &[(String, String)]) -> String {
    header_value(headers, "content-type").unwrap_or_default()
}

fn chunked(headers: &[(String, String)]) -> bool {
    header_value(headers, "transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
}

/// A zero-length chunked payload is an empty body. The terminator is framing.
fn finished_empty_chunk(headers: &[(String, String)], body: &[u8]) -> bool {
    if !chunked(headers) {
        return false;
    }
    match decode_available(body) {
        Some(decoded) => decoded.finished && decoded.bytes.is_empty(),
        None => false,
    }
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

struct Decoded {
    bytes: Vec<u8>,
    finished: bool,
}

/// Complete chunks only. A zero-size chunk sets `finished`. A short chunk does not.
fn decode_available(body: &[u8]) -> Option<Decoded> {
    let mut bytes = Vec::new();
    let mut rest = body;
    loop {
        let Some(end) = rest.windows(2).position(|window| window == b"\r\n") else {
            return Some(Decoded {
                bytes,
                finished: false,
            });
        };
        let size =
            usize::from_str_radix(std::str::from_utf8(&rest[..end]).ok()?.trim(), 16).ok()?;
        rest = &rest[end + 2..];
        if size == 0 {
            return Some(Decoded {
                bytes,
                finished: true,
            });
        }
        if rest.len() < size + 2 {
            return Some(Decoded {
                bytes,
                finished: false,
            });
        }
        bytes.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

fn event_bytes(headers: &[(String, String)], body: &[u8]) -> Vec<u8> {
    if content_type(headers).contains("event-stream")
        && chunked(headers)
        && let Some(decoded) = decode_available(body)
    {
        return decoded.bytes;
    }
    body.to_vec()
}

fn decode_chunks(body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let end = rest.windows(2).position(|window| window == b"\r\n")?;
        let size = std::str::from_utf8(&rest[..end]).ok()?.trim();
        let size = usize::from_str_radix(size, 16).ok()?;
        rest = &rest[end + 2..];
        if size == 0 {
            return Some(out);
        }
        if rest.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

/// Payload of the first `data:` line that a blank line has ended.
fn terminated_data(body: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    take_event(text).map(|(payload, _)| payload.to_string())
}

fn take_event(text: &str) -> Option<(&str, &str)> {
    let mut rest = text;
    while !rest.is_empty() {
        let (raw, tail) = rest.split_once('\n')?;
        let line = raw.trim_end_matches('\r');
        if let Some(payload) = line.strip_prefix("data:") {
            let payload = payload.strip_prefix(' ').unwrap_or(payload);
            let tail = tail.strip_prefix('\r').unwrap_or(tail);
            if !tail.starts_with('\n') {
                return None;
            }
            let next = &tail[1..];
            if !payload.is_empty() {
                return Some((payload, next));
            }
            rest = next;
            continue;
        }
        rest = tail;
    }
    None
}

fn classify(raw: Raw) -> Observed {
    if raw.body.is_empty() || finished_empty_chunk(&raw.headers, &raw.body) {
        return Observed::Empty {
            status: raw.status,
            headers: raw.headers,
        };
    }
    if content_type(&raw.headers).contains("event-stream") {
        let scan = event_bytes(&raw.headers, &raw.body);
        if let Some(data) = terminated_data(&scan) {
            if let Ok(value) = serde_json::from_str::<Value>(&data)
                && value.get("jsonrpc").is_some()
            {
                return Observed::Rpc {
                    status: raw.status,
                    headers: raw.headers,
                    body: value,
                };
            }
            return Observed::Other {
                status: raw.status,
                headers: raw.headers,
                body: data.into_bytes(),
            };
        }
        return Observed::Other {
            status: raw.status,
            headers: raw.headers,
            body: raw.body,
        };
    }
    if let Ok(value) = serde_json::from_slice::<Value>(&raw.body)
        && value.get("jsonrpc").is_some()
    {
        return Observed::Rpc {
            status: raw.status,
            headers: raw.headers,
            body: value,
        };
    }
    Observed::Other {
        status: raw.status,
        headers: raw.headers,
        body: raw.body,
    }
}

fn read_more(stream: &mut TcpStream, budget: Duration) -> Vec<u8> {
    let deadline = Instant::now() + budget;
    let mut buf = Vec::new();
    while Instant::now() < deadline {
        let remain = deadline.saturating_duration_since(Instant::now());
        let _ = stream.set_read_timeout(Some(remain.min(Duration::from_millis(200))));
        let mut tmp = [0u8; 4096];
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(count) => buf.extend_from_slice(&tmp[..count]),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
    buf
}

fn all_json(body: &[u8]) -> Vec<Value> {
    let Ok(text) = std::str::from_utf8(body) else {
        return Vec::new();
    };
    let mut rest = text;
    let mut out = Vec::new();
    while let Some((payload, next)) = take_event(rest) {
        if let Ok(value) = serde_json::from_str::<Value>(payload)
            && value.get("jsonrpc").is_some()
        {
            out.push(value);
        }
        rest = next;
    }
    out
}

fn headers_of(observed: &Observed) -> &[(String, String)] {
    match observed {
        Observed::Rpc { headers, .. }
        | Observed::Other { headers, .. }
        | Observed::Empty { headers, .. } => headers,
    }
}
