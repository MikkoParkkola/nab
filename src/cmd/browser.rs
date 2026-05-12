use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{
    Message,
    client::IntoClientRequest,
    http::{HeaderName, HeaderValue},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use nab::content::budget::{max_tokens_with_output_headroom, truncate_to_budget};

use super::output::{output_body, write_stdout_line};
use crate::OutputFormat;

const DEFAULT_CDP_ENV: &str = "NAB_BROWSER_CDP_WS";
const RENDER_TIMEOUT: Duration = Duration::from_secs(90);
const PAGE_LOAD_TIMEOUT: Duration = Duration::from_secs(45);
const CONVERSION_TIMEOUT: Duration = Duration::from_mins(1);

#[derive(Clone)]
pub struct BrowserConfig {
    pub url: String,
    pub format: OutputFormat,
    pub output_file: Option<PathBuf>,
    pub max_body: usize,
    pub max_output_tokens: Option<usize>,
    pub cdp_url: Option<String>,
    pub headers_env: String,
    pub wait_ms: u64,
    pub html_options: nab::content::html::HtmlConversionOptions,
}

pub async fn cmd_browser(cfg: &BrowserConfig) -> Result<()> {
    let started = Instant::now();
    let cdp_url = resolve_cdp_url(cfg.cdp_url.as_deref())?;
    let headers = load_header_overrides(&cfg.headers_env)?;

    let rendered = tokio::time::timeout(
        RENDER_TIMEOUT,
        render_with_browser(BrowserRenderOptions {
            url: cfg.url.clone(),
            cdp_url,
            headers,
            wait_ms: cfg.wait_ms,
        }),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "browser rendering timed out after {}s",
            RENDER_TIMEOUT.as_secs()
        )
    })??;

    let converted = html_to_markdown(
        rendered.html.clone(),
        rendered.final_url.clone(),
        cfg.format,
        cfg.html_options,
    )
    .await?;
    let guarded =
        nab::security::guard_fetch_output(&converted.markdown, "cli_browser", &rendered.final_url)?;
    let markdown = apply_browser_output_token_budget(&guarded, cfg.max_output_tokens);
    let elapsed = started.elapsed();

    print_browser_output(
        cfg,
        &BrowserResponse {
            final_url: &rendered.final_url,
            title: &rendered.title,
            html_bytes: rendered.html.len(),
            markdown: &markdown,
            elapsed,
            conversion_elapsed_ms: converted.elapsed_ms,
        },
    )
}

struct BrowserRenderOptions {
    url: String,
    cdp_url: String,
    headers: Vec<(HeaderName, HeaderValue)>,
    wait_ms: u64,
}

struct BrowserRenderResult {
    final_url: String,
    title: String,
    html: String,
}

async fn render_with_browser(options: BrowserRenderOptions) -> Result<BrowserRenderResult> {
    let mut request = options
        .cdp_url
        .as_str()
        .into_client_request()
        .context("invalid CDP WebSocket URL")?;
    for (name, value) in options.headers {
        request.headers_mut().insert(name, value);
    }

    let _ = rustls::crypto::ring::default_provider().install_default();
    let (stream, _) = connect_async(request)
        .await
        .context("failed to connect to CDP WebSocket endpoint")?;
    let mut client = CdpClient::new(stream);

    let target = client
        .command("Target.createTarget", json!({ "url": "about:blank" }), None)
        .await?;
    let target_id = target
        .get("targetId")
        .and_then(Value::as_str)
        .context("CDP Target.createTarget response did not include targetId")?
        .to_string();

    let attach = client
        .command(
            "Target.attachToTarget",
            json!({ "targetId": target_id, "flatten": true }),
            None,
        )
        .await?;
    let session_id = attach
        .get("sessionId")
        .and_then(Value::as_str)
        .context("CDP Target.attachToTarget response did not include sessionId")?
        .to_string();

    client
        .command("Page.enable", json!({}), Some(&session_id))
        .await?;
    client
        .command("Runtime.enable", json!({}), Some(&session_id))
        .await?;
    client
        .command(
            "Page.navigate",
            json!({ "url": options.url }),
            Some(&session_id),
        )
        .await?;

    let _ = tokio::time::timeout(
        PAGE_LOAD_TIMEOUT,
        client.wait_event("Page.loadEventFired", Some(&session_id)),
    )
    .await;

    if options.wait_ms > 0 {
        tokio::time::sleep(Duration::from_millis(options.wait_ms)).await;
    }

    let eval_result = client
        .command(
            "Runtime.evaluate",
            json!({
                "expression": r#"(() => {
                    const root = document.documentElement;
                    return {
                        url: location.href,
                        title: document.title || "",
                        html: root ? root.outerHTML : ""
                    };
                })()"#,
                "returnByValue": true,
                "awaitPromise": true
            }),
            Some(&session_id),
        )
        .await?;
    if let Some(exception) = eval_result.get("exceptionDetails") {
        bail!("browser DOM extraction failed: {exception}");
    }
    let dom: RenderedDom = serde_json::from_value(
        eval_result
            .get("result")
            .and_then(|result| result.get("value"))
            .cloned()
            .context("CDP Runtime.evaluate response did not include a by-value DOM result")?,
    )
    .context("failed to decode rendered DOM payload")?;

    let _ = client
        .command("Target.closeTarget", json!({ "targetId": target_id }), None)
        .await;

    if dom.html.trim().is_empty() {
        bail!("browser rendered an empty DOM");
    }

    Ok(BrowserRenderResult {
        final_url: dom.url,
        title: dom.title,
        html: dom.html,
    })
}

#[derive(Debug, Deserialize)]
struct RenderedDom {
    url: String,
    title: String,
    html: String,
}

struct ConvertedBrowserBody {
    markdown: String,
    elapsed_ms: f64,
}

async fn html_to_markdown(
    html: String,
    final_url: String,
    format: OutputFormat,
    html_options: nab::content::html::HtmlConversionOptions,
) -> Result<ConvertedBrowserBody> {
    let router = nab::content::ContentRouter::with_html_options(html_options);
    let bytes = Bytes::from(html).to_vec();

    let result = tokio::time::timeout(
        CONVERSION_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            router.convert_with_url(&bytes, "text/html; charset=utf-8", Some(&final_url))
        }),
    )
    .await
    .map_err(|_| anyhow!("Content conversion timed out after 60s"))???;

    if matches!(format, OutputFormat::Full)
        && let Some(pages) = result.page_count
    {
        write_stdout_line(&format!("   Pages: {pages}"))?;
        write_stdout_line(&format!("   Conversion: {:.1}ms", result.elapsed_ms))?;
    }

    Ok(ConvertedBrowserBody {
        markdown: result.markdown,
        elapsed_ms: result.elapsed_ms,
    })
}

struct BrowserResponse<'a> {
    final_url: &'a str,
    title: &'a str,
    html_bytes: usize,
    markdown: &'a str,
    elapsed: Duration,
    conversion_elapsed_ms: f64,
}

fn print_browser_output(cfg: &BrowserConfig, resp: &BrowserResponse<'_>) -> Result<()> {
    let out_path = cfg.output_file.as_deref();
    match cfg.format {
        OutputFormat::Compact => {
            write_stdout_line(&format!(
                "200 {}B {:.0}ms",
                resp.html_bytes,
                resp.elapsed.as_secs_f64() * 1000.0
            ))?;
            output_body(resp.markdown, out_path, false, cfg.max_body)?;
        }
        OutputFormat::Json => {
            let output = json!({
                "url": cfg.url,
                "final_url": resp.final_url,
                "title": resp.title,
                "status": 200,
                "content_type": "text/html",
                "markdown": resp.markdown,
                "metadata": {
                    "renderer": "cdp",
                    "html_bytes": resp.html_bytes,
                    "conversion_elapsed_ms": (resp.conversion_elapsed_ms * 10.0).round() / 10.0
                },
                "elapsed_ms": (resp.elapsed.as_secs_f64() * 1000.0 * 10.0).round() / 10.0,
            });
            write_stdout_line(&serde_json::to_string(&output)?)?;
            if let Some(path) = out_path {
                std::fs::write(path, resp.markdown)?;
            }
        }
        OutputFormat::Full => {
            write_stdout_line(&format!("🌐 Browser rendering: {}", cfg.url))?;
            write_stdout_line("🔌 CDP endpoint: configured")?;
            write_stdout_line("\n📊 Response:")?;
            write_stdout_line("   Status: 200")?;
            write_stdout_line(&format!("   Final URL: {}", resp.final_url))?;
            if !resp.title.is_empty() {
                write_stdout_line(&format!("   Title: {}", resp.title))?;
            }
            write_stdout_line(&format!(
                "   Time: {:.2}ms",
                resp.elapsed.as_secs_f64() * 1000.0
            ))?;
            write_stdout_line(&format!(
                "   Conversion: {:.1}ms",
                resp.conversion_elapsed_ms
            ))?;
            write_stdout_line(&format!("\n📄 Body: {} bytes", resp.html_bytes))?;
            output_body(resp.markdown, out_path, false, cfg.max_body)?;
        }
    }
    Ok(())
}

fn resolve_cdp_url(explicit: Option<&str>) -> Result<String> {
    if let Some(url) = explicit
        && !url.trim().is_empty()
    {
        return Ok(url.trim().to_string());
    }
    if let Ok(url) = std::env::var(DEFAULT_CDP_ENV)
        && !url.trim().is_empty()
    {
        return Ok(url.trim().to_string());
    }

    bail!(
        "browser rendering requires a CDP WebSocket endpoint. Set {DEFAULT_CDP_ENV}=wss://... or pass --cdp-url. nab fetch never auto-launches a browser or remote provider."
    );
}

fn load_header_overrides(env_name: &str) -> Result<Vec<(HeaderName, HeaderValue)>> {
    let Ok(raw) = std::env::var(env_name) else {
        return Ok(Vec::new());
    };
    parse_header_overrides(&raw)
}

fn parse_header_overrides(raw: &str) -> Result<Vec<(HeaderName, HeaderValue)>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let pairs = if trimmed.starts_with('{') {
        let decoded: BTreeMap<String, String> =
            serde_json::from_str(trimmed).context("failed to parse CDP header JSON object")?;
        decoded.into_iter().collect()
    } else {
        trimmed
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                line.split_once(':')
                    .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
                    .ok_or_else(|| anyhow!("invalid CDP header override; expected `Name: value`"))
            })
            .collect::<Result<Vec<_>>>()?
    };

    pairs
        .into_iter()
        .map(|(name, value)| {
            let name = HeaderName::from_bytes(name.as_bytes())
                .with_context(|| format!("invalid CDP header name `{name}`"))?;
            let value = HeaderValue::from_str(&value)
                .with_context(|| format!("invalid CDP header value for `{name}`"))?;
            Ok((name, value))
        })
        .collect()
}

fn apply_browser_output_token_budget(markdown: &str, max_output_tokens: Option<usize>) -> String {
    let Some(max_tokens) = max_output_tokens else {
        return markdown.to_string();
    };
    let content_budget = max_tokens_with_output_headroom(max_tokens);
    truncate_to_budget(markdown, Some(content_budget)).markdown
}

type CdpStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct CdpClient {
    stream: CdpStream,
    next_id: u64,
    buffered_events: Vec<Value>,
}

impl CdpClient {
    fn new(stream: CdpStream) -> Self {
        Self {
            stream,
            next_id: 1,
            buffered_events: Vec::new(),
        }
    }

    async fn command(
        &mut self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let mut message = json!({
            "id": id,
            "method": method,
            "params": params
        });
        if let Some(session_id) = session_id {
            message["sessionId"] = json!(session_id);
        }

        self.stream
            .send(Message::Text(message.to_string().into()))
            .await
            .with_context(|| format!("failed to send CDP command {method}"))?;

        loop {
            let incoming = self.next_json().await?;
            if incoming.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = incoming.get("error") {
                    bail!("CDP command {method} failed: {error}");
                }
                return Ok(incoming.get("result").cloned().unwrap_or_else(|| json!({})));
            }
            if incoming.get("method").is_some() {
                self.buffered_events.push(incoming);
            }
        }
    }

    async fn wait_event(&mut self, method: &str, session_id: Option<&str>) -> Result<Value> {
        if let Some(index) = self
            .buffered_events
            .iter()
            .position(|event| event_matches(event, method, session_id))
        {
            return Ok(self.buffered_events.remove(index));
        }

        loop {
            let incoming = self.next_json().await?;
            if event_matches(&incoming, method, session_id) {
                return Ok(incoming);
            }
            if incoming.get("method").is_some() {
                self.buffered_events.push(incoming);
            }
        }
    }

    async fn next_json(&mut self) -> Result<Value> {
        loop {
            let message = self
                .stream
                .next()
                .await
                .context("CDP WebSocket closed")?
                .context("failed to read CDP WebSocket frame")?;
            match message {
                Message::Text(text) => {
                    return serde_json::from_str(&text).context("failed to parse CDP JSON frame");
                }
                Message::Binary(bytes) => {
                    return serde_json::from_slice(&bytes)
                        .context("failed to parse binary CDP JSON frame");
                }
                Message::Ping(payload) => {
                    self.stream
                        .send(Message::Pong(payload))
                        .await
                        .context("failed to respond to CDP ping")?;
                }
                Message::Close(frame) => {
                    bail!("CDP WebSocket closed: {frame:?}");
                }
                Message::Pong(_) | Message::Frame(_) => {}
            }
        }
    }
}

fn event_matches(event: &Value, method: &str, session_id: Option<&str>) -> bool {
    event.get("method").and_then(Value::as_str) == Some(method)
        && session_id
            .is_none_or(|expected| event.get("sessionId").and_then(Value::as_str) == Some(expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_header_overrides() {
        let headers =
            parse_header_overrides(r#"{"Authorization":"Bearer token","X-Test":"yes"}"#).unwrap();

        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].0.as_str(), "authorization");
        assert_eq!(headers[0].1.to_str().unwrap(), "Bearer token");
        assert_eq!(headers[1].0.as_str(), "x-test");
        assert_eq!(headers[1].1.to_str().unwrap(), "yes");
    }

    #[test]
    fn parses_line_header_overrides() {
        let headers = parse_header_overrides("Authorization: Bearer token\nX-Test: yes").unwrap();

        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].0.as_str(), "authorization");
        assert_eq!(headers[0].1.to_str().unwrap(), "Bearer token");
        assert_eq!(headers[1].0.as_str(), "x-test");
        assert_eq!(headers[1].1.to_str().unwrap(), "yes");
    }

    #[test]
    fn rejects_invalid_line_header_override() {
        let error = parse_header_overrides("Authorization").unwrap_err();

        assert!(error.to_string().contains("Name: value"));
    }

    #[tokio::test]
    async fn render_with_browser_extracts_dom_from_configured_cdp_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(message) = socket.next().await {
                let message = message.unwrap();
                let Message::Text(text) = message else {
                    continue;
                };
                let request: Value = serde_json::from_str(&text).unwrap();
                let id = request.get("id").and_then(Value::as_u64).unwrap();
                let method = request.get("method").and_then(Value::as_str).unwrap();
                let mut response = json!({
                    "id": id,
                    "result": cdp_mock_result(method)
                });
                if let Some(session_id) = request.get("sessionId") {
                    response["sessionId"] = session_id.clone();
                }

                socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .unwrap();

                if method == "Page.navigate" {
                    socket
                        .send(Message::Text(
                            json!({
                                "method": "Page.loadEventFired",
                                "sessionId": "session-1",
                                "params": {}
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .unwrap();
                }
                if method == "Target.closeTarget" {
                    break;
                }
            }
        });

        let rendered = render_with_browser(BrowserRenderOptions {
            url: "https://example.test/app".to_string(),
            cdp_url: format!("ws://{addr}"),
            headers: Vec::new(),
            wait_ms: 0,
        })
        .await
        .unwrap();

        assert_eq!(rendered.final_url, "https://example.test/rendered");
        assert_eq!(rendered.title, "Rendered");
        assert!(rendered.html.contains("Hello rendered DOM"));
        server.await.unwrap();
    }

    fn cdp_mock_result(method: &str) -> Value {
        match method {
            "Target.createTarget" => json!({ "targetId": "target-1" }),
            "Target.attachToTarget" => json!({ "sessionId": "session-1" }),
            "Runtime.evaluate" => json!({
                "result": {
                    "type": "object",
                    "value": {
                        "url": "https://example.test/rendered",
                        "title": "Rendered",
                        "html": "<html><head><title>Rendered</title></head><body><main>Hello rendered DOM</main></body></html>"
                    }
                }
            }),
            _ => json!({}),
        }
    }
}
