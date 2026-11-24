//! Whether a workflow command runs this crate's tests with feature `task`.
//!
//! Skip matching uses cargo's rule: a filter omits a test when the test path contains it.
//! Paths are the `fn tp_` tests compiled with feature `task` on. The two
//! `cfg(not(feature = "task"))` tests are not in that binary, so a skip of only those names
//! still runs the required set.
//!
//! A bare `--` ends Cargo flags. Quote characters are removed from every word before
//! the word is classified. After the separator, a libtest option fails the guard only
//! when it drops the required tests. `--exact` after that separator compares a skip
//! with the whole test path.
//!
//! The first `cargo` and the first `test` are the invocation. A later word that is
//! not a flag and not a flag's value is a test-name filter, including another
//! `cargo` or `test`.
//!
//! A cargo option this guard does not recognize fails it. That includes a flag
//! that selects another package (`--manifest-path`, `--package`, `-p`). Recognized
//! cargo options are the target filters, `--skip`, `--include-ignored`, `--locked`,
//! `--offline`, `--features`, `-F`, `--jobs`, and `-j`. A word containing `$`, `\`,
//! a backtick, `;`, `&`, `|`, `<`, `>`, `{`, `}`, `*`, `?`, `[`, or `]` fails
//! the guard. The guard
//! does not interpret shell syntax.
//!
//! A matrix row inside a block scalar is not a job. The step that runs
//! `- run: ${{ matrix.command }}` has to be a real line in that job. An assignment
//! of `NAB_NET_TESTS` on that step wins over the job-level `env:` map. With no
//! assignment on the step, the job-level map is the one that counts. A real `if:`
//! on that step means the step is conditional, so the row is not established.
//! The only job-level `if:` that counts is the changes filter this workflow
//! already uses. A matrix row is a `- name:` line at indent 10 whose nearest
//! preceding `include:` or `exclude:` key is `include:`, and its `command`
//! and `net_tests` are siblings at indent 12. An assignment written as
//! `env: {NAB_NET_TESTS: ...}` is still an assignment. That braces form
//! forwards only when the whole line is one of the four exact assignments.
//! A quote around the key `if` or `NAB_NET_TESTS` does not hide that key.
//! A backslash inside a double-quoted key does not hide that key either.
//! `env: *name` is an assignment. The guard does not resolve the alias.
//! A value nested under a block scalar does not count. A block header is
//! `|` or `>` with an optional chomp mark and an optional indent digit; the
//! header is not decoded. This is not a YAML parser and not an expression
//! parser.

use std::fs;
use std::path::Path;

use crate::check::Check;

pub(super) fn command_checks(command: &str, manifest: &Path) -> Result<Vec<Check>, String> {
    let owned: Vec<String> = command.split_whitespace().map(plain).collect();
    let words: Vec<&str> = owned.iter().map(String::as_str).collect();
    let dash = words
        .iter()
        .position(|word| *word == "--")
        .unwrap_or(words.len());
    let paths = required_paths(manifest)?;
    Ok(vec![
        task_feature(&words[..dash]),
        suite_result(&words[..dash], words.get(dash + 1..).unwrap_or(&[]), &paths),
    ])
}

fn task_feature(words: &[&str]) -> Check {
    let enabled = words.iter().enumerate().any(|(index, word)| {
        let (key, inline) = split_flag(word);
        matches!(key, "--features" | "-F")
            && inline
                .or_else(|| words.get(index + 1).copied())
                .is_some_and(has_task)
    });
    if enabled {
        Ok(())
    } else {
        Err("command does not enable feature task".into())
    }
}

fn has_task(value: &str) -> bool {
    value
        .trim_matches(['"', '\''])
        .split(',')
        .any(|item| item == "task")
}

fn suite_result(cargo: &[&str], libtest: &[&str], paths: &[String]) -> Check {
    if suite_command(cargo, libtest, paths) {
        Ok(())
    } else {
        Err("command does not run the nab-mcp tests".into())
    }
}

fn suite_command(cargo: &[&str], libtest: &[&str], paths: &[String]) -> bool {
    if cargo.iter().chain(libtest).any(|word| shell_opaque(word)) {
        return false;
    }
    if !(cargo.contains(&"cargo") && cargo.contains(&"test")) {
        return false;
    }
    let mut flags = RunFlags {
        includes: false,
        excludes: false,
        blocks: false,
        paths,
    };
    let exact = libtest_has_exact(libtest);
    scan(cargo, &mut flags, Phase::Cargo, exact);
    scan(libtest, &mut flags, Phase::Libtest, exact);
    !flags.blocks && (!flags.excludes || flags.includes)
}

#[derive(Clone, Copy)]
enum Phase {
    Cargo,
    Libtest,
}

struct RunFlags<'a> {
    includes: bool,
    excludes: bool,
    blocks: bool,
    paths: &'a [String],
}

fn scan(words: &[&str], flags: &mut RunFlags<'_>, phase: Phase, exact: bool) {
    let mut index = 0;
    let mut seen_cargo = false;
    let mut seen_test = false;
    while index < words.len() {
        let (key, inline) = split_flag(words[index]);
        let next = words.get(index + 1).copied();
        if matches!(phase, Phase::Cargo) && !key.starts_with('-') {
            if key == "cargo" && !seen_cargo {
                seen_cargo = true;
                index += 1;
                continue;
            }
            if key == "test" && !seen_test {
                seen_test = true;
                index += 1;
                continue;
            }
        }
        index += match phase {
            Phase::Cargo => apply_flag(key, inline, next, flags, exact),
            Phase::Libtest => apply_libtest(key, inline, next, flags, exact),
        };
    }
}

fn libtest_has_exact(words: &[&str]) -> bool {
    let mut index = 0;
    let mut exact = false;
    while index < words.len() {
        let (key, inline) = split_flag(words[index]);
        if key == "--exact" {
            exact = true;
        }
        index += libtest_advance(key, inline);
    }
    exact
}

fn libtest_advance(key: &str, inline: Option<&str>) -> usize {
    if key == "--skip"
        || (matches!(key, "--test-threads" | "--color" | "--format") && inline.is_none())
    {
        usize::from(inline.is_none()) + 1
    } else {
        1
    }
}

fn apply_flag(
    key: &str,
    inline: Option<&str>,
    next: Option<&str>,
    flags: &mut RunFlags<'_>,
    exact: bool,
) -> usize {
    if stops_suite(key) {
        flags.blocks = true;
    }
    if key == "--tests" || key == "--all-targets" {
        flags.includes = true;
    }
    if matches!(
        key,
        "--lib" | "--bins" | "--doc" | "--examples" | "--benches"
    ) {
        flags.excludes = true;
    }
    if matches!(key, "--bin" | "--example" | "--bench") {
        flags.excludes = true;
        return usize::from(inline.is_none()) + 1;
    }
    if key == "--test" {
        if inline
            .or(next)
            .is_some_and(|name| plain(name) == "mcp_2026")
        {
            flags.includes = true;
        } else {
            flags.excludes = true;
        }
        return usize::from(inline.is_none()) + 1;
    }
    if key == "--skip" {
        if skip_omits(inline, next, flags, exact) {
            flags.blocks = true;
        }
        return usize::from(inline.is_none()) + 1;
    }
    if takes_value(key) && inline.is_none() {
        return 2;
    }
    // A positional filter and an option this guard does not recognize both fail it.
    if !recognized_cargo(key) {
        flags.blocks = true;
    }
    1
}

fn apply_libtest(
    key: &str,
    inline: Option<&str>,
    next: Option<&str>,
    flags: &mut RunFlags<'_>,
    exact: bool,
) -> usize {
    if key == "--skip" {
        if skip_omits(inline, next, flags, exact) {
            flags.blocks = true;
        }
    } else if matches!(key, "--bench" | "--ignored" | "--list" | "--help" | "-h") {
        flags.blocks = true;
    } else if !key.starts_with('-') {
        // A word here that is not an option value is a test-name filter.
        flags.blocks = true;
    }
    libtest_advance(key, inline)
}

fn skip_omits(inline: Option<&str>, next: Option<&str>, flags: &RunFlags<'_>, exact: bool) -> bool {
    inline
        .or(next)
        .is_some_and(|name| omits_suite(&plain(name), flags.paths, exact))
}

fn shell_opaque(word: &str) -> bool {
    word.chars().any(|ch| {
        matches!(
            ch,
            '$' | '\\' | '`' | ';' | '&' | '|' | '<' | '>' | '{' | '}' | '*' | '?' | '[' | ']'
        )
    })
}

fn plain(value: &str) -> String {
    value
        .chars()
        .filter(|ch| *ch != '"' && *ch != '\'')
        .collect()
}

fn stops_suite(key: &str) -> bool {
    matches!(key, "--no-run" | "--list" | "--help" | "-h" | "--ignored")
}

fn omits_suite(name: &str, paths: &[String], exact: bool) -> bool {
    if exact {
        return paths.iter().any(|path| path == name);
    }
    name.is_empty()
        || name.contains("net_cases")
        || name.contains("mcp_2026")
        || paths.iter().any(|path| path.contains(name))
}

fn required_paths(manifest: &Path) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for entry in fs::read_dir(manifest.join("tests/mcp2026")).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let Some(module) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let text = fs::read_to_string(&path).map_err(|error| error.to_string())?;
        names.extend(task_tests(module, &text));
    }
    if names.is_empty() {
        return Err("required test names absent".into());
    }
    Ok(names)
}

fn task_tests(module: &str, text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut absent_on_task = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "#[cfg(not(feature = \"task\"))]" {
            absent_on_task = true;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("fn tp_") {
            if !absent_on_task {
                let end = rest.find(['(', ' ', '\t']).unwrap_or(rest.len());
                names.push(format!("{module}::tp_{}", &rest[..end]));
            }
            absent_on_task = false;
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        absent_on_task = false;
    }
    names
}

fn split_flag(word: &str) -> (&str, Option<&str>) {
    match word.split_once('=') {
        Some((key, value)) if key.starts_with('-') => (key, Some(value)),
        _ => (word, None),
    }
}

fn recognized_cargo(key: &str) -> bool {
    matches!(
        key,
        "--locked"
            | "--offline"
            | "--features"
            | "-F"
            | "--jobs"
            | "-j"
            | "--test"
            | "--tests"
            | "--all-targets"
            | "--lib"
            | "--bins"
            | "--doc"
            | "--examples"
            | "--benches"
            | "--bin"
            | "--example"
            | "--bench"
            | "--skip"
            | "--include-ignored"
    )
}

fn takes_value(key: &str) -> bool {
    matches!(key, "--features" | "-F" | "--jobs" | "-j")
}

pub(super) fn yaml_code(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut single = false;
    let mut double = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\'' && !double {
            single = !single;
            index += 1;
            continue;
        }
        if byte == b'"' && !single {
            double = !double;
            index += 1;
            continue;
        }
        if byte == b'\\' && double {
            index += 2;
            continue;
        }
        if byte == b'#' && !single && !double {
            return &line[..index];
        }
        index += 1;
    }
    line
}

fn is_job_key(line: &str) -> bool {
    let code = yaml_code(line).trim_end();
    let Some(body) = code.strip_prefix("  ") else {
        return false;
    };
    if body.starts_with(' ') {
        return false;
    }
    match body.split_once(':') {
        Some((key, value)) => !key.is_empty() && !key.contains(' ') && value.trim().is_empty(),
        None => false,
    }
}

pub(super) fn enclosing_job(text: &str, at: usize) -> Result<&str, String> {
    let mut start = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let next = offset + line.len();
        if is_job_key(line.trim_end_matches(['\n', '\r'])) {
            if offset > at {
                let begin = start.ok_or_else(|| "job key absent".to_string())?;
                return Ok(&text[begin..offset]);
            }
            start = Some(offset);
        }
        offset = next;
    }
    let begin = start.ok_or_else(|| "job key absent".to_string())?;
    Ok(&text[begin..])
}

fn block_header(value: &str) -> bool {
    let Some((indicator, rest)) = value.split_at_checked(1) else {
        return false;
    };
    if indicator != "|" && indicator != ">" {
        return false;
    }
    let mut saw_chomp = false;
    let mut saw_digit = false;
    for byte in rest.bytes() {
        match byte {
            b'+' | b'-' if !saw_chomp => saw_chomp = true,
            b'1'..=b'9' if !saw_digit => saw_digit = true,
            _ => return false,
        }
    }
    true
}

fn opens_block(trimmed: &str) -> bool {
    trimmed
        .rsplit_once(':')
        .is_some_and(|(_, value)| block_header(value.trim()))
}

fn each_real_line(text: &str, mut visit: impl FnMut(usize, &str)) {
    let mut block_at = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let raw = line.trim_end_matches(['\n', '\r']);
        let code = yaml_code(raw);
        let trimmed = code.trim();
        if !trimmed.is_empty() {
            let indent = code.len() - code.trim_start().len();
            if block_at.is_some_and(|opened: usize| indent > opened) {
                offset += line.len();
                continue;
            }
            block_at = None;
            visit(offset, raw);
            if opens_block(trimmed) {
                block_at = Some(indent);
            }
        }
        offset += line.len();
    }
}

fn find_real(text: &str, pred: impl Fn(&str) -> bool) -> Option<usize> {
    let mut found = None;
    each_real_line(text, |offset, raw| {
        if found.is_none() && pred(raw) {
            found = Some(offset);
        }
    });
    found
}

pub(super) fn real_line(text: &str, needle: &str) -> Option<usize> {
    find_real(text, |raw| yaml_code(raw).trim() == needle)
}

pub(super) fn real_matrix_name(text: &str, name: &str) -> Option<usize> {
    let needle = format!("- name: {name}");
    let mut under_include = false;
    let mut found = None;
    each_real_line(text, |offset, raw| {
        if found.is_some() {
            return;
        }
        let trimmed = yaml_code(raw).trim();
        if trimmed == "include:" {
            under_include = true;
            return;
        }
        if trimmed == "exclude:" {
            under_include = false;
            return;
        }
        if under_include && line_indent(raw) == 10 && trimmed == needle {
            found = Some(offset);
        }
    });
    found
}

pub(super) fn has_setting_at(text: &str, needle: &str, indent: usize) -> bool {
    find_real(text, |raw| {
        line_indent(raw) == indent && yaml_code(raw).trim() == needle
    })
    .is_some()
}

pub(super) fn sibling_value(text: &str, prefix: &str) -> Option<String> {
    let at = find_real(text, |raw| {
        line_indent(raw) == 12 && yaml_code(raw).trim().starts_with(prefix)
    })?;
    let line = text[at..].split('\n').next().unwrap_or("");
    yaml_code(line)
        .trim()
        .strip_prefix(prefix)
        .map(str::to_string)
}

pub(super) fn real_job_key(text: &str, key: &str) -> Option<usize> {
    find_real(text, |raw| {
        is_job_key(raw)
            && yaml_code(raw)
                .trim_end()
                .strip_prefix("  ")
                .is_some_and(|body| body.strip_suffix(':') == Some(key))
    })
}

pub(super) fn has_setting(text: &str, needle: &str) -> bool {
    real_line(text, needle).is_some()
}

fn line_indent(raw: &str) -> usize {
    let code = yaml_code(raw);
    code.len() - code.trim_start().len()
}

fn deeper(text: &str, at: usize) -> &str {
    let line_end = text[at..]
        .find('\n')
        .map_or(text.len(), |index| at + index + 1);
    let indent = line_indent(&text[at..line_end]);
    let mut block_at = None;
    let mut end = line_end;
    let mut offset = line_end;
    for line in text[line_end..].split_inclusive('\n') {
        let raw = line.trim_end_matches(['\n', '\r']);
        let trimmed = yaml_code(raw).trim();
        if trimmed.is_empty() {
            offset += line.len();
            end = offset;
            continue;
        }
        let indent_here = line_indent(raw);
        if block_at.is_some_and(|opened: usize| indent_here > opened) {
            offset += line.len();
            end = offset;
            continue;
        }
        block_at = None;
        if indent_here <= indent {
            break;
        }
        if opens_block(trimmed) {
            block_at = Some(indent_here);
        }
        offset += line.len();
        end = offset;
    }
    &text[line_end..end]
}

fn names_net_assignment(trimmed: &str) -> bool {
    let Some(at) = trimmed.find("NAB_NET_TESTS") else {
        return false;
    };
    let rest = &trimmed[at + "NAB_NET_TESTS".len()..];
    let rest = rest
        .strip_prefix('"')
        .or_else(|| rest.strip_prefix('\''))
        .unwrap_or(rest);
    rest.trim_start().starts_with(':')
}

fn alias_env(trimmed: &str) -> bool {
    trimmed
        .strip_prefix("env:")
        .is_some_and(|rest| rest.trim_start().starts_with('*'))
}

fn nab_assignment(text: &str) -> bool {
    find_real(text, |raw| {
        let trimmed = yaml_code(raw).trim();
        names_net_assignment(trimmed) || alias_env(trimmed)
    })
    .is_some()
}

fn unreadable_key(trimmed: &str) -> bool {
    let bytes = trimmed.as_bytes();
    let Some(&quote) = bytes.first() else {
        return false;
    };
    if quote != b'"' {
        return false;
    }
    let mut index = 1;
    let mut escaped = false;
    let mut saw_escape = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            saw_escape = true;
            escaped = false;
            index += 1;
            continue;
        }
        if byte == b'\\' {
            escaped = true;
            index += 1;
            continue;
        }
        if byte == quote {
            return saw_escape && trimmed[index + 1..].trim_start().starts_with(':');
        }
        index += 1;
    }
    false
}

fn is_if_key(trimmed: &str) -> bool {
    if unreadable_key(trimmed) {
        return true;
    }
    let rest = trimmed
        .strip_prefix("\"if\"")
        .or_else(|| trimmed.strip_prefix("'if'"))
        .or_else(|| trimmed.strip_prefix("if"));
    rest.is_some_and(|rest| rest.trim_start().starts_with(':'))
}

pub(super) fn command_step_conditioned(window: &str) -> bool {
    let Some(step_at) = real_line(window, "- run: ${{ matrix.command }}") else {
        return false;
    };
    find_real(deeper(window, step_at), |raw| {
        is_if_key(yaml_code(raw).trim())
    })
    .is_some()
}

pub(super) fn job_condition_rejected(window: &str) -> bool {
    find_real(window, |raw| {
        let trimmed = yaml_code(raw).trim();
        line_indent(raw) == 4
            && is_if_key(trimmed)
            && trimmed != "if: needs.changes.outputs.code == 'true'"
    })
    .is_some()
}

pub(super) fn command_step_forwards(window: &str) -> bool {
    let Some(step_at) = real_line(window, "- run: ${{ matrix.command }}") else {
        return false;
    };
    let step = deeper(window, step_at);
    if nab_assignment(step) {
        return forwards_net(step);
    }
    find_real(window, |raw| {
        line_indent(raw) == 4 && yaml_code(raw).trim() == "env:"
    })
    .is_some_and(|at| forwards_net(deeper(window, at)))
}

pub(super) fn forwards_net(window: &str) -> bool {
    [
        "NAB_NET_TESTS: ${{ matrix.net_tests }}",
        "NAB_NET_TESTS: \"1\"",
        "NAB_NET_TESTS: '1'",
        "NAB_NET_TESTS: 1",
    ]
    .into_iter()
    .any(|needle| has_setting(window, needle))
}
