//! Loopback listener that answers both HTTP/1.1 and an h2c prior-knowledge preface.
//!
//! Watch's initial fetch is HTTP/1.1. `fetch_batch` and `benchmark` use
//! `http2_prior_knowledge` and do not send HTTP/1.1. Both responses are HTTP 200
//! with no `ETag` and no `Last-Modified`. The body can change while the listener runs.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const FRAME_CAP: usize = 1024 * 1024;

pub struct DualListener {
    addr: SocketAddr,
    body: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DualListener {
    pub fn bind_loopback(body: &str) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|error| format!("premise: listener bind: {error}"))?;
        Self::from_listener(listener, body)
    }

    pub fn bind_ip(ip: IpAddr, body: &str) -> Result<Self, String> {
        let listener = TcpListener::bind(SocketAddr::new(ip, 0))
            .map_err(|error| format!("premise: listener bind: {error}"))?;
        Self::from_listener(listener, body)
    }

    pub fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    pub fn set_body(&self, body: &str) {
        *self
            .body
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = body.to_string();
    }

    fn from_listener(listener: TcpListener, body: &str) -> Result<Self, String> {
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("premise: listener: {error}"))?;
        let addr = listener
            .local_addr()
            .map_err(|error| format!("premise: listener: {error}"))?;
        let body = Arc::new(Mutex::new(body.to_string()));
        let stop = Arc::new(AtomicBool::new(false));
        let body_thread = Arc::clone(&body);
        let stop_thread = Arc::clone(&stop);
        let thread = thread::spawn(move || accept_loop(&listener, &body_thread, &stop_thread));
        Ok(Self {
            addr,
            body,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for DualListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn accept_loop(listener: &TcpListener, body: &Mutex<String>, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((socket, _)) => {
                let text = body
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let _ = socket.set_nonblocking(false);
                let _ = socket.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = socket.set_write_timeout(Some(Duration::from_secs(5)));
                serve(socket, text.as_bytes());
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
}

fn serve(mut socket: TcpStream, body: &[u8]) {
    let mut buf = [0u8; 24];
    let mut filled = 0;
    while filled < PREFACE.len() {
        match socket.read(&mut buf[filled..]) {
            Ok(0) => return,
            Ok(count) => {
                filled += count;
                if !PREFACE.starts_with(&buf[..filled]) {
                    serve_h1(&mut socket, body);
                    return;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
    serve_h2(socket, body);
}

fn serve_h1(socket: &mut TcpStream, body: &[u8]) {
    let mut buf = [0u8; 8192];
    let _ = socket.read(&mut buf);
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = socket.write_all(header.as_bytes());
    let _ = socket.write_all(body);
}

fn serve_h2(mut socket: TcpStream, body: &[u8]) {
    // `serve` already consumed the preface.
    let _ = socket.write_all(&frame(4, 0, 0, &[]));
    let mut stream_id = 0u32;
    let mut headers_done = false;
    let mut end_stream = false;
    for _ in 0..32 {
        let Some((kind, flags, stream, _payload)) = read_frame(&mut socket) else {
            return;
        };
        if kind == 4 && flags & 0x1 == 0 {
            let _ = socket.write_all(&frame(4, 0x1, 0, &[]));
        }
        if kind == 1 {
            stream_id = stream;
            if flags & 0x4 != 0 {
                headers_done = true;
            }
            if flags & 0x1 != 0 {
                end_stream = true;
            }
        }
        if kind == 9 && flags & 0x4 != 0 {
            headers_done = true;
        }
        if kind == 0 && stream == stream_id && flags & 0x1 != 0 {
            end_stream = true;
        }
        if headers_done && end_stream && stream_id != 0 {
            let _ = socket.write_all(&frame(1, 0x4, stream_id, &[0x88]));
            let _ = socket.write_all(&frame(0, 0x1, stream_id, body));
            return;
        }
    }
}

fn read_frame(socket: &mut TcpStream) -> Option<(u8, u8, u32, Vec<u8>)> {
    let mut head = [0u8; 9];
    read_exact(socket, &mut head).ok()?;
    let length = ((head[0] as usize) << 16) | ((head[1] as usize) << 8) | head[2] as usize;
    if length > FRAME_CAP {
        return None;
    }
    let mut payload = vec![0u8; length];
    if length > 0 {
        read_exact(socket, &mut payload).ok()?;
    }
    let stream = u32::from_be_bytes([head[5], head[6], head[7], head[8]]) & 0x7fff_ffff;
    Some((head[3], head[4], stream, payload))
}

fn frame(kind: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let length = u32::try_from(payload.len()).unwrap_or(0x00ff_ffff);
    let encoded = length.to_be_bytes();
    let mut out = Vec::with_capacity(9 + payload.len());
    out.extend_from_slice(&encoded[1..]);
    out.push(kind);
    out.push(flags);
    out.extend_from_slice(&stream.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn read_exact(socket: &mut TcpStream, buf: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        match socket.read(&mut buf[filled..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "eof",
                ));
            }
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
