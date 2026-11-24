//! Discover, lists, reads, refused methods, and the 2025 guards.
//!
//! TP-H26 is the same discover body as TP-D1. TP-N1 calls the HTTP and stdio
//! notification cases one after the other. A child is dropped before the next starts.

use serde_json::{Value, json};

use crate::check::{self, Check};
use crate::harness::{Answer, StdioChild};
use crate::http::{Accept, HttpChild, Observed, Proto};
use crate::listen::DualListener;
use crate::wire::{self, Caps, Field};

fn http_envelope(message: &Value) -> Result<Value, String> {
    let mut child = HttpChild::start();
    let observed = child.post(
        message,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::TWO,
    );
    Ok(check::rpc_success(&observed)?.clone())
}

fn stdio_envelope(message: &Value) -> Result<Value, String> {
    let mut child = StdioChild::start();
    let exchange = child.exchange(message, wire::TWO, Answer::Record);
    Ok(check::stdio_success(&exchange)?.clone())
}

fn rpc_both(message: &Value, examine: fn(&Value) -> Check) -> Check {
    let http = http_envelope(message).and_then(|body| examine(&body));
    let stdio = stdio_envelope(message).and_then(|body| examine(&body));
    check::both(http, stdio)
}

fn post_2025(child: &mut HttpChild, session: &str, message: &Value) -> Observed {
    child.post(
        message,
        Proto::Value(wire::V2025),
        Some(session),
        Accept::JsonAndSse,
        wire::TWO,
    )
}

fn init_stdio() -> Result<StdioChild, String> {
    let mut child = StdioChild::start();
    let exchange = child.exchange(
        &wire::initialize(wire::V2025, Field::Absent, json!({})),
        wire::TWO,
        Answer::Record,
    );
    check::stdio_success(&exchange)?;
    Ok(child)
}

fn discover_examine(body: &Value) -> Check {
    check::id_echo(body, &wire::id_num())?;
    check::assert_discover(&body["result"])
}

fn discover_2026(caps: Caps) -> Check {
    let message = wire::discover(wire::id_num(), Field::Value(wire::V2026), caps);
    rpc_both(&message, discover_examine)
}

#[test]
fn tp_d1() {
    check::ok(discover_2026(Caps::Empty));
}

#[test]
fn tp_h26() {
    check::ok(discover_2026(Caps::Empty));
}

#[test]
fn tp_d2() {
    check::ok(discover_2026(Caps::Absent));
}

fn list_2026(body: &Value) -> Check {
    check::id_echo(body, &wire::id_num())?;
    check::assert_list_2026(&body["result"])
}

#[cfg(not(feature = "task"))]
#[test]
fn tp_l1() {
    check::ok(l1());
}

#[cfg(not(feature = "task"))]
fn l1() -> Check {
    let message = wire::tools_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty);
    rpc_both(&message, list_2026)
}

#[test]
fn tp_l2() {
    check::ok(l2());
}

fn l2() -> Check {
    let base = check::both(l2_http(), l2_stdio());
    #[cfg(feature = "task")]
    {
        check::join([base, l2_2026()])
    }
    #[cfg(not(feature = "task"))]
    {
        base
    }
}

#[cfg(feature = "task")]
fn l2_2026() -> Check {
    let message = wire::tools_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty);
    rpc_both(&message, list_2026)
}

fn l2_http() -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let observed = post_2025(
        &mut child,
        &session,
        &wire::tools_list(wire::id_num(), Field::Absent, Caps::Absent),
    );
    let body = check::rpc_success(&observed)?;
    check::assert_list_2025(&body["result"])
}

fn l2_stdio() -> Check {
    let mut child = init_stdio()?;
    let exchange = child.exchange(
        &wire::tools_list(wire::id_num(), Field::Absent, Caps::Absent),
        wire::TWO,
        Answer::Record,
    );
    let body = check::stdio_success(&exchange)?;
    check::assert_list_2025(&body["result"])
}

#[test]
fn tp_p1() {
    check::ok(p1());
}

fn p1() -> Check {
    rpc_both(
        &wire::prompts_get(wire::id_num(), Field::Value(wire::V2026), Caps::Empty),
        p1_examine,
    )
}

fn p1_examine(body: &Value) -> Check {
    let result = &body["result"];
    check::want_complete(result)?;
    let text = check::joined_text(result, "messages");
    for needle in [wire::PAGE, "the-title-line", "Use the `fetch` tool"] {
        if !text.contains(needle) {
            return Err(format!("text missing {needle:?}"));
        }
    }
    check::validate_pinned("GetPromptResult", result)
}

#[test]
fn tp_p2() {
    check::ok(p2());
}

fn p2() -> Check {
    check::join([p2_2026(), p2_2025()])
}

fn p2_2026() -> Check {
    rpc_both(
        &wire::prompts_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty),
        p2_examine,
    )
}

fn p2_examine(body: &Value) -> Check {
    let result = &body["result"];
    check::list_meta_2026(result, "public")?;
    check::exact_names(result, "prompts", "name", wire::PROMPTS)?;
    check::validate_pinned("ListPromptsResult", result)
}

fn p2_2025() -> Check {
    check::both(p2_2025_http(), p2_2025_stdio())
}

fn p2_2025_http() -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let observed = post_2025(
        &mut child,
        &session,
        &wire::prompts_list(wire::id_num(), Field::Absent, Caps::Absent),
    );
    let body = check::rpc_success(&observed)?;
    check::exact_names(&body["result"], "prompts", "name", wire::PROMPTS)
}

fn p2_2025_stdio() -> Check {
    let mut child = init_stdio()?;
    let exchange = child.exchange(
        &wire::prompts_list(wire::id_num(), Field::Absent, Caps::Absent),
        wire::TWO,
        Answer::Record,
    );
    let body = check::stdio_success(&exchange)?;
    check::exact_names(&body["result"], "prompts", "name", wire::PROMPTS)
}

#[test]
fn tp_r1() {
    check::ok(r1());
}

fn r1() -> Check {
    rpc_both(
        &wire::resources_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty),
        r1_examine,
    )
}

fn r1_examine(body: &Value) -> Check {
    let result = &body["result"];
    check::list_meta_2026(result, "private")?;
    check::exact_names(
        result,
        "resources",
        "uri",
        &["nab://guide/quickstart", "nab://status"],
    )?;
    check::validate_pinned("ListResourcesResult", result)
}

#[test]
fn tp_r2() {
    check::ok(r2());
}

fn r2() -> Check {
    let message = wire::resource(
        "resources/read",
        "nab://guide/quickstart",
        wire::id_num(),
        Field::Value(wire::V2026),
        Caps::Empty,
    );
    rpc_both(&message, r2_examine)
}

fn r2_examine(body: &Value) -> Check {
    let result = &body["result"];
    check::list_meta_2026(result, "public")?;
    let text = check::joined_text(result, "contents");
    if !text.contains("# nab Quickstart Guide") {
        return Err(format!("heading absent: {text}"));
    }
    check::validate_pinned("ReadResourceResult", result)
}

#[test]
fn tp_r3() {
    check::ok(r3());
}

fn r3() -> Check {
    check::both(r3_http(), r3_stdio())
}

fn r3_result(result: &Value) -> Check {
    check::want_absent_key(result, "cacheScope")?;
    let names = check::names_of(result, "resources", "uri")?;
    for uri in ["nab://guide/quickstart", "nab://status"] {
        if !names.contains(uri) {
            return Err(format!("missing {uri}"));
        }
    }
    Ok(())
}

fn r3_http() -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let observed = post_2025(
        &mut child,
        &session,
        &wire::resources_list(wire::id_num(), Field::Absent, Caps::Absent),
    );
    let body = check::rpc_success(&observed)?;
    r3_result(&body["result"])
}

fn r3_stdio() -> Check {
    let mut child = init_stdio()?;
    let exchange = child.exchange(
        &wire::resources_list(wire::id_num(), Field::Absent, Caps::Absent),
        wire::TWO,
        Answer::Record,
    );
    let body = check::stdio_success(&exchange)?;
    r3_result(&body["result"])
}

#[test]
fn tp_r4() {
    check::ok(r4());
}

fn r4() -> Check {
    let listener = DualListener::bind_loopback(wire::BATCH_BODY)?;
    check::both(r4_http(&listener), r4_stdio(&listener))
}

fn r4_http(listener: &DualListener) -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let id = create_http(&mut child, &session, &listener.url())?;
    let list = child.post(
        &wire::resources_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty),
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::TWO,
    );
    let watch = read_observed(&mut child, &format!("nab://watch/{id}"));
    let guide = read_observed(&mut child, "nab://guide/quickstart");
    check::join([
        check::labeled("list", list_private(&list, &id)),
        check::labeled("watch", read_scope(&watch, "private")),
        check::labeled("guide", read_scope(&guide, "public")),
    ])
}

fn r4_stdio(listener: &DualListener) -> Check {
    let mut child = init_stdio()?;
    let id = create_stdio(&mut child, &listener.url())?;
    let list = child.exchange(
        &wire::resources_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty),
        wire::TWO,
        Answer::Record,
    );
    let watch = read_exchange(&mut child, &format!("nab://watch/{id}"));
    let guide = read_exchange(&mut child, "nab://guide/quickstart");
    check::join([
        check::labeled("list", list_private_stdio(&list, &id)),
        check::labeled("watch", read_scope_stdio(&watch, "private")),
        check::labeled("guide", read_scope_stdio(&guide, "public")),
    ])
}

fn create_http(child: &mut HttpChild, session: &str, url: &str) -> Result<String, String> {
    let message = wire::tool_call(
        "watch_create",
        json!({"url": url, "interval": "1h"}),
        wire::id_num(),
        Field::Absent,
        Caps::Absent,
        None,
    );
    let observed = child.post(
        &message,
        Proto::Value(wire::V2025),
        Some(session),
        Accept::JsonAndSse,
        wire::LISTENER,
    );
    let body = check::rpc_success(&observed)?;
    check::watch_id(&check::tool_text(&body["result"]))
}

fn create_stdio(child: &mut StdioChild, url: &str) -> Result<String, String> {
    let message = wire::tool_call(
        "watch_create",
        json!({"url": url, "interval": "1h"}),
        wire::id_num(),
        Field::Absent,
        Caps::Absent,
        None,
    );
    let exchange = child.exchange(&message, wire::LISTENER, Answer::Record);
    let body = check::stdio_success(&exchange)?;
    check::watch_id(&check::tool_text(&body["result"]))
}

fn read_observed(child: &mut HttpChild, uri: &str) -> Observed {
    let message = wire::resource(
        "resources/read",
        uri,
        wire::id_num(),
        Field::Value(wire::V2026),
        Caps::Empty,
    );
    child.post(
        &message,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::TWO,
    )
}

fn read_exchange(child: &mut StdioChild, uri: &str) -> crate::harness::Exchange {
    let message = wire::resource(
        "resources/read",
        uri,
        wire::id_num(),
        Field::Value(wire::V2026),
        Caps::Empty,
    );
    child.exchange(&message, wire::TWO, Answer::Record)
}

fn list_private(observed: &Observed, id: &str) -> Check {
    let body = check::rpc_success(observed)?;
    resource_list(&body["result"], id)
}

fn list_private_stdio(exchange: &crate::harness::Exchange, id: &str) -> Check {
    let body = check::stdio_success(exchange)?;
    resource_list(&body["result"], id)
}

fn resource_list(result: &Value, id: &str) -> Check {
    check::list_meta_2026(result, "private")?;
    let uri = format!("nab://watch/{id}");
    let names = check::names_of(result, "resources", "uri")?;
    if !names.contains(&uri) {
        return Err(format!("missing {uri}"));
    }
    check::validate_pinned("ListResourcesResult", result)
}

fn read_scope(observed: &Observed, cache: &str) -> Check {
    let body = check::rpc_success(observed)?;
    check::list_meta_2026(&body["result"], cache)?;
    check::validate_pinned("ReadResourceResult", &body["result"])
}

fn read_scope_stdio(exchange: &crate::harness::Exchange, cache: &str) -> Check {
    let body = check::stdio_success(exchange)?;
    check::list_meta_2026(&body["result"], cache)?;
    check::validate_pinned("ReadResourceResult", &body["result"])
}

#[test]
fn tp_k1() {
    check::ok(k1());
}

fn k1() -> Check {
    rpc_both(
        &wire::completion(wire::id_num(), Field::Value(wire::V2026), Caps::Empty),
        k1_examine,
    )
}

fn k1_examine(body: &Value) -> Check {
    let result = &body["result"];
    check::want_complete(result)?;
    let values = result["completion"]["values"]
        .as_array()
        .ok_or("no completion values")?;
    for needle in ["brave", "chrome"] {
        if !values.iter().any(|value| value.as_str() == Some(needle)) {
            return Err(format!("missing {needle}"));
        }
    }
    check::validate_pinned("CompleteResult", result)
}

#[test]
fn tp_w1() {
    check::ok(w1());
}

fn w1() -> Check {
    check::join([w1_2026(), w1_2025()])
}

fn w1_2026() -> Check {
    let message = wire::tool_call(
        "watch_list",
        json!({}),
        wire::id_num(),
        Field::Value(wire::V2026),
        Caps::Empty,
        None,
    );
    rpc_both(&message, w1_2026_examine)
}

fn w1_2026_examine(body: &Value) -> Check {
    let result = &body["result"];
    check::want_complete(result)?;
    exact_watch(result)?;
    check::validate_pinned("CallToolResult", result)
}

fn w1_2025() -> Check {
    check::both(w1_2025_http(), w1_2025_stdio())
}

fn w1_2025_http() -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let message = wire::tool_call(
        "watch_list",
        json!({}),
        wire::id_num(),
        Field::Absent,
        Caps::Absent,
        None,
    );
    let observed = post_2025(&mut child, &session, &message);
    let body = check::rpc_success(&observed)?;
    w1_2025_result(&body["result"])
}

fn w1_2025_stdio() -> Check {
    let mut child = init_stdio()?;
    let message = wire::tool_call(
        "watch_list",
        json!({}),
        wire::id_num(),
        Field::Absent,
        Caps::Absent,
        None,
    );
    let exchange = child.exchange(&message, wire::TWO, Answer::Record);
    let body = check::stdio_success(&exchange)?;
    w1_2025_result(&body["result"])
}

fn w1_2025_result(result: &Value) -> Check {
    check::want_absent_key(result, "resultType")?;
    exact_watch(result)
}

fn exact_watch(result: &Value) -> Check {
    let text = check::tool_text(result);
    if text == wire::WATCH_EMPTY {
        Ok(())
    } else {
        Err(format!("text {text:?}"))
    }
}

#[test]
fn tp_e1() {
    check::ok(e1());
}

fn e1() -> Check {
    check::join([e1_methods(), e1_log()])
}

fn e1_methods() -> Check {
    let http = e1_http();
    let stdio = e1_stdio();
    check::both(http, stdio)
}

fn e1_messages() -> Vec<(&'static str, Value)> {
    let id = wire::id_num();
    let field = Field::Value(wire::V2026);
    vec![
        (
            "logging/setLevel",
            wire::set_level("notice", id.clone(), field, Caps::Empty),
        ),
        (
            "resources/subscribe",
            wire::resource(
                "resources/subscribe",
                "nab://watch/reg-3",
                id.clone(),
                field,
                Caps::Empty,
            ),
        ),
        (
            "resources/unsubscribe",
            wire::resource(
                "resources/unsubscribe",
                "nab://watch/reg-3",
                id.clone(),
                field,
                Caps::Empty,
            ),
        ),
        ("ping", wire::ping(id.clone(), field, Caps::Empty)),
        (
            "tasks/list",
            wire::tasks("tasks/list", None, id.clone(), field, Caps::Empty),
        ),
        (
            "tasks/get",
            wire::tasks("tasks/get", Some("t-1"), id.clone(), field, Caps::Empty),
        ),
        (
            "tasks/cancel",
            wire::tasks("tasks/cancel", Some("t-1"), id.clone(), field, Caps::Empty),
        ),
        (
            "tasks/result",
            wire::tasks("tasks/result", Some("t-1"), id.clone(), field, Caps::Empty),
        ),
        (
            "resources/templates/list",
            wire::call(
                "resources/templates/list",
                &json!({}),
                Some(id),
                field,
                Caps::Empty,
            ),
        ),
    ]
}

fn e1_http() -> Check {
    let mut child = HttpChild::start();
    let mut checks = Vec::new();
    for (name, message) in e1_messages() {
        let observed = child.post(
            &message,
            Proto::Value(wire::V2026),
            None,
            Accept::JsonAndSse,
            wire::TWO,
        );
        checks.push(check::labeled(
            name,
            check::http_refusal(&observed, &wire::id_num()),
        ));
    }
    check::join(checks)
}

fn e1_stdio() -> Check {
    let mut checks = Vec::new();
    for (name, message) in e1_messages() {
        let mut child = StdioChild::start();
        let exchange = child.exchange(&message, wire::TWO, Answer::Record);
        checks.push(check::labeled(
            name,
            check::stdio_refusal(&exchange, &wire::id_num()),
        ));
    }
    check::join(checks)
}

fn e1_log() -> Check {
    let mut child = init_stdio()?;
    notice_control(&mut child, true)?;
    let message = wire::set_level(
        "notice",
        wire::id_num(),
        Field::Value(wire::V2026),
        Caps::Empty,
    );
    let exchange = child.exchange(&message, wire::TWO, Answer::Record);
    check::stdio_refusal(&exchange, &wire::id_num())?;
    let early = check::count_method(&exchange.before, "notifications/message");
    if early != 0 {
        return Err(format!("notifications/message before the refusal: {early}"));
    }
    let drained = child.drain(wire::TWO);
    if check::count_method(&drained, "notifications/message") == 0 {
        Ok(())
    } else {
        Err("notifications/message arrived after the refusal".into())
    }
}

fn notice_control(child: &mut StdioChild, premise: bool) -> Check {
    let message = wire::set_level("notice", wire::id_num(), Field::Absent, Caps::Absent);
    let exchange = child.exchange(&message, wire::TWO, Answer::Record);
    check::stdio_success(&exchange)?;
    let mut count = check::count_method(&exchange.before, "notifications/message");
    let drained = child.drain(wire::TWO);
    count += check::count_method(&drained, "notifications/message");
    if count == 0 {
        return Err(if premise {
            "premise: notifications/message did not arrive".into()
        } else {
            "notice did not notify".into()
        });
    }
    if count == 1 {
        Ok(())
    } else {
        Err(format!("notifications/message count {count}"))
    }
}

#[test]
fn tp_e2() {
    check::ok(e2());
}

fn e2() -> Check {
    let message = wire::call(
        "server/not-a-nab-method",
        &json!({}),
        Some(wire::id_num()),
        Field::Value(wire::V2026),
        Caps::Empty,
    );
    let http = {
        let mut child = HttpChild::start();
        let observed = child.post(
            &message,
            Proto::Value(wire::V2026),
            None,
            Accept::JsonAndSse,
            wire::TWO,
        );
        check::http_refusal(&observed, &wire::id_num())
    };
    let stdio = {
        let mut child = StdioChild::start();
        let exchange = child.exchange(&message, wire::TWO, Answer::Record);
        check::stdio_refusal(&exchange, &wire::id_num())
    };
    check::both(http, stdio)
}

#[test]
fn tp_n1() {
    check::ok(n1());
}

fn n1() -> Check {
    let http = crate::http_cases::h9();
    let stdio = crate::stdio_cases::s7();
    check::join([http, stdio])
}

#[test]
fn tp_log_1() {
    check::ok(log_1());
}

fn log_1() -> Check {
    let mut child = init_stdio()?;
    notice_control(&mut child, false)?;
    let message = wire::set_level("warning", wire::id_num(), Field::Absent, Caps::Absent);
    let exchange = child.exchange(&message, wire::TWO, Answer::Record);
    check::stdio_success(&exchange)?;
    let drained = child.drain(wire::TWO);
    let count = check::count_method(&exchange.before, "notifications/message")
        + check::count_method(&drained, "notifications/message");
    if count == 0 {
        Ok(())
    } else {
        Err(format!("warning notified {count}"))
    }
}

#[test]
fn tp_log_2() {
    check::ok(log_2());
}

fn log_2() -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let message = wire::set_level("notice", wire::id_num(), Field::Absent, Caps::Absent);
    let (observed, events) = child.post_drain(
        &message,
        Proto::Value(wire::V2025),
        Some(&session),
        Accept::JsonAndSse,
        wire::TWO,
    );
    check::rpc_success(&observed)?;
    if check::has_method(&events, "notifications/message") {
        Err("notifications/message in the POST drain".into())
    } else {
        Ok(())
    }
}

#[test]
fn tp_watch_2() {
    check::ok(watch_2());
}

fn watch_2() -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let message = wire::resource(
        "resources/subscribe",
        "nab://watch/reg-3",
        wire::id_num(),
        Field::Absent,
        Caps::Absent,
    );
    let (observed, events) = child.post_drain(
        &message,
        Proto::Value(wire::V2025),
        Some(&session),
        Accept::JsonAndSse,
        wire::TWO,
    );
    check::rpc_success(&observed)?;
    if check::has_method(&events, "notifications/resources/updated") {
        Err("notifications/resources/updated in the POST drain".into())
    } else {
        Ok(())
    }
}

#[test]
fn tp_reg_2() {
    check::ok(reg_2());
}

fn reg_2() -> Check {
    check::both(reg2_http(), reg2_stdio())
}

fn reg2_messages() -> Vec<Value> {
    let id = wire::id_num();
    vec![
        wire::tool_call(
            "watch_list",
            json!({}),
            id.clone(),
            Field::Absent,
            Caps::Absent,
            None,
        ),
        wire::prompts_get(id.clone(), Field::Absent, Caps::Absent),
        wire::resource(
            "resources/read",
            "nab://guide/quickstart",
            id.clone(),
            Field::Absent,
            Caps::Absent,
        ),
        wire::completion(id, Field::Absent, Caps::Absent),
    ]
}

fn reg2_http() -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let mut checks = Vec::new();
    for message in reg2_messages() {
        checks.push(check::rpc_success(&post_2025(&mut child, &session, &message)).map(|_| ()));
    }
    check::join(checks)
}

fn reg2_stdio() -> Check {
    let mut child = init_stdio()?;
    let mut checks = Vec::new();
    for message in reg2_messages() {
        let exchange = child.exchange(&message, wire::TWO, Answer::Record);
        checks.push(check::stdio_success(&exchange).map(|_| ()));
    }
    check::join(checks)
}

#[test]
fn tp_reg_3() {
    check::ok(reg_3());
}

fn reg_3() -> Check {
    check::both(reg3_http(), reg3_stdio())
}

fn reg3_messages() -> Vec<Value> {
    let id = wire::id_num();
    vec![
        wire::resource(
            "resources/subscribe",
            "nab://watch/reg-3",
            id.clone(),
            Field::Absent,
            Caps::Absent,
        ),
        wire::resource(
            "resources/unsubscribe",
            "nab://watch/reg-3",
            id,
            Field::Absent,
            Caps::Absent,
        ),
    ]
}

fn reg3_http() -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let mut checks = Vec::new();
    for message in reg3_messages() {
        let observed = post_2025(&mut child, &session, &message);
        checks.push(check::rpc_success(&observed).map(|_| ()));
    }
    check::join(checks)
}

fn reg3_stdio() -> Check {
    let mut child = init_stdio()?;
    let mut checks = Vec::new();
    for message in reg3_messages() {
        let exchange = child.exchange(&message, wire::TWO, Answer::Record);
        checks.push(check::stdio_success(&exchange).map(|_| ()));
    }
    check::join(checks)
}

#[test]
fn tp_reg_5() {
    check::ok(reg_5());
}

fn reg_5() -> Check {
    check::both(reg5_http(), reg5_stdio())
}

fn reg5_specs() -> Vec<(&'static str, Option<&'static str>)> {
    vec![
        ("tasks/list", None),
        ("tasks/get", Some("t-1")),
        ("tasks/cancel", Some("t-1")),
        ("tasks/result", Some("t-1")),
    ]
}

fn reg5_http() -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let mut checks = Vec::new();
    for (method, task_id) in reg5_specs() {
        let message = wire::tasks(method, task_id, wire::id_num(), Field::Absent, Caps::Absent);
        let observed = post_2025(&mut child, &session, &message);
        checks.push(check::labeled(method, jsonrpc_code(&observed, -32601)));
    }
    check::join(checks)
}

fn reg5_stdio() -> Check {
    let mut child = init_stdio()?;
    let mut checks = Vec::new();
    for (method, task_id) in reg5_specs() {
        let message = wire::tasks(method, task_id, wire::id_num(), Field::Absent, Caps::Absent);
        let exchange = child.exchange(&message, wire::TWO, Answer::Record);
        let body = exchange
            .response
            .as_ref()
            .ok_or_else(|| "zero bytes, wanted -32601".to_string())?;
        checks.push(check::labeled(
            method,
            check::jsonrpc_error(body, &wire::id_num(), -32601),
        ));
    }
    check::join(checks)
}

fn jsonrpc_code(observed: &Observed, code: i64) -> Check {
    match observed {
        Observed::Rpc {
            status: 200, body, ..
        } => check::jsonrpc_error(body, &wire::id_num(), code),
        other => Err(format!(
            "wanted JSON-RPC error {code}, {}",
            check::describe(other)
        )),
    }
}

#[test]
fn tp_reg_6() {
    check::ok(reg_6());
}

fn reg_6() -> Check {
    check::both(reg6_http(), reg6_stdio())
}

fn reg6_http() -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let observed = post_2025(
        &mut child,
        &session,
        &wire::ping(wire::id_num(), Field::Absent, Caps::Absent),
    );
    check::rpc_success(&observed).map(|_| ())
}

fn reg6_stdio() -> Check {
    let mut child = init_stdio()?;
    let exchange = child.exchange(
        &wire::ping(wire::id_num(), Field::Absent, Caps::Absent),
        wire::TWO,
        Answer::Record,
    );
    check::stdio_success(&exchange).map(|_| ())
}

#[test]
fn tp_reg_7() {
    check::ok(reg_7());
}

fn reg_7() -> Check {
    check::join([reg7_delete(), reg7_bad_header()])
}

fn reg7_delete() -> Check {
    let mut child = HttpChild::start();
    let session = crate::http_cases::open_2025(&mut child)?;
    let ping = wire::ping(wire::id_num(), Field::Absent, Caps::Absent);
    check::rpc_success(&post_2025(&mut child, &session, &ping))?;
    let deleted = child.delete(
        Proto::Value(wire::V2025),
        Some(&session),
        Accept::JsonAndSse,
        wire::TWO,
    );
    if check::status_of(&deleted) != 200 {
        return Err(format!(
            "DELETE status {}, wanted 200",
            check::status_of(&deleted)
        ));
    }
    let next = post_2025(&mut child, &session, &ping);
    check::sdk_message(&next, 404, -32016, "Session not found")
}

fn reg7_bad_header() -> Check {
    let mut child = HttpChild::start();
    let observed = child.post(
        &wire::initialize(wire::V2025, Field::Absent, json!({})),
        Proto::Value("not-a-version"),
        None,
        Accept::JsonAndSse,
        wire::TWO,
    );
    check::produce_400(&observed)?;
    if HttpChild::session_of(&observed).is_none() {
        Ok(())
    } else {
        Err("session id present".into())
    }
}
