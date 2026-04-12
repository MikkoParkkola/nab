//! `nab mcp` — MCP server management (serve + install).
//!
//! - `nab mcp` / `nab mcp serve`: Start the MCP server on stdio (or HTTP).
//! - `nab mcp install`: Auto-configure an AI client's MCP config to point at nab.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

// ─── install ─────────────────────────────────────────────────────────────────

/// Supported MCP client targets.
#[derive(Clone, Debug)]
pub enum McpClient {
    ClaudeDesktop,
    ClaudeCode,
    Cursor,
    Windsurf,
    Codex,
    VSCode,
    Gemini,
    AmazonQ,
    Zed,
    LmStudio,
}

impl McpClient {
    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "claude-desktop" | "claude" => Ok(Self::ClaudeDesktop),
            "claude-code" => Ok(Self::ClaudeCode),
            "cursor" => Ok(Self::Cursor),
            "windsurf" => Ok(Self::Windsurf),
            "codex" => Ok(Self::Codex),
            "vscode" | "vs-code" | "copilot" => Ok(Self::VSCode),
            "gemini" => Ok(Self::Gemini),
            "amazon-q" | "q" => Ok(Self::AmazonQ),
            "zed" => Ok(Self::Zed),
            "lm-studio" => Ok(Self::LmStudio),
            other => bail!(
                "unknown client {other:?}\n\n\
                 Supported clients:\n  \
                 claude-desktop  Claude Desktop (default)\n  \
                 claude-code     Claude Code\n  \
                 cursor          Cursor\n  \
                 windsurf        Windsurf\n  \
                 codex           OpenAI Codex CLI\n  \
                 vscode          VS Code Copilot\n  \
                 gemini          Gemini CLI\n  \
                 amazon-q        Amazon Q Developer\n  \
                 zed             Zed\n  \
                 lm-studio       LM Studio"
            ),
        }
    }

    /// Config file path for this client.
    fn config_path(&self) -> Result<PathBuf> {
        let home = dirs::home_dir().context("cannot resolve home directory")?;
        let path = match self {
            Self::ClaudeDesktop => {
                #[cfg(target_os = "macos")]
                {
                    home.join("Library/Application Support/Claude/claude_desktop_config.json")
                }
                #[cfg(target_os = "linux")]
                {
                    home.join(".config/Claude/claude_desktop_config.json")
                }
                #[cfg(target_os = "windows")]
                {
                    let appdata = std::env::var("APPDATA")
                        .unwrap_or_else(|_| home.join("AppData/Roaming").display().to_string());
                    PathBuf::from(appdata).join("Claude/claude_desktop_config.json")
                }
            }
            Self::ClaudeCode => home.join(".claude.json"),
            Self::Cursor => home.join(".cursor/mcp.json"),
            Self::Windsurf => home.join(".codeium/windsurf/mcp_config.json"),
            Self::Codex => home.join(".codex/config.toml"),
            Self::VSCode => PathBuf::from(".vscode/mcp.json"), // workspace-relative
            Self::Gemini => home.join(".gemini/settings.json"),
            Self::AmazonQ => home.join(".aws/amazonq/mcp.json"),
            Self::Zed => {
                #[cfg(target_os = "macos")]
                {
                    home.join("Library/Application Support/Zed/settings.json")
                }
                #[cfg(not(target_os = "macos"))]
                {
                    home.join(".config/zed/settings.json")
                }
            }
            Self::LmStudio => home.join(".lm-studio/mcp.json"),
        };
        Ok(path)
    }

    fn display_name(&self) -> &'static str {
        match self {
            Self::ClaudeDesktop => "Claude Desktop",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::Windsurf => "Windsurf",
            Self::Codex => "Codex",
            Self::VSCode => "VS Code Copilot",
            Self::Gemini => "Gemini CLI",
            Self::AmazonQ => "Amazon Q Developer",
            Self::Zed => "Zed",
            Self::LmStudio => "LM Studio",
        }
    }

    /// JSON key for MCP server entries. Most clients use `mcpServers`, but
    /// VS Code Copilot uses `servers` and Zed uses `context_servers`.
    fn mcp_config_key(&self) -> &'static str {
        match self {
            Self::VSCode => "servers",
            Self::Zed => "context_servers",
            _ => "mcpServers",
        }
    }

    /// Whether this client uses TOML instead of JSON.
    fn is_toml(&self) -> bool {
        matches!(self, Self::Codex)
    }
}

pub struct InstallConfig {
    pub client: String,
    pub force: bool,
    pub dry_run: bool,
}

/// Resolve the absolute path to the `nab-mcp` binary.
///
/// Prefers a sibling of the currently running binary (e.g. installed via
/// `cargo install` or Homebrew puts both in the same directory). Falls back
/// to `$PATH` lookup.
fn nab_mcp_binary() -> Result<String> {
    // 1. Sibling of the current executable.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("nab-mcp");
        if sibling.is_file() {
            return Ok(sibling.display().to_string());
        }
    }

    // 2. $PATH lookup.
    if let Ok(path) = which::which("nab-mcp") {
        return Ok(path.display().to_string());
    }

    bail!(
        "cannot locate nab-mcp binary. \
         Install it first: cargo install nab (installs both nab and nab-mcp)"
    )
}

pub fn cmd_mcp_install(cfg: &InstallConfig) -> Result<()> {
    let client = McpClient::from_str(&cfg.client)?;
    let config_path = client.config_path()?;
    let binary = nab_mcp_binary()?;

    // Codex uses TOML, not JSON.
    if client.is_toml() {
        return cmd_mcp_install_toml(&config_path, &binary, &client, cfg.force, cfg.dry_run);
    }

    let key = client.mcp_config_key();

    // Load existing config or start fresh.
    let mut root: serde_json::Map<String, serde_json::Value> = if config_path.is_file() {
        let data = std::fs::read_to_string(&config_path)
            .with_context(|| format!("read {}", config_path.display()))?;
        if data.trim().is_empty() {
            serde_json::Map::new()
        } else {
            serde_json::from_str(&data).with_context(|| {
                format!(
                    "parse {} (fix the JSON or use --force to overwrite)",
                    config_path.display()
                )
            })?
        }
    } else {
        serde_json::Map::new()
    };

    // Ensure MCP servers section exists (key varies by client).
    let servers = root
        .entry(key)
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let servers = servers
        .as_object_mut()
        .with_context(|| format!("{key} is not a JSON object"))?;

    // Check for existing entry.
    if servers.contains_key("nab") && !cfg.force {
        if cfg.dry_run {
            println!(
                "nab is already installed in {}\n  would not change (use --force to overwrite)",
                config_path.display()
            );
        } else {
            println!(
                "nab is already installed in {}\nUse --force to overwrite.",
                config_path.display()
            );
        }
        return Ok(());
    }

    // Build the nab MCP server entry.
    let entry = serde_json::json!({
        "command": binary,
    });
    servers.insert("nab".to_string(), entry);

    let out = serde_json::to_string_pretty(&root)?;

    if cfg.dry_run {
        println!("Would write to {}:\n\n{}", config_path.display(), out);
        return Ok(());
    }

    // Create parent directory if needed.
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create config directory {}", parent.display()))?;
    }

    // Backup existing file.
    if config_path.is_file() {
        let backup = config_path.with_extension("nab.bak");
        if let Ok(data) = std::fs::read(&config_path) {
            let _ = std::fs::write(&backup, data);
            println!("  backup: {}", backup.display());
        }
    }

    std::fs::write(&config_path, out)
        .with_context(|| format!("write {}", config_path.display()))?;

    println!("Installed nab as MCP server for {}.", client.display_name());
    println!("  config: {}", config_path.display());
    println!("  binary: {binary}");
    println!();
    println!("Restart {} to pick up the change.", client.display_name());
    Ok(())
}

/// Install into a TOML config file (Codex).
fn cmd_mcp_install_toml(
    config_path: &std::path::Path,
    binary: &str,
    client: &McpClient,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    let existing = if config_path.is_file() {
        std::fs::read_to_string(config_path)
            .with_context(|| format!("read {}", config_path.display()))?
    } else {
        String::new()
    };

    // Check for existing entry.
    if existing.contains("[mcp_servers.nab]") && !force {
        if dry_run {
            println!(
                "nab is already installed in {}\n  would not change (use --force to overwrite)",
                config_path.display()
            );
        } else {
            println!(
                "nab is already installed in {}\nUse --force to overwrite.",
                config_path.display()
            );
        }
        return Ok(());
    }

    let block = format!(
        "\n[mcp_servers.nab]\ncommand = \"{binary}\"\n",
    );

    let out = if existing.contains("[mcp_servers.nab]") {
        // Force mode: replace the existing block (simple heuristic —
        // replace from [mcp_servers.nab] to the next [section] or EOF).
        let start = existing.find("[mcp_servers.nab]").unwrap();
        let rest = &existing[start + "[mcp_servers.nab]".len()..];
        let end = rest.find("\n[").map_or(existing.len(), |i| start + "[mcp_servers.nab]".len() + i);
        format!("{}{}{}", &existing[..start], block.trim(), &existing[end..])
    } else {
        format!("{}{}", existing.trim_end(), block)
    };

    if dry_run {
        println!("Would write to {}:\n\n{out}", config_path.display());
        return Ok(());
    }

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create config directory {}", parent.display()))?;
    }

    if config_path.is_file() {
        let backup = config_path.with_extension("nab.bak");
        if let Ok(data) = std::fs::read(config_path) {
            let _ = std::fs::write(&backup, data);
            println!("  backup: {}", backup.display());
        }
    }

    std::fs::write(config_path, out)
        .with_context(|| format!("write {}", config_path.display()))?;

    println!("Installed nab as MCP server for {}.", client.display_name());
    println!("  config: {}", config_path.display());
    println!("  binary: {binary}");
    println!();
    println!("Restart {} to pick up the change.", client.display_name());
    Ok(())
}

// ─── serve ───────────────────────────────────────────────────────────────────

pub struct ServeConfig {
    pub http: Option<String>,
    pub http_allow_origin: Option<String>,
}

/// Launch the MCP server by exec-ing `nab-mcp` with the same arguments.
///
/// This keeps all MCP server code in the `nab-mcp` binary (no duplication)
/// while providing `nab mcp [serve]` as a convenient entry point.
pub fn cmd_mcp_serve(cfg: &ServeConfig) -> Result<()> {
    let binary = nab_mcp_binary()?;

    let mut args = Vec::new();
    if let Some(ref bind) = cfg.http {
        args.push("--http".to_string());
        args.push(bind.clone());
    }
    if let Some(ref origin) = cfg.http_allow_origin {
        args.push("--http-allow-origin".to_string());
        args.push(origin.clone());
    }

    // exec replaces the current process — no fork overhead, no zombie.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&binary).args(&args).exec();
        // exec only returns on error.
        bail!("exec {binary}: {err}");
    }

    #[cfg(not(unix))]
    {
        let status = std::process::Command::new(&binary)
            .args(&args)
            .status()
            .with_context(|| format!("spawn {binary}"))?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── from_str ────────────────────────────────────────────────────────

    #[test]
    fn from_str_original_clients() {
        for name in &["claude-desktop", "claude", "claude-code", "cursor", "windsurf"] {
            assert!(McpClient::from_str(name).is_ok(), "should accept {name}");
        }
    }

    #[test]
    fn from_str_new_clients() {
        for name in &["codex", "vscode", "vs-code", "copilot", "gemini", "amazon-q", "q", "zed", "lm-studio"] {
            assert!(McpClient::from_str(name).is_ok(), "should accept {name}");
        }
    }

    #[test]
    fn from_str_rejects_unknown() {
        assert!(McpClient::from_str("not-a-client").is_err());
    }

    // ── config_path ─────────────────────────────────────────────────────

    #[test]
    fn config_path_claude_desktop() {
        let p = McpClient::ClaudeDesktop.config_path().unwrap();
        assert!(p.to_string_lossy().contains("claude_desktop_config.json"));
    }

    #[test]
    fn config_path_claude_code() {
        let p = McpClient::ClaudeCode.config_path().unwrap();
        assert!(p.to_string_lossy().ends_with(".claude.json"));
    }

    #[test]
    fn config_path_cursor() {
        let p = McpClient::Cursor.config_path().unwrap();
        assert!(p.to_string_lossy().contains(".cursor/mcp.json"));
    }

    #[test]
    fn config_path_windsurf() {
        let p = McpClient::Windsurf.config_path().unwrap();
        assert!(p.to_string_lossy().contains("windsurf/mcp_config.json"));
    }

    #[test]
    fn config_path_codex() {
        let p = McpClient::Codex.config_path().unwrap();
        assert!(p.to_string_lossy().contains(".codex/config.toml"));
    }

    #[test]
    fn config_path_vscode() {
        let p = McpClient::VSCode.config_path().unwrap();
        assert!(p.to_string_lossy().contains(".vscode/mcp.json"));
    }

    #[test]
    fn config_path_gemini() {
        let p = McpClient::Gemini.config_path().unwrap();
        assert!(p.to_string_lossy().contains(".gemini/settings.json"));
    }

    #[test]
    fn config_path_amazon_q() {
        let p = McpClient::AmazonQ.config_path().unwrap();
        assert!(p.to_string_lossy().contains("amazonq/mcp.json"));
    }

    #[test]
    fn config_path_zed() {
        let p = McpClient::Zed.config_path().unwrap();
        assert!(p.to_string_lossy().contains("settings.json"));
    }

    #[test]
    fn config_path_lm_studio() {
        let p = McpClient::LmStudio.config_path().unwrap();
        assert!(p.to_string_lossy().contains(".lm-studio/mcp.json"));
    }

    // ── mcp_config_key ──────────────────────────────────────────────────

    #[test]
    fn mcp_config_key_defaults_to_mcp_servers() {
        assert_eq!(McpClient::ClaudeDesktop.mcp_config_key(), "mcpServers");
        assert_eq!(McpClient::Cursor.mcp_config_key(), "mcpServers");
    }

    #[test]
    fn mcp_config_key_vscode_uses_servers() {
        assert_eq!(McpClient::VSCode.mcp_config_key(), "servers");
    }

    #[test]
    fn mcp_config_key_zed_uses_context_servers() {
        assert_eq!(McpClient::Zed.mcp_config_key(), "context_servers");
    }

    // ── is_toml ─────────────────────────────────────────────────────────

    #[test]
    fn is_toml_only_codex() {
        assert!(McpClient::Codex.is_toml());
        assert!(!McpClient::ClaudeDesktop.is_toml());
        assert!(!McpClient::VSCode.is_toml());
        assert!(!McpClient::Zed.is_toml());
    }
}
