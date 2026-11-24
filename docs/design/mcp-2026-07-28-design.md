# Solution design — MIK-7614

Status: awaiting ratification. Third statement of the solution.

The ratified acceptance is `docs/design/mcp-2026-07-28-problem.md`, thirteenth statement, frozen. Both seats shipped that text on material hash `134c6e05bc3ca11d0d9164a1298c096affd7f22c5e3dc7a7369713e47339ab90`. Gemini: ledger `2026-10-08T14:53:37Z`, run `agy-20261008T145315Z-93997`, process status ok, exit 0. GPT: ledger `2026-10-08T14:54:26Z`, run `gpt-20261008T145315Z-93994`, process status ok, exit 0. This design does not edit that file. Its status line still says "awaiting ratification." The ledger rows are the ratification.

The first statement of this design was not ratified. Both seats returned SHIP-WITH-FIXES on material hash `bdb54aad80498761ae8840109c6ddc33f3c76cb728e0c56e196aeecd2fe536d1`. Gemini: ledger `2026-10-08T15:29:23Z`, run `agy-20261008T152708Z-3091`, process status ok, exit 0. GPT: ledger `2026-10-08T15:32:04Z`, run `gpt-20261008T152708Z-3089`, process status ok, exit 0. The second statement repaired those findings. That hash is not ratification.

The first statement changes in six places. A POST whose field and header are both present and name different revisions is HTTP 400 before any `Continue`, with the initialize exception below. An `initialize` whose header is `2026-07-28` keeps those header bytes, and the parser check is skipped only for that method and that header. A matching 2026 HTTP notification is HTTP 202 with an empty body. Stdio classification reads the typed message. An unknown stdio revision is a JSON-RPC error after client details are stored and silence before that store. Sampling on a 2026 call is read from that request's capabilities object.

The second statement was not ratified. Both seats returned SHIP-WITH-FIXES on material hash `7b81450a4d1a3f37410d85e89e7eb4587ca99f1619587848df365653e0205441`. Gemini: ledger `2026-10-08T15:43:43Z`, run `agy-20261008T154105Z-91211`, process status ok, exit 0. GPT: ledger `2026-10-08T15:45:15Z`, run `gpt-20261008T154105Z-91209`, process status ok, command exit 0. This statement repairs those findings. That hash is not ratification.

The second statement changes in four places. `GetTaskParams`, `CancelTaskParams`, and `GetTaskPayloadParams` keep `_meta`. Those three structs drop it today, so a 2026 stdio call of `tasks/get`, `tasks/cancel`, or `tasks/result` would otherwise look like a message whose field is absent. An unknown-revision stdio notification writes nothing, and the JSON-RPC error is the message that has an id. Any HTTP request the rows above do not match is `Continue`. An `analyze` call whose capabilities advertise sampling returns the JSON-RPC error, and the passive transcript is not that error.

FOR: a nab server that meets items 1 through 5 of that acceptance, on `feat/mcp-2026-07-28` cut from `038a856`.

OUT: the exclusions already listed in the problem. Also out: a commit, push, pull request, merge, or tag; a plugin-launch change; a new ASR backend; a numeric JSON-RPC code other than the one named below; retargeting the outbound `2025-11-25` pins.

## What has to be true

Items 1 through 5 of the frozen problem are the acceptance. This document does not restate them. The deferred unknown is closed here, or this design is not ready.

Today's `run_http` hands the socket to `hyper_server::create_server` (`main.rs` line 1414). Inside that server, `handle_http_post` returns HTTP 400 at `mcp_http_handler.rs` lines 354–357 when `validate_mcp_protocol_version_header` rejects the header, and it does that before it uses `request.body()`. The body is already buffered on `Request<&str>`. `HyperServerOptions` has no hook that runs before that return. The same rejection runs for GET (lines 405–408) and DELETE (lines 443–446). A sessionless non-initialize POST then returns HTTP 400 whose body is an `SdkError` object, not a JSON-RPC response (lines 376–384, `http_utils.rs` lines 669–682). A handled message is HTTP 200 when the payload contains a request, and HTTP 202 otherwise (`http_utils.rs` lines 263–267).

On stdio, `server.start()` calls `handle_message`. That function is `pub(crate)` and its argument is `ClientMessage`, not the raw line (`server_runtime.rs` lines 333–335). `consume_string_payload` has already parsed the line (line 328). A handler success is written whether or not client details are stored. A handler error while `is_initialized()` is false returns `Err`, and the read loop writes nothing (`server_runtime.rs` lines 238–241 and 367–368). `message_observer` records messages. It does not change that branch.

`_meta` on a typed 2025 request keeps unknown keys, except `GetTaskParams`, `CancelTaskParams`, and `GetTaskPayloadParams`. The schema copy adds `_meta` to those three. HTTP classification reads the raw body. Stdio classification reads the typed message, because `handle_message` does not receive the raw line. After that copy, the selecting key is present in both.

`MicroFetchHandler` implements `ServerHandler`. That trait's `handle_initialize_request` has a default (`mcp_server_handler.rs` lines 36–64) which calls `enforce_compatible_protocol_version` and stores client details only after `Ok`. Nab does not override it today. The trait allows an override.

## Mechanism

Nab stays on `rust-mcp-sdk` 0.9.0. The crate is copied into this repo at `third_party/rust-mcp-sdk-0.9.0` and selected with a Cargo patch path. The copy's version stays 0.9.0. The copy is the crates.io 0.9.0 sources plus the callback below. It is not a commit on the upstream `rust-mcp-sdk` repository, and it is not rust-mcp-sdk 2.0.

`rust-mcp-schema` 0.10.0 is copied to `third_party/rust-mcp-schema-0.10.0` and selected with a Cargo patch path. The copy's version stays 0.10.0. It is the crates.io sources plus one field on three structs: `GetTaskParams`, `CancelTaskParams`, and `GetTaskPayloadParams` each gain `_meta` as an optional JSON map, omitted when empty, so a missing `_meta` still deserializes. Their constructors pass an empty meta. No other schema type changes. `tasks/list` already uses `PaginatedRequestParams`, which keeps the key. Nab's Cargo.toml patches both crates. The SDK copy's dependency line stays its crates.io requirement. The schema copy is not a commit on the upstream schema repository. `generated_schema/2025_11_25/mcp_schema.rs` is 408 KiB. The copy stays under 5 MiB. `plugin/` does not include it.

The transport crate stays the crates.io `rust-mcp-transport` 0.9 dependency. It is not copied. Stdio parses the line with `serde_json::from_str` inside that crate (`mcp_stream.rs` line 125) before `start()` sees a `ClientMessage`. The schema field is what lets the key survive that parse.

The patch adds one callback the process sets before the server starts. On HTTP the callback sees the method, the headers, and the raw body. On stdio it sees the typed `ClientMessage`. It returns one of four answers. It reads `is_initialized` only for the stdio unknown-revision row.

| Answer | What the 0.9 code does |
|---|---|
| `Continue` | The existing 0.9 path runs, including its HTTP 400s and its stdio silence. |
| `Http` | The given status and bytes are written. The 0.9 handler does not run. |
| `JsonRpc` | The given JSON-RPC message is written. On HTTP the status is 200. The 0.9 handler does not run. |
| `Silent` | Nothing is written. The 0.9 handler does not run. |

Nab owns the callback. The patch owns the four call sites and the write. The patch does not implement tools, discover, cache hints, or elicitation policy. The patch does not implement MRTR or `input_required`.

Call sites. On HTTP the callback is the first check inside the handler, before the accept check, the content-type check, and the protocol-version return. A produce-400 row is HTTP 400 even when those later checks would have returned another status. On `Continue`, those later checks still run, except the one initialize skip named below.

- `handle_http_post`, before the accept check at lines 342–346 and before the return at lines 354–357. The body is `&str`.
- `handle_http_get`, before the accept check at lines 399–403 and before the return at lines 405–408.
- `handle_http_delete`, before the return at lines 443–446.
- `handle_message`, before dispatch, so a 2026 message does not enter the 2025 handler, and before the silence return at lines 367–368. The argument is `ClientMessage`.

Origin checks, the session store, GET streaming, and DELETE stay inside 0.9 and run on `Continue`.

### HTTP

The callback reads `MCP-Protocol-Version` and, on POST, the JSON-RPC method, the presence of `id`, and `_meta["io.modelcontextprotocol/protocolVersion"]` from the raw body. The capabilities key does not select the revision. A missing capabilities key is not a refusal.

The first matching row wins.

| Request | Answer |
|---|---|
| POST, field and header both present and they name different revisions. This row does not apply to `initialize` whose header is absent, `2025-11-25`, or `2026-07-28`. | `Http` 400. The body is the 0.9 `SdkError` object (`code`, `message`, `data`). It is not a JSON-RPC envelope. Handlers do not run. Checked before any `Continue`. |
| POST, field names any value other than `2026-07-28` and `2025-11-25`, and the header is one the parser accepts, including an absent header. This row does not apply to `initialize` whose header is absent, `2025-11-25`, or `2026-07-28`. | `Http` 400, same `SdkError` body. When this row and the mismatch row both match, the mismatch row already won. |
| `initialize`, header absent, `2025-11-25`, or `2026-07-28`, for any selecting field | `Continue`. The patch skips `validate_mcp_protocol_version_header` only when the method is `initialize` and the header is exactly `2026-07-28`. The header bytes stay `2026-07-28`. The header is not rewritten and not removed. The body is not rewritten. Every other request still runs that check. |
| `initialize` that the two rows above did not take | `Continue`. A header the parser accepts, such as `2024-11-05` with the field absent, keeps today's echo. A header the parser rejects, other than `2026-07-28`, keeps today's HTTP 400 and starts no session. |
| POST, field and header both `2026-07-28`, method is not `initialize`, the message has an `id`, session id present or absent | `JsonRpc` from the 2026 dispatcher. HTTP status is 200. The session check does not run. The parser rejection does not run. |
| POST, field and header both `2026-07-28`, method is not `initialize`, the message has no `id`, session id present or absent | `Http` 202 with an empty body. Handlers do not run. The body is not a JSON-RPC message. |
| POST, field `2026-07-28`, header absent or a different revision the parser accepts, session id present or absent | `Http` 400, same `SdkError` body. A different header already matched the mismatch row. This row owns the absent header. |
| POST, field absent, header `2026-07-28` | `Http` 400, same `SdkError` body. Not served as either revision. |
| POST, not `initialize`, does not select `2026-07-28`, no session id, and the rows above did not match | `Continue`. The 0.9 session rule returns HTTP 400 with the `SdkError` body. |
| POST, field absent or `2025-11-25`, header absent or `2025-11-25`, session id present | `Continue`. Item 3. |
| GET or DELETE, header `2026-07-28`, session id present or absent | `Http` 400, same `SdkError` body. |
| GET or DELETE with any other header | `Continue`. |
| Any other HTTP request | `Continue`. This row is last. A produce-400 row above it still wins. |

An empty `MCP-Protocol-Version` is the absent header. Today's check treats an empty value as acceptable (`http_utils.rs` lines 637–639). A POST whose field is absent, whose header is `2024-11-05`, `2025-03-26`, `2025-06-18`, or `DRAFT-2026-v1`, and whose session id is present, matches none of the rows above the catch-all, so it is `Continue`. A header the parser rejects, on a POST the rows above did not take, is the same `Continue`, and today's HTTP 400 still happens.

A produce-400 row is never HTTP 200. A matching item 1 POST that has an `id` is never the session 400 and never the parser 400. A matching item 1 POST that has no `id` is HTTP 202 with an empty body.

An `initialize` whose header is `2024-11-05`, `2025-03-26`, `2025-06-18`, or `DRAFT-2026-v1`, and whose field is present and names a different revision, is the mismatch row. An `initialize` whose field is absent and whose header is `2024-11-05` is the later `initialize` `Continue` row, and today's echo stands. A sessionless POST that is also a mismatch is the mismatch row. The session rule does not own that POST.

### Initialize

`MicroFetchHandler` overrides `handle_initialize_request`.

- Client `protocolVersion` `2026-07-28` is accepted. Client details are stored. The result `protocolVersion` is `2025-11-25`. The selecting field does not change that result. The rest of the result stays today's initialize result, including `list_changed` true and `resources.subscribe` true. That result is item 3, not `server/discover`.
- Client `protocolVersion` `2025-11-25` stays a success with result `2025-11-25`, and details are stored.
- `2024-11-05`, `2025-03-26`, and `2025-06-18` keep today's echo.
- A greater client `protocolVersion` other than `2026-07-28` still returns the error the default returns, and still does not store client details. On stdio the read loop still sends nothing, because `initialize` does not select 2026.

An `initialize` whose header is `2026-07-28` reaches this override because the callback answered `Continue` and skipped only that parser rejection. The header on that request is still `2026-07-28`. The 0.9 session start still runs for that POST when no session id is present.

### Stdio

The callback reads `_meta["io.modelcontextprotocol/protocolVersion"]` from the typed `ClientMessage`. It does not read a raw line, and the call site stays in `handle_message`.

On the 2025-11-25 types this server deserializes, that key survives in `params._meta` for every method except the three named here. These meta structs keep unknown keys in a flattened `extra` map: `CallToolMeta`, `RequestParamsMeta`, `PaginatedMeta`, `GetPromptMeta`, `ReadResourceMeta`, `CompleteRequestMeta`, `SetLevelMeta`, `SubscribeMeta`, `UnsubscribeMeta`, and `InitializeMeta` (`rust-mcp-schema` 0.10.0, `generated_schema/2025_11_25/mcp_schema.rs`). `NotificationParams.meta` is a map. `CustomRequest.params` and `JsonrpcNotification.params` are maps. `ListResourceTemplatesRequest` uses `PaginatedRequestParams`. The capabilities key lives in the same object.

`GetTaskParams`, `CancelTaskParams`, and `GetTaskPayloadParams` contain only `taskId` (`mcp_schema.rs` lines 3349–3353, 539–543, and 3374–3378). Serde drops `_meta`. `ClientJsonrpcRequest` deserializes `tasks/get`, `tasks/result`, and `tasks/cancel` as those structs (`schema_utils.rs` lines 382–384), ahead of `CustomRequest`. On stdio the callback would then see no field and answer `Continue`. Before client details are stored, that `Continue` writes nothing. Item 1 requires a JSON-RPC error for those methods, including as the first message. The schema copy adds `_meta` to those three structs so the key is present on the typed message. HTTP still classifies those methods from the raw body, which never dropped the key. A 2025 call of the same method still has an empty meta, the handler still does not read it, and the result stays method-not-found after the store.

A typed params object that still dropped the key would hide a 2026 selection. That case is the fallback at the end of this document. After this copy, the three structs do not drop it.

The first matching row wins.

- Method `initialize`, for any selecting field: `Continue`. After the override, `protocolVersion` `2026-07-28` is a success, so the existing success path writes it and stores client details. A later request that does not select 2026 can then observe an error.
- Field `2026-07-28`, method is not `initialize`, the message is a request: `JsonRpc` from the 2026 dispatcher. A success is written. An error is written even when client details are not stored, and it may be the first message. The silence branch does not run.
- Field `2026-07-28`, method is not `initialize`, the message is a notification: `Silent`. No JSON-RPC response is written, and no server-to-client request is sent.
- Field present, and the value is neither `2026-07-28` nor `2025-11-25`, method is not `initialize`, the message is a request: not `Continue`. When client details are stored, `JsonRpc` with code `-32600`, and the 2025 handler does not run. When client details are not stored, `Silent`. The callback may read `is_initialized` only for this row. Item 2's JSON-RPC error is this message, because it has an id.
- Field present, and the value is neither `2026-07-28` nor `2025-11-25`, method is not `initialize`, the message is a notification: `Silent`, whether or not client details are stored. The 2025 handler does not run. Nothing is written. A notification has no id, so there is no JSON-RPC error to send.
- Field absent or `2025-11-25`: `Continue`. An error before client details are stored still writes nothing. An error after the store is still written. A success is still written.

On one connection, a 2026 message and a message whose field is absent or `2025-11-25` may follow each other in either order. Each message is classified from its own field.

### 2026 dispatcher

The dispatcher runs only for `JsonRpc`. It does not read `is_initialized`. It does not start a session. It does not send elicitation, sampling, `notifications/message`, or `notifications/resources/updated`.

`resultType` on every success is `complete`. `ttlMs` is `0` on every result whose schema requires it. Zero is the schema minimum. This change does not send list-change notices, so it does not advertise a positive cache lifetime. `resources/list` has `cacheScope` `private`. A `resources/read` whose URI starts with `nab://watch/` has `cacheScope` `private`. Every other 2026 result whose schema requires `cacheScope` uses `public`.

- `server/discover` returns `supportedVersions` including `2026-07-28` and `2025-11-25`. Capabilities include `tools`, `prompts`, `resources`, and `completions`. `logging` is absent. `resources.subscribe` is not true. The `tools`, `prompts`, and `resources` objects do not set `listChanged` true. There is no tasks field.
- `tools/list` returns the same names as `origin/main` built the same way. The default build includes `login` and `analyze` and omits `task`. The task-enabled build includes `task`. No 2026 tool carries `execution`. The 2025 list still sets `execution.taskSupport`.
- `prompts/list` and `resources/list` return the same entries as the 2025 builders, plus the cache fields.
- `tools/call` of a tool that does not wait calls the same `run` the 2025 path calls. A tool result is a `CallToolResult`. A tool error stays that tool error. `watch_list` with no stored watches is the content-equality proof. `fetch_batch` and `analyze` return the `run` result and do not return a task.
- `prompts/get`, `resources/read`, and `completion/complete` use the same builders as the 2025 path, plus the fields item 1 requires for that result.
- `logging/setLevel`, `resources/subscribe`, `resources/unsubscribe`, `ping`, `tasks/list`, `tasks/get`, `tasks/cancel`, `tasks/result`, and any other method, including `resources/templates/list`, return a JSON-RPC error. Today's handlers do not run. `initialize` is not in this set.
- The error code for those refusals, and for a call that would elicit or sample, is `-32600`. The codes `-32020`, `-32021`, and `-32022` are not used. Item 1 does not require a particular code. This design picks one so two call sites cannot drift.

### Ask

Waiting is decided per call by `Ask`, and, on the 2026 path, by the sampling advertisement on that same request. `Allow` is the 2025 path and the default. `Refuse` is what the 2026 dispatcher passes. The value is per call, not a process global, so a 2025 HTTP session and a 2026 POST do not share it.

`Allow` still uses `sampling::is_supported`, which calls `client_supports_sampling` (`mcp_traits/mcp_server.rs` lines 63–66) and reads the sampling member stored at initialize. A 2026 call does not use that function. A standalone 2026 request has no stored client.

On `Refuse`, the dispatcher reads `_meta["io.modelcontextprotocol/clientCapabilities"]` from the request it is dispatching. An absent key and an empty object both mean sampling is not advertised. A `sampling` member on that object means sampling is advertised. The pinned schema describes that member as present when the client supports sampling.

- Each of the five `login` elicitation branches returns before any server-to-client send, on every `Refuse` call. The capabilities object does not keep those branches. The dispatcher turns that return into the JSON-RPC error `-32600`. A `login` call that would not elicit returns the `run` outcome.
- The `analyze` sampling send is reached only when `active_reading` is true, a transcript exists, and `default_backend().is_available()` is true. On `Refuse`, when this request advertises sampling, `apply_active_reading` returns an error before `sampling/createMessage`, and `run` returns that error. The dispatcher writes `-32600`. The passive transcript is not that error. Today's helper returns the passive transcript when `is_supported` is false (`tools/analyze.rs` lines 237–243). That return stays the served branch when this request does not advertise sampling. It is not the refusal. When the backend is unavailable, `run` returns that tool error and does not reach this decision.
- On the task-enabled build, the `task` sampling send returns before `sampling/createMessage` when `autonomous` is true and this request advertises sampling. When `autonomous` is false, or when `autonomous` is true and this request does not advertise sampling, `run` returns the served outcome. The fetch that `run` performs before that decision still runs.

On `Allow`, those branches send as they do today, using the stored initialize capabilities. The missing capability check on the 2025 elicitation path stays missing.

The 2026 dispatcher does not enter `handle_task_augmented_tool_call`. `Refuse` is not a blanket error. A 2026 call that does not elicit and does not sample is served.

### Docs, comment, CI

The six documentation locations are updated as item 5 says. The comment at `sampling.rs` lines 12–13 is corrected so it names the `analyze` and `task` call sites.

`ubuntu-default` keeps `NAB_NET_TESTS=0` and gains the offline 2026 fixtures, the default-build assertion that `task` is absent, and the six date strings. The changes filter gains `README.md`, `llms.txt`, and `docs/**`, so a docs-only edit runs that string check. The problem recorded that the filter omits those paths. This design closes that gap.

A new job, `mcp-2026`, runs the `nab-mcp` tests with feature `task` and with network allowed. It proves the task-enabled bullets and the network `tools/call` successes, including `fetch`. The existing `task-feature` job stays as it is and does not discharge those bullets.

The `analyze` sampling refusal, and the item 3 `analyze` sampling send, are a hand run on a host where `default_backend().is_available()` is true. The result is recorded with the change. No job installs `fluidaudiocli`. No stand-in backend is added.

`claude plugin validate` on `plugin/` is re-run by hand. This design changes no plugin file. The existing unquoted-hook warning may remain. CI still does not run that command.

The outbound pins in `hebb_client.rs` and `src/cmd/fetch.rs` stay `2025-11-25`. HTTP still does not start `LOGGER` or `spawn_watch_fanout`.

## Options rejected

- rust-mcp-sdk 2.0 as the server. The migration guide removes initialize, sessions, the task system, ping, setLevel, subscribe, and the server-to-client requests item 3 keeps. The frozen problem already says that replacement fails AC2.
- Rewriting every `2026-07-28` header to `2025-11-25` and letting 0.9 serve the body as a 2025 request. A sessionless item 1 POST would still be the session 400. A 2026 `tools/call` would elicit. `server/discover` would be method-not-found. Results would lack `resultType` and the cache fields.
- Rewriting or removing the `2026-07-28` header on `initialize` before `Continue`. The bytes would then be indistinguishable from an absent header. The patch skips the parser check for that method and that header, and leaves the bytes in place.
- A second HTTP server that proxies 2025 traffic to a private 0.9 listener. Sessions, origin checks, and DELETE would have two owners.
- Adopting the `rmcp` crate. Nab does not depend on it, and it has not been shown to keep item 3.
- Leaving the 0.9 handler untouched and configuring `HyperServerOptions`. That struct has no pre-dispatch hook. The measured 400 and the measured silence would remain.
- Copying `rust-mcp-transport` so the callback can read the stdio line before `serde_json::from_str`. The schema field keeps the key on the typed message. The transport crate stays uncopied.

## Resolved unknown

Question: how the avoid-400 requests, the produce-400 rows, and the stdio rule in the problem's deferred unknown are met without dropping item 3. Checked against `mcp_http_handler.rs` lines 354–384 and 405–446, `server_runtime.rs` lines 228–241, 328, and 333–368, `http_utils.rs` lines 263–267 and 669–682, `HyperServerOptions` in `hyper_servers/server.rs`, `ServerHandler::handle_initialize_request`, and the flattened `extra` maps on the 2025-11-25 meta structs named above. Answer: the callback table, the initialize override, and the `_meta` field on the three task param structs in this document. Effect: a matching sessionless 2026 POST that has an `id` is `JsonRpc` at HTTP 200; a matching 2026 POST with no `id` is `Http` 202 with an empty body; each produce-400 row, including a field and header that name different revisions, is `Http` 400 with an `SdkError` body, except an `initialize` whose header is absent, `2025-11-25`, or `2026-07-28`; a sessionless POST that does not select 2026 and is not one of those 400 rows is `Continue` into the session rule; any other HTTP request is `Continue`; a 2026-selecting stdio error, including `tasks/get`, `tasks/cancel`, and `tasks/result`, is written before client details are stored; an unknown stdio revision that has an id is a JSON-RPC error after that store and silence before it; an unknown-revision notification writes nothing; a field that is absent or `2025-11-25` keeps today's silence before the store; `Continue` keeps the 0.9 item 3 path.

What if a later edit cannot place the callback at those four sites without changing a `Continue` result, or a typed params object still drops the selecting key: record that on MIK-7614 and return it to the operator. Implementation stays blocked. The requirements stay as written. SDK 2.0 stays out.

## Test plan boundary

This document names the behaviors the tests must pin. The test plan is the next document, written after this design is ratified, and the tests are written before the implementation. No test code is in this document.

## Asked and answered

- 2026-10-08, operator, "continue": write this solution design. The frozen problem stays the acceptance.
- 2026-10-08, both design seats, SHIP-WITH-FIXES on `bdb54aad80498761ae8840109c6ddc33f3c76cb728e0c56e196aeecd2fe536d1`: the six repairs in the status paragraph. Gemini's suggestion to replace the initialize header is the rejected option above. The header stays `2026-07-28`.
- 2026-10-08, both design seats, SHIP-WITH-FIXES on `7b81450a4d1a3f37410d85e89e7eb4587ca99f1619587848df365653e0205441`: the four repairs in the status paragraph. The GET and DELETE row for header `2026-07-28` stays. The catch-all `Continue` is the row after it.
