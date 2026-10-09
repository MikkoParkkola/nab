//! One `nab-mcp` stdio process per case.
//!
//! Empty `HOME` does not isolate the watch store. On macOS `dirs` ignores
//! `XDG_DATA_HOME` and an empty `HOME` falls through to the passwd home, which
//! would read the operator's watches. Each child gets a fresh directory on both
//! `HOME` and `XDG_DATA_HOME`. `PATH` stays so `op` detection is the real one.
//! Both SSRF opt-outs are removed unless a case sets the allowlist.
//!
//! The readiness pause is measured once, from a throwaway field-absent ping, and
//! applied before the case message is written. The case budget starts at that
//! write. The calibration child does not take the process lock a second time.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use crate::wire::{self, Caps, Field};

static NAB: Mutex<()> = Mutex::new(());
static READY: OnceLock<Duration> = OnceLock::new();

pub fn nab_lock() -> MutexGuard<'static, ()> {
    NAB.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn server_bin() -> std::path::PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_nab_mcp") {
        return std::path::PathBuf::from(path);
    }
    let exe = std::env::current_exe().expect("harness: current_exe");
    let name = if cfg!(windows) {
        "nab-mcp.exe"
    } else {
        "nab-mcp"
    };
    exe.parent()
        .and_then(|dir| dir.parent())
        .expect("harness: profile dir")
        .join(name)
}

#[derive(Clone, Copy)]
pub enum Answer {
    /// Keep server requests. An elicitation or a sampling request ends the wait.
    Record,
    Decline,
    #[cfg_attr(not(feature = "task"), allow(dead_code))]
    Sample,
}

enum Stdout {
    Line(String),
    Pending(usize),
}

pub struct Exchange {
    pub before: Vec<Value>,
    pub response: Option<Value>,
    pub trailing: usize,
    pub raw_lines: usize,
}

pub struct StdioChild {
    child: Child,
    stdin: Option<ChildStdin>,
    incoming: mpsc::Receiver<Stdout>,
    reader: Option<JoinHandle<()>>,
    stderr_path: std::path::PathBuf,
    _home: TempDir,
    _lock: Option<MutexGuard<'static, ()>>,
}

impl StdioChild {
    pub fn start() -> Self {
        Self::start_allow(None)
    }

    pub fn start_allow(allowlist: Option<&str>) -> Self {
        let guard = nab_lock();
        let pause = ensure_ready();
        let mut child = spawn_inner(Some(guard), allowlist);
        thread::sleep(pause);
        child.alive_or_panic();
        child
    }

    pub fn exchange(&mut self, message: &Value, budget: Duration, answer: Answer) -> Exchange {
        self.write_message(message);
        self.collect(budget, true, answer)
    }

    pub fn drain(&mut self, budget: Duration) -> Vec<Value> {
        self.collect(budget, false, Answer::Record).before
    }

    fn write_message(&mut self, message: &Value) {
        let line = serde_json::to_string(message).expect("request json");
        let stdin = self.stdin.as_mut().expect("stdin");
        assert!(
            writeln!(stdin, "{line}")
                .and_then(|()| stdin.flush())
                .is_ok(),
            "harness: stdio write failed\n{}",
            self.stderr_tail()
        );
    }

    fn collect(&mut self, budget: Duration, stop_on_response: bool, answer: Answer) -> Exchange {
        let deadline = Instant::now() + budget;
        let mut before = Vec::new();
        let mut response = None;
        let mut trailing = 0usize;
        let mut raw_lines = 0usize;
        while Instant::now() < deadline {
            let remain = deadline.saturating_duration_since(Instant::now());
            match self.incoming.recv_timeout(remain) {
                Ok(Stdout::Pending(count)) => trailing = count,
                Ok(Stdout::Line(line)) => {
                    trailing = 0;
                    match serde_json::from_str(&line) {
                        Ok(value) => {
                            if take_message(
                                self,
                                &mut before,
                                &mut response,
                                value,
                                stop_on_response,
                                answer,
                            ) {
                                break;
                            }
                        }
                        Err(_) => raw_lines += 1,
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }
        self.alive_or_panic();
        Exchange {
            before,
            response,
            trailing,
            raw_lines,
        }
    }

    fn alive_or_panic(&mut self) {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                panic!("harness: nab-mcp exited {status}\n{}", self.stderr_tail());
            }
            Ok(None) => {}
            Err(error) => panic!("harness: wait failed: {error}\n{}", self.stderr_tail()),
        }
    }

    fn stderr_tail(&self) -> String {
        fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }
}

impl Drop for StdioChild {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn answer_request(child: &mut StdioChild, value: &Value, method: &str, answer: Answer) -> bool {
    let interesting = method == "elicitation/create" || method == "sampling/createMessage";
    if !interesting {
        return false;
    }
    match answer {
        Answer::Decline if method == "elicitation/create" => {
            let id = value.get("id").cloned().unwrap_or(Value::Null);
            child.write_message(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"action": "decline"}
            }));
            false
        }
        Answer::Sample if method == "sampling/createMessage" => {
            let id = value.get("id").cloned().unwrap_or(Value::Null);
            child.write_message(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "role": "assistant",
                    "model": "nab-test",
                    "content": {"type": "text", "text": "ok"}
                }
            }));
            false
        }
        Answer::Record | Answer::Decline | Answer::Sample => true,
    }
}

fn take_message(
    child: &mut StdioChild,
    before: &mut Vec<Value>,
    response: &mut Option<Value>,
    value: Value,
    stop_on_response: bool,
    answer: Answer,
) -> bool {
    if is_response(&value) && stop_on_response {
        *response = Some(value);
        return true;
    }
    if stop_on_response && is_request(&value) {
        let method = value.get("method").and_then(Value::as_str).unwrap_or("");
        if answer_request(child, &value, method, answer) {
            before.push(value);
            return matches!(answer, Answer::Record);
        }
    }
    before.push(value);
    false
}

fn is_response(value: &Value) -> bool {
    value.get("method").is_none()
        && value.get("id").is_some()
        && (value.get("result").is_some() || value.get("error").is_some())
}

fn is_request(value: &Value) -> bool {
    value.get("method").is_some() && value.get("id").is_some()
}

fn ensure_ready() -> Duration {
    if let Some(measured) = READY.get() {
        return *measured + Duration::from_millis(300);
    }
    let mut child = spawn_inner(None, None);
    let started = Instant::now();
    let ping = wire::ping(wire::id_num(), Field::Absent, Caps::Absent);
    let exchange = child.exchange(&ping, wire::READY_CAP, Answer::Record);
    let elapsed = started.elapsed();
    let success = exchange
        .response
        .as_ref()
        .is_some_and(|value| value.get("result").is_some() && value.get("error").is_none());
    assert!(
        success,
        "harness: stdio readiness ping failed after {elapsed:?}\n{}",
        child.stderr_tail()
    );
    let _ = READY.set(elapsed);
    drop(child);
    elapsed + Duration::from_millis(300)
}

fn spawn_inner(lock: Option<MutexGuard<'static, ()>>, allowlist: Option<&str>) -> StdioChild {
    let home = TempDir::new().expect("temp home");
    let stderr_path = home.path().join("stderr.txt");
    let stderr = File::create(&stderr_path).expect("stderr file");
    let mut command = Command::new(server_bin());
    command
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path())
        .env_remove("NAB_SSRF_ALLOWLIST")
        .env_remove("NAB_SSRF_ALLOW_PRIVATE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr));
    if let Some(allowlist) = allowlist {
        command.env("NAB_SSRF_ALLOWLIST", allowlist);
    }
    let mut child = command.spawn().unwrap_or_else(|error| {
        panic!("harness: failed to spawn nab-mcp: {error}");
    });
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (sender, incoming) = mpsc::channel();
    let reader = thread::spawn(move || read_stdout(stdout, sender));
    StdioChild {
        child,
        stdin: Some(stdin),
        incoming,
        reader: Some(reader),
        stderr_path,
        _home: home,
        _lock: lock,
    }
}

/// A partial stdout line is reported while the pipe stays open. Silence is zero bytes.
fn read_stdout(mut stdout: ChildStdout, sender: mpsc::Sender<Stdout>) {
    set_nonblocking(&stdout);
    let mut pending = Vec::new();
    let mut announced = 0usize;
    let mut tmp = [0u8; 4096];
    loop {
        match stdout.read(&mut tmp) {
            Ok(0) => {
                let _ = send_pending(&sender, &pending, &mut announced);
                break;
            }
            Ok(count) => {
                pending.extend_from_slice(&tmp[..count]);
                if !send_lines(&sender, &mut pending, &mut announced) {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if !send_pending(&sender, &pending, &mut announced) {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
    drop(sender);
}

fn send_lines(sender: &mpsc::Sender<Stdout>, pending: &mut Vec<u8>, announced: &mut usize) -> bool {
    while let Some(pos) = pending.iter().position(|byte| *byte == b'\n') {
        let mut raw: Vec<u8> = pending.drain(..=pos).collect();
        raw.pop();
        if raw.last() == Some(&b'\r') {
            raw.pop();
        }
        let line = String::from_utf8_lossy(&raw).into_owned();
        if sender.send(Stdout::Line(line)).is_err() {
            return false;
        }
        *announced = 0;
    }
    send_pending(sender, pending, announced)
}

fn send_pending(sender: &mpsc::Sender<Stdout>, pending: &[u8], announced: &mut usize) -> bool {
    if pending.is_empty() || pending.len() == *announced {
        return true;
    }
    if sender.send(Stdout::Pending(pending.len())).is_err() {
        return false;
    }
    *announced = pending.len();
    true
}

fn set_nonblocking(stdout: &impl AsRawFd) {
    const F_GETFL: i32 = 3;
    const F_SETFL: i32 = 4;
    #[cfg(target_os = "linux")]
    const O_NONBLOCK: i32 = 0x800;
    #[cfg(not(target_os = "linux"))]
    const O_NONBLOCK: i32 = 4;
    unsafe extern "C" {
        fn fcntl(fd: i32, cmd: i32, arg: i32) -> i32;
    }
    let fd = stdout.as_raw_fd();
    // SAFETY: `fd` is this child's live stdout pipe. Both commands only read or set status flags.
    let flags = unsafe { fcntl(fd, F_GETFL, 0) };
    assert!(flags >= 0, "harness: stdout F_GETFL failed");
    let status = unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) };
    assert!(status >= 0, "harness: stdout F_SETFL failed");
}
