//! Served tools, login, analyze, and the task-augmented calls.
//!
//! TP-LOGIN-4 is stdio only. The HTTP client returns after one event and cannot
//! post the decline back onto the stream it already closed. The plan does not
//! require both transports for that case.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::check::{self, Check};
use crate::harness::{Answer, Exchange, StdioChild};
use crate::http::{Accept, HttpChild, Observed, Proto};
use crate::listen::DualListener;
use crate::wire::{self, Caps, Field};

fn tool(name: &str, arguments: Value, field: Field, caps: Caps, task: Option<Value>) -> Value {
    wire::tool_call(name, arguments, wire::id_num(), field, caps, task)
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
}

fn fluid_present() -> bool {
    on_path("fluidaudiocli")
        || Path::new("/opt/homebrew/bin/fluidaudiocli").is_file()
        || Path::new("/private/tmp/FluidAudio/.build/arm64-apple-macosx/release/fluidaudiocli")
            .is_file()
}

fn require_no_op() -> Check {
    if on_path("op") {
        Err("premise: op is on PATH".into())
    } else {
        Ok(())
    }
}

fn served_http(message: &Value, budget: Duration, needle: &str, schema: Option<&str>) -> Check {
    let mut child = HttpChild::start();
    let observed = child.post(
        message,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        budget,
    );
    finish_http(&mut child, &observed, needle, schema)
}

fn finish_http(
    child: &mut HttpChild,
    observed: &Observed,
    needle: &str,
    schema: Option<&str>,
) -> Check {
    let result = check::http_tool(observed)?;
    check::text_has(result, needle)?;
    check::validate_pinned("CallToolResult", result)?;
    schema_http(child, schema, result.get("structuredContent"))
}

fn schema_http(child: &mut HttpChild, schema: Option<&str>, structured: Option<&Value>) -> Check {
    let Some(name) = schema else {
        return Ok(());
    };
    let list = child.post(
        &wire::tools_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty),
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::TWO,
    );
    let listed = check::http_tool(&list)?;
    check::validate_output(listed, name, structured)
}

fn served_stdio(message: &Value, budget: Duration, needle: &str, schema: Option<&str>) -> Check {
    let mut child = StdioChild::start();
    let exchange = child.exchange(message, budget, Answer::Record);
    finish_stdio(&mut child, &exchange, needle, schema)
}

fn finish_stdio(
    child: &mut StdioChild,
    exchange: &Exchange,
    needle: &str,
    schema: Option<&str>,
) -> Check {
    let result = check::stdio_tool(exchange)?;
    check::text_has(result, needle)?;
    check::validate_pinned("CallToolResult", result)?;
    schema_stdio(child, schema, result.get("structuredContent"))
}

fn schema_stdio(child: &mut StdioChild, schema: Option<&str>, structured: Option<&Value>) -> Check {
    let Some(name) = schema else {
        return Ok(());
    };
    let list = child.exchange(
        &wire::tools_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty),
        wire::TWO,
        Answer::Record,
    );
    let listed = check::stdio_tool(&list)?;
    check::validate_output(listed, name, structured)
}

fn served(message: &Value, budget: Duration, needle: &str, schema: Option<&str>) -> Check {
    let http = served_http(message, budget, needle, schema);
    let stdio = served_stdio(message, budget, needle, schema);
    check::both(http, stdio)
}

fn refusal(message: &Value) -> Check {
    let http = {
        let mut child = HttpChild::start();
        let observed = child.post(
            message,
            Proto::Value(wire::V2026),
            None,
            Accept::JsonAndSse,
            wire::TWO,
        );
        check::http_refusal(&observed, &wire::id_num())
    };
    let stdio = {
        let mut child = StdioChild::start();
        let exchange = child.exchange(message, wire::TWO, Answer::Record);
        check::stdio_refusal(&exchange, &wire::id_num())
    };
    check::both(http, stdio)
}

#[test]
fn tp_c1() {
    check::ok(c1());
}

fn c1() -> Check {
    let message = tool(
        "fingerprint",
        json!({"count": 1, "browser": "chrome"}),
        Field::Value(wire::V2026),
        Caps::Empty,
        None,
    );
    served(
        &message,
        wire::TWO,
        "Generating 1 browser fingerprints",
        Some("fingerprint"),
    )
}

#[test]
fn tp_c2() {
    check::ok(c2());
}

fn c2() -> Check {
    require_no_op()?;
    let message = tool(
        "auth_lookup",
        json!({"url": wire::PAGE}),
        Field::Value(wire::V2026),
        Caps::Empty,
        None,
    );
    served(&message, wire::TWO, "1Password CLI not available", None)
}

#[test]
fn tp_c3() {
    check::ok(c3());
}

fn c3() -> Check {
    let listener = DualListener::bind_loopback(wire::BATCH_BODY)?;
    let message = tool(
        "watch_create",
        json!({"url": listener.url(), "interval": "1h"}),
        Field::Value(wire::V2026),
        Caps::Empty,
        None,
    );
    check::both(c3_http(&message), c3_stdio(&message))
}

fn c3_http(message: &Value) -> Check {
    let mut child = HttpChild::start();
    let observed = child.post(
        message,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::LISTENER,
    );
    let result = check::http_tool(&observed)?;
    check::text_has(result, "Watch created.")?;
    check::validate_pinned("CallToolResult", result)?;
    let id = check::watch_id(&check::tool_text(result))?;
    watch_follow_http(&mut child, &id)
}

fn watch_follow_http(child: &mut HttpChild, id: &str) -> Check {
    let remove = tool(
        "watch_remove",
        json!({"id": id}),
        Field::Value(wire::V2026),
        Caps::Empty,
        None,
    );
    let removed = child.post(
        &remove,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::TWO,
    );
    let removed_result = check::http_tool(&removed)?;
    check::validate_pinned("CallToolResult", removed_result)?;
    let list = tool(
        "watch_list",
        json!({}),
        Field::Value(wire::V2026),
        Caps::Empty,
        None,
    );
    let listed = child.post(
        &list,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::TWO,
    );
    let result = check::http_tool(&listed)?;
    check::validate_pinned("CallToolResult", result)?;
    if check::tool_text(result).contains(id) {
        Err(format!("watch {id} still listed"))
    } else {
        Ok(())
    }
}

fn c3_stdio(message: &Value) -> Check {
    let mut child = StdioChild::start();
    let exchange = child.exchange(message, wire::LISTENER, Answer::Record);
    let result = check::stdio_tool(&exchange)?;
    check::text_has(result, "Watch created.")?;
    check::validate_pinned("CallToolResult", result)?;
    let id = check::watch_id(&check::tool_text(result))?;
    watch_follow_stdio(&mut child, &id)
}

fn watch_follow_stdio(child: &mut StdioChild, id: &str) -> Check {
    let remove = tool(
        "watch_remove",
        json!({"id": id}),
        Field::Value(wire::V2026),
        Caps::Empty,
        None,
    );
    let removed = child.exchange(&remove, wire::TWO, Answer::Record);
    let removed_result = check::stdio_tool(&removed)?;
    check::validate_pinned("CallToolResult", removed_result)?;
    let list = tool(
        "watch_list",
        json!({}),
        Field::Value(wire::V2026),
        Caps::Empty,
        None,
    );
    let listed = child.exchange(&list, wire::TWO, Answer::Record);
    let result = check::stdio_tool(&listed)?;
    check::validate_pinned("CallToolResult", result)?;
    if check::tool_text(result).contains(id) {
        Err(format!("watch {id} still listed"))
    } else {
        Ok(())
    }
}

#[test]
fn tp_c8() {
    check::ok(c8());
}

fn c8() -> Check {
    let message = tool(
        "submit",
        json!({"url": "http://192.168.1.1/form", "fields": []}),
        Field::Value(wire::V2026),
        Caps::Empty,
        None,
    );
    check::join([served_error(&message), c8_after_init()])
}

fn served_error(message: &Value) -> Check {
    let http = error_http(message);
    let stdio = error_stdio(message);
    check::both(http, stdio)
}

fn error_http(message: &Value) -> Check {
    let mut child = HttpChild::start();
    let observed = child.post(
        message,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::TWO,
    );
    let result = check::http_tool(&observed)?;
    tool_error(result, wire::SSRF_TEXT)?;
    structured_http(&mut child, result, message)
}

fn error_stdio(message: &Value) -> Check {
    let mut child = StdioChild::start();
    let exchange = child.exchange(message, wire::TWO, Answer::Record);
    let result = check::stdio_tool(&exchange)?;
    tool_error(result, wire::SSRF_TEXT)?;
    structured_stdio(&mut child, result, message)
}

fn tool_error(result: &Value, needle: &str) -> Check {
    if !check::is_error(result) {
        return Err(format!("isError is not true: {}", check::short(result)));
    }
    check::text_has(result, needle)?;
    check::validate_pinned("CallToolResult", result)
}

fn tool_error_2025(result: &Value, needle: &str) -> Check {
    if !check::is_error(result) {
        return Err(format!("isError is not true: {}", check::short(result)));
    }
    check::text_has(result, needle)
}

fn c8_after_init() -> Check {
    check::both(c8_http_2025(), c8_stdio_2025())
}

fn c8_http_2025() -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let message = tool(
        "submit",
        json!({"url": "http://192.168.1.1/form", "fields": []}),
        Field::Absent,
        Caps::Absent,
        None,
    );
    let observed = child.post(
        &message,
        Proto::Value(wire::V2025),
        Some(&session),
        Accept::JsonAndSse,
        wire::TWO,
    );
    let body = check::rpc_success(&observed)?;
    tool_error_2025(&body["result"], wire::SSRF_TEXT)
}

fn c8_stdio_2025() -> Check {
    let mut child = StdioChild::start();
    let init = child.exchange(
        &wire::initialize(wire::V2025, Field::Absent, json!({})),
        wire::TWO,
        Answer::Record,
    );
    check::stdio_success(&init)?;
    let message = tool(
        "submit",
        json!({"url": "http://192.168.1.1/form", "fields": []}),
        Field::Absent,
        Caps::Absent,
        None,
    );
    let exchange = child.exchange(&message, wire::TWO, Answer::Record);
    let body = check::stdio_success(&exchange)?;
    tool_error_2025(&body["result"], wire::SSRF_TEXT)
}

#[test]
fn tp_login_1() {
    check::ok(check::join([
        login_refusal(Caps::Empty),
        login_refusal(Caps::Absent),
    ]));
}

fn login_refusal(caps: Caps) -> Check {
    let message = tool(
        "login",
        json!({"url": wire::OAUTH}),
        Field::Value(wire::V2026),
        caps,
        None,
    );
    refusal(&message)
}

#[test]
fn tp_login_2() {
    check::ok(login_2());
}

fn login_2() -> Check {
    require_no_op()?;
    let message = tool(
        "login",
        json!({"url": wire::PAGE, "cookies": "none"}),
        Field::Value(wire::V2026),
        Caps::Empty,
        None,
    );
    refusal(&message)
}

#[test]
fn tp_login_4() {
    check::ok(login_4());
}

fn login_4() -> Check {
    login_4_oauth()?;
    if on_path("op") {
        return Ok(());
    }
    login_4_unavailable()
}

fn login_4_oauth() -> Check {
    let mut child = StdioChild::start();
    let init = child.exchange(
        &wire::initialize(wire::V2025, Field::Absent, json!({})),
        wire::TWO,
        Answer::Record,
    );
    check::stdio_success(&init)?;
    let message = tool(
        "login",
        json!({"url": wire::OAUTH}),
        Field::Absent,
        Caps::Absent,
        None,
    );
    let exchange = child.exchange(&message, wire::TWO, Answer::Decline);
    if !check::has_method(&exchange.before, "elicitation/create") {
        return Err("elicitation/create absent".into());
    }
    check::stdio_success(&exchange).map(|_| ())
}

fn login_4_unavailable() -> Check {
    let mut child = StdioChild::start();
    let init = child.exchange(
        &wire::initialize(wire::V2025, Field::Absent, json!({})),
        wire::TWO,
        Answer::Record,
    );
    check::stdio_success(&init)?;
    let message = tool(
        "login",
        json!({"url": wire::PAGE, "cookies": "none"}),
        Field::Absent,
        Caps::Absent,
        None,
    );
    let exchange = child.exchange(&message, wire::TWO, Answer::Decline);
    if !check::has_method(&exchange.before, "elicitation/create") {
        return Err("elicitation/create absent".into());
    }
    check::stdio_success(&exchange).map(|_| ())
}

#[test]
fn tp_an_1() {
    check::ok(an_1());
}

fn an_1() -> Check {
    if fluid_present() {
        return Err("premise: fluidaudiocli is present".into());
    }
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let path = dir.path().join("clip.wav");
    tiny_wav(&path)?;
    let message = tool(
        "analyze",
        json!({"input": path.display().to_string(), "active_reading": true}),
        Field::Value(wire::V2026),
        Caps::Sampling,
        None,
    );
    served_error_text(&message, "is not available")
}

fn served_error_text(message: &Value, needle: &str) -> Check {
    check::both(
        error_text_http(message, needle),
        error_text_stdio(message, needle),
    )
}

fn error_text_http(message: &Value, needle: &str) -> Check {
    let mut child = HttpChild::start();
    let observed = child.post(
        message,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::TWO,
    );
    let result = check::http_tool(&observed)?;
    tool_error(result, needle)?;
    structured_http(&mut child, result, message)
}

fn error_text_stdio(message: &Value, needle: &str) -> Check {
    let mut child = StdioChild::start();
    let exchange = child.exchange(message, wire::TWO, Answer::Record);
    let result = check::stdio_tool(&exchange)?;
    tool_error(result, needle)?;
    structured_stdio(&mut child, result, message)
}

#[test]
fn tp_notask_1() {
    check::ok(notask_1());
}

fn notask_1() -> Check {
    let listener = DualListener::bind_loopback(wire::BATCH_BODY)?;
    check::both(notask1_http(&listener), notask1_stdio(&listener))
}

fn notask1_http(listener: &DualListener) -> Check {
    check::join([notask_batch_http(listener), notask_analyze_http()])
}

fn notask1_stdio(listener: &DualListener) -> Check {
    check::join([notask_batch_stdio(listener), notask_analyze_stdio()])
}

fn notask_batch_http(listener: &DualListener) -> Check {
    let message = tool(
        "fetch_batch",
        json!({"urls": [listener.url()]}),
        Field::Value(wire::V2026),
        Caps::Empty,
        Some(json!({})),
    );
    let mut child = HttpChild::start();
    let observed = child.post(
        &message,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::LISTENER,
    );
    let result = check::http_tool(&observed)?;
    batch_body(result, &listener.url())?;
    output_schema_http(&mut child, result, "fetch_batch")
}

fn notask_batch_stdio(listener: &DualListener) -> Check {
    let message = tool(
        "fetch_batch",
        json!({"urls": [listener.url()]}),
        Field::Value(wire::V2026),
        Caps::Empty,
        Some(json!({})),
    );
    let mut child = StdioChild::start();
    let exchange = child.exchange(&message, wire::LISTENER, Answer::Record);
    let result = check::stdio_tool(&exchange)?;
    batch_body(result, &listener.url())?;
    output_schema_stdio(&mut child, result, "fetch_batch")
}

fn batch_body(result: &Value, url: &str) -> Check {
    if result.get("task").is_some() || result.get("taskId").is_some() {
        return Err(format!("task id present: {}", check::short(result)));
    }
    if check::is_error(result) {
        return Err("isError is true".into());
    }
    let results = result
        .pointer("/structuredContent/results")
        .and_then(Value::as_array)
        .ok_or("results absent")?;
    if results.len() != 1 {
        return Err(format!("results length {}, wanted 1", results.len()));
    }
    let item = &results[0];
    check::exact_key_set(item, &["content", "status", "timing_ms", "url"])?;
    if item["url"].as_str() != Some(url) {
        return Err(format!("url {:?}", item["url"]));
    }
    if item["status"].as_i64() != Some(200) {
        return Err(format!("status {:?}", item["status"]));
    }
    let content = item["content"].as_str().unwrap_or("");
    if !content.contains(wire::BATCH_BODY) {
        return Err(format!("content missing batch body: {content}"));
    }
    check::validate_pinned("CallToolResult", result)
}

fn absent_task(result: &Value) -> Check {
    if result.get("task").is_some() || result.get("taskId").is_some() {
        Err(format!("task id present: {}", check::short(result)))
    } else {
        Ok(())
    }
}

fn structured_http(child: &mut HttpChild, result: &Value, message: &Value) -> Check {
    if result.get("structuredContent").is_none() {
        return Ok(());
    }
    output_schema_http(child, result, tool_name(message))
}

fn structured_stdio(child: &mut StdioChild, result: &Value, message: &Value) -> Check {
    if result.get("structuredContent").is_none() {
        return Ok(());
    }
    output_schema_stdio(child, result, tool_name(message))
}

fn tool_name(message: &Value) -> &str {
    message["params"]["name"].as_str().unwrap_or("")
}

fn output_schema_http(child: &mut HttpChild, result: &Value, tool: &str) -> Check {
    let list = child.post(
        &wire::tools_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty),
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::TWO,
    );
    let listed = check::http_tool(&list)?;
    check::validate_output(listed, tool, result.get("structuredContent"))
}

fn output_schema_stdio(child: &mut StdioChild, result: &Value, tool: &str) -> Check {
    let list = child.exchange(
        &wire::tools_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty),
        wire::TWO,
        Answer::Record,
    );
    let listed = check::stdio_tool(&list)?;
    check::validate_output(listed, tool, result.get("structuredContent"))
}

fn notask_analyze_http() -> Check {
    if fluid_present() {
        return Err("premise: fluidaudiocli is present".into());
    }
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let path = dir.path().join("clip.wav");
    tiny_wav(&path)?;
    let message = tool(
        "analyze",
        json!({"input": path.display().to_string(), "active_reading": true}),
        Field::Value(wire::V2026),
        Caps::Sampling,
        Some(json!({})),
    );
    let mut child = HttpChild::start();
    let observed = child.post(
        &message,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::TWO,
    );
    let result = check::http_tool(&observed)?;
    absent_task(result)?;
    tool_error(result, "is not available")?;
    structured_http(&mut child, result, &message)
}

fn notask_analyze_stdio() -> Check {
    if fluid_present() {
        return Err("premise: fluidaudiocli is present".into());
    }
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let path = dir.path().join("clip.wav");
    tiny_wav(&path)?;
    let message = tool(
        "analyze",
        json!({"input": path.display().to_string(), "active_reading": true}),
        Field::Value(wire::V2026),
        Caps::Sampling,
        Some(json!({})),
    );
    let mut child = StdioChild::start();
    let exchange = child.exchange(&message, wire::TWO, Answer::Record);
    let result = check::stdio_tool(&exchange)?;
    absent_task(result)?;
    tool_error(result, "is not available")?;
    structured_stdio(&mut child, result, &message)
}

#[test]
fn tp_notask_2() {
    check::ok(notask_2());
}

#[test]
fn tp_reg_4() {
    check::ok(notask_2());
}

pub(crate) fn notask_2() -> Check {
    let listener = DualListener::bind_loopback(wire::BATCH_BODY)?;
    check::both(notask2_http(&listener), notask2_stdio(&listener))
}

fn notask2_http(listener: &DualListener) -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let batch = tool(
        "fetch_batch",
        json!({"urls": [listener.url()]}),
        Field::Absent,
        Caps::Absent,
        Some(json!({})),
    );
    create_then_missing_http(&mut child, &session, &batch)?;
    let analyze = tool(
        "analyze",
        json!({"input": "nab-mcp-2026-missing.wav"}),
        Field::Absent,
        Caps::Absent,
        Some(json!({})),
    );
    create_then_missing_http(&mut child, &session, &analyze)
}

fn create_then_missing_http(child: &mut HttpChild, session: &str, message: &Value) -> Check {
    let observed = child.post(
        message,
        Proto::Value(wire::V2025),
        Some(session),
        Accept::JsonAndSse,
        wire::TWO,
    );
    let body = check::rpc_success(&observed)?;
    check::assert_create_task(&body["result"])?;
    let id = body["result"]["task"]["taskId"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let follow = wire::tasks(
        "tasks/result",
        Some(&id),
        wire::id_num(),
        Field::Absent,
        Caps::Absent,
    );
    let next = child.post(
        &follow,
        Proto::Value(wire::V2025),
        Some(session),
        Accept::JsonAndSse,
        wire::TWO,
    );
    match &next {
        Observed::Rpc {
            status: 200, body, ..
        } => check::jsonrpc_error(body, &wire::id_num(), -32601),
        other => Err(format!(
            "wanted JSON-RPC error -32601, {}",
            check::describe(other)
        )),
    }
}

fn notask2_stdio(listener: &DualListener) -> Check {
    let mut child = StdioChild::start();
    let init = child.exchange(
        &wire::initialize(wire::V2025, Field::Absent, json!({})),
        wire::TWO,
        Answer::Record,
    );
    check::stdio_success(&init)?;
    let batch = tool(
        "fetch_batch",
        json!({"urls": [listener.url()]}),
        Field::Absent,
        Caps::Absent,
        Some(json!({})),
    );
    create_then_missing_stdio(&mut child, &batch)?;
    let analyze = tool(
        "analyze",
        json!({"input": "nab-mcp-2026-missing.wav"}),
        Field::Absent,
        Caps::Absent,
        Some(json!({})),
    );
    create_then_missing_stdio(&mut child, &analyze)
}

fn create_then_missing_stdio(child: &mut StdioChild, message: &Value) -> Check {
    let exchange = child.exchange(message, wire::TWO, Answer::Record);
    let body = check::stdio_success(&exchange)?;
    check::assert_create_task(&body["result"])?;
    let id = body["result"]["task"]["taskId"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let follow = wire::tasks(
        "tasks/result",
        Some(&id),
        wire::id_num(),
        Field::Absent,
        Caps::Absent,
    );
    let next = child.exchange(&follow, wire::TWO, Answer::Record);
    let found = next
        .response
        .as_ref()
        .ok_or_else(|| "zero bytes, wanted -32601".to_string())?;
    check::jsonrpc_error(found, &wire::id_num(), -32601)
}

#[cfg(not(feature = "task"))]
#[test]
fn tp_task_1() {
    check::ok(task_1());
}

#[cfg(not(feature = "task"))]
fn task_1() -> Check {
    let message = wire::tools_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty);
    let http = {
        let mut child = HttpChild::start();
        let observed = child.post(
            &message,
            Proto::Value(wire::V2026),
            None,
            Accept::JsonAndSse,
            wire::TWO,
        );
        let body = check::rpc_success(&observed)?;
        check::assert_list_2026(&body["result"])
    };
    let stdio = {
        let mut child = StdioChild::start();
        let exchange = child.exchange(&message, wire::TWO, Answer::Record);
        let body = check::stdio_success(&exchange)?;
        check::assert_list_2026(&body["result"])
    };
    check::both(http, stdio)
}

fn tiny_wav(path: &Path) -> Result<(), String> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&44u32.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&8000u32.to_le_bytes());
    bytes.extend_from_slice(&16000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&8u32.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 8]);
    std::fs::write(path, bytes).map_err(|error| error.to_string())
}
