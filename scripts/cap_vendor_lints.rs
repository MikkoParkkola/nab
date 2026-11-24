//! Restore registry lint capping for the two path-patched crates.
//!
//! Cargo caps a crates.io dependency and does not cap a path patch. Under
//! `-Dwarnings` that turns the upstream feature-gated helpers into errors.
//! This program is `RUSTC_WRAPPER`. It adds `--cap-lints=warn` for those two
//! crates and leaves every other crate, including nab, alone.

use std::process::{Command, exit};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(compiler) = args.next() else {
        eprintln!("cap-vendor-lints: compiler path missing");
        exit(2);
    };
    let rest: Vec<String> = args.collect();
    let mut crate_name = None;
    let mut index = 0;
    while index + 1 < rest.len() {
        if rest[index] == "--crate-name" {
            crate_name = Some(rest[index + 1].as_str());
            break;
        }
        index += 1;
    }
    let mut command = Command::new(&compiler);
    command.args(&rest);
    if matches!(crate_name, Some("rust_mcp_sdk" | "rust_mcp_schema")) {
        command.arg("--cap-lints").arg("warn");
    }
    let status = command.status().unwrap_or_else(|error| {
        eprintln!("cap-vendor-lints: {error}");
        exit(2);
    });
    exit(status.code().unwrap_or(1));
}
