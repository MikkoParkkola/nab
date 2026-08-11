//! Integration tests for the `nab fetch` command.
//!
//! Protocol-level request/response behavior uses a local deterministic proxy.
//! Tests that genuinely require external network access are gated behind the
//! `NAB_NET_TESTS` env var so CI can skip them when offline.

#![allow(deprecated)] // cargo_bin deprecation — replacement not yet stable

mod common;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::process::Command as StdCommand;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use common::{net_tests_enabled, net_tests_enabled_for};

/// Helper: get a Command for the `nab` binary.
fn nab() -> Command {
    Command::cargo_bin("nab").expect("binary 'nab' should be built")
}

/// Public IP literal used as the logical destination for local proxy tests.
///
/// Using an IP literal avoids DNS, while routing through `spawn_test_proxy`
/// prevents any connection to the address itself.
const PROXY_TEST_ORIGIN: &str = "http://93.184.216.34";
const PROXY_ACCEPT_TIMEOUT: Duration = Duration::from_secs(15);
const PROXY_READ_TIMEOUT: Duration = Duration::from_secs(15);
const PROXY_COMMAND_TIMEOUT: Duration = Duration::from_secs(25);
const PROXY_COMPLETION_TIMEOUT: Duration = Duration::from_secs(35);

fn spawn_test_proxy<F>(response: &str, inspect_request: F) -> (String, Receiver<Result<(), String>>)
where
    F: FnOnce(&str) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test proxy");
    listener
        .set_nonblocking(true)
        .expect("make local test proxy nonblocking");
    let address = listener
        .local_addr()
        .expect("read local test proxy address");
    let response = response.to_owned();
    let (result_sender, result_receiver) = mpsc::channel();

    thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let deadline = Instant::now() + PROXY_ACCEPT_TIMEOUT;
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "timed out waiting for proxied request"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept proxied request: {error}"),
                }
            };
            stream
                .set_nonblocking(false)
                .expect("make proxied connection blocking");
            stream
                .set_read_timeout(Some(PROXY_READ_TIMEOUT))
                .expect("set proxied request read timeout");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];

            loop {
                let read = stream.read(&mut buffer).expect("read proxied request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                assert!(
                    request.len() <= 1024 * 1024,
                    "proxied request exceeded 1 MiB"
                );

                let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().expect("valid content length"))
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }

            let request = String::from_utf8(request).expect("HTTP request should be UTF-8");
            inspect_request(&request);
            stream
                .write_all(response.as_bytes())
                .expect("write proxy response");
        }));

        let outcome = outcome.map_err(|panic| {
            panic.downcast_ref::<&str>().map_or_else(
                || {
                    panic.downcast_ref::<String>().map_or_else(
                        || "test proxy panicked".to_owned(),
                        std::clone::Clone::clone,
                    )
                },
                |message| (*message).to_owned(),
            )
        });
        let _ = result_sender.send(outcome);
    });

    (format!("http://{address}"), result_receiver)
}

fn wait_for_test_proxy(result: &Receiver<Result<(), String>>) {
    result
        .recv_timeout(PROXY_COMPLETION_TIMEOUT)
        .expect("test proxy should finish before its deadline")
        .expect("test proxy should finish cleanly");
}

fn http_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[test]
fn external_network_tests_are_explicitly_opt_in() {
    assert!(!net_tests_enabled_for(None));
    assert!(!net_tests_enabled_for(Some(std::ffi::OsStr::new("0"))));
    assert!(!net_tests_enabled_for(Some(std::ffi::OsStr::new("false"))));
    assert!(net_tests_enabled_for(Some(std::ffi::OsStr::new("1"))));
    assert!(net_tests_enabled_for(Some(std::ffi::OsStr::new("TRUE"))));
    assert!(net_tests_enabled_for(Some(std::ffi::OsStr::new("yes"))));
}

#[test]
fn fetch_help_includes_explicit_browser_render_flags() {
    nab()
        .args(["fetch", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--render"))
        .stdout(predicate::str::contains("--interactive"))
        .stdout(predicate::str::contains("--browser-cdp-url"));
}

#[test]
fn fetch_render_requires_configured_cdp_endpoint() {
    let mut cmd = nab();
    cmd.env_remove("NAB_BROWSER_CDP_WS")
        .env_remove("NAB_BROWSER_CDP_HEADERS")
        .args([
            "fetch",
            "--render",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("NAB_BROWSER_CDP_WS"))
        .stderr(predicate::str::contains("never auto-launches"));
}

#[test]
fn browser_help_lists_external_cdp_options() {
    nab()
        .args(["browser", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("NAB_BROWSER_CDP_WS"))
        .stdout(predicate::str::contains("--cdp-url"))
        .stdout(predicate::str::contains("--headers-env"));
}

#[test]
fn browser_requires_configured_cdp_endpoint() {
    let mut cmd = nab();
    cmd.env_remove("NAB_BROWSER_CDP_WS")
        .env_remove("NAB_BROWSER_CDP_HEADERS")
        .args(["browser", "https://example.com"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("NAB_BROWSER_CDP_WS"))
        .stderr(predicate::str::contains("never auto-launches"));
}

// ─── Basic fetch (network) ───────────────────────────────────────────────────

#[test]
fn fetch_example_dot_com_full_format() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args(["fetch", "--cookies", "none", "https://example.com"])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains("Fetching:"))
        .stdout(predicate::str::contains("Response:"))
        .stdout(predicate::str::contains("Status:"))
        .stdout(predicate::str::contains("Body:"));
}

#[test]
fn fetch_compact_format() {
    if !net_tests_enabled() {
        return;
    }

    // Compact format outputs: STATUS SIZE TIME
    nab()
        .args([
            "fetch",
            "--format",
            "compact",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"200 \d+B \d+").unwrap());
}

#[test]
fn fetch_json_format() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args([
            "fetch",
            "--format",
            "json",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""status":200"#))
        .stdout(predicate::str::contains(r#""url":"https://example.com""#));
}

#[test]
fn fetch_with_headers_flag() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args(["fetch", "-H", "--cookies", "none", "https://example.com"])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains("Headers:"))
        .stdout(predicate::str::contains("content-type"));
}

#[test]
fn fetch_body_flag_shows_content() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args([
            "fetch",
            "--body",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        // Readability extracts article body (strips h1 title); check for actual content
        .stdout(predicate::str::contains("documentation examples"));
}

#[test]
fn fetch_raw_html_flag() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args([
            "fetch",
            "--body",
            "--raw-html",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        // Raw HTML should still contain the text, but without markdown conversion
        .stdout(predicate::str::contains("Example Domain"));
}

#[test]
fn fetch_links_flag() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args([
            "fetch",
            "--links",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        // example.com has a link to iana.org
        .stdout(predicate::str::contains("iana.org"))
        .stdout(predicate::str::contains("links)"));
}

#[test]
fn fetch_output_to_file() {
    if !net_tests_enabled() {
        return;
    }

    let tmp = std::env::temp_dir().join("nab_test_output.html");
    // Clean up from previous runs
    let _ = fs::remove_file(&tmp);

    nab()
        .args([
            "fetch",
            "--output",
            tmp.to_str().unwrap(),
            "--raw-html",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains("Saved"));

    // Verify the file was created with content
    let content = fs::read_to_string(&tmp).expect("output file should exist");
    assert!(
        content.contains("Example Domain"),
        "saved file should contain page content"
    );
    assert!(
        content.len() > 100,
        "saved file should have substantial content"
    );

    // Clean up
    let _ = fs::remove_file(&tmp);
}

#[test]
fn fetch_custom_method_head() {
    if !net_tests_enabled() {
        return;
    }

    // HEAD request should succeed (no body)
    nab()
        .args([
            "fetch",
            "-X",
            "HEAD",
            "--format",
            "compact",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        // HEAD returns 200 with 0 bytes body
        .stdout(predicate::str::is_match(r"200 0B \d+").unwrap());
}

#[test]
fn fetch_custom_header() {
    let body = r#"{"headers":{"X-Nab-Test":"integration"}}"#;
    let (proxy_url, proxy_result) = spawn_test_proxy(&http_response("200 OK", body), |request| {
        assert!(
            request.starts_with("GET http://93.184.216.34/headers HTTP/1.1\r\n"),
            "unexpected request line: {request}"
        );
        assert!(
            request
                .lines()
                .any(|line| line.eq_ignore_ascii_case("X-Nab-Test: integration")),
            "custom header missing from request: {request}"
        );
    });

    nab()
        .args([
            "fetch",
            "--body",
            "--raw-html",
            "--add-header",
            "X-Nab-Test: integration",
            "--cookies",
            "none",
            "--proxy",
            &proxy_url,
            &format!("{PROXY_TEST_ORIGIN}/headers"),
        ])
        .timeout(PROXY_COMMAND_TIMEOUT)
        .assert()
        .success()
        .stdout(predicate::str::contains("X-Nab-Test"));
    wait_for_test_proxy(&proxy_result);
}

#[test]
fn fetch_max_body_truncates() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args([
            "fetch",
            "--body",
            "--max-body",
            "50",
            "--cookies",
            "none",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains("more bytes]"));
}

#[test]
fn fetch_greaterwrong_mirror_returns_lesswrong_article_body() {
    if !net_tests_enabled() {
        return;
    }

    nab()
        .args([
            "fetch",
            "--cookies",
            "none",
            "--no-save",
            "--no-ocr",
            "--no-transcribe",
            "--max-body",
            "1600",
            "https://www.greaterwrong.com/posts/fewDbvpKMZLgGuWT2/the-world-can-t-keep-up-with-ai-labs",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "# The World Can't Keep Up With AI Labs",
        ))
        .stdout(predicate::str::contains(
            "Late last year a new AI psychosis kicked off",
        ))
        .stdout(predicate::str::contains("LessWrong 2.0 viewer").not());
}

// ─── Error handling ──────────────────────────────────────────────────────────

#[test]
fn fetch_invalid_url_fails() {
    nab()
        .args(["fetch", "--cookies", "none", "not-a-url"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .failure();
}

#[test]
fn fetch_unreachable_host_fails() {
    let (proxy_url, proxy_result) = spawn_test_proxy("", |request| {
        assert!(
            request.starts_with("GET http://93.184.216.34/disconnect HTTP/1.1\r\n"),
            "unexpected request line: {request}"
        );
    });

    nab()
        .args([
            "fetch",
            "--cookies",
            "none",
            "--proxy",
            &proxy_url,
            &format!("{PROXY_TEST_ORIGIN}/disconnect"),
        ])
        .timeout(PROXY_COMMAND_TIMEOUT)
        .assert()
        .failure();
    wait_for_test_proxy(&proxy_result);
}

// ─── Cookie flag parsing ─────────────────────────────────────────────────────

#[test]
fn fetch_cookies_none_works() {
    if !net_tests_enabled() {
        return;
    }

    // "none" should skip cookie loading entirely
    nab()
        .args([
            "fetch",
            "--cookies",
            "none",
            "--format",
            "compact",
            "https://example.com",
        ])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"^200 ").unwrap());
}

#[test]
fn fetch_cookies_flag_accepts_browser_names() {
    // These should be accepted as valid values without crashing on argument
    // parsing. The actual cookie extraction may or may not work depending on
    // local browser state, but the flags should be accepted.
    for browser in &["brave", "chrome", "firefox", "safari", "edge"] {
        nab()
            .args(["fetch", "--cookies", browser, "--help"])
            .assert()
            .success();
    }
}

// ─── No-redirect flag ────────────────────────────────────────────────────────

#[test]
fn fetch_no_redirect_captures_302() {
    let response = "HTTP/1.1 302 Found\r\nLocation: http://93.184.216.34/final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let (proxy_url, proxy_result) = spawn_test_proxy(response, |request| {
        assert!(
            request.starts_with("GET http://93.184.216.34/redirect/1 HTTP/1.1\r\n"),
            "unexpected request line: {request}"
        );
    });

    nab()
        .args([
            "fetch",
            "--no-redirect",
            "--format",
            "compact",
            "--cookies",
            "none",
            "--proxy",
            &proxy_url,
            &format!("{PROXY_TEST_ORIGIN}/redirect/1"),
        ])
        .timeout(PROXY_COMMAND_TIMEOUT)
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"^302 ").unwrap());
    wait_for_test_proxy(&proxy_result);
}

// ─── POST with data ─────────────────────────────────────────────────────────

#[test]
fn fetch_post_with_data() {
    let body = r#"{"json":{"key": "value"}}"#;
    let (proxy_url, proxy_result) = spawn_test_proxy(&http_response("200 OK", body), |request| {
        assert!(
            request.starts_with("POST http://93.184.216.34/post HTTP/1.1\r\n"),
            "unexpected request line: {request}"
        );
        assert!(
            request.ends_with(r#"{"key":"value"}"#),
            "POST body missing from request: {request}"
        );
    });

    nab()
        .args([
            "fetch",
            "-X",
            "POST",
            "-d",
            r#"{"key":"value"}"#,
            "--body",
            "--raw-html",
            "--cookies",
            "none",
            "--proxy",
            &proxy_url,
            &format!("{PROXY_TEST_ORIGIN}/post"),
        ])
        .timeout(PROXY_COMMAND_TIMEOUT)
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""key": "value""#));
    wait_for_test_proxy(&proxy_result);
}

#[cfg(unix)]
#[test]
fn fetch_piped_to_head_exits_without_broken_pipe_panic() {
    if !net_tests_enabled() {
        return;
    }

    let nab = Command::cargo_bin("nab").expect("binary 'nab' should be built");
    let output = StdCommand::new("sh")
        .arg("-c")
        .arg(r#""$1" fetch --cookies none https://example.com | head -n 5 > /dev/null"#)
        .arg("sh")
        .arg(nab.get_program())
        .output()
        .expect("shell pipeline should execute");

    assert!(
        output.status.success(),
        "pipeline should exit successfully: {:?}",
        output.status
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Broken pipe"),
        "stderr should not contain broken pipe panic: {stderr}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "stderr should not contain panic output: {stderr}"
    );
}
