//! HTTP cases TP-H1 through TP-H34. There is no TP-H30.
//! TP-H26 is the same body as TP-D1 and lives in `method_cases`.

use serde_json::{Value, json};

use crate::check::{self, Check};
use crate::http::{Accept, HttpChild, Observed, Proto};
use crate::wire::{self, Caps, Field};

fn server() -> HttpChild {
    HttpChild::start()
}

fn post(
    server: &mut HttpChild,
    body: &Value,
    header: Proto<'_>,
    session: Option<&str>,
    accept: Accept,
) -> Observed {
    server.post(body, header, session, accept, wire::TWO)
}

pub(crate) fn open_2025(server: &mut HttpChild) -> Result<String, String> {
    let observed = post(
        server,
        &wire::initialize(wire::V2025, Field::Absent, json!({})),
        Proto::Omit,
        None,
        Accept::JsonAndSse,
    );
    let body = check::rpc_success(&observed)?;
    check::protocol_version(body, wire::V2025)?;
    check::require_session(&observed)
}

fn listed(field: Field, caps: Caps, id: &Value, session: Option<&str>) -> Check {
    let mut child = server();
    let observed = post(
        &mut child,
        &wire::tools_list(id.clone(), field, caps),
        Proto::Value(wire::V2026),
        session,
        Accept::JsonAndSse,
    );
    let body = check::rpc_success_id(&observed, id)?;
    check::id_echo(body, id)?;
    check::assert_list_2026(&body["result"])
}

fn mismatch(field: Field, header: Proto<'_>) -> Check {
    let mut child = server();
    let session = open_2025(&mut child)?;
    let observed = post(
        &mut child,
        &wire::tools_list(wire::id_num(), field, Caps::Empty),
        header,
        Some(&session),
        Accept::JsonAndSse,
    );
    check::produce_400(&observed)
}

fn echo(protocol: &str, header: Proto<'_>) -> Check {
    let mut child = server();
    let observed = post(
        &mut child,
        &wire::initialize(protocol, Field::Absent, json!({})),
        header,
        None,
        Accept::JsonAndSse,
    );
    let body = check::rpc_success(&observed)?;
    check::protocol_version(body, protocol)
}

fn continue_list(field: Field, header: Proto<'_>) -> Check {
    let mut child = server();
    let session = open_2025(&mut child)?;
    let observed = post(
        &mut child,
        &wire::tools_list(wire::id_num(), field, Caps::Absent),
        header,
        Some(&session),
        Accept::JsonAndSse,
    );
    let body = check::rpc_success(&observed)?;
    check::assert_list_2025(&body["result"])
}

fn rpc_error(observed: &Observed, code: i64) -> Check {
    let body = match observed {
        Observed::Rpc {
            status: 200, body, ..
        } => body,
        other => {
            return Err(format!(
                "wanted JSON-RPC error {code}, {}",
                check::describe(other)
            ));
        }
    };
    check::jsonrpc_error(body, &wire::id_num(), code)
}

#[test]
fn tp_h1() {
    check::ok(listed(
        Field::Value(wire::V2026),
        Caps::Empty,
        &wire::id_num(),
        None,
    ));
}

#[test]
fn tp_h2() {
    check::ok(h2());
}

fn h2() -> Check {
    let mut child = server();
    let session = open_2025(&mut child)?;
    listed_on(&mut child, &session)
}

fn listed_on(child: &mut HttpChild, session: &str) -> Check {
    let id = wire::id_num();
    let observed = post(
        child,
        &wire::tools_list(id.clone(), Field::Value(wire::V2026), Caps::Empty),
        Proto::Value(wire::V2026),
        Some(session),
        Accept::JsonAndSse,
    );
    let body = check::rpc_success(&observed)?;
    check::id_echo(body, &id)?;
    check::assert_list_2026(&body["result"])
}

#[test]
fn tp_h3() {
    check::ok(listed(
        Field::Value(wire::V2026),
        Caps::Absent,
        &wire::id_str(),
        None,
    ));
}

#[test]
fn tp_h4() {
    check::ok(mismatch(
        Field::Value(wire::V2026),
        Proto::Value(wire::V2025),
    ));
}

#[test]
fn tp_h5() {
    check::ok(mismatch(Field::Value(wire::V2026), Proto::Omit));
}

#[test]
fn tp_h5b() {
    check::ok(mismatch(Field::Value(wire::V2026), Proto::Empty));
}

#[test]
fn tp_h6() {
    check::ok(h6());
}

fn h6() -> Check {
    let mut child = server();
    let session = open_2025(&mut child)?;
    let observed = post(
        &mut child,
        &wire::tools_list(wire::id_num(), Field::Absent, Caps::Empty),
        Proto::Value(wire::V2026),
        Some(&session),
        Accept::Omit,
    );
    check::want_400_not_406(&observed)
}

#[test]
fn tp_h7() {
    check::ok(mismatch(Field::Value(wire::UNKNOWN), Proto::Omit));
}

#[test]
fn tp_h8() {
    check::ok(h8());
}

fn h8() -> Check {
    let mut child = server();
    let observed = post(
        &mut child,
        &wire::tools_list(wire::id_num(), Field::Absent, Caps::Absent),
        Proto::Omit,
        None,
        Accept::JsonAndSse,
    );
    check::sdk_message(&observed, 400, -32015, "Bad Request: Session not found")
}

#[test]
fn tp_h9() {
    check::ok(h9());
}

pub(crate) fn h9() -> Check {
    let mut child = server();
    let note = wire::notification(
        "notifications/initialized",
        Field::Value(wire::V2026),
        Caps::Absent,
    );
    let observed = post(
        &mut child,
        &note,
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
    );
    check::want_empty(&observed, 202)
}

#[test]
fn tp_h10() {
    check::ok(h10());
}

fn h10() -> Check {
    let mut child = server();
    let observed = post(
        &mut child,
        &wire::initialize(wire::V2026, Field::Value(wire::V2026), json!({})),
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
    );
    init_2026_ok(&observed)
}

fn init_2026_ok(observed: &Observed) -> Check {
    let body = check::rpc_success(observed)?;
    check::protocol_version(body, wire::V2025)?;
    check::init_flags(body)?;
    check::require_session(observed).map(|_| ())
}

#[test]
fn tp_h11() {
    check::ok(h11());
}

fn h11() -> Check {
    let mut child = server();
    let observed = post(
        &mut child,
        &wire::initialize(wire::V2026, Field::Value(wire::V2026), json!({})),
        Proto::Value(wire::V2026),
        None,
        Accept::Omit,
    );
    if check::status_of(&observed) == 406 {
        return match &observed {
            Observed::Rpc { body, .. }
                if body.get("result").is_some() && body.get("error").is_none() =>
            {
                Err(format!(
                    "JSON-RPC success, wanted HTTP 406, {}",
                    check::describe(&observed)
                ))
            }
            _ => Ok(()),
        };
    }
    Err(format!("wanted HTTP 406, {}", check::describe(&observed)))
}

#[test]
fn tp_h12() {
    check::ok(h12());
}

fn h12() -> Check {
    let mut child = server();
    open_2025(&mut child).map(|_| ())
}

#[test]
fn tp_h13() {
    check::ok(echo("2024-11-05", Proto::Omit));
}

#[test]
fn tp_h14() {
    check::ok(echo("2025-03-26", Proto::Value("2025-03-26")));
}

#[test]
fn tp_h15() {
    check::ok(echo("2025-06-18", Proto::Omit));
}

#[test]
fn tp_h16() {
    check::ok(check::join([h16_fresh(), h16_stored()]));
}

fn h16_fresh() -> Check {
    let mut child = server();
    let observed = post(
        &mut child,
        &wire::initialize(wire::GREATER, Field::Absent, json!({})),
        Proto::Omit,
        None,
        Accept::JsonAndSse,
    );
    check::fresh_body(&observed)
}

fn h16_stored() -> Check {
    let mut child = server();
    let session = open_2025(&mut child)?;
    let observed = post(
        &mut child,
        &wire::initialize(wire::GREATER, Field::Absent, json!({})),
        Proto::Omit,
        Some(&session),
        Accept::JsonAndSse,
    );
    rpc_error(&observed, -32603)?;
    let list = post(
        &mut child,
        &wire::tools_list(wire::id_num(), Field::Absent, Caps::Absent),
        Proto::Omit,
        Some(&session),
        Accept::JsonAndSse,
    );
    let body = check::rpc_success(&list)?;
    check::assert_list_2025(&body["result"])
}

#[test]
fn tp_h17() {
    check::ok(h17());
}

fn h17() -> Check {
    let mut child = server();
    let observed = post(
        &mut child,
        &wire::initialize(wire::V2025, Field::Value(wire::V2025), json!({})),
        Proto::Value("DRAFT-2026-v1"),
        None,
        Accept::JsonAndSse,
    );
    check::produce_400(&observed)
}

#[test]
fn tp_h18() {
    check::ok(echo("2024-11-05", Proto::Value("2024-11-05")));
}

#[test]
fn tp_h19() {
    check::ok(h19());
}

fn h19() -> Check {
    let mut child = server();
    let observed = child.get(Proto::Value(wire::V2026), None, Accept::Omit, wire::TWO);
    check::want_400_not_406(&observed)
}

#[test]
fn tp_h20() {
    check::ok(h20());
}

fn h20() -> Check {
    let mut child = server();
    let observed = child.get(
        Proto::Value(wire::V2025),
        None,
        Accept::EventStream,
        wire::TWO,
    );
    check::sdk_message(&observed, 400, -32015, "Bad request: session not found")
}

#[test]
fn tp_h21() {
    check::ok(h21());
}

fn h21() -> Check {
    let mut child = server();
    let observed = child.delete(
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
        wire::TWO,
    );
    check::produce_400(&observed)
}

#[test]
fn tp_h22() {
    check::ok(h22());
}

fn h22() -> Check {
    let mut child = server();
    let observed = child.delete(
        Proto::Value(wire::V2025),
        None,
        Accept::JsonAndSse,
        wire::TWO,
    );
    check::sdk_message(&observed, 400, -32015, "Bad Request: Session not found")
}

#[test]
fn tp_h23() {
    check::ok(continue_list(Field::Absent, Proto::Value("2024-11-05")));
}

#[test]
fn tp_h24() {
    check::ok(h24());
}

fn h24() -> Check {
    let mut child = server();
    let session = open_2025(&mut child)?;
    let observed = post(
        &mut child,
        &wire::tools_list(wire::id_num(), Field::Absent, Caps::Absent),
        Proto::Value("not-a-version"),
        Some(&session),
        Accept::JsonAndSse,
    );
    check::produce_400(&observed)
}

#[test]
fn tp_h25() {
    check::ok(h25());
}

fn h25() -> Check {
    let mut child = server();
    let init = post(
        &mut child,
        &wire::initialize(wire::V2026, Field::Value(wire::V2026), json!({})),
        Proto::Value(wire::V2026),
        None,
        Accept::JsonAndSse,
    );
    let body = check::rpc_success(&init)?;
    check::protocol_version(body, wire::V2025)?;
    let session = check::require_session(&init)?;
    let observed = post(
        &mut child,
        &wire::discover(wire::id_num(), Field::Absent, Caps::Absent),
        Proto::Value(wire::V2025),
        Some(&session),
        Accept::JsonAndSse,
    );
    rpc_error(&observed, -32601)
}

#[test]
fn tp_h27() {
    check::ok(mismatch(
        Field::Value(wire::V2025),
        Proto::Value("2024-11-05"),
    ));
}

#[test]
fn tp_h28() {
    check::ok(h28());
}

fn h28() -> Check {
    let mut child = server();
    let observed = post(
        &mut child,
        &wire::initialize(wire::V2025, Field::Value(wire::V2026), json!({})),
        Proto::Value(wire::V2025),
        None,
        Accept::JsonAndSse,
    );
    let body = check::rpc_success(&observed)?;
    check::protocol_version(body, wire::V2025)?;
    check::require_session(&observed).map(|_| ())
}

#[test]
fn tp_h29() {
    check::ok(h29());
}

fn h29() -> Check {
    let mut child = server();
    let observed = post(
        &mut child,
        &wire::initialize(wire::V2025, Field::Value(wire::UNKNOWN), json!({})),
        Proto::Omit,
        None,
        Accept::JsonAndSse,
    );
    let body = check::rpc_success(&observed)?;
    check::protocol_version(body, wire::V2025)
}

#[test]
fn tp_h31() {
    check::ok(continue_list(
        Field::Value(wire::V2025),
        Proto::Value(wire::V2025),
    ));
}

#[test]
fn tp_h32() {
    check::ok(continue_list(Field::Absent, Proto::Omit));
}

#[test]
fn tp_h33() {
    check::ok(h33());
}

fn h33() -> Check {
    let mut child = server();
    let init = post(
        &mut child,
        &wire::initialize(wire::V2026, Field::Absent, json!({})),
        Proto::Omit,
        None,
        Accept::JsonAndSse,
    );
    init_2026_ok(&init)?;
    let session = check::require_session(&init)?;
    let discover = post(
        &mut child,
        &wire::discover(wire::id_num(), Field::Absent, Caps::Absent),
        Proto::Value(wire::V2025),
        Some(&session),
        Accept::JsonAndSse,
    );
    rpc_error(&discover, -32601)?;
    let list = post(
        &mut child,
        &wire::tools_list(wire::id_num(), Field::Absent, Caps::Absent),
        Proto::Omit,
        Some(&session),
        Accept::JsonAndSse,
    );
    let body = check::rpc_success(&list)?;
    check::assert_list_2025(&body["result"])
}

#[test]
fn tp_h34() {
    check::ok(continue_list(Field::Value(wire::V2025), Proto::Omit));
}
