//! Daily heartbeat for release builds.
//!
//! At most one POST per day. The body is the seven fields the receiver accepts.
//! Debug builds and tests return before any of this runs, so `cargo run` and the
//! test suite stay quiet. A failure never surfaces to the caller.
//!
//! The caller holds the returned guard until the command is finished. Drop waits
//! for the post for at most three seconds, so a short command still sends.
//! Startup does not wait.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rand::RngExt;

const DEFAULT_ENDPOINT: &str = "https://telemetry.revaluator.ai/v1/heartbeat";
const ENDPOINT_ENV: &str = "NAB_TELEMETRY_ENDPOINT";
const MAX_BODY: usize = 2048;
const DAY: i64 = 24 * 60 * 60;
/// Bound on the request and on shutdown. Certificate loading has no deadline
/// of its own, so the shutdown wait is what keeps a stall from holding the process.
const WORKER_LIMIT: Duration = Duration::from_secs(3);
/// A stamp inside this window was just written by the process that won the
/// lock, or the clock moved a little. Farther ahead is a stuck stamp.
const FUTURE_SKEW: i64 = 120;
const STATE_DIR: &[&str] = &[".nab", "telemetry"];
const CI_ENVS: &[&str] = &[
    "CI",
    "CONTINUOUS_INTEGRATION",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "CIRCLECI",
    "TRAVIS",
    "BUILDKITE",
    "DRONE",
    "JENKINS_URL",
    "TEAMCITY_VERSION",
    "BITBUCKET_BUILD_NUMBER",
    "APPVEYOR",
    "CODEBUILD_BUILD_ID",
];

/// Holds the heartbeat thread until the command finishes.
///
/// Binding this to `_` drops it at the end of that statement, which waits
/// for the post there. Keep a named binding until the command is finished.
pub struct Heartbeat;

impl Drop for Heartbeat {
    fn drop(&mut self) {
        finish();
    }
}

struct SignalDone(std::sync::mpsc::Sender<()>);

impl Drop for SignalDone {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

struct Job {
    handle: std::thread::JoinHandle<()>,
    done: std::sync::mpsc::Receiver<()>,
}

fn slot() -> &'static Mutex<Option<Job>> {
    static SLOT: Mutex<Option<Job>> = Mutex::new(None);
    &SLOT
}

fn wait_for_worker(job: Job, limit: Duration) {
    let finished = match job.done.recv_timeout(limit) {
        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => true,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => false,
    };
    if finished {
        let _ = job.handle.join();
    }
}

/// Wait for an in-flight heartbeat. `nab mcp serve` must call this before
/// `exec`, which does not run destructors. The wait is bounded: a worker
/// stuck while loading certificates does not hold the process open.
pub fn finish() {
    let job = slot().lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(job) = job {
        wait_for_worker(job, WORKER_LIMIT);
    }
}

/// Fire the daily heartbeat from a release binary.
///
/// Returns immediately. Drop of the guard waits for the post, at most three
/// seconds, and only on a day when a send is due. Never panics.
#[must_use = "hold the guard until the command exits so the post can finish"]
pub fn maybe_send() -> Heartbeat {
    // Test and debug builds are the development copies. The published binary is
    // a release build, which is the one that sends.
    if cfg!(test) || cfg!(debug_assertions) {
        return Heartbeat;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("nab-heartbeat".to_string())
        .spawn(move || {
            let _signal = SignalDone(tx);
            let Some(home) = home_dir() else {
                return;
            };
            let env = |key: &str| std::env::var(key).ok();
            let mut post = post_blocking;
            let _ = drive(
                &home,
                &env,
                env!("CARGO_PKG_VERSION"),
                Utc::now(),
                &mut post,
            );
        })
        .ok();
    *slot().lock().unwrap_or_else(PoisonError::into_inner) =
        handle.map(|handle| Job { handle, done: rx });
    Heartbeat
}

fn home_dir() -> Option<PathBuf> {
    let raw = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let path = PathBuf::from(raw);
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

fn truthy(value: Option<&str>) -> bool {
    match value {
        None | Some("" | "0" | "false") => false,
        Some(_) => true,
    }
}

fn suppressed(version: &str, env: &impl Fn(&str) -> Option<String>) -> bool {
    if version.is_empty() || version == "dev" {
        return true;
    }
    if CI_ENVS.iter().any(|name| truthy(env(name).as_deref())) {
        return true;
    }
    [
        "DO_NOT_TRACK",
        "NO_TELEMETRY",
        "NAB_NO_TELEMETRY",
        "CARGO_MANIFEST_DIR",
    ]
    .iter()
    .any(|name| truthy(env(name).as_deref()))
}

fn resolve_endpoint(env: &impl Fn(&str) -> Option<String>) -> Option<String> {
    match env(ENDPOINT_ENV) {
        None => Some(DEFAULT_ENDPOINT.to_string()),
        Some(value) if value.is_empty() => Some(DEFAULT_ENDPOINT.to_string()),
        Some(value) if value.starts_with("https://") || value.starts_with("http://") => Some(value),
        Some(_) => None,
    }
}

fn state_dir(home: &Path) -> PathBuf {
    STATE_DIR
        .iter()
        .fold(home.to_path_buf(), |dir, part| dir.join(part))
}

fn read_trim(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn write_private(dir: &Path, name: &str, contents: &str) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    set_dir_private(dir);
    let dest = dir.join(name);
    let tmp = dir.join(format!("{name}.tmp"));
    {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(contents.as_bytes())?;
    }
    set_file_private(&tmp);
    fs::rename(tmp, dest)?;
    Ok(())
}

fn set_dir_private(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

fn set_file_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn format_stamp(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn parse_stamp(text: &str) -> Option<DateTime<Utc>> {
    let trimmed = text.trim();
    DateTime::parse_from_rfc3339(trimmed)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn due(dir: &Path, now: DateTime<Utc>) -> bool {
    let Some(text) = read_trim(&dir.join("heartbeat")) else {
        return true;
    };
    let Some(last) = parse_stamp(&text) else {
        return true;
    };
    let last_at = last.timestamp();
    let now_at = now.timestamp();
    if last_at > now_at {
        return last_at - now_at >= FUTURE_SKEW;
    }
    now_at.saturating_sub(last_at) >= DAY
}

fn mark_sent(dir: &Path, now: DateTime<Utc>) -> std::io::Result<()> {
    write_private(dir, "heartbeat", &format_stamp(now))
}

/// Exclusive claim for this daily slot. `None` means do not post: another
/// process holds the slot, the stamp is still fresh, or the stamp could not
/// be written. The returned file keeps the lock until the post finishes.
fn claim_day(dir: &Path, now: DateTime<Utc>) -> Option<std::fs::File> {
    fs::create_dir_all(dir).ok()?;
    set_dir_private(dir);
    let path = dir.join("heartbeat.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).ok()?;
    set_file_private(&path);
    // Another nab is already sending. Skipping is the once-a-day outcome.
    if file.try_lock().is_err() {
        return None;
    }
    if !due(dir, now) {
        return None;
    }
    mark_sent(dir, now).ok()?;
    Some(file)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn random_id() -> String {
    let mut rng = rand::rng();
    let bytes: [u8; 16] = rng.random();
    hex_encode(&bytes)
}

struct Install {
    id: String,
    fresh: bool,
}

fn ensure_install(dir: &Path) -> Install {
    let path = dir.join("install-id");
    if let Some(existing) = read_trim(&path) {
        return Install {
            id: existing,
            fresh: false,
        };
    }
    let id = random_id();
    if write_private(dir, "install-id", &id).is_err() {
        return Install {
            id: String::new(),
            fresh: false,
        };
    }
    Install { id, fresh: true }
}

fn valid_day(value: &str) -> bool {
    let mut parts = value.split('-');
    let Some(year) = parts.next().and_then(|p| p.parse::<i32>().ok()) else {
        return false;
    };
    let Some(month) = parts.next().and_then(|p| p.parse::<u32>().ok()) else {
        return false;
    };
    let Some(day) = parts.next().and_then(|p| p.parse::<u32>().ok()) else {
        return false;
    };
    if parts.next().is_some() || value.len() != 10 {
        return false;
    }
    chrono::NaiveDate::from_ymd_opt(year, month, day).is_some()
}

fn format_day(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%d").to_string()
}

fn birth_day(path: &Path) -> Option<String> {
    let created = fs::metadata(path).ok()?.created().ok()?;
    let dt: DateTime<Utc> = created.into();
    if dt.timestamp() <= 86_400 {
        return None;
    }
    let day = format_day(dt);
    valid_day(&day).then_some(day)
}

fn install_date(dir: &Path, now: DateTime<Utc>, fresh: bool) -> String {
    let path = dir.join("install-date");
    if let Some(existing) = read_trim(&path)
        && valid_day(&existing)
    {
        return existing;
    }
    let mut day = format_day(now);
    if !fresh && let Some(birth) = birth_day(&dir.join("install-id")) {
        day = birth;
    }
    let _ = write_private(dir, "install-date", &day);
    day
}

fn valid_machine(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn machine_id(home: &Path) -> String {
    let dir = home.join(".revaluator");
    let path = dir.join("machine-id");
    // Shared with other tools. Create the file only when it is absent.
    // Empty, non-UTF-8, and any other unrecognized bytes stay where they are.
    match fs::metadata(&path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let id = random_id();
            if write_private(&dir, "machine-id", &id).is_err() {
                String::new()
            } else {
                id
            }
        }
        Err(_) => String::new(),
        Ok(_) => {
            let existing = read_trim(&path).unwrap_or_default();
            if valid_machine(&existing) {
                existing
            } else {
                String::new()
            }
        }
    }
}

fn runtime_string() -> String {
    format!(
        "{}/{}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        env!("CARGO_PKG_RUST_VERSION")
    )
}

fn build_body(
    version: &str,
    install_id: &str,
    install_date: &str,
    machine_id: &str,
) -> Option<Vec<u8>> {
    #[derive(serde::Serialize)]
    struct Body<'a> {
        project: &'a str,
        event: &'a str,
        version: &'a str,
        runtime: String,
        #[serde(skip_serializing_if = "str::is_empty")]
        install_id: &'a str,
        #[serde(skip_serializing_if = "str::is_empty")]
        install_date: &'a str,
        #[serde(skip_serializing_if = "str::is_empty")]
        machine_id: &'a str,
    }
    let body = serde_json::to_vec(&Body {
        project: "nab",
        event: "heartbeat",
        version,
        runtime: runtime_string(),
        install_id,
        install_date,
        machine_id,
    })
    .ok()?;
    if body.len() > MAX_BODY {
        None
    } else {
        Some(body)
    }
}

fn heartbeat_tls() -> Option<rustls::ClientConfig> {
    // An explicit provider. install_default() would replace the ring provider
    // the HTTP/3 and browser paths install, and the first caller wins for the
    // whole process.
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .ok()?;
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        return None;
    }
    Some(builder.with_root_certificates(roots).with_no_client_auth())
}

fn post_blocking(url: &str, body: &[u8]) {
    let Some(tls) = heartbeat_tls() else {
        return;
    };
    let Ok(client) = reqwest::blocking::Client::builder()
        .use_preconfigured_tls(tls)
        .timeout(WORKER_LIMIT)
        .connect_timeout(WORKER_LIMIT)
        .user_agent("heartbeat")
        .build()
    else {
        return;
    };
    let _ = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_vec())
        .send();
}

fn drive(
    home: &Path,
    env: &impl Fn(&str) -> Option<String>,
    version: &str,
    now: DateTime<Utc>,
    post: &mut impl FnMut(&str, &[u8]),
) -> bool {
    if suppressed(version, env) {
        return false;
    }
    let Some(url) = resolve_endpoint(env) else {
        return false;
    };
    let dir = state_dir(home);
    let Some(_claim) = claim_day(&dir, now) else {
        return false;
    };
    let install = ensure_install(&dir);
    let installed = install_date(&dir, now, install.fresh);
    let machine = machine_id(home);
    let Some(body) = build_body(version, &install.id, &installed, &machine) else {
        return false;
    };
    post(&url, &body);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::time::Instant;

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempHome(PathBuf);

    impl TempHome {
        fn new() -> Self {
            let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("nab-hb-{}-{n}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        chrono::NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, 0, 0)
            .unwrap()
            .and_utc()
    }

    #[test]
    fn opt_out_and_ci_and_dev_skip_without_writing() {
        let home = TempHome::new();
        let now = at(2026, 10, 2, 12);
        for pairs in [
            &[("NAB_NO_TELEMETRY", "1")][..],
            &[("NO_TELEMETRY", "yes")],
            &[("DO_NOT_TRACK", "1")],
            &[("CARGO_MANIFEST_DIR", "/tmp/nab-src")],
            &[("CI", "true")],
            &[("GITHUB_ACTIONS", "true")],
        ] {
            let mut posts = 0;
            let sent = drive(&home.0, &env_from(pairs), "0.12.4", now, &mut |_, _| {
                posts += 1;
            });
            assert!(!sent);
            assert_eq!(posts, 0);
            assert!(!home.0.join(".nab").exists());
        }
        let mut posts = 0;
        assert!(!drive(&home.0, &env_from(&[]), "dev", now, &mut |_, _| {
            posts += 1;
        }));
        assert!(!drive(&home.0, &env_from(&[]), "", now, &mut |_, _| {
            posts += 1;
        }));
        assert_eq!(posts, 0);
    }

    #[test]
    fn zero_and_false_leave_the_heartbeat_on() {
        let home = TempHome::new();
        let mut url = String::new();
        let sent = drive(
            &home.0,
            &env_from(&[
                ("NAB_NO_TELEMETRY", "0"),
                ("NO_TELEMETRY", "false"),
                ("DO_NOT_TRACK", "false"),
                ("CI", "0"),
            ]),
            "0.12.4",
            at(2026, 10, 2, 12),
            &mut |got, _| url = got.to_string(),
        );
        assert!(sent);
        assert_eq!(url, DEFAULT_ENDPOINT);
    }

    #[test]
    fn endpoint_override_and_rejection() {
        let home = TempHome::new();
        let now = at(2026, 10, 2, 12);
        let mut url = String::new();
        assert!(drive(
            &home.0,
            &env_from(&[("NAB_TELEMETRY_ENDPOINT", "")]),
            "0.12.4",
            now,
            &mut |got, _| url = got.to_string(),
        ));
        assert_eq!(url, DEFAULT_ENDPOINT);

        let home = TempHome::new();
        let mut posts = 0;
        assert!(!drive(
            &home.0,
            &env_from(&[(
                "NAB_TELEMETRY_ENDPOINT",
                "ftp://telemetry.revaluator.ai/v1/heartbeat"
            )]),
            "0.12.4",
            now,
            &mut |_, _| {
                posts += 1;
            },
        ));
        assert_eq!(posts, 0);

        let home = TempHome::new();
        let custom = "http://127.0.0.1:9/v1/heartbeat";
        assert!(drive(
            &home.0,
            &env_from(&[("NAB_TELEMETRY_ENDPOINT", custom)]),
            "0.12.4",
            now,
            &mut |got, _| url = got.to_string(),
        ));
        assert_eq!(url, custom);
    }

    #[test]
    fn body_is_the_seven_fields_and_the_stamp_lands_before_the_post() {
        let home = TempHome::new();
        let now = at(2026, 10, 2, 8);
        let mut raw = Vec::new();
        let mut stamped_first = false;
        drive(&home.0, &env_from(&[]), "0.12.4", now, &mut |_, body| {
            stamped_first = read_trim(&state_dir(&home.0).join("heartbeat")).is_some();
            raw = body.to_vec();
        });
        assert!(stamped_first);
        let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        let obj = value.as_object().unwrap();
        let keys: Vec<_> = obj.keys().cloned().collect();
        assert_eq!(
            keys,
            vec![
                "project",
                "event",
                "version",
                "runtime",
                "install_id",
                "install_date",
                "machine_id",
            ]
        );
        assert_eq!(obj["project"], "nab");
        assert_eq!(obj["event"], "heartbeat");
        assert_eq!(obj["version"], "0.12.4");
        assert!(obj["runtime"].as_str().unwrap().contains('/'));
        let install = obj["install_id"].as_str().unwrap();
        assert!(valid_machine(install));
        assert_eq!(obj["install_date"], "2026-10-02");
        assert!(valid_machine(obj["machine_id"].as_str().unwrap()));
        assert_eq!(
            read_trim(&home.0.join(".revaluator").join("machine-id")).unwrap(),
            obj["machine_id"].as_str().unwrap()
        );
    }

    #[test]
    fn a_second_call_the_same_day_does_not_post() {
        let home = TempHome::new();
        let now = at(2026, 10, 2, 8);
        let mut posts = 0;
        assert!(drive(
            &home.0,
            &env_from(&[]),
            "0.12.4",
            now,
            &mut |_, _| {
                posts += 1;
            }
        ));
        assert!(!drive(
            &home.0,
            &env_from(&[]),
            "0.12.4",
            now + chrono::Duration::hours(23),
            &mut |_, _| {
                posts += 1;
            },
        ));
        assert!(drive(
            &home.0,
            &env_from(&[]),
            "0.12.4",
            now + chrono::Duration::hours(24),
            &mut |_, _| {
                posts += 1;
            },
        ));
        assert_eq!(posts, 2);
        let id = read_trim(&state_dir(&home.0).join("install-id")).unwrap();
        assert_eq!(id.len(), 32);
    }

    #[test]
    fn an_existing_install_date_is_kept() {
        let home = TempHome::new();
        let dir = state_dir(&home.0);
        write_private(&dir, "install-id", &"ab".repeat(16)).unwrap();
        write_private(&dir, "install-date", "2019-05-01").unwrap();
        let mut raw = Vec::new();
        drive(
            &home.0,
            &env_from(&[]),
            "0.12.4",
            at(2026, 10, 2, 8),
            &mut |_, body| {
                raw = body.to_vec();
            },
        );
        let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(value["install_date"], "2019-05-01");
        assert_eq!(value["install_id"], "ab".repeat(16));
    }

    #[test]
    fn a_stamp_a_few_seconds_ahead_is_not_due() {
        let home = TempHome::new();
        let now = at(2026, 10, 2, 12);
        let dir = state_dir(&home.0);
        write_private(
            &dir,
            "heartbeat",
            &format_stamp(now + chrono::Duration::seconds(30)),
        )
        .unwrap();
        let mut posts = 0;
        assert!(!drive(
            &home.0,
            &env_from(&[]),
            "0.12.4",
            now,
            &mut |_, _| {
                posts += 1;
            },
        ));
        assert_eq!(posts, 0);
    }

    #[test]
    fn a_future_stamp_is_due_again() {
        let home = TempHome::new();
        let dir = state_dir(&home.0);
        write_private(&dir, "heartbeat", "2099-01-01T00:00:00Z").unwrap();
        let mut posts = 0;
        assert!(drive(
            &home.0,
            &env_from(&[]),
            "0.12.4",
            at(2026, 10, 2, 12),
            &mut |_, _| {
                posts += 1;
            },
        ));
        assert_eq!(posts, 1);
    }

    fn assert_machine_file_untouched(bytes: &[u8]) {
        let home = TempHome::new();
        let dir = home.0.join(".revaluator");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("machine-id");
        fs::write(&path, bytes).unwrap();
        let mut raw = Vec::new();
        drive(
            &home.0,
            &env_from(&[]),
            "0.12.4",
            at(2026, 10, 2, 12),
            &mut |_, body| raw = body.to_vec(),
        );
        let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert!(value.get("machine_id").is_none());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn an_empty_or_non_utf8_machine_id_is_left_alone() {
        assert_machine_file_untouched(b"");
        assert_machine_file_untouched(b" \n");
        assert_machine_file_untouched(&[0xff, 0xfe]);
    }

    #[test]
    fn an_invalid_shared_machine_id_is_left_alone() {
        let home = TempHome::new();
        let dir = home.0.join(".revaluator");
        write_private(&dir, "machine-id", "not-ours").unwrap();
        let mut raw = Vec::new();
        drive(
            &home.0,
            &env_from(&[]),
            "0.12.4",
            at(2026, 10, 2, 12),
            &mut |_, body| raw = body.to_vec(),
        );
        let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert!(value.get("machine_id").is_none());
        assert_eq!(
            fs::read_to_string(dir.join("machine-id")).unwrap().trim(),
            "not-ours"
        );
    }

    #[test]
    fn unwritable_state_does_not_post() {
        let home = TempHome::new();
        fs::write(home.0.join(".nab"), "not a directory").unwrap();
        let mut posts = 0;
        let sent = drive(
            &home.0,
            &env_from(&[]),
            "0.12.4",
            at(2026, 10, 2, 12),
            &mut |_, _| {
                posts += 1;
            },
        );
        assert!(!sent);
        assert_eq!(posts, 0);
    }

    #[test]
    fn a_stalled_worker_does_not_hold_shutdown() {
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let _ = release_rx.recv();
            let _ = done_tx.send(());
        });
        let (result_tx, result_rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let started = Instant::now();
            wait_for_worker(
                Job {
                    handle,
                    done: done_rx,
                },
                Duration::from_millis(100),
            );
            let _ = result_tx.send(started.elapsed());
        });
        let outcome = result_rx.recv_timeout(Duration::from_secs(2));
        let _ = release_tx.send(());
        let _ = waiter.join();
        let elapsed = outcome.expect("shutdown wait did not return");
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
    }

    #[test]
    fn a_finished_worker_is_waited_for() {
        let (done_tx, done_rx) = mpsc::channel();
        let flag = Arc::new(AtomicBool::new(false));
        let flag_for_worker = Arc::clone(&flag);
        let handle = std::thread::spawn(move || {
            let _signal = SignalDone(done_tx);
            flag_for_worker.store(true, Ordering::SeqCst);
        });
        wait_for_worker(
            Job {
                handle,
                done: done_rx,
            },
            Duration::from_secs(2),
        );
        assert!(flag.load(Ordering::SeqCst));
    }

    #[test]
    fn maybe_send_returns_without_touching_a_home() {
        // The test build takes the early return. This guards that path against a panic.
        let _guard = maybe_send();
    }

    #[test]
    fn tls_config_does_not_install_a_process_provider() {
        let before = rustls::crypto::CryptoProvider::get_default().map(Arc::as_ptr);
        let config = heartbeat_tls();
        let after = rustls::crypto::CryptoProvider::get_default().map(Arc::as_ptr);
        assert_eq!(before, after);
        assert!(config.is_some());
    }
}
