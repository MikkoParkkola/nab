//! Failing tests for MCP revision 2026-07-28, written before the product code.
//!
//! TP-REG-1 is the existing CI jobs. This crate does not re-invoke them and does not
//! carry an always-green stand-in.
//!
//! Hand records, not automated and not ignored: TP-LOGIN-3, the vault half of
//! TP-LOGIN-4, TP-AN-2, TP-DOC-3, and TP-PLUGIN.

mod common;

#[path = "mcp2026/check.rs"]
mod check;
#[path = "mcp2026/file_cases.rs"]
mod file_cases;
#[path = "mcp2026/harness.rs"]
mod harness;
#[path = "mcp2026/http.rs"]
mod http;
#[path = "mcp2026/http_cases.rs"]
mod http_cases;
#[path = "mcp2026/listen.rs"]
mod listen;
#[path = "mcp2026/method_cases.rs"]
mod method_cases;
#[path = "mcp2026/net_cases.rs"]
mod net_cases;
#[path = "mcp2026/stdio_cases.rs"]
mod stdio_cases;
#[path = "mcp2026/tool_cases.rs"]
mod tool_cases;
#[path = "mcp2026/wire.rs"]
mod wire;
