//! Schema round-trip, vendored bytes, docs, CI, and the outbound pin.
//!
//! The pinned schema loader lives in `check`. These cases do not call it.
//! A missing `third_party/` copy is the vendor red. The hash walk runs only
//! after both copies are present.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::check::{self, Check};
use crate::wire;

#[path = "ci_command.rs"]
mod ci_command;

const FIVE_MIB: u64 = 5 * 1024 * 1024;
const KIB_256: u64 = 256 * 1024;
const SCHEMA_EDIT: &str = "src/generated_schema/2025_11_25/mcp_schema.rs";
const TASK_STRUCTS: [&str; 3] = ["GetTaskParams", "CancelTaskParams", "GetTaskPayloadParams"];
/// SHA-256 of the crates.io schema after those three structs are removed.
const STRIPPED_SCHEMA_SHA: &str =
    "7e4a9af06d1fade4369851c8d4a957c096aea30465974e3a3fa1a8a81f809a19";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn round_trip<T: DeserializeOwned + Serialize>(raw: &str) -> Result<Value, String> {
    let parsed: T = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    serde_json::to_value(&parsed).map_err(|error| error.to_string())
}

#[test]
fn tp_schema_1() {
    check::ok(schema_1());
}

fn schema_1() -> Check {
    let raw =
        r#"{"taskId":"t-1","_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}"#;
    check::join([
        selecting_kept::<rust_mcp_schema::GetTaskParams>(raw),
        selecting_kept::<rust_mcp_schema::CancelTaskParams>(raw),
        selecting_kept::<rust_mcp_schema::GetTaskPayloadParams>(raw),
    ])
}

fn selecting_kept<T: DeserializeOwned + Serialize>(raw: &str) -> Check {
    let value = round_trip::<T>(raw)?;
    let found = value
        .get("_meta")
        .and_then(|meta| meta.get(wire::PROTO))
        .and_then(Value::as_str);
    if found == Some(wire::V2026) {
        Ok(())
    } else {
        Err(format!("selecting key absent: {}", check::short(&value)))
    }
}

#[test]
fn tp_schema_2() {
    check::ok(schema_2());
}

fn schema_2() -> Check {
    let raw = r#"{"taskId":"t-1"}"#;
    check::join([
        meta_absent::<rust_mcp_schema::GetTaskParams>(raw),
        meta_absent::<rust_mcp_schema::CancelTaskParams>(raw),
        meta_absent::<rust_mcp_schema::GetTaskPayloadParams>(raw),
    ])
}

fn meta_absent<T: DeserializeOwned + Serialize>(raw: &str) -> Check {
    let value = round_trip::<T>(raw)?;
    if value.get("_meta").is_none() {
        Ok(())
    } else {
        Err(format!("_meta is present: {}", check::short(&value)))
    }
}

#[test]
fn tp_vendor() {
    check::ok(vendor());
}

fn vendor() -> Check {
    let base = root();
    let sdk = base.join("third_party/rust-mcp-sdk-0.9.0");
    let schema = base.join("third_party/rust-mcp-schema-0.10.0");
    if !sdk.is_dir() || !schema.is_dir() {
        return check::join([
            Err("copy missing so crates.io files are absent from the copy: third_party/rust-mcp-sdk-0.9.0 and third_party/rust-mcp-schema-0.10.0".into()),
            plugin_clear(&base),
        ]);
    }
    check::join([
        transport_absent(&base),
        plugin_clear(&base),
        inventory(
            &base,
            "sdk",
            &sdk,
            &[
                "src/mcp_http/mcp_http_handler.rs",
                "src/mcp_runtimes/server_runtime.rs",
            ],
            true,
        ),
        inventory(&base, "schema", &schema, &[SCHEMA_EDIT], false),
        schema_diff(&schema),
        copy_limits(&sdk),
        copy_limits(&schema),
        schema_large(&schema),
    ])
}

fn transport_absent(base: &Path) -> Check {
    let Ok(entries) = fs::read_dir(base.join("third_party")) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "rust-mcp-transport" || name.starts_with("rust-mcp-transport-") {
            return Err(format!("third_party/{name} exists"));
        }
        let manifest = path.join("Cargo.toml");
        if let Ok(text) = fs::read_to_string(&manifest)
            && text
                .lines()
                .any(|line| line.trim() == "name = \"rust-mcp-transport\"")
        {
            return Err(format!("third_party/{name} is rust-mcp-transport"));
        }
    }
    Ok(())
}

fn plugin_clear(base: &Path) -> Check {
    let plugin = base.join("plugin");
    let mut hits = Vec::new();
    visit(&plugin, &mut |path, kind| {
        let name = path
            .file_name()
            .and_then(|item| item.to_str())
            .unwrap_or("");
        if name == "mcp_schema.rs" || (kind && name == "third_party") {
            hits.push(path.display().to_string());
        }
    })?;
    if hits.is_empty() {
        Ok(())
    } else {
        Err(format!("plugin contains {hits:?}"))
    }
}

struct Row {
    kind: String,
    path: String,
    hash: String,
}

fn load_rows(base: &Path) -> Result<Vec<Row>, String> {
    let text = fs::read_to_string(base.join("tests/mcp2026/vendor-hashes.txt"))
        .map_err(|error| error.to_string())?;
    let mut rows = Vec::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let mut parts = line.split('\t');
        let kind = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();
        let hash = parts.next().unwrap_or("").to_string();
        if kind.is_empty() || path.is_empty() || hash.len() != 64 {
            return Err(format!("bad inventory line {line}"));
        }
        rows.push(Row { kind, path, hash });
    }
    Ok(rows)
}

fn inventory(base: &Path, kind: &str, copy: &Path, edited: &[&str], allow_extra: bool) -> Check {
    let rows = load_rows(base)?;
    let mut expected = std::collections::HashSet::new();
    let mut missing = Vec::new();
    for row in rows.iter().filter(|row| row.kind == kind) {
        expected.insert(row.path.clone());
        let path = copy.join(&row.path);
        if !path.is_file() {
            missing.push(row.path.clone());
            continue;
        }
        if edited.contains(&row.path.as_str()) {
            continue;
        }
        let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", row.path))?;
        let found = check::hex_sha256(&bytes);
        // Design event, recorded here because the ratified design file stays
        // frozen. Adding `_meta` makes every struct literal name `meta`.
        // Forgiving only lines whose trim is `meta: None` or `meta: None,`
        // covers that family. A whole-file skip would hide the next edit.
        if found != row.hash && check::hex_sha256(&without_meta_lines(&bytes)) != row.hash {
            return Err(format!("{} hash mismatch {}", row.path, found));
        }
    }
    if !missing.is_empty() {
        return Err(format!("{kind} missing {missing:?}"));
    }
    let mut extras = Vec::new();
    visit(copy, &mut |path, is_dir| {
        if is_dir {
            return;
        }
        let rel = path
            .strip_prefix(copy)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if !expected.contains(&rel) {
            extras.push(rel);
        }
    })?;
    if allow_extra {
        one_rs_extra(&extras)
    } else if extras.is_empty() {
        Ok(())
    } else {
        Err(format!("{kind} extra files {extras:?}"))
    }
}

fn without_meta_lines(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let Ok(text) = std::str::from_utf8(line) else {
            out.extend_from_slice(line);
            continue;
        };
        let core = text.trim_end_matches(['\n', '\r']).trim();
        if core == "meta: None," || core == "meta: None" {
            continue;
        }
        out.extend_from_slice(line);
    }
    out
}

fn one_rs_extra(extras: &[String]) -> Check {
    if extras.is_empty() {
        return Ok(());
    }
    let one_rust = extras.len() == 1
        && extras[0].starts_with("src/")
        && std::path::Path::new(&extras[0])
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"));
    if one_rust {
        return Ok(());
    }
    Err(format!("extra files {extras:?}"))
}

fn schema_diff(copy: &Path) -> Check {
    let rows = load_rows(&root())?;
    let wanted = rows
        .into_iter()
        .find(|row| row.kind == "schema" && row.path == SCHEMA_EDIT);
    let Some(row) = wanted else {
        return Err("schema inventory missing mcp_schema.rs".into());
    };
    let path = copy.join(SCHEMA_EDIT);
    let bytes = fs::read(&path).map_err(|error| error.to_string())?;
    if check::hex_sha256(&bytes) == row.hash {
        return Ok(());
    }
    let edited = String::from_utf8_lossy(&bytes);
    let stripped = strip_structs(&edited);
    if check::hex_sha256(stripped.as_bytes()) == STRIPPED_SCHEMA_SHA {
        Ok(())
    } else {
        Err("schema diff touches more than GetTaskParams, CancelTaskParams, and GetTaskPayloadParams".into())
    }
}

fn strip_structs(source: &str) -> String {
    let mut current = source.to_string();
    for name in TASK_STRUCTS {
        current = strip_one(&current, name);
    }
    current
}

fn strip_one(source: &str, name: &str) -> String {
    let marker = format!("pub struct {name} {{");
    let Some(start) = source.find(&marker) else {
        return source.to_string();
    };
    let mut depth = 0;
    let mut end = start;
    let mut seen = false;
    for (index, byte) in source.as_bytes().iter().enumerate().skip(start) {
        if *byte == b'{' {
            depth += 1;
            seen = true;
        } else if *byte == b'}' {
            depth -= 1;
            if seen && depth == 0 {
                end = index + 1;
                break;
            }
        }
    }
    let mut result = String::new();
    result.push_str(&source[..start]);
    result.push_str(&source[end..]);
    result
}

fn copy_limits(copy: &Path) -> Check {
    let mut too_big = Vec::new();
    visit(copy, &mut |path, is_dir| {
        if is_dir {
            return;
        }
        let len = fs::metadata(path).map_or(0, |meta| meta.len());
        if len > FIVE_MIB {
            too_big.push(path.display().to_string());
        }
    })?;
    if too_big.is_empty() {
        Ok(())
    } else {
        Err(format!("file over 5 MiB: {too_big:?}"))
    }
}

fn schema_large(copy: &Path) -> Check {
    let path = copy.join(SCHEMA_EDIT);
    let len = fs::metadata(&path).map_or(0, |meta| meta.len());
    if len > KIB_256 {
        Ok(())
    } else {
        Err(format!(
            "mcp_schema.rs is {len} bytes, wanted more than 256 KiB"
        ))
    }
}

fn visit(dir: &Path, each: &mut dyn FnMut(&Path, bool)) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!("missing {}", dir.display()));
    }
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries = fs::read_dir(&current)
            .map_err(|error| format!("read {}: {error}", current.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("read {error}"))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|error| format!("{error}"))?;
            if kind.is_symlink() {
                return Err(format!("symlink {}", path.display()));
            }
            if kind.is_dir() {
                each(&path, true);
                pending.push(path);
            } else {
                each(&path, false);
            }
        }
    }
    Ok(())
}

#[test]
fn tp_doc_1() {
    check::ok(doc_1());
}

fn doc_1() -> Check {
    let base = root();
    let readme = fs::read_to_string(base.join("README.md")).map_err(|error| error.to_string())?;
    let llms = fs::read_to_string(base.join("llms.txt")).map_err(|error| error.to_string())?;
    let architecture =
        fs::read_to_string(base.join("docs/ARCHITECTURE.md")).map_err(|error| error.to_string())?;
    check::join([
        short_line(&readme, "badge/MCP-", "2026--07--28", "2025--11--25"),
        short_line(
            &readme,
            "Token-optimized web fetcher",
            "2026-07-28",
            "2025-11-25",
        ),
        short_line(&readme, "12 tools, 4 prompts", "2026-07-28", "2025-11-25"),
        short_line(
            &llms,
            "Token-optimized web fetcher",
            "2026-07-28",
            "2025-11-25",
        ),
        long_paragraph(&readme, "## MCP integration", "nab-mcp"),
        long_line(&architecture, "MCP protocol"),
    ])
}

fn short_line(text: &str, anchor: &str, date_2026: &str, date_2025: &str) -> Check {
    let line = anchored_line(text, anchor)?;
    if !line.contains(date_2026) {
        return Err(format!("missing {date_2026} in {anchor}"));
    }
    if line.contains(date_2025) || line.contains("partial") {
        Ok(())
    } else {
        Err(format!(
            "{anchor} has {date_2026} without {date_2025} or partial"
        ))
    }
}

fn long_line(text: &str, anchor: &str) -> Check {
    let line = anchored_line(text, anchor)?;
    if line.contains("2026-07-28") {
        Ok(())
    } else {
        Err(format!("missing 2026-07-28 in {anchor}"))
    }
}

fn long_paragraph(text: &str, heading: &str, needle: &str) -> Check {
    let paragraph = paragraph_under(text, heading, needle)?;
    if paragraph.contains("2026-07-28") {
        Ok(())
    } else {
        Err(format!("missing 2026-07-28 under {heading}"))
    }
}

fn anchored_line(text: &str, anchor: &str) -> Result<String, String> {
    text.lines()
        .find(|line| line.contains(anchor))
        .map(ToString::to_string)
        .ok_or_else(|| format!("empty anchor {anchor}"))
}

fn paragraph_under(text: &str, heading: &str, needle: &str) -> Result<String, String> {
    let start = text
        .find(heading)
        .ok_or_else(|| format!("empty anchor {heading}"))?;
    let after = &text[start + heading.len()..];
    let end = after.find("\n## ").unwrap_or(after.len());
    for paragraph in after[..end].split("\n\n") {
        if paragraph.contains(needle) {
            return Ok(paragraph.to_string());
        }
    }
    Err(format!("empty anchor {needle}"))
}

#[test]
fn tp_doc_2() {
    check::ok(doc_2());
}

fn doc_2() -> Check {
    let text = fs::read_to_string(root().join("src/bin/mcp_server/sampling.rs"))
        .map_err(|error| error.to_string())?;
    let comment = module_comment(&text);
    let names = if comment.contains("analyze") && comment.contains("task") {
        Ok(())
    } else {
        Err("module comment missing analyze or task".into())
    };
    let absence = if comment.contains("not called from any tool path") {
        Err("module comment still says not called from any tool path".into())
    } else {
        Ok(())
    };
    check::join([names, absence])
}

fn module_comment(text: &str) -> String {
    let mut parts = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("//!") {
            parts.push(rest.trim());
            continue;
        }
        break;
    }
    parts.join(" ")
}

#[test]
fn tp_ci_1() {
    check::ok(ci_1());
}

fn ci_1() -> Check {
    let text = fs::read_to_string(root().join(".github/workflows/ci.yml"))
        .map_err(|error| error.to_string())?;
    let filter = code_filter(&text)?;
    let ubuntu = matrix_entry(&text, "ubuntu-default")?;
    let task = matrix_entry(&text, "task-feature")?;
    check::join([
        has_entry(&filter, "'README.md'"),
        has_entry(&filter, "'llms.txt'"),
        has_entry(&filter, "'docs/**'"),
        has_entry(&filter, "'src/**'"),
        command_is(&ubuntu, "cargo test --locked"),
        yaml_has(&ubuntu, "net_tests: \"0\""),
        command_is(
            &task,
            "cargo test --locked --features task --lib --bin nab task",
        ),
        absent(&task, "nab-mcp"),
    ])
}

fn code_filter(text: &str) -> Result<String, String> {
    let marker = "\n            code:\n";
    let start = text.find(marker).ok_or("code filter absent")?;
    let mut lines = Vec::new();
    for line in text[start + marker.len()..].lines() {
        if let Some(rest) = line.strip_prefix("              - ") {
            lines.push(ci_command::yaml_code(rest).trim().to_string());
            continue;
        }
        // A comment-only line is not an entry and it is not the end of the list.
        if line.trim().is_empty() || ci_command::yaml_code(line).trim().is_empty() {
            continue;
        }
        break;
    }
    if lines.is_empty() {
        return Err("code filter absent".into());
    }
    Ok(lines.join("\n"))
}

fn matrix_entry(text: &str, name: &str) -> Result<String, String> {
    let start =
        ci_command::real_matrix_name(text, name).ok_or_else(|| format!("missing {name}"))?;
    let rest = &text[start..];
    let end = rest.find("\n          - name:").unwrap_or(rest.len());
    Ok(rest[..end].to_string())
}

fn command_is(stanza: &str, expected: &str) -> Check {
    let found = stanza
        .lines()
        .find_map(|line| line.trim().strip_prefix("command: "));
    match found {
        Some(value) if value == expected => Ok(()),
        Some(value) => Err(format!("command {value:?}, wanted {expected}")),
        None => Err("command absent".into()),
    }
}

fn has_entry(text: &str, needle: &str) -> Check {
    // The whole list entry is the glob. A longer entry is a different glob.
    if text.lines().any(|line| line == needle) {
        Ok(())
    } else {
        Err(format!("missing {needle}"))
    }
}

fn yaml_has(text: &str, needle: &str) -> Check {
    // The whole remaining line is the setting. A longer key is a different setting.
    if ci_command::has_setting(text, needle) {
        Ok(())
    } else {
        Err(format!("missing {needle}"))
    }
}

fn absent(text: &str, needle: &str) -> Check {
    if text.contains(needle) {
        Err(format!("{needle} is present"))
    } else {
        Ok(())
    }
}

#[test]
fn tp_ci_2() {
    check::ok(ci_2());
}

fn ci_2() -> Check {
    let text = fs::read_to_string(root().join(".github/workflows/ci.yml"))
        .map_err(|error| error.to_string())?;
    ci_2_text(&text)
}

fn ci_2_text(text: &str) -> Check {
    let named = ci_command::real_matrix_name(text, "mcp-2026");
    let place = named
        .or_else(|| ci_command::real_job_key(text, "mcp-2026"))
        .ok_or("job mcp-2026 absent")?;
    let window = ci_command::enclosing_job(text, place)?;
    if ci_command::real_line(window, "- run: ${{ matrix.command }}").is_none() {
        return Err("job does not run matrix.command".into());
    }
    if ci_command::command_step_conditioned(window) {
        return Err("matrix command is conditional".into());
    }
    if ci_command::job_condition_rejected(window) {
        return Err("job condition is not the changes filter".into());
    }
    if !ci_command::command_step_forwards(window) {
        return Err("job does not assign NAB_NET_TESTS".into());
    }
    let stanza = if named == Some(place) {
        matrix_entry(text, "mcp-2026")
    } else {
        job_block(text)
    }?;
    let Some(command) = ci_command::sibling_value(&stanza, "command: ") else {
        return Err("command absent".into());
    };
    let mut checks = ci_command::command_checks(&command, &root())?;
    checks.push(
        if ci_command::has_setting_at(&stanza, "net_tests: \"1\"", 12) {
            Ok(())
        } else {
            Err("missing net_tests: \"1\"".into())
        },
    );
    check::join(checks)
}

fn job_block(text: &str) -> Result<String, String> {
    let start = ci_command::real_job_key(text, "mcp-2026").ok_or("job mcp-2026 absent")?;
    let rest = &text[start..];
    let bytes = rest.as_bytes();
    let mut end = rest.len();
    let mut index = 1;
    while index + 3 < bytes.len() {
        if bytes[index] == b'\n'
            && bytes[index + 1] == b' '
            && bytes[index + 2] == b' '
            && bytes[index + 3] != b' '
        {
            end = index;
            break;
        }
        index += 1;
    }
    Ok(rest[..end].to_string())
}

#[test]
fn tp_pin() {
    check::ok(pin());
}

fn pin() -> Check {
    let base = root();
    let hebb = fs::read_to_string(base.join("src/bin/mcp_server/hebb_client.rs"))
        .map_err(|error| error.to_string())?;
    let fetch =
        fs::read_to_string(base.join("src/cmd/fetch.rs")).map_err(|error| error.to_string())?;
    check::join([
        has_pin(&hebb, "hebb_client.rs"),
        has_pin(&fetch, "fetch.rs"),
    ])
}

fn strip_block_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start + 2..].find("*/") else {
            return out;
        };
        rest = &rest[start + 2 + end + 2..];
    }
    out.push_str(rest);
    out
}

fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut in_string = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'"' {
            in_string = !in_string;
            index += 1;
            continue;
        }
        if in_string && byte == b'\\' {
            index += 2;
            continue;
        }
        if !in_string && byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            return &line[..index];
        }
        index += 1;
    }
    line
}

fn has_pin(text: &str, name: &str) -> Check {
    let cleaned = strip_block_comments(text);
    let found = cleaned.lines().any(|line| {
        let trimmed = strip_line_comment(line).trim();
        !trimmed.is_empty()
            && trimmed.contains("protocolVersion")
            && trimmed.contains("\"2025-11-25\"")
    });
    if found {
        Ok(())
    } else {
        Err(format!("{name} initialize body missing 2025-11-25"))
    }
}
