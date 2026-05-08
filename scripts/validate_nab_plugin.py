#!/usr/bin/env python3
"""Validate the nab Claude Code plugin package for MIK-3402."""

from __future__ import annotations

import json
import os
import re
import stat
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python < 3.11 fallback
    tomllib = None


ROOT = Path(__file__).resolve().parents[1]
PLUGIN = ROOT / "plugin"
MANIFEST = PLUGIN / ".claude-plugin" / "plugin.json"
REQUIRED_SKILLS = ("url-insight", "research", "wayback", "ia", "oreilly")


class Checks:
    def __init__(self) -> None:
        self.failures: list[str] = []

    def check(self, label: str, condition: bool, detail: str) -> None:
        status = "PASS" if condition else "FAIL"
        print(f"{status} {label}: {detail}")
        if not condition:
            self.failures.append(label)


def load_json(path: Path, checks: Checks, label: str) -> dict:
    if not path.exists():
        checks.check(label, False, f"{path.relative_to(ROOT)} exists")
        return {}
    try:
        return json.loads(path.read_text())
    except json.JSONDecodeError as exc:
        checks.check(label, False, f"{path.relative_to(ROOT)} parses as JSON: {exc}")
        return {}


def cargo_version() -> str | None:
    cargo_toml = ROOT / "Cargo.toml"
    if tomllib is not None:
        return tomllib.loads(cargo_toml.read_text())["package"]["version"]
    match = re.search(r'^version\s*=\s*"([^"]+)"', cargo_toml.read_text(), re.MULTILINE)
    return match.group(1) if match else None


def is_executable(path: Path) -> bool:
    return path.exists() and bool(path.stat().st_mode & stat.S_IXUSR)


def text(path: Path) -> str:
    return path.read_text() if path.exists() else ""


def validate_manifest(checks: Checks) -> dict:
    manifest = load_json(MANIFEST, checks, "MIK-3402.PLUG.1")
    if not manifest:
        return manifest

    version = cargo_version()
    checks.check("MIK-3402.PLUG.1", manifest.get("name") == "nab", "manifest name is nab")
    checks.check(
        "MIK-3402.PLUG.1",
        manifest.get("version") == version,
        f"manifest version matches Cargo.toml ({version})",
    )
    checks.check(
        "MIK-3402.PLUG.1",
        manifest.get("skills") == "./skills/",
        "plugin.json declares the skills directory",
    )
    checks.check(
        "MIK-3402.PLUG.1",
        "./commands/nab.md" in manifest.get("commands", []),
        "plugin.json declares the /nab command entry point",
    )
    checks.check(
        "MIK-3402.PLUG.1",
        manifest.get("hooks") == "./hooks/hooks.json",
        "plugin.json declares hooks/hooks.json",
    )
    checks.check(
        "MIK-3402.PLUG.1",
        manifest.get("mcpServers") == "./.mcp.json",
        "plugin.json declares .mcp.json",
    )
    author = manifest.get("author", {})
    checks.check(
        "MIK-3402.PLUG.1",
        "MikkoParkkola" in json.dumps(author) and "claude-elite" in json.dumps(manifest),
        "manifest carries nab and claude-elite attribution",
    )
    return manifest


def validate_skills(checks: Checks) -> None:
    for skill in REQUIRED_SKILLS:
        checks.check(
            "MIK-3402.SKILL.1",
            (PLUGIN / "skills" / skill / "SKILL.md").exists(),
            f"bundles skill {skill}",
        )
    sources = text(PLUGIN / "SOURCES.md")
    checks.check(
        "MIK-3402.SKILL.1",
        all(skill in sources for skill in REQUIRED_SKILLS) and "claude-elite" in sources,
        "SOURCES.md records skill provenance",
    )


def validate_command(checks: Checks) -> None:
    body = text(PLUGIN / "commands" / "nab.md")
    expected = (
        "/nab fetch <url>",
        "/nab fetch --cookies brave <url>",
        "/nab archive <url>",
        "/nab research <topic>",
    )
    checks.check(
        "MIK-3402.CMD.1",
        all(example in body for example in expected),
        "command documents fetch, Brave-cookie fetch, archive, and research entry points",
    )
    checks.check(
        "MIK-3402.CMD.1",
        "nab fetch" in body and "nab-mcp" in body and "$ARGUMENTS" in body,
        "command routes arguments to nab MCP or CLI fallback",
    )


def validate_hook(checks: Checks) -> None:
    hooks_path = PLUGIN / "hooks" / "hooks.json"
    hooks = load_json(hooks_path, checks, "MIK-3402.HOOK.1")
    hook_text = json.dumps(hooks)
    script = PLUGIN / "hooks" / "nab-yara-edge.sh"
    script_body = text(script)
    checks.check(
        "MIK-3402.HOOK.1",
        "PreToolUse" in hooks.get("hooks", {}) and "nab-yara-edge.sh" in hook_text,
        "PreToolUse hook invokes nab-yara-edge",
    )
    checks.check(
        "MIK-3402.HOOK.1",
        is_executable(script),
        "nab-yara-edge hook stub is executable",
    )
    checks.check(
        "MIK-3402.HOOK.1",
        all(token in script_body for token in ("WARN", "YARA-X", "MIK-3390")),
        "nab-yara-edge stub warns and cross-references MIK-3390",
    )


def validate_mcp(checks: Checks) -> None:
    mcp_path = PLUGIN / ".mcp.json"
    compat_path = PLUGIN / "mcp.json"
    mcp = load_json(mcp_path, checks, "MIK-3402.MCP.1")
    compat = load_json(compat_path, checks, "MIK-3402.MCP.1")
    server = mcp.get("mcpServers", {}).get("nab", {})
    checks.check(
        "MIK-3402.MCP.1",
        mcp == compat and bool(mcp),
        "mcp.json compatibility copy matches .mcp.json",
    )
    checks.check(
        "MIK-3402.MCP.1",
        server.get("command") == "${CLAUDE_PLUGIN_ROOT}/bin/nab-mcp-wrapper",
        "mcp.json auto-registers nab through the bundled wrapper",
    )
    wrapper = PLUGIN / "bin" / "nab-mcp-wrapper"
    wrapper_text = text(wrapper)
    checks.check(
        "MIK-3402.MCP.1",
        is_executable(wrapper) and "brew" in wrapper_text.lower() and "nab-mcp" in wrapper_text,
        "wrapper is executable and includes Homebrew/cargo fallback lookup",
    )


def validate_readme(checks: Checks) -> None:
    readme = text(PLUGIN / "README.md")
    checks.check(
        "MIK-3402.AUTH.1",
        all(token in readme for token in ("--cookies brave", "1Password", "auth-aware")),
        "README documents Brave cookies and 1Password auth-aware fetch",
    )
    checks.check(
        "MIK-3402.SHIP.1",
        all(
            token in readme
            for token in (
                "claude --plugin-dir ./plugin",
                "Authenticated Fetch",
                "Archived Snapshot Retrieval",
                "Multi-Source Research",
            )
        ),
        "README includes install instructions and three worked examples",
    )


def main() -> int:
    checks = Checks()
    validate_manifest(checks)
    validate_skills(checks)
    validate_command(checks)
    validate_hook(checks)
    validate_mcp(checks)
    validate_readme(checks)

    if checks.failures:
        print(f"\nMIK-3402 plugin validation failed: {len(checks.failures)} checks")
        return 1
    print("\nMIK-3402 plugin validation passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
