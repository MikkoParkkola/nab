//! Revision `2026-07-28` for one process.
//!
//! One classifier reads the selecting field. One dispatcher writes the
//! JSON-RPC bytes. The SDK hook only carries the answer to the wire.
//! Design event, recorded here because the ratified design file stays frozen:
//! a typed `ServerMessage` drops `resultType` and `cacheScope`, so every 2026
//! result is raw JSON. A malformed params object of a served method is
//! `-32600`. An unknown tool name stays a tool error.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rust_mcp_sdk::mcp_http::{Answer, Ingress, IngressHook, install_ingress};
use rust_mcp_sdk::schema::schema_utils::SdkError;
use rust_mcp_sdk::schema::{
    CallToolRequestParams, CallToolResult, CompleteRequestParams, GetPromptRequestParams,
    ListPromptsResult, ListResourcesResult, ListToolsResult, ProtocolVersion,
    ReadResourceRequestParams, RpcError,
};
use serde_json::{Value, json};

use nab::watch::WatchManager;

use crate::ask::{self, Ask};
use crate::completion;

const PROTO: &str = "io.modelcontextprotocol/protocolVersion";
const CAPS: &str = "io.modelcontextprotocol/clientCapabilities";
const V2026: &str = "2026-07-28";
const V2025: &str = "2025-11-25";
const HEADER: &str = "mcp-protocol-version";

pub(crate) fn install(watch: Arc<WatchManager>) {
    install_ingress(Arc::new(NabHook { watch }));
}

struct NabHook {
    watch: Arc<WatchManager>,
}

impl IngressHook for NabHook {
    fn on_ingress<'a>(
        &'a self,
        ingress: Ingress<'a>,
    ) -> Pin<Box<dyn Future<Output = Answer> + Send + 'a>> {
        Box::pin(self.answer(ingress))
    }
}

impl NabHook {
    async fn answer(&self, ingress: Ingress<'_>) -> Answer {
        // Nab's SDK dependency always enables `hyper-server`, so `Ingress::Http`
        // is present in this crate. The variant is cfg-gated inside the SDK.
        match ingress {
            Ingress::Http {
                method,
                headers,
                body,
            } => self.http(method, headers, body).await,
            Ingress::Stdio {
                message,
                initialized,
            } => {
                let Ok(value) = serde_json::to_value(message) else {
                    return Answer::Continue;
                };
                self.stdio(&value, initialized).await
            }
        }
    }

    async fn http(&self, method: &http::Method, headers: &http::HeaderMap, body: &str) -> Answer {
        if *method == http::Method::GET || *method == http::Method::DELETE {
            return if header_text(headers) == Some(V2026) {
                http_400()
            } else {
                Answer::Continue
            };
        }
        if *method != http::Method::POST {
            return Answer::Continue;
        }
        let message = serde_json::from_str(body).unwrap_or(Value::Null);
        let header = header_text(headers);
        let name = method_name(&message);
        let field = selecting_field(&message);
        let init_open = initialize_open(name, header);

        if let (Some(field_text), Some(header_text)) = (field.as_deref(), header)
            && field_text != header_text
            && !init_open
        {
            return http_400();
        }
        if let Some(field_text) = field.as_deref()
            && field_text != V2026
            && field_text != V2025
            && header_accepted(header)
            && !init_open
        {
            return http_400();
        }
        if name == Some("initialize") {
            return Answer::Continue;
        }
        if field.as_deref() == Some(V2026) && header == Some(V2026) {
            return if request_id(&message).is_some() {
                Answer::JsonRpc(self.dispatch(&message).await)
            } else {
                Answer::Http {
                    status: 202,
                    body: Vec::new(),
                }
            };
        }
        if field.as_deref() == Some(V2026) && header.is_none() {
            return http_400();
        }
        if field.is_none() && header == Some(V2026) {
            return http_400();
        }
        Answer::Continue
    }

    async fn stdio(&self, message: &Value, initialized: bool) -> Answer {
        let name = method_name(message);
        if name == Some("initialize") || name.is_none() {
            // A response or an error has no method. Those stay on the 0.9 path.
            return Answer::Continue;
        }
        match selecting_field(message).as_deref() {
            Some(V2026) => {
                if request_id(message).is_some() {
                    Answer::JsonRpc(self.dispatch(message).await)
                } else {
                    Answer::Silent
                }
            }
            None | Some(V2025) => Answer::Continue,
            Some(_) => match request_id(message) {
                Some(id) if initialized => Answer::JsonRpc(error_bytes(&id)),
                _ => Answer::Silent,
            },
        }
    }

    async fn dispatch(&self, message: &Value) -> Vec<u8> {
        let id = request_id(message).unwrap_or(Value::Null);
        let Some(name) = method_name(message) else {
            return error_bytes(&id);
        };
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match name {
            "server/discover" => ok_bytes(&id, &stamp(discover(), Some("public"))),
            "tools/list" => ok_bytes(&id, &stamp(Self::tools(), Some("public"))),
            "prompts/list" => ok_bytes(&id, &stamp(Self::prompts(), Some("public"))),
            "resources/list" => ok_bytes(&id, &stamp(self.resources().await, Some("private"))),
            "tools/call" => self.call(&id, &params, message).await,
            "prompts/get" => Self::prompt(&id, &params),
            "resources/read" => self.read(&id, &params).await,
            "completion/complete" => Self::complete(&id, &params),
            // logging/setLevel, subscribe, unsubscribe, ping, tasks/*, and
            // every other method, including resources/templates/list.
            _ => error_bytes(&id),
        }
    }

    fn tools() -> Value {
        let listed = ListToolsResult {
            meta: None,
            next_cursor: None,
            tools: crate::prepared_tools(false),
        };
        json_value(&listed)
    }

    fn prompts() -> Value {
        let listed = ListPromptsResult {
            meta: None,
            next_cursor: None,
            prompts: crate::all_prompts(),
        };
        json_value(&listed)
    }

    async fn resources(&self) -> Value {
        let listed = ListResourcesResult {
            meta: None,
            next_cursor: None,
            resources: crate::all_resources(&self.watch).await,
        };
        json_value(&listed)
    }

    async fn call(&self, id: &Value, params: &Value, message: &Value) -> Vec<u8> {
        let Ok(parsed) = serde_json::from_value::<CallToolRequestParams>(params.clone()) else {
            return error_bytes(id);
        };
        let sampling = sampling_advertised(message);
        let outcome = ask::scope(
            Ask::Refuse { sampling },
            crate::run_named_tool(parsed, None),
        )
        .await;
        match outcome {
            Ok(result) => ok_bytes(id, &stamp(json_value(&result), None)),
            Err(crate::ToolCallFailure::Unknown(error) | crate::ToolCallFailure::Tool(error)) => {
                let result = CallToolResult::from(error);
                ok_bytes(id, &stamp(json_value(&result), None))
            }
            Err(crate::ToolCallFailure::Wait(_) | crate::ToolCallFailure::Malformed(_)) => {
                error_bytes(id)
            }
        }
    }

    fn prompt(id: &Value, params: &Value) -> Vec<u8> {
        let Ok(parsed) = serde_json::from_value::<GetPromptRequestParams>(params.clone()) else {
            return error_bytes(id);
        };
        let name = parsed.name.clone();
        let args = parsed.arguments.unwrap_or_default();
        match crate::build_prompt_result(&name, &args) {
            Some(result) => ok_bytes(id, &stamp(json_value(&result), None)),
            None => named_error(id, format!("Unknown prompt: '{name}'")),
        }
    }

    async fn read(&self, id: &Value, params: &Value) -> Vec<u8> {
        let Ok(parsed) = serde_json::from_value::<ReadResourceRequestParams>(params.clone()) else {
            return error_bytes(id);
        };
        let scope = if parsed.uri.starts_with("nab://watch/") {
            "private"
        } else {
            "public"
        };
        match crate::read_resource_result(&self.watch, &parsed.uri).await {
            Ok(result) => ok_bytes(id, &stamp(json_value(&result), Some(scope))),
            Err(error) => named_error(id, error.message),
        }
    }

    fn complete(id: &Value, params: &Value) -> Vec<u8> {
        let Ok(parsed) = serde_json::from_value::<CompleteRequestParams>(params.clone()) else {
            return error_bytes(id);
        };
        let result = completion::handle_complete(&parsed);
        ok_bytes(id, &stamp(json_value(&result), None))
    }
}

fn discover() -> Value {
    json!({
        "supportedVersions": [V2026, V2025],
        "capabilities": {
            "tools": {},
            "prompts": {},
            "resources": {},
            "completions": {}
        }
    })
}

/// `resultType` on every success. Cache fields only where the schema requires them.
fn stamp(mut value: Value, cache: Option<&str>) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.insert("resultType".into(), json!("complete"));
        if let Some(scope) = cache {
            object.insert("cacheScope".into(), json!(scope));
            object.insert("ttlMs".into(), json!(0));
        }
        object.retain(|_, item| !item.is_null());
    }
    value
}

fn json_value(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn ok_bytes(id: &Value, result: &Value) -> Vec<u8> {
    encode(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn error_bytes(id: &Value) -> Vec<u8> {
    let error = RpcError::invalid_request();
    encode(&json!({"jsonrpc": "2.0", "id": id, "error": error}))
}

fn named_error(id: &Value, message: String) -> Vec<u8> {
    let error = RpcError::method_not_found().with_message(message);
    encode(&json!({"jsonrpc": "2.0", "id": id, "error": error}))
}

fn encode(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| {
        br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}"#
            .to_vec()
    })
}

fn http_400() -> Answer {
    let error = SdkError::bad_request();
    Answer::Http {
        status: 400,
        body: serde_json::to_vec(&error).unwrap_or_default(),
    }
}

fn header_text(headers: &http::HeaderMap) -> Option<&str> {
    let text = headers.get(HEADER)?.to_str().ok()?;
    if text.is_empty() { None } else { Some(text) }
}

fn header_accepted(header: Option<&str>) -> bool {
    match header {
        None => true,
        Some(text) => ProtocolVersion::try_from(text).is_ok(),
    }
}

fn initialize_open(method: Option<&str>, header: Option<&str>) -> bool {
    method == Some("initialize") && matches!(header, None | Some(V2025 | V2026))
}

fn method_name(message: &Value) -> Option<&str> {
    message.get("method").and_then(Value::as_str)
}

fn request_id(message: &Value) -> Option<Value> {
    match message.get("id") {
        None | Some(Value::Null) => None,
        Some(id) => Some(id.clone()),
    }
}

/// The key contains a slash, so this is a map lookup, not a JSON pointer.
fn selecting_field(message: &Value) -> Option<String> {
    let raw = message.get("params")?.get("_meta")?.get(PROTO)?;
    Some(match raw {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    })
}

fn sampling_advertised(message: &Value) -> bool {
    message
        .get("params")
        .and_then(|params| params.get("_meta"))
        .and_then(|meta| meta.get(CAPS))
        .and_then(|caps| caps.get("sampling"))
        .is_some()
}
