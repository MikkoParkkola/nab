//! Assertions. A red case fails on the named observation, not on a parser panic.
//! Item 1 schema validation runs only after JSON-RPC success and `resultType`.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use crate::harness::Exchange;
use crate::http::Observed;
use crate::wire::{self, TOOLS};

pub type Check = Result<(), String>;

const DEFINITIONS: &[&str] = &[
    "DiscoverResult",
    "ListToolsResult",
    "ListPromptsResult",
    "ListResourcesResult",
    "CallToolResult",
    "GetPromptResult",
    "ReadResourceResult",
    "CompleteResult",
];

pub fn ok(check: Check) {
    if let Err(error) = check {
        panic!("{error}");
    }
}

pub fn both(http: Check, stdio: Check) -> Check {
    match (http, stdio) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(http), Err(stdio)) => Err(format!("http: {http}\nstdio: {stdio}")),
        (Err(http), Ok(())) => Err(format!("http: {http}")),
        (Ok(()), Err(stdio)) => Err(format!("stdio: {stdio}")),
    }
}

pub fn join(checks: impl IntoIterator<Item = Check>) -> Check {
    let errors: Vec<String> = checks.into_iter().filter_map(Result::err).collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

pub fn describe(observed: &Observed) -> String {
    match observed {
        Observed::Rpc { status, body, .. } => format!("status {status} {}", snippet(body)),
        Observed::Other { status, body, .. } => format!("status {status} {}", snippet_bytes(body)),
        Observed::Empty { status, .. } => format!("status {status} empty body"),
    }
}

pub fn rpc_success(observed: &Observed) -> Result<&Value, String> {
    rpc_success_id(observed, &Value::from(7614))
}

pub fn rpc_success_id<'a>(observed: &'a Observed, id: &Value) -> Result<&'a Value, String> {
    match observed {
        Observed::Rpc { status, body, .. } if *status == 200 => {
            if body.get("error").is_some() || body.get("result").is_none() {
                return Err(format!("wanted JSON-RPC success, {}", describe(observed)));
            }
            response_envelope(body, id)?;
            Ok(body)
        }
        _ => Err(format!(
            "wanted HTTP 200 JSON-RPC success, {}",
            describe(observed)
        )),
    }
}

pub fn protocol_version(body: &Value, expected: &str) -> Check {
    let found = body["result"]["protocolVersion"].as_str().unwrap_or("");
    if found == expected {
        Ok(())
    } else {
        Err(format!("protocolVersion {found:?}, wanted {expected}"))
    }
}

pub fn init_flags(body: &Value) -> Check {
    let caps = &body["result"]["capabilities"];
    for path in [
        "tools.listChanged",
        "prompts.listChanged",
        "resources.listChanged",
        "resources.subscribe",
    ] {
        let mut cursor = caps;
        for part in path.split('.') {
            cursor = &cursor[part];
        }
        if cursor.as_bool() != Some(true) {
            return Err(format!("{path} is not true"));
        }
    }
    Ok(())
}

pub fn want_complete(result: &Value) -> Check {
    if result.get("resultType").and_then(Value::as_str) == Some("complete") {
        Ok(())
    } else {
        Err(format!("result.resultType absent: {}", snippet(result)))
    }
}

pub fn want_absent_key(result: &Value, key: &str) -> Check {
    if result.get(key).is_none() {
        Ok(())
    } else {
        Err(format!("{key} is present: {}", snippet(result)))
    }
}

pub fn error_code(body: &Value, code: i64) -> Check {
    let found = body["error"]["code"].as_i64();
    if found == Some(code) && body.get("result").is_none() {
        Ok(())
    } else {
        Err(format!("wanted error {code}, {}", snippet(body)))
    }
}

pub fn id_echo(body: &Value, id: &Value) -> Check {
    if body.get("id") == Some(id) {
        Ok(())
    } else {
        Err(format!("id {:?}, wanted {id}", body.get("id")))
    }
}

fn jsonrpc_version(body: &Value) -> Check {
    if body.get("jsonrpc").and_then(Value::as_str) == Some("2.0") {
        Ok(())
    } else {
        Err(format!("jsonrpc {:?}, wanted 2.0", body.get("jsonrpc")))
    }
}

/// `jsonrpc` 2.0, the id that was sent, and a result object.
fn response_envelope(body: &Value, id: &Value) -> Check {
    jsonrpc_version(body)?;
    id_echo(body, id)?;
    if !body.get("result").is_some_and(Value::is_object) {
        return Err(format!("result is not an object: {}", short(body)));
    }
    Ok(())
}

pub fn jsonrpc_error(body: &Value, id: &Value, code: i64) -> Check {
    jsonrpc_version(body)?;
    id_echo(body, id)?;
    error_code(body, code)
}

pub fn sdk_error(observed: &Observed, status: u16) -> Result<Value, String> {
    match observed {
        Observed::Other {
            status: found,
            body,
            ..
        } if *found == status => {
            let value: Value = serde_json::from_slice(body)
                .map_err(|_| format!("wanted HTTP {status} SdkError, {}", describe(observed)))?;
            if value.get("jsonrpc").is_some() {
                return Err(format!("body has jsonrpc: {}", snippet(&value)));
            }
            exact_key_set(&value, &["code", "data", "message"])?;
            if !value.get("data").is_some_and(Value::is_null) {
                return Err(format!("data {:?}, wanted null", value.get("data")));
            }
            if !value.get("message").is_some_and(Value::is_string) {
                return Err(format!(
                    "message {:?}, wanted a string",
                    value.get("message")
                ));
            }
            Ok(value)
        }
        Observed::Rpc { .. } => Err(format!(
            "got JSON-RPC, wanted HTTP {status} SdkError, {}",
            describe(observed)
        )),
        _ => Err(format!(
            "wanted HTTP {status} SdkError, {}",
            describe(observed)
        )),
    }
}

pub fn sdk_message(observed: &Observed, status: u16, code: i64, message: &str) -> Check {
    let value = sdk_error(observed, status)?;
    if value["code"].as_i64() != Some(code) {
        return Err(format!("code {:?}, wanted {code}", value["code"]));
    }
    if value["message"].as_str() != Some(message) {
        return Err(format!("message {:?}, wanted {message}", value["message"]));
    }
    Ok(())
}

pub fn refusal(before: &[Value], response: Option<&Value>) -> Check {
    for message in before {
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        if method == "elicitation/create" || method == "sampling/createMessage" {
            return Err(method.to_string());
        }
    }
    match response {
        None if before.is_empty() => Err("zero bytes, wanted -32600".into()),
        None => Err("no JSON-RPC error, wanted -32600".into()),
        Some(body) => error_code(body, -32600),
    }
}

pub fn tool_names(result: &Value) -> Check {
    let mut names: HashSet<&str> = result["tools"]
        .as_array()
        .ok_or_else(|| format!("no tools array: {}", snippet(result)))?
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    let mut expected: HashSet<&str> = TOOLS.iter().copied().collect();
    if cfg!(feature = "task") {
        expected.insert("task");
    }
    if names != expected {
        names.retain(|name| !expected.contains(name));
        let missing: Vec<&str> = expected
            .iter()
            .copied()
            .filter(|name| {
                !result["tools"].as_array().is_some_and(|tools| {
                    tools.iter().any(|tool| tool["name"].as_str() == Some(name))
                })
            })
            .collect();
        return Err(format!("tool names extra {names:?} missing {missing:?}"));
    }
    Ok(())
}

pub fn no_execution(result: &Value) -> Check {
    let tools = result["tools"].as_array().ok_or("no tools")?;
    if let Some(tool) = tools.iter().find(|tool| tool.get("execution").is_some()) {
        return Err(format!("execution present on {}", tool["name"]));
    }
    Ok(())
}

pub fn task_support(result: &Value, name: &str, support: &str) -> Check {
    let tools = result["tools"].as_array().ok_or("no tools")?;
    let tool = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some(name))
        .ok_or_else(|| format!("missing {name}"))?;
    let found = tool["execution"]["taskSupport"].as_str().unwrap_or("");
    if found == support {
        Ok(())
    } else {
        Err(format!("{name} taskSupport {found:?}, wanted {support}"))
    }
}

pub fn list_meta_2026(result: &Value, cache: &str) -> Check {
    want_complete(result)?;
    if result.get("cacheScope").and_then(Value::as_str) != Some(cache) {
        return Err(format!(
            "cacheScope {:?}, wanted {cache}",
            result.get("cacheScope")
        ));
    }
    if result.get("ttlMs").and_then(Value::as_i64) != Some(0) {
        return Err(format!("ttlMs {:?}, wanted 0", result.get("ttlMs")));
    }
    Ok(())
}

pub fn tool_text(result: &Value) -> String {
    result["content"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

pub fn is_error(result: &Value) -> bool {
    result.get("isError").and_then(Value::as_bool) == Some(true)
}

pub fn watch_id(text: &str) -> Result<String, String> {
    let marker = "- **ID**: `";
    let start = text
        .find(marker)
        .ok_or_else(|| format!("no watch id in {text}"))?
        + marker.len();
    let rest = &text[start..];
    let end = rest.find('`').ok_or("watch id not closed")?;
    Ok(rest[..end].to_string())
}

pub fn names_of(result: &Value, key: &str, field: &str) -> Result<HashSet<String>, String> {
    let items = result[key].as_array().ok_or_else(|| format!("no {key}"))?;
    Ok(items
        .iter()
        .filter_map(|item| item[field].as_str().map(str::to_string))
        .collect())
}

pub fn joined_text(result: &Value, key: &str) -> String {
    result[key]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item["text"]
                        .as_str()
                        .or_else(|| item["content"]["text"].as_str())
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

pub fn assert_create_task(result: &Value) -> Check {
    let object = result.as_object().ok_or("result is not an object")?;
    for key in object.keys() {
        if key != "task" && key != "_meta" {
            return Err(format!("unexpected result key {key}"));
        }
    }
    if object.get("_meta").is_some_and(|meta| !meta.is_object()) {
        return Err("_meta is not an object".into());
    }
    for key in ["content", "isError", "structuredContent", "resultType"] {
        if result.get(key).is_some() {
            return Err(format!("{key} is present on a task result"));
        }
    }
    let task = result
        .get("task")
        .and_then(Value::as_object)
        .ok_or("missing task")?;
    let allowed = [
        "createdAt",
        "lastUpdatedAt",
        "pollInterval",
        "status",
        "taskId",
        "ttl",
    ];
    for key in task.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("unexpected task key {key}"));
        }
    }
    if task.contains_key("statusMessage") {
        return Err("statusMessage is present".into());
    }
    if task
        .get("taskId")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err("taskId is empty".into());
    }
    if task.get("status").and_then(Value::as_str) != Some("working") {
        return Err(format!("status {:?}", task.get("status")));
    }
    if task.get("pollInterval").and_then(Value::as_i64) != Some(1000) {
        return Err(format!("pollInterval {:?}", task.get("pollInterval")));
    }
    if !task.get("ttl").is_some_and(Value::is_null) {
        return Err(format!("ttl {:?}", task.get("ttl")));
    }
    for key in ["createdAt", "lastUpdatedAt"] {
        if task.get(key).and_then(Value::as_str).is_none() {
            return Err(format!("{key} is not a string"));
        }
    }
    Ok(())
}

pub fn validate_pinned(definition: &str, result: &Value) -> Check {
    let validators = validators()?;
    let found = validators
        .get(definition)
        .ok_or_else(|| format!("harness: no validator {definition}"))?;
    found
        .validate(result)
        .map_err(|error| format!("schema {definition}: {error}"))
}

pub fn short(value: &Value) -> String {
    snippet(value)
}

pub fn has_method(messages: &[Value], method: &str) -> bool {
    messages
        .iter()
        .any(|message| message.get("method").and_then(Value::as_str) == Some(method))
}

pub fn exact_key_set(value: &Value, keys: &[&str]) -> Check {
    let object = value
        .as_object()
        .ok_or_else(|| format!("not an object: {}", short(value)))?;
    let found: HashSet<&str> = object.keys().map(String::as_str).collect();
    let expected: HashSet<&str> = keys.iter().copied().collect();
    if found == expected {
        Ok(())
    } else {
        Err(format!("keys {found:?}, wanted {keys:?}"))
    }
}

pub fn produce_400(observed: &Observed) -> Check {
    let value = sdk_error(observed, 400)?;
    exact_key_set(&value, &["code", "data", "message"])?;
    if value.get("code").and_then(Value::as_i64) != Some(-32015) {
        return Err(format!("code {:?}, wanted -32015", value.get("code")));
    }
    if !value.get("data").is_some_and(Value::is_null) {
        return Err(format!("data {:?}, wanted null", value.get("data")));
    }
    Ok(())
}

pub fn stdio_success(exchange: &Exchange) -> Result<&Value, String> {
    let Some(body) = exchange.response.as_ref() else {
        return Err(if exchange.before.is_empty() {
            "zero bytes, wanted JSON-RPC success".into()
        } else {
            "no JSON-RPC success".into()
        });
    };
    if body.get("result").is_none() || body.get("error").is_some() {
        return Err(format!("wanted JSON-RPC success, {}", short(body)));
    }
    response_envelope(body, &Value::from(7614))?;
    Ok(body)
}

pub fn stdio_silence(exchange: &Exchange) -> Check {
    if exchange.response.is_none() && exchange.before.is_empty() {
        if exchange.trailing == 0 && exchange.raw_lines == 0 {
            return Ok(());
        }
        return Err(format!(
            "wanted zero bytes, raw lines {} trailing {}",
            exchange.raw_lines, exchange.trailing
        ));
    }
    if exchange
        .response
        .as_ref()
        .is_some_and(|body| body.pointer("/result/tools").is_some())
    {
        return Err(format!(
            "wanted zero bytes, got a list: {}",
            short(exchange.response.as_ref().expect("response"))
        ));
    }
    Err(format!(
        "wanted zero bytes, got {}",
        exchange
            .response
            .as_ref()
            .map_or_else(|| format!("{} messages", exchange.before.len()), short)
    ))
}

/// A written 2025 success fails on `resultType` before the `-32600` check.
/// Zero bytes, or an elicitation, fails as a refusal.
pub fn stdio_refusal(exchange: &Exchange, id: &Value) -> Check {
    if let Some(body) = exchange.response.as_ref() {
        refusal_or_type(body, id)?;
        if exchange.before.is_empty() {
            return Ok(());
        }
        return Err(format!(
            "{} messages before the refusal",
            exchange.before.len()
        ));
    }
    refusal(&exchange.before, None)
}

pub fn http_refusal(observed: &Observed, id: &Value) -> Check {
    match observed {
        Observed::Rpc {
            status: 200, body, ..
        } => refusal_or_type(body, id),
        other => Err(format!("wanted JSON-RPC error -32600, {}", describe(other))),
    }
}

pub fn refusal_or_type(body: &Value, id: &Value) -> Check {
    jsonrpc_version(body)?;
    id_echo(body, id)?;
    if let Some(result) = body.get("result") {
        want_complete(result)?;
        return Err(format!("wanted -32600, got a success {}", short(body)));
    }
    error_code(body, -32600)
}

pub fn require_session(observed: &Observed) -> Result<String, String> {
    match crate::http::HttpChild::session_of(observed) {
        Some(id) if !id.is_empty() => Ok(id),
        _ => Err("session id absent".into()),
    }
}

pub fn labeled(label: &str, check: Check) -> Check {
    check.map_err(|error| format!("{label}: {error}"))
}

pub fn validate_output(list: &Value, tool: &str, structured: Option<&Value>) -> Check {
    let Some(structured) = structured else {
        return Ok(());
    };
    let tools = list["tools"]
        .as_array()
        .ok_or("outputSchema list has no tools")?;
    let schema = tools
        .iter()
        .find(|item| item["name"].as_str() == Some(tool))
        .and_then(|item| item.get("outputSchema"))
        .ok_or_else(|| format!("no outputSchema for {tool}"))?;
    let validator = jsonschema::draft202012::new(schema)
        .map_err(|error| format!("outputSchema {tool}: {error}"))?;
    validator
        .validate(structured)
        .map_err(|error| format!("structuredContent {tool}: {error}"))
}

const PIN_FILE: &str = "/tmp/mcp-2026-schema-pinned.json";
const PIN_SHA: &str = "ef70b61f99b6d2e5e3b46863822eab08dff6a45bedc7a08914e0e5b133f40203";
const PIN_URL: &str = "https://raw.githubusercontent.com/modelcontextprotocol/modelcontextprotocol/271ecc9accafdd9b83a3c869fa67c22953b2af80/schema/2026-07-28/schema.json";

pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn validators()
-> Result<std::sync::MutexGuard<'static, HashMap<String, jsonschema::Validator>>, String> {
    let mut cache = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.len() == DEFINITIONS.len() {
        return Ok(cache);
    }
    let file = pinned_text()?;
    let pinned: Value =
        serde_json::from_str(&file).map_err(|error| format!("harness: pinned schema: {error}"))?;
    for definition in DEFINITIONS {
        if cache.contains_key(*definition) {
            continue;
        }
        let document = serde_json::json!({
            "$schema": pinned.get("$schema").cloned().unwrap_or(Value::Null),
            "$defs": pinned.get("$defs").cloned().unwrap_or(Value::Null),
            "allOf": [{ "$ref": format!("#/$defs/{definition}") }]
        });
        let built = jsonschema::draft202012::new(&document)
            .map_err(|error| format!("harness: validator {definition}: {error}"))?;
        cache.insert((*definition).to_string(), built);
    }
    Ok(cache)
}

fn snippet(value: &Value) -> String {
    let text = value.to_string();
    truncate(&text)
}

fn snippet_bytes(body: &[u8]) -> String {
    truncate(&String::from_utf8_lossy(body))
}

fn truncate(text: &str) -> String {
    let mut end = text.len().min(300);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

pub fn status_of(observed: &Observed) -> u16 {
    match observed {
        Observed::Rpc { status, .. }
        | Observed::Other { status, .. }
        | Observed::Empty { status, .. } => *status,
    }
}

pub fn want_empty(observed: &Observed, status: u16) -> Check {
    match observed {
        Observed::Empty { status: found, .. } if *found == status => Ok(()),
        other => Err(format!("wanted {status} empty, {}", describe(other))),
    }
}

pub fn want_400_not_406(observed: &Observed) -> Check {
    if status_of(observed) == 406 {
        return Err(format!(
            "status 406, wanted HTTP 400, {}",
            describe(observed)
        ));
    }
    produce_400(observed)
}

/// A fresh greater-version POST is not a JSON-RPC object.
/// An error object and a success object fail with different sentences.
/// A body that is not an envelope still fails when it carries `result` or `error.code` `-32603`.
pub fn fresh_body(observed: &Observed) -> Check {
    match observed {
        Observed::Rpc { body, .. } => fresh_rpc(body),
        Observed::Other { body, .. } => fresh_loose(body),
        Observed::Empty { .. } => Ok(()),
    }
}

fn fresh_rpc(body: &Value) -> Check {
    if body.get("error").is_some() {
        Err("JSON-RPC error on this fresh POST".into())
    } else {
        Err("JSON-RPC success on this fresh POST".into())
    }
}

fn fresh_loose(body: &[u8]) -> Check {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Ok(());
    };
    if value.get("jsonrpc").is_some() {
        return fresh_rpc(&value);
    }
    if value.get("result").is_some() {
        return Err("result on this fresh POST".into());
    }
    if value.pointer("/error/code").and_then(Value::as_i64) == Some(-32603) {
        return Err("JSON-RPC error on this fresh POST".into());
    }
    Ok(())
}

pub fn text_has(result: &Value, needle: &str) -> Check {
    let text = tool_text(result);
    if text.contains(needle) {
        Ok(())
    } else {
        Err(format!("text missing {needle:?}: {}", snippet(result)))
    }
}

pub fn http_tool(observed: &Observed) -> Result<&Value, String> {
    let body = rpc_success(observed)?;
    let result = body
        .get("result")
        .ok_or_else(|| format!("no result {}", snippet(body)))?;
    want_complete(result)?;
    Ok(result)
}

pub fn stdio_tool(exchange: &Exchange) -> Result<&Value, String> {
    let body = stdio_success(exchange)?;
    let result = body
        .get("result")
        .ok_or_else(|| format!("no result {}", snippet(body)))?;
    want_complete(result)?;
    Ok(result)
}

pub fn assert_list_2026(result: &Value) -> Check {
    list_meta_2026(result, "public")?;
    tool_names(result)?;
    no_execution(result)?;
    validate_pinned("ListToolsResult", result)
}

pub fn assert_list_2025(result: &Value) -> Check {
    tool_names(result)?;
    task_support(result, "fetch_batch", "optional")?;
    task_support(result, "analyze", "required")?;
    want_absent_key(result, "resultType")
}

pub fn assert_discover(result: &Value) -> Check {
    list_meta_2026(result, "public")?;
    let versions = result["supportedVersions"]
        .as_array()
        .ok_or("no supportedVersions")?;
    let has = |name: &str| versions.iter().any(|item| item.as_str() == Some(name));
    if !has(wire::V2026) || !has(wire::V2025) {
        return Err(format!("supportedVersions {}", snippet(result)));
    }
    let caps = &result["capabilities"];
    for key in ["tools", "prompts", "resources", "completions"] {
        if caps.get(key).is_none() {
            return Err(format!("capabilities missing {key}"));
        }
    }
    if caps.get("logging").is_some() || caps.get("tasks").is_some() {
        return Err("capabilities has logging or tasks".into());
    }
    if caps["resources"]["subscribe"].as_bool() == Some(true) {
        return Err("resources.subscribe is true".into());
    }
    for key in ["tools", "prompts", "resources"] {
        if caps[key]["listChanged"].as_bool() == Some(true) {
            return Err(format!("{key}.listChanged is true"));
        }
    }
    for key in [
        "cacheScope",
        "capabilities",
        "resultType",
        "supportedVersions",
        "ttlMs",
    ] {
        if result.get(key).is_none() {
            return Err(format!("missing {key}"));
        }
    }
    validate_pinned("DiscoverResult", result)
}

pub fn exact_names(result: &Value, key: &str, field: &str, expected: &[&str]) -> Check {
    let found = names_of(result, key, field)?;
    let wanted: HashSet<String> = expected.iter().copied().map(str::to_string).collect();
    if found == wanted {
        Ok(())
    } else {
        Err(format!("{key} {found:?}, wanted {expected:?}"))
    }
}

pub fn count_method(messages: &[Value], method: &str) -> usize {
    messages
        .iter()
        .filter(|message| message.get("method").and_then(Value::as_str) == Some(method))
        .count()
}

fn pinned_text() -> Result<String, String> {
    if let Ok(text) = std::fs::read_to_string(PIN_FILE)
        && hex_sha256(text.as_bytes()) == PIN_SHA
    {
        return Ok(text);
    }
    let text = fetch_pin()?;
    if hex_sha256(text.as_bytes()) != PIN_SHA {
        return Err("harness: pinned schema: sha256 mismatch".into());
    }
    std::fs::write(PIN_FILE, &text).map_err(|error| format!("harness: pinned schema: {error}"))?;
    Ok(text)
}

fn fetch_pin() -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| format!("harness: pinned schema: {error}"))?;
    client
        .get(PIN_URL)
        .send()
        .map_err(|error| format!("harness: pinned schema: {error}"))?
        .error_for_status()
        .map_err(|error| format!("harness: pinned schema: {error}"))?
        .text()
        .map_err(|error| format!("harness: pinned schema: {error}"))
}

fn cache() -> &'static Mutex<HashMap<String, jsonschema::Validator>> {
    static CACHE: OnceLock<Mutex<HashMap<String, jsonschema::Validator>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}
