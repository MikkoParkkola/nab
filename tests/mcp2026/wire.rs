//! JSON-RPC bodies for the 2026-07-28 cases. No I/O.

use serde_json::{Map, Value, json};
use std::time::Duration;

pub const PROTO: &str = "io.modelcontextprotocol/protocolVersion";
pub const CAPS_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
pub const V2026: &str = "2026-07-28";
pub const V2025: &str = "2025-11-25";
pub const UNKNOWN: &str = "1999-01-01";
pub const GREATER: &str = "2026-08-01";

pub const TWO: Duration = Duration::from_secs(2);
pub const LISTENER: Duration = Duration::from_secs(60);
pub const REMOTE: Duration = Duration::from_secs(180);
pub const WATCH: Duration = Duration::from_secs(75);
pub const READY_CAP: Duration = Duration::from_secs(45);

pub const WATCH_EMPTY: &str = "No watches registered. Use `watch_create` to add one.";
pub const SSRF_TEXT: &str = "SSRF blocked: IP address 192.168.1.1 is in a denied range";
pub const BATCH_BODY: &str = "nab-mcp-2026-batch-body";
pub const FETCH_BODY: &str = "nab-mcp-2026-fetch-body";
pub const PAGE: &str = "https://nab-test.invalid/page";
pub const OAUTH: &str = "https://accounts.google.com/o/oauth2/auth?client_id=nab-test";

pub const TOOLS: &[&str] = &[
    "fetch",
    "fetch_batch",
    "submit",
    "login",
    "auth_lookup",
    "fingerprint",
    "validate",
    "benchmark",
    "analyze",
    "watch_create",
    "watch_list",
    "watch_remove",
];

pub const PROMPTS: &[&str] = &[
    "fetch-and-extract",
    "multi-page-research",
    "authenticated-fetch",
    "match-speakers-with-hebb",
];

#[derive(Clone, Copy)]
pub enum Field {
    Absent,
    Value(&'static str),
}

#[derive(Clone, Copy)]
pub enum Caps {
    Absent,
    Empty,
    Sampling,
}

pub fn id_num() -> Value {
    json!(7614)
}

pub fn id_str() -> Value {
    json!("req-7614")
}

pub fn initialize(protocol: &str, field: Field, capabilities: Value) -> Value {
    let mut params = Map::new();
    params.insert("protocolVersion".into(), json!(protocol));
    params.insert("capabilities".into(), capabilities);
    params.insert(
        "clientInfo".into(),
        json!({"name": "nab-mcp-2026-test", "version": "0"}),
    );
    request(
        "initialize",
        Some(id_num()),
        attach(params, field, Caps::Absent),
    )
}

pub fn tools_list(id: Value, field: Field, caps: Caps) -> Value {
    request("tools/list", Some(id), attach(Map::new(), field, caps))
}

pub fn tool_call(
    name: &str,
    arguments: Value,
    id: Value,
    field: Field,
    caps: Caps,
    task: Option<Value>,
) -> Value {
    let mut params = Map::new();
    params.insert("name".into(), json!(name));
    params.insert("arguments".into(), arguments);
    if let Some(task) = task {
        params.insert("task".into(), task);
    }
    request("tools/call", Some(id), attach(params, field, caps))
}

pub fn discover(id: Value, field: Field, caps: Caps) -> Value {
    request("server/discover", Some(id), attach(Map::new(), field, caps))
}

pub fn ping(id: Value, field: Field, caps: Caps) -> Value {
    request("ping", Some(id), attach(Map::new(), field, caps))
}

pub fn set_level(level: &str, id: Value, field: Field, caps: Caps) -> Value {
    let mut params = Map::new();
    params.insert("level".into(), json!(level));
    request("logging/setLevel", Some(id), attach(params, field, caps))
}

pub fn resource(method: &str, uri: &str, id: Value, field: Field, caps: Caps) -> Value {
    let mut params = Map::new();
    params.insert("uri".into(), json!(uri));
    request(method, Some(id), attach(params, field, caps))
}

pub fn resources_list(id: Value, field: Field, caps: Caps) -> Value {
    request("resources/list", Some(id), attach(Map::new(), field, caps))
}

pub fn prompts_list(id: Value, field: Field, caps: Caps) -> Value {
    request("prompts/list", Some(id), attach(Map::new(), field, caps))
}

pub fn prompts_get(id: Value, field: Field, caps: Caps) -> Value {
    let mut params = Map::new();
    params.insert("name".into(), json!("fetch-and-extract"));
    params.insert(
        "arguments".into(),
        json!({"url": PAGE, "extract_query": "the-title-line"}),
    );
    request("prompts/get", Some(id), attach(params, field, caps))
}

pub fn completion(id: Value, field: Field, caps: Caps) -> Value {
    let mut params = Map::new();
    params.insert(
        "ref".into(),
        json!({"type": "ref/prompt", "name": "fetch-and-extract"}),
    );
    params.insert("argument".into(), json!({"name": "cookies", "value": ""}));
    request("completion/complete", Some(id), attach(params, field, caps))
}

pub fn tasks(method: &str, task_id: Option<&str>, id: Value, field: Field, caps: Caps) -> Value {
    let mut params = Map::new();
    if let Some(task_id) = task_id {
        params.insert("taskId".into(), json!(task_id));
    }
    request(method, Some(id), attach(params, field, caps))
}

pub fn notification(method: &str, field: Field, caps: Caps) -> Value {
    request(method, None, attach(Map::new(), field, caps))
}

pub fn call(method: &str, params: &Value, id: Option<Value>, field: Field, caps: Caps) -> Value {
    let map = params.as_object().cloned().unwrap_or_default();
    request(method, id, attach(map, field, caps))
}

#[cfg(feature = "task")]
pub fn sampling_capabilities() -> Value {
    json!({"sampling": {}})
}

fn request(method: &str, id: Option<Value>, params: Option<Value>) -> Value {
    let mut body = Map::new();
    body.insert("jsonrpc".into(), json!("2.0"));
    body.insert("method".into(), json!(method));
    if let Some(id) = id {
        body.insert("id".into(), id);
    }
    if let Some(params) = params {
        body.insert("params".into(), params);
    }
    Value::Object(body)
}

fn attach(mut params: Map<String, Value>, field: Field, caps: Caps) -> Option<Value> {
    let mut meta = Map::new();
    if let Field::Value(version) = field {
        meta.insert(PROTO.into(), json!(version));
    }
    match caps {
        Caps::Absent => {}
        Caps::Empty => {
            meta.insert(CAPS_KEY.into(), json!({}));
        }
        Caps::Sampling => {
            meta.insert(CAPS_KEY.into(), json!({"sampling": {}}));
        }
    }
    if !meta.is_empty() {
        params.insert("_meta".into(), Value::Object(meta));
    }
    if params.is_empty() {
        None
    } else {
        Some(Value::Object(params))
    }
}
