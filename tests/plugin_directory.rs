//! The shipped Claude plugin is one directory bundle.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn plugin_dir() -> PathBuf {
    repo_root().join("plugin")
}

fn read_json(path: &Path) -> serde_json::Value {
    let text =
        fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|err| panic!("read dir {}: {err}", dir.display()));
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" || name == "target" || name == "cache" {
            continue;
        }
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn words_outside_fences(text: &str) -> usize {
    let mut outside = String::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            outside.push_str(line);
            outside.push(' ');
        }
    }
    outside.split_whitespace().count()
}

#[test]
fn one_plugin_manifest() {
    let root = repo_root();
    assert!(
        !root.join(".claude-plugin/plugin.json").exists(),
        "root .claude-plugin/plugin.json must be removed"
    );
    let mut found = Vec::new();
    walk(&root, &mut found);
    let manifests: Vec<_> = found
        .iter()
        .filter(|path| path.file_name().and_then(|n| n.to_str()) == Some("plugin.json"))
        .collect();
    assert_eq!(
        manifests.len(),
        1,
        "expected one plugin.json, found {manifests:?}"
    );
    assert_eq!(
        manifests[0].as_path(),
        root.join("plugin/.claude-plugin/plugin.json")
    );
}

#[test]
fn version_matches_github_latest_release() {
    let manifest = read_json(&plugin_dir().join(".claude-plugin/plugin.json"));
    let version = manifest["version"].as_str().expect("plugin version string");
    assert_eq!(version, "0.12.3");

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("nab-plugin-directory-test")
        .build()
        .expect("http client");
    let response = client
        .get("https://api.github.com/repos/MikkoParkkola/nab/releases/latest")
        .header("Accept", "application/vnd.github+json")
        .send()
        .expect("github releases/latest")
        .error_for_status()
        .expect("github releases/latest status");
    let body: serde_json::Value = response.json().expect("github release json");
    let tag = body["tag_name"].as_str().expect("tag_name");
    let latest = tag.trim_start_matches('v');
    assert_eq!(
        version, latest,
        "plugin version must equal GitHub latest {tag}"
    );
}

#[test]
fn readme_has_examples_and_enough_words() {
    let readme = fs::read_to_string(plugin_dir().join("README.md")).expect("plugin README");
    assert!(
        words_outside_fences(&readme) >= 40,
        "README needs at least 40 words outside code fences"
    );
    let examples = readme
        .lines()
        .filter(|line| line.trim_start().starts_with("### Example"))
        .count();
    assert!(examples >= 3, "found {examples} example headings");
    assert!(readme.contains("### Example: Fetch a URL"));
    assert!(readme.contains("### Example: Fetch with the user's browser cookies"));
    assert!(readme.contains("### Example: Read an archive or Wayback page"));
}

#[test]
fn license_matches_root() {
    let root = fs::read(repo_root().join("LICENSE")).expect("root LICENSE");
    let plugin = fs::read(plugin_dir().join("LICENSE")).expect("plugin LICENSE");
    assert_eq!(root, plugin);
    let manifest = read_json(&plugin_dir().join(".claude-plugin/plugin.json"));
    assert_eq!(manifest["license"], "MIT");
}

#[test]
fn launcher_is_node_inside_the_plugin() {
    let plugin = plugin_dir();
    let mcp_path = plugin.join(".mcp.json");
    let mcp = read_json(&mcp_path);
    let compat = fs::read(&plugin.join("mcp.json")).expect("mcp.json");
    assert_eq!(fs::read(&mcp_path).expect(".mcp.json"), compat);
    let server = &mcp["mcpServers"]["nab"];
    let command = server["command"].as_str().expect("command");
    assert_eq!(command, "node");
    for shell in ["sh", "bash", "zsh", "cmd", "powershell"] {
        assert_ne!(command, shell);
    }
    let args = server["args"].as_array().expect("args");
    assert_eq!(args.len(), 1);
    let arg = args[0].as_str().expect("launcher arg");
    assert!(arg.starts_with("${CLAUDE_PLUGIN_ROOT}/"), "{arg}");
    let rel = arg.trim_start_matches("${CLAUDE_PLUGIN_ROOT}/");
    assert!(!rel.contains(".."), "{rel}");
    let launch = plugin.join(rel);
    assert!(launch.is_file(), "missing {}", launch.display());
    assert!(launch.starts_with(&plugin));
    let js = fs::read_to_string(&launch).expect("launcher source");
    assert!(js.contains("0.12.3"));
    assert!(js.contains("spawn("));
    assert!(js.contains("shell: false") || js.contains("shell:false"));
    assert!(
        !js.contains("exec("),
        "launcher must not use child_process exec"
    );
    assert!(!js.contains("execSync"));
    assert!(!js.contains("execFile"));
    assert!(!js.contains("process.env.PATH"));
    assert!(!js.contains("nab-mcp-wrapper"));
    assert!(!js.contains("command -v"));
    assert!(js.contains("https://github.com/MikkoParkkola/nab/releases/download/v0.12.3/"));
    assert!(!plugin.join("bin/nab-mcp-wrapper").exists());
}

#[test]
fn privacy_facts_and_no_email() {
    let plugin = plugin_dir();
    let privacy = fs::read_to_string(plugin.join("PRIVACY.md")).expect("PRIVACY.md");
    let lower = privacy.to_ascii_lowercase();
    assert!(lower.contains("cookie"), "privacy must mention cookies");
    assert!(
        lower.contains("stays on this machine"),
        "fetching must stay on the machine"
    );
    assert!(
        lower.contains("cookie values are not sent"),
        "cookie values must not be sent to the author"
    );
    assert!(
        lower.contains("the url you asked for"),
        "fetches must go to the URL the user asked for"
    );
    assert!(lower.contains("https://github.com/mikkoparkkola/nab/issues"));
    assert!(
        privacy.contains("Mikko Parkkola"),
        "support must name Mikko Parkkola"
    );
    assert!(
        lower.contains("stores its cookies on this machine"),
        "privacy must say where cookies are stored"
    );
    assert!(!privacy.contains('@'), "PRIVACY.md must not contain @");

    let email =
        regex::Regex::new(r"(?i)[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}").expect("email pattern");
    assert!(!email.is_match(&privacy));

    let mut files = Vec::new();
    walk(&plugin, &mut files);
    for path in files {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if let Some(found) = email.find(&text) {
            panic!(
                "{} contains an email address: {}",
                path.display(),
                found.as_str()
            );
        }
    }
}
