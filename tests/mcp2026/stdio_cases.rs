//! Stdio cases TP-S1 through TP-S12, plus the two order cases.
//! A 2026 initialize that is silent today is not followed by discover.

use serde_json::{Value, json};

use crate::check::{self, Check};
use crate::harness::{Answer, Exchange, StdioChild};
use crate::wire::{self, Caps, Field};

fn pipe() -> StdioChild {
    StdioChild::start()
}

fn exch(pipe: &mut StdioChild, message: &Value) -> Exchange {
    pipe.exchange(message, wire::TWO, Answer::Record)
}

fn init_2025(pipe: &mut StdioChild) -> Check {
    let exchange = exch(
        pipe,
        &wire::initialize(wire::V2025, Field::Absent, json!({})),
    );
    let body = check::stdio_success(&exchange)?;
    check::protocol_version(body, wire::V2025)
}

fn init_2026_body(field: Field) -> Value {
    wire::initialize(wire::V2026, field, json!({}))
}

fn after_store(pipe: &mut StdioChild, exchange: &Exchange) -> Check {
    let body = check::stdio_success(exchange)?;
    check::protocol_version(body, wire::V2025)?;
    let discover = exch(
        pipe,
        &wire::discover(wire::id_num(), Field::Absent, Caps::Absent),
    );
    let found = discover
        .response
        .as_ref()
        .ok_or_else(|| "zero bytes, wanted -32601".to_string())?;
    check::jsonrpc_error(found, &wire::id_num(), -32601)?;
    let list = exch(
        pipe,
        &wire::tools_list(wire::id_num(), Field::Absent, Caps::Absent),
    );
    let listed = check::stdio_success(&list)?;
    check::assert_list_2025(&listed["result"])
}

#[test]
fn tp_s1() {
    check::ok(s1());
}

fn s1() -> Check {
    let mut child = pipe();
    let exchange = exch(&mut child, &init_2026_body(Field::Value(wire::V2026)));
    after_store(&mut child, &exchange)
}

#[test]
fn tp_s2() {
    check::ok(s2());
}

fn s2() -> Check {
    let mut child = pipe();
    let init = exch(
        &mut child,
        &wire::initialize(wire::GREATER, Field::Absent, json!({})),
    );
    check::stdio_silence(&init)?;
    let discover = exch(
        &mut child,
        &wire::discover(wire::id_num(), Field::Absent, Caps::Absent),
    );
    check::stdio_silence(&discover)?;
    let list = exch(
        &mut child,
        &wire::tools_list(wire::id_num(), Field::Absent, Caps::Absent),
    );
    let body = check::stdio_success(&list)?;
    check::assert_list_2025(&body["result"])
}

#[test]
fn tp_s3() {
    check::ok(s3());
}

fn s3() -> Check {
    check::join(
        ["tasks/get", "tasks/cancel", "tasks/result"]
            .into_iter()
            .map(s3_one),
    )
}

fn s3_one(method: &str) -> Check {
    let mut child = pipe();
    let message = wire::tasks(
        method,
        Some("t-1"),
        wire::id_num(),
        Field::Value(wire::V2026),
        Caps::Empty,
    );
    let exchange = exch(&mut child, &message);
    check::stdio_refusal(&exchange, &wire::id_num())
}

#[test]
fn tp_s4() {
    check::ok(s4());
}

fn s4() -> Check {
    let mut child = pipe();
    let init = exch(&mut child, &init_2026_body(Field::Value(wire::V2026)));
    let exchange = exch(
        &mut child,
        &wire::tools_list(wire::id_num(), Field::Value(wire::UNKNOWN), Caps::Empty),
    );
    check::stdio_refusal(&exchange, &wire::id_num())?;
    let body = check::stdio_success(&init)?;
    check::protocol_version(body, wire::V2025)
}

#[test]
fn tp_s5() {
    check::ok(s5());
}

fn s5() -> Check {
    let mut child = pipe();
    let exchange = exch(
        &mut child,
        &wire::tools_list(wire::id_num(), Field::Value(wire::UNKNOWN), Caps::Empty),
    );
    if exchange.response.is_none() && exchange.before.is_empty() {
        return check::stdio_silence(&exchange);
    }
    let body = check::stdio_success(&exchange)?;
    check::want_complete(&body["result"])?;
    Err(format!("wanted zero bytes, got {}", check::short(body)))
}

#[test]
fn tp_s6() {
    check::ok(check::join([s6_before(), s6_after()]));
}

fn s6_before() -> Check {
    let mut child = pipe();
    let exchange = exch(
        &mut child,
        &wire::discover(wire::id_num(), Field::Absent, Caps::Absent),
    );
    check::stdio_silence(&exchange)
}

fn s6_after() -> Check {
    let mut child = pipe();
    init_2025(&mut child)?;
    let exchange = exch(
        &mut child,
        &wire::discover(wire::id_num(), Field::Absent, Caps::Absent),
    );
    let body = exchange
        .response
        .as_ref()
        .ok_or_else(|| "zero bytes, wanted -32601".to_string())?;
    check::jsonrpc_error(body, &wire::id_num(), -32601)
}

#[test]
fn tp_s7() {
    check::ok(s7());
}

pub(crate) fn s7() -> Check {
    check::join([s7_fresh(), s7_after()])
}

fn s7_fresh() -> Check {
    note_silence(Field::Value(wire::V2026), false)
}

fn s7_after() -> Check {
    note_silence(Field::Value(wire::V2026), true)
}

fn note_silence(field: Field, stored: bool) -> Check {
    let mut child = pipe();
    if stored {
        init_2025(&mut child)?;
    }
    let exchange = exch(
        &mut child,
        &wire::notification("notifications/initialized", field, Caps::Absent),
    );
    check::stdio_silence(&exchange)
}

#[test]
fn tp_s8() {
    check::ok(check::join([
        s8_field(Field::Absent),
        s8_field(Field::Value(wire::V2025)),
    ]));
}

fn s8_field(field: Field) -> Check {
    let mut child = pipe();
    let before = exch(&mut child, &wire::ping(wire::id_num(), field, Caps::Absent));
    check::stdio_success(&before).map(|_| ())?;
    init_2025(&mut child)?;
    let after = exch(&mut child, &wire::ping(wire::id_num(), field, Caps::Absent));
    check::stdio_success(&after).map(|_| ())
}

#[test]
fn tp_s9() {
    check::ok(check::join([s9_once(false), s9_once(true)]));
}

fn s9_once(stored: bool) -> Check {
    let mut child = pipe();
    if stored {
        init_2025(&mut child)?;
    }
    let exchange = exch(
        &mut child,
        &wire::notification(
            "notifications/initialized",
            Field::Value(wire::UNKNOWN),
            Caps::Absent,
        ),
    );
    check::stdio_silence(&exchange)?;
    let ping = exch(
        &mut child,
        &wire::ping(wire::id_num(), Field::Absent, Caps::Absent),
    );
    check::stdio_success(&ping).map(|_| ())
}

#[test]
fn tp_s10() {
    check::ok(check::join([
        s10_one(Field::Absent),
        s10_one(Field::Value(wire::V2025)),
    ]));
}

fn s10_one(field: Field) -> Check {
    let mut child = pipe();
    let exchange = exch(&mut child, &init_2026_body(field));
    after_store(&mut child, &exchange)
}

#[test]
fn tp_s11() {
    check::ok(check::join(
        ["2024-11-05", "2025-03-26", "2025-06-18"]
            .into_iter()
            .map(s11_one),
    ));
}

fn s11_one(protocol: &str) -> Check {
    let mut child = pipe();
    let exchange = exch(
        &mut child,
        &wire::initialize(protocol, Field::Absent, json!({})),
    );
    let body = check::stdio_success(&exchange)?;
    check::id_echo(body, &wire::id_num())?;
    check::protocol_version(body, protocol)
}

#[test]
fn tp_s12() {
    check::ok(s12());
}

fn s12() -> Check {
    let mut child = pipe();
    let exchange = exch(
        &mut child,
        &wire::initialize(wire::V2025, Field::Value(wire::UNKNOWN), json!({})),
    );
    let body = check::stdio_success(&exchange)?;
    check::protocol_version(body, wire::V2025)?;
    let discover = exch(
        &mut child,
        &wire::discover(wire::id_num(), Field::Absent, Caps::Absent),
    );
    let found = discover
        .response
        .as_ref()
        .ok_or_else(|| "zero bytes, wanted -32601".to_string())?;
    check::jsonrpc_error(found, &wire::id_num(), -32601)
}

#[test]
fn tp_ord_1() {
    check::ok(ord_1());
}

fn ord_1() -> Check {
    let mut child = pipe();
    let list = exch(
        &mut child,
        &wire::tools_list(wire::id_num(), Field::Value(wire::V2026), Caps::Empty),
    );
    let list_check = (|| {
        let body = check::stdio_success(&list)?;
        check::assert_list_2026(&body["result"])
    })();
    let init = exch(
        &mut child,
        &wire::initialize(wire::V2025, Field::Absent, json!({})),
    );
    let init_check =
        check::stdio_success(&init).and_then(|body| check::protocol_version(body, wire::V2025));
    let ping = exch(
        &mut child,
        &wire::ping(wire::id_num(), Field::Absent, Caps::Absent),
    );
    let ping_check = check::stdio_success(&ping).map(|_| ());
    let level = exch(
        &mut child,
        &wire::set_level(
            "notice",
            wire::id_num(),
            Field::Value(wire::V2026),
            Caps::Empty,
        ),
    );
    let level_check = check::stdio_refusal(&level, &wire::id_num());
    check::join([list_check, init_check, ping_check, level_check])
}

#[test]
fn tp_ord_2() {
    check::ok(ord_2());
}

fn ord_2() -> Check {
    let mut child = pipe();
    let init = exch(
        &mut child,
        &wire::initialize(wire::V2025, Field::Absent, json!({})),
    );
    let init_check =
        check::stdio_success(&init).and_then(|body| check::protocol_version(body, wire::V2025));
    let discover = exch(
        &mut child,
        &wire::discover(wire::id_num(), Field::Value(wire::V2026), Caps::Empty),
    );
    let discover_check = (|| {
        let body = check::stdio_success(&discover)?;
        check::assert_discover(&body["result"])
    })();
    let ping = exch(
        &mut child,
        &wire::ping(wire::id_num(), Field::Absent, Caps::Absent),
    );
    let ping_check = check::stdio_success(&ping).map(|_| ());
    check::join([init_check, discover_check, ping_check])
}
