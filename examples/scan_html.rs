// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Scan an HTML file for machine-targeted markup and print a report.
//!
//! Usage:
//!
//! ```text
//! cargo run --example scan_html -- path/to/page.html
//! cargo run --example scan_html -- path/to/page.html --sanitize > clean.html
//! ```
//!
//! Output goes to stdout; the detection report goes to stderr so
//! `--sanitize` can be redirected cleanly.

use std::env;
use std::fs;
use std::process::ExitCode;

use nab::security::{Severity, detect, sanitize};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: scan_html <path> [--sanitize]");
        return ExitCode::from(2);
    };
    let do_sanitize = args.iter().any(|a| a == "--sanitize");

    let html = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            return ExitCode::from(1);
        }
    };

    let (out, report) = if do_sanitize {
        sanitize(&html)
    } else {
        let r = detect(&html);
        (String::new(), r)
    };

    eprintln!("=== machine-targeted markup report for {path} ===");
    eprintln!("AI-addressed comments  : {}", report.ai_comment_count);
    eprintln!("Machine-only attributes: {}", report.machine_attr_count);
    eprintln!("Machine-class elements : {}", report.machine_class_count);
    eprintln!("display:none w/ text   : {}", report.hidden_inline_count);
    eprintln!("aria-hidden w/ text    : {}", report.aria_hidden_count);
    eprintln!("total                  : {}", report.total());
    if !report.samples.is_empty() {
        eprintln!("\n--- samples (first 5) ---");
        for (i, s) in report.samples.iter().take(5).enumerate() {
            let sev = match s.severity {
                Severity::Info => "INFO ",
                Severity::Warn => "WARN ",
                Severity::Block => "BLOCK",
            };
            eprintln!("[{i}] {sev} {:?}", s.kind);
            eprintln!("    {}", s.excerpt);
        }
    }

    if do_sanitize {
        print!("{out}");
    }

    ExitCode::SUCCESS
}
