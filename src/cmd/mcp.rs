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
}

impl McpClient {
    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "claude-desktop" | "claude" => Ok(Self::ClaudeDesktop),
            "claude-code" => Ok(Self::ClaudeCode),
            "cursor" => Ok(Self::Cursor),
            "windsurf" => Ok(Self::Windsurf),
            other => bail!(
                "unknown client {other:?} (supported: claude-desktop, claude-code, cursor, windsurf)"
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
        };
        Ok(path)
    }

    fn display_name(&self) -> &'static str {
        match self {
            Self::ClaudeDesktop => "Claude Desktop",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
            Self::Windsurf => "Windsurf",
        }
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
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("nab-mcp");
            if sibling.is_file() {
                return Ok(sibling.display().to_string());
            }
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

    // Ensure mcpServers section exists.
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let servers = servers
        .as_object_mut()
        .context("mcpServers is not a JSON object")?;

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
    println!("  binary: {}", binary);
    println!();
    println!(
        "Restart {} to pick up the change.",
        client.display_name()
    );
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
