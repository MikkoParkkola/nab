# Changelog

All notable changes to nab will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] - 2026-03-13

### Added
- **MCP protocol 2025-11-25** — upgraded from 2025-06-18 via rust-mcp-sdk 0.8.3
- **URL elicitation** for OAuth/SSO login flows (Google, GitHub, Microsoft, Apple, Facebook, and 8 more providers)
- **Task-augmented execution** for `fetch_batch` — returns immediately with task ID, fetches in background with push notifications
- **Multi-select cookie elicitation** — pick multiple browser cookie stores (Brave, Chrome, Firefox, Safari) at once
- **Structured content** (`structured_content`) on 5 tools alongside human-readable text
- **Output schemas** (`outputSchema`) on fetch, fetch_batch, auth_lookup, fingerprint, benchmark
- **Tool annotations** on all 8 tools (read_only_hint, destructive_hint, open_world_hint)
- **Server icons** — globe SVG in light and dark themes
- Google Workspace site provider: extract Google Docs (markdown), Sheets (markdown table), and Slides (plain text) using browser cookie authentication
- Comments and suggested edits extraction from OOXML (docx/xlsx/pptx) for Google Workspace documents

### Changed
- MCP server module split into 6 files (tools, elicitation, helpers, structured, tests) for maintainability

### Fixed
- `stream --duration` flag now works for file output (was only working for player piping)
- `analyze` command now properly detects audio-only files and skips video frame extraction

### Changed
- Native HLS backend respects duration limit via segment counting
- FFmpeg backend passes duration via `-t` flag
