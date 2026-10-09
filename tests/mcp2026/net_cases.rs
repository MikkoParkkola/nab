//! Network cases. `ubuntu-default` leaves `NAB_NET_TESTS` unset, and each case
//! returns before an assertion. Job `mcp-2026` sets the variable and runs them.
//!
//! TP-TASK-3 is stdio only. HTTP cannot complete a sampling request.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};

use serde_json::{Value, json};

use crate::check::{self, Check};
use crate::common::net_tests_enabled;
use crate::harness::{Answer, StdioChild};
use crate::http::{Accept, HttpChild, Observed, Proto};
use crate::listen::DualListener;
use crate::wire::{self, Caps, Field};

fn enabled() -> bool {
    net_tests_enabled()
}

fn idle() -> Check {
    // Job split: mcp-2026 sets NAB_NET_TESTS. ubuntu-default leaves it unset.
    Ok(())
}

fn routed_ip() -> Result<IpAddr, String> {
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|error| format!("premise: udp: {error}"))?;
    socket
        .connect("1.1.1.1:80")
        .map_err(|error| format!("premise: udp: {error}"))?;
    socket
        .local_addr()
        .map(|addr| addr.ip())
        .map_err(|error| format!("premise: udp: {error}"))
}

fn host_allowance(ip: IpAddr) -> Result<Option<String>, String> {
    if ip.is_loopback() {
        return Err("premise: only loopback is available".into());
    }
    if is_link_local(ip) {
        return Err("premise: routed address is link-local".into());
    }
    if relaxable(ip) {
        Ok(Some(ip.to_string()))
    } else {
        Ok(None)
    }
}

const fn is_cgn(ip: Ipv4Addr) -> bool {
    let [first, second, _, _] = ip.octets();
    first == 100 && (second & 0xc0) == 64
}

fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(value) => value.is_link_local(),
        IpAddr::V6(value) => value.is_unicast_link_local(),
    }
}

fn relaxable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(value) => value.is_private() || is_cgn(value),
        IpAddr::V6(value) => (value.octets()[0] & 0xfe) == 0xfc,
    }
}

fn fetch_listener() -> Result<(DualListener, Option<String>), String> {
    let ip = routed_ip()?;
    let allow = host_allowance(ip)?;
    let listener = DualListener::bind_ip(ip, wire::FETCH_BODY)?;
    Ok((listener, allow))
}

fn tool(name: &str, arguments: Value, field: Field, caps: Caps) -> Value {
    wire::tool_call(name, arguments, wire::id_num(), field, caps, None)
}

#[test]
fn tp_c4() {
    check::ok(c4());
}

fn c4() -> Check {
    if !enabled() {
        return idle();
    }
    let (listener, allow) = fetch_listener()?;
    let url = listener.url();
    let http = c4_http(&url, allow.as_deref());
    let stdio = c4_stdio(&url, allow.as_deref());
    check::both(http, stdio)
}

fn c4_http(url: &str, allow: Option<&str>) -> Check {
    let message = tool(
        "fetch",
        json!({"url": url}),
        Field::Value(wire::V2026),
        Caps::Empty,
    );
    let mut child = HttpChild::start_allow(allow);
    let observed = child.post(
        &message,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::LISTENER,
    );
    let result = check::http_tool(&observed)?.clone();
    c4_body(&result)?;
    schema_http(&mut child, &result, "fetch", Structured::Required)
}

fn c4_stdio(url: &str, allow: Option<&str>) -> Check {
    let message = tool(
        "fetch",
        json!({"url": url}),
        Field::Value(wire::V2026),
        Caps::Empty,
    );
    let mut child = StdioChild::start_allow(allow);
    let exchange = child.exchange(&message, wire::LISTENER, Answer::Record);
    let result = check::stdio_tool(&exchange)?.clone();
    c4_body(&result)?;
    schema_stdio(&mut child, &result, "fetch", Structured::Required)
}

fn c4_body(result: &Value) -> Check {
    if check::is_error(result) {
        return Err("isError is true".into());
    }
    check::text_has(result, wire::FETCH_BODY)?;
    check::validate_pinned("CallToolResult", result)
}

#[derive(Clone, Copy)]
enum Structured {
    Required,
    Optional,
}

fn schema_http(child: &mut HttpChild, result: &Value, tool: &str, structured: Structured) -> Check {
    if matches!(structured, Structured::Required) && result.get("structuredContent").is_none() {
        return Err("structuredContent absent".into());
    }
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

fn schema_stdio(
    child: &mut StdioChild,
    result: &Value,
    tool: &str,
    structured: Structured,
) -> Check {
    if matches!(structured, Structured::Required) && result.get("structuredContent").is_none() {
        return Err("structuredContent absent".into());
    }
    let list = child.exchange(
        &wire::tools_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty),
        wire::TWO,
        Answer::Record,
    );
    let listed = check::stdio_tool(&list)?;
    check::validate_output(listed, tool, result.get("structuredContent"))
}

#[test]
fn tp_c5() {
    check::ok(c5());
}

fn c5() -> Check {
    if !enabled() {
        return idle();
    }
    let (listener, allow) = fetch_listener()?;
    let url = listener.url();
    let message = tool(
        "fetch_batch",
        json!({"urls": [url.clone()]}),
        Field::Value(wire::V2026),
        Caps::Empty,
    );
    let http = c5_http(&message, &url, allow.as_deref());
    let stdio = c5_stdio(&message, &url, allow.as_deref());
    check::both(http, stdio)
}

fn c5_http(message: &Value, url: &str, allow: Option<&str>) -> Check {
    let mut child = HttpChild::start_allow(allow);
    let observed = child.post(
        message,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::LISTENER,
    );
    let result = check::http_tool(&observed)?;
    c5_body(result, url)?;
    schema_http(&mut child, result, "fetch_batch", Structured::Required)
}

fn c5_stdio(message: &Value, url: &str, allow: Option<&str>) -> Check {
    let mut child = StdioChild::start_allow(allow);
    let exchange = child.exchange(message, wire::LISTENER, Answer::Record);
    let result = check::stdio_tool(&exchange)?;
    c5_body(result, url)?;
    schema_stdio(&mut child, result, "fetch_batch", Structured::Required)
}

fn c5_body(result: &Value, url: &str) -> Check {
    check::text_has(result, wire::FETCH_BODY)?;
    if result.get("task").is_some() || result.get("taskId").is_some() {
        return Err(format!("task id present: {}", check::short(result)));
    }
    let item = &result["structuredContent"]["results"][0];
    check::exact_key_set(item, &["content", "status", "timing_ms", "url"])?;
    if item["status"].as_i64() != Some(200) {
        return Err(format!("status {:?}", item["status"]));
    }
    if item["url"].as_str() != Some(url) {
        return Err(format!("url {:?}", item["url"]));
    }
    check::validate_pinned("CallToolResult", result)
}

#[test]
fn tp_c6() {
    check::ok(c6());
}

fn c6() -> Check {
    if !enabled() {
        return idle();
    }
    let (listener, allow) = fetch_listener()?;
    let message = tool(
        "benchmark",
        json!({"urls": listener.url(), "iterations": 1}),
        Field::Value(wire::V2026),
        Caps::Empty,
    );
    let http = c6_http(&message, allow.as_deref());
    let stdio = c6_stdio(&message, allow.as_deref());
    check::both(http, stdio)
}

fn c6_http(message: &Value, allow: Option<&str>) -> Check {
    let mut child = HttpChild::start_allow(allow);
    let observed = child.post(
        message,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::LISTENER,
    );
    let result = check::http_tool(&observed)?;
    check::text_has(result, "Benchmarking")?;
    check::validate_pinned("CallToolResult", result)?;
    schema_http(&mut child, result, "benchmark", Structured::Optional)
}

fn c6_stdio(message: &Value, allow: Option<&str>) -> Check {
    let mut child = StdioChild::start_allow(allow);
    let exchange = child.exchange(message, wire::LISTENER, Answer::Record);
    let result = check::stdio_tool(&exchange)?;
    check::text_has(result, "Benchmarking")?;
    check::validate_pinned("CallToolResult", result)?;
    schema_stdio(&mut child, result, "benchmark", Structured::Optional)
}

#[test]
fn tp_c7() {
    check::ok(c7());
}

fn c7() -> Check {
    if !enabled() {
        return idle();
    }
    let message = tool(
        "validate",
        json!({}),
        Field::Value(wire::V2026),
        Caps::Empty,
    );
    let http = c7_http(&message);
    let stdio = c7_stdio(&message);
    check::both(http, stdio)
}

fn c7_http(message: &Value) -> Check {
    let mut child = HttpChild::start();
    let observed = child.post(
        message,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::REMOTE,
    );
    let result = check::http_tool(&observed)?;
    check::text_has(result, "MicroFetch Validation Suite")?;
    check::validate_pinned("CallToolResult", result)?;
    schema_http(&mut child, result, "validate", Structured::Optional)
}

fn c7_stdio(message: &Value) -> Check {
    let mut child = StdioChild::start();
    let exchange = child.exchange(message, wire::REMOTE, Answer::Record);
    let result = check::stdio_tool(&exchange)?;
    check::text_has(result, "MicroFetch Validation Suite")?;
    check::validate_pinned("CallToolResult", result)?;
    schema_stdio(&mut child, result, "validate", Structured::Optional)
}

#[test]
fn tp_watch_1() {
    check::ok(watch_1());
}

fn watch_1() -> Check {
    if !enabled() {
        return idle();
    }
    let listener = DualListener::bind_loopback(wire::BATCH_BODY)?;
    let mut child = StdioChild::start();
    let init = child.exchange(
        &wire::initialize(wire::V2025, Field::Absent, json!({})),
        wire::TWO,
        Answer::Record,
    );
    check::stdio_success(&init)?;
    let uri = watch_and_subscribe(&mut child, &listener.url())?;
    listener.set_body("nab-mcp-2026-watch-changed");
    if updated(&child.drain(wire::WATCH), &uri) {
        Ok(())
    } else {
        Err("premise: notifications/resources/updated did not arrive".into())
    }
}

fn watch_and_subscribe(child: &mut StdioChild, url: &str) -> Result<String, String> {
    let create = wire::tool_call(
        "watch_create",
        json!({"url": url, "interval": "1s"}),
        wire::id_num(),
        Field::Absent,
        Caps::Absent,
        None,
    );
    let exchange = child.exchange(&create, wire::LISTENER, Answer::Record);
    let body = check::stdio_success(&exchange)?;
    let id = check::watch_id(&check::tool_text(&body["result"]))?;
    let uri = format!("nab://watch/{id}");
    let subscribe = wire::resource(
        "resources/subscribe",
        &uri,
        wire::id_num(),
        Field::Absent,
        Caps::Absent,
    );
    let subscribed = child.exchange(&subscribe, wire::TWO, Answer::Record);
    check::stdio_success(&subscribed)?;
    Ok(uri)
}

fn updated(messages: &[Value], uri: &str) -> bool {
    messages.iter().any(|message| {
        message.get("method").and_then(Value::as_str) == Some("notifications/resources/updated")
            && message.pointer("/params/uri").and_then(Value::as_str) == Some(uri)
    })
}

#[test]
fn tp_e1_subscribe() {
    check::ok(e1_subscribe());
}

fn e1_subscribe() -> Check {
    if !enabled() {
        return idle();
    }
    let listener = DualListener::bind_loopback(wire::BATCH_BODY)?;
    let mut child = StdioChild::start();
    let init = child.exchange(
        &wire::initialize(wire::V2025, Field::Absent, json!({})),
        wire::TWO,
        Answer::Record,
    );
    check::stdio_success(&init)?;
    let uri = watch_and_subscribe(&mut child, &listener.url())?;
    positive_control(&mut child, &listener, &uri)?;
    quiet_after_unsubscribe(&mut child, &listener, &uri)?;
    refused_subscribe(&mut child, &listener, &uri)?;
    refused_unsubscribe_still_notifies(&mut child, &listener, &uri)?;
    final_quiet(&mut child, &listener, &uri)
}

fn positive_control(child: &mut StdioChild, listener: &DualListener, uri: &str) -> Check {
    listener.set_body("e1-control");
    if updated(&child.drain(wire::WATCH), uri) {
        Ok(())
    } else {
        Err("premise: notifications/resources/updated did not arrive".into())
    }
}

fn quiet_after_unsubscribe(child: &mut StdioChild, listener: &DualListener, uri: &str) -> Check {
    let message = wire::resource(
        "resources/unsubscribe",
        uri,
        wire::id_num(),
        Field::Absent,
        Caps::Absent,
    );
    let exchange = child.exchange(&message, wire::TWO, Answer::Record);
    check::stdio_success(&exchange)?;
    listener.set_body("e1-after-unsubscribe");
    if updated(&child.drain(wire::WATCH), uri) {
        Err("notification after unsubscribe".into())
    } else {
        Ok(())
    }
}

fn refused_subscribe(child: &mut StdioChild, listener: &DualListener, uri: &str) -> Check {
    let message = wire::resource(
        "resources/subscribe",
        uri,
        wire::id_num(),
        Field::Value(wire::V2026),
        Caps::Empty,
    );
    let exchange = child.exchange(&message, wire::TWO, Answer::Record);
    check::stdio_refusal(&exchange, &wire::id_num())?;
    listener.set_body("e1-after-2026-subscribe");
    if updated(&child.drain(wire::WATCH), uri) {
        Err("notification after 2026 subscribe".into())
    } else {
        Ok(())
    }
}

fn refused_unsubscribe_still_notifies(
    child: &mut StdioChild,
    listener: &DualListener,
    uri: &str,
) -> Check {
    let again = wire::resource(
        "resources/subscribe",
        uri,
        wire::id_num(),
        Field::Absent,
        Caps::Absent,
    );
    let subscribed = child.exchange(&again, wire::TWO, Answer::Record);
    check::stdio_success(&subscribed)?;
    let message = wire::resource(
        "resources/unsubscribe",
        uri,
        wire::id_num(),
        Field::Value(wire::V2026),
        Caps::Empty,
    );
    let exchange = child.exchange(&message, wire::TWO, Answer::Record);
    check::stdio_refusal(&exchange, &wire::id_num())?;
    listener.set_body("e1-2026-unsubscribe");
    if updated(&child.drain(wire::WATCH), uri) {
        Ok(())
    } else {
        Err("premise: notifications/resources/updated did not arrive".into())
    }
}

fn final_quiet(child: &mut StdioChild, listener: &DualListener, uri: &str) -> Check {
    let message = wire::resource(
        "resources/unsubscribe",
        uri,
        wire::id_num(),
        Field::Absent,
        Caps::Absent,
    );
    let exchange = child.exchange(&message, wire::TWO, Answer::Record);
    check::stdio_success(&exchange)?;
    listener.set_body("e1-final");
    if updated(&child.drain(wire::WATCH), uri) {
        Err("notification after the final unsubscribe".into())
    } else {
        Ok(())
    }
}

#[cfg(feature = "task")]
#[test]
fn tp_task_2() {
    check::ok(task_2());
}

#[cfg(feature = "task")]
fn task_2() -> Check {
    if !enabled() {
        return idle();
    }
    let (listener, allow) = fetch_listener()?;
    let url = listener.url();
    task_list(allow.as_deref())?;
    check::join([
        task_fresh(false, Caps::Empty, &url, allow.as_deref()),
        task_after_sampling(&url, allow.as_deref()),
        task_fresh(true, Caps::Sampling, &url, allow.as_deref()),
    ])
}

#[cfg(feature = "task")]
fn task_list(allow: Option<&str>) -> Check {
    let message = wire::tools_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty);
    let http = {
        let mut child = HttpChild::start_allow(allow);
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
        let mut child = StdioChild::start_allow(allow);
        let exchange = child.exchange(&message, wire::TWO, Answer::Record);
        let body = check::stdio_success(&exchange)?;
        check::assert_list_2026(&body["result"])
    };
    check::both(http, stdio)
}

#[cfg(feature = "task")]
fn task_call(autonomous: bool, url: &str, caps: Caps) -> Value {
    tool(
        "task",
        json!({"goal": "read the page", "url": url, "autonomous": autonomous}),
        Field::Value(wire::V2026),
        caps,
    )
}

#[cfg(feature = "task")]
fn task_fresh(autonomous: bool, caps: Caps, url: &str, allow: Option<&str>) -> Check {
    let message = task_call(autonomous, url, caps);
    let mut child = StdioChild::start_allow(allow);
    let exchange = child.exchange(&message, wire::LISTENER, Answer::Record);
    drop(child);
    if caps_is_sampling(caps) {
        refused_task(&exchange)
    } else {
        served_task(&exchange, url, allow)
    }
}

#[cfg(feature = "task")]
const fn caps_is_sampling(caps: Caps) -> bool {
    matches!(caps, Caps::Sampling)
}

#[cfg(feature = "task")]
fn task_after_sampling(url: &str, allow: Option<&str>) -> Check {
    let mut child = StdioChild::start_allow(allow);
    let init = wire::initialize(wire::V2025, Field::Absent, wire::sampling_capabilities());
    let started = child.exchange(&init, wire::TWO, Answer::Record);
    check::stdio_success(&started)?;
    let empty = child.exchange(
        &task_call(true, url, Caps::Empty),
        wire::LISTENER,
        Answer::Record,
    );
    let absent = child.exchange(
        &task_call(true, url, Caps::Absent),
        wire::LISTENER,
        Answer::Record,
    );
    drop(child);
    served_task(&empty, url, allow)?;
    served_task(&absent, url, allow)
}

#[cfg(feature = "task")]
fn served_task(exchange: &crate::harness::Exchange, url: &str, allow: Option<&str>) -> Check {
    if check::has_method(&exchange.before, "sampling/createMessage") {
        return Err("sampling/createMessage".into());
    }
    let body = check::stdio_success(exchange)?;
    let result = &body["result"];
    check::want_complete(result)?;
    if result.get("task").is_some() || result.get("taskId").is_some() {
        return Err("task id present".into());
    }
    check::validate_pinned("CallToolResult", result)?;
    if check::is_error(result) {
        return baseline_task(url, allow, result);
    }
    Ok(())
}

#[cfg(feature = "task")]
fn baseline_task(url: &str, allow: Option<&str>, result: &Value) -> Check {
    let mut child = StdioChild::start_allow(allow);
    let started = child.exchange(
        &wire::initialize(wire::V2025, Field::Absent, json!({})),
        wire::TWO,
        Answer::Record,
    );
    check::stdio_success(&started)?;
    let message = tool(
        "task",
        json!({"goal": "read the page", "url": url, "autonomous": false}),
        Field::Absent,
        Caps::Absent,
    );
    let exchange = child.exchange(&message, wire::LISTENER, Answer::Record);
    let body = check::stdio_success(&exchange)?;
    let baseline = &body["result"];
    if !check::is_error(baseline) {
        return Err(format!(
            "2026 tool error, 2025 returned a tool result\n2026: {}\n2025: {}",
            check::tool_text(result),
            check::tool_text(baseline)
        ));
    }
    if check::tool_text(baseline) != check::tool_text(result) {
        return Err(format!(
            "tool text {:?} vs 2025 {:?}",
            check::tool_text(result),
            check::tool_text(baseline)
        ));
    }
    Ok(())
}

#[cfg(feature = "task")]
fn refused_task(exchange: &crate::harness::Exchange) -> Check {
    if check::has_method(&exchange.before, "sampling/createMessage") {
        return Err("sampling/createMessage".into());
    }
    check::stdio_refusal(exchange, &wire::id_num())
}

#[cfg(feature = "task")]
#[test]
fn tp_task_3() {
    check::ok(task_3());
}

#[cfg(feature = "task")]
fn task_3() -> Check {
    if !enabled() {
        return idle();
    }
    // HTTP cannot post the sampling answer back onto a stream this client already closed.
    let (listener, allow) = fetch_listener()?;
    let mut child = StdioChild::start_allow(allow.as_deref());
    let init = wire::initialize(wire::V2025, Field::Absent, wire::sampling_capabilities());
    let started = child.exchange(&init, wire::TWO, Answer::Record);
    check::stdio_success(&started)?;
    let message = tool(
        "task",
        json!({"goal": "read the page", "url": listener.url(), "autonomous": true}),
        Field::Absent,
        Caps::Absent,
    );
    let exchange = child.exchange(&message, wire::LISTENER, Answer::Sample);
    if !check::has_method(&exchange.before, "sampling/createMessage") {
        return Err("sampling/createMessage absent".into());
    }
    let body = check::stdio_success(&exchange)?;
    check::want_absent_key(&body["result"], "resultType")
}

#[allow(dead_code)]
fn _observe_unused(observed: Observed) -> Observed {
    observed
}
