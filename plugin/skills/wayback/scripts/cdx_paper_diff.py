#!/usr/bin/env python3
"""Detect Wayback CDX digest changes for tracked prior-art papers."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


DEFAULT_CONFIG = Path("~/.claude/data/portfolio/cdx_paper_diff_config.json").expanduser()
DEFAULT_STATE = Path("~/.claude/data/portfolio/cdx_paper_diff_state.json").expanduser()
DEFAULT_GATEWAY_CONFIG = Path("~/github/mcp-gateway/gateway.yaml").expanduser()
DEFAULT_REPORT_DIR = Path("~/.claude/data/portfolio").expanduser()
DEFAULT_TEAM_ID = "1201daa3-35d8-4d9c-8700-1a131346902e"


class CdxPaperDiffError(RuntimeError):
    """Raised when a required CDX diff precondition is not met."""


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def cdx_now() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d%H%M%S")


def read_json(path: Path, default: Any) -> Any:
    if not path.exists():
        return default
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def write_json(path: Path, payload: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        "w",
        encoding="utf-8",
        dir=str(path.parent),
        delete=False,
    ) as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
        handle.write("\n")
        tmp_name = handle.name
    os.replace(tmp_name, path)


def json_from_noisy_stdout(stdout: str) -> Any:
    stdout = stdout.strip()
    if not stdout:
        raise CdxPaperDiffError("gateway returned empty stdout")
    for index, char in enumerate(stdout):
        if char not in "[{":
            continue
        try:
            return json.loads(stdout[index:])
        except json.JSONDecodeError:
            continue
    raise CdxPaperDiffError(f"gateway stdout did not contain JSON: {stdout[:200]}")


def invoke_gateway(
    tool: str,
    args: dict[str, Any],
    *,
    gateway_config: Path,
) -> Any:
    gateway_bin = shutil.which("mcp-gateway") or str(Path("~/.claude/bin/mcp-gateway").expanduser())
    capabilities_dir = gateway_config.parent / "capabilities"
    cli_tool = tool.removeprefix("fulcrum:")
    command = [
        gateway_bin,
        "tool",
        "--log-level",
        "error",
        "invoke",
        "-c",
        str(gateway_config),
        "-C",
        str(capabilities_dir),
        cli_tool,
        "--args",
        json.dumps(args, sort_keys=True),
        "-f",
        "json",
    ]
    try:
        completed = subprocess.run(
            command,
            check=False,
            capture_output=True,
            text=True,
            timeout=60,
        )
    except FileNotFoundError as exc:
        raise CdxPaperDiffError("mcp-gateway CLI not found on PATH") from exc
    except subprocess.TimeoutExpired as exc:
        raise CdxPaperDiffError(f"{tool} timed out after 60s") from exc

    if completed.returncode != 0:
        message = (completed.stderr or completed.stdout).strip()
        raise CdxPaperDiffError(f"{tool} failed: {message[:500]}")
    return json_from_noisy_stdout(completed.stdout)


def structured_payload(payload: Any) -> Any:
    if isinstance(payload, dict) and "structuredContent" in payload:
        return payload["structuredContent"]
    return payload


def paper_key(paper: dict[str, Any]) -> str:
    if paper.get("arxiv_id"):
        return f"arxiv:{paper['arxiv_id']}"
    if paper.get("url"):
        return f"url:{paper['url']}"
    raise CdxPaperDiffError("paper entry must define arxiv_id or url")


def paper_url(paper: dict[str, Any]) -> str:
    if paper.get("url"):
        return str(paper["url"])
    return f"arxiv.org/abs/{paper['arxiv_id']}"


def normalize_cdx_rows(payload: Any) -> list[dict[str, str]]:
    payload = structured_payload(payload)
    if not isinstance(payload, list) or not payload:
        return []

    header = payload[0]
    rows = payload[1:] if isinstance(header, list) else payload
    if not isinstance(header, list):
        raise CdxPaperDiffError("CDX payload must start with a header row")

    normalized: list[dict[str, str]] = []
    for row in rows:
        if not isinstance(row, list):
            continue
        values = {str(key): str(value) for key, value in zip(header, row)}
        if values.get("timestamp") and values.get("digest"):
            normalized.append(values)
    return normalized


def latest_snapshot(rows: list[dict[str, str]]) -> dict[str, str] | None:
    if not rows:
        return None
    return max(rows, key=lambda row: row.get("timestamp", ""))


def wayback_url(snapshot: dict[str, str]) -> str | None:
    timestamp = snapshot.get("timestamp")
    original = snapshot.get("original")
    if not timestamp or not original:
        return None
    return f"https://web.archive.org/web/{timestamp}/{original}"


def cdx_args_for_paper(
    paper: dict[str, Any],
    previous: dict[str, Any] | None,
    config: dict[str, Any],
) -> dict[str, Any]:
    args: dict[str, Any] = {
        "url": paper_url(paper),
        "collapse": "digest",
        "output": "json",
    }
    if config.get("limit"):
        args["limit"] = int(config["limit"])
    since = paper.get("from") or (previous or {}).get("last_checked_cdx") or config.get("default_from")
    if since:
        args["from"] = str(since)
    return args


def load_mock_payload(mock_payloads: dict[str, Any] | None, paper: dict[str, Any]) -> Any | None:
    if not mock_payloads:
        return None
    key = paper_key(paper)
    url = paper_url(paper)
    return mock_payloads.get(key) or mock_payloads.get(url) or mock_payloads.get(paper.get("arxiv_id", ""))


def resolve_linear_issue_id(
    paper: dict[str, Any],
    *,
    gateway_config: Path,
) -> str | None:
    if paper.get("linear_issue_id"):
        return str(paper["linear_issue_id"])
    identifier = paper.get("linear_identifier")
    if not identifier:
        return None
    payload = invoke_gateway(
        "fulcrum:linear_get_issue",
        {"identifier": str(identifier)},
        gateway_config=gateway_config,
    )
    payload = structured_payload(payload)
    issue = payload.get("issue") if isinstance(payload, dict) else None
    if not issue or not issue.get("id"):
        raise CdxPaperDiffError(f"linear_get_issue returned no id for {identifier}")
    return str(issue["id"])


def create_linear_issue(
    paper: dict[str, Any],
    change: dict[str, Any],
    *,
    config: dict[str, Any],
    gateway_config: Path,
) -> str:
    team_id = config.get("linear", {}).get("team_id") or DEFAULT_TEAM_ID
    title = f"[AUTO] CDX digest changed for {paper_key(paper)}"
    description = build_linear_body(paper, change)
    payload = invoke_gateway(
        "fulcrum:linear_create_issue",
        {
            "teamId": team_id,
            "title": title,
            "description": description,
            "priority": int(config.get("linear", {}).get("priority", 3)),
        },
        gateway_config=gateway_config,
    )
    payload = structured_payload(payload)
    issue = payload.get("issue") if isinstance(payload, dict) else None
    if not issue or not issue.get("id"):
        raise CdxPaperDiffError("linear_create_issue returned no issue id")
    return str(issue["id"])


def build_linear_body(paper: dict[str, Any], change: dict[str, Any]) -> str:
    return "\n".join(
        [
            f"CDX digest change detected for {paper_key(paper)}.",
            "",
            f"Source URL: {paper_url(paper)}",
            f"Previous digest: {change['previous_digest']}",
            f"New digest: {change['new_digest']}",
            f"Latest CDX timestamp: {change['latest_timestamp']}",
            f"Wayback URL: {change.get('wayback_url') or 'unavailable'}",
            "",
            "Action: re-read the source and update the corresponding prior-art assessment if wording changed.",
        ]
    )


def emit_linear_alert(
    paper: dict[str, Any],
    change: dict[str, Any],
    *,
    config: dict[str, Any],
    gateway_config: Path,
) -> dict[str, Any]:
    issue_id = resolve_linear_issue_id(paper, gateway_config=gateway_config)
    created_issue = False
    if not issue_id:
        if not config.get("linear", {}).get("create_untracked", False):
            return {"skipped": True, "reason": "no-linear-target"}
        issue_id = create_linear_issue(
            paper,
            change,
            config=config,
            gateway_config=gateway_config,
        )
        created_issue = True

    payload = invoke_gateway(
        "fulcrum:linear_add_comment",
        {"issueId": issue_id, "body": build_linear_body(paper, change)},
        gateway_config=gateway_config,
    )
    payload = structured_payload(payload)
    comment = payload.get("comment") if isinstance(payload, dict) else None
    return {
        "skipped": False,
        "created_issue": created_issue,
        "issue_id": issue_id,
        "comment_id": comment.get("id") if comment else None,
    }


def check_paper(
    paper: dict[str, Any],
    *,
    state: dict[str, Any],
    config: dict[str, Any],
    gateway_config: Path,
    mock_payloads: dict[str, Any] | None,
    now: str,
    now_cdx: str,
    linear_enabled: bool,
) -> dict[str, Any]:
    key = paper_key(paper)
    previous = state.get("papers", {}).get(key)
    query_args = cdx_args_for_paper(paper, previous, config)
    result: dict[str, Any] = {
        "key": key,
        "url": paper_url(paper),
        "query": query_args,
        "previous_digest": previous.get("digest") if previous else None,
        "status": "ok",
        "changed": False,
    }

    try:
        payload = load_mock_payload(mock_payloads, paper)
        if payload is None:
            payload = invoke_gateway("fulcrum:wayback_cdx", query_args, gateway_config=gateway_config)
        rows = normalize_cdx_rows(payload)
        snapshot = latest_snapshot(rows)
    except CdxPaperDiffError as exc:
        result.update({"status": "error", "error": str(exc)})
        return result

    if snapshot is None:
        result.update({"status": "no-cdx-rows", "row_count": 0})
        state.setdefault("papers", {})[key] = {
            "digest": previous.get("digest") if previous else None,
            "last_checked": now,
            "last_checked_cdx": now_cdx,
            "last_status": "no-cdx-rows",
            "url": paper_url(paper),
        }
        return result

    digest = snapshot["digest"]
    changed = bool(previous and previous.get("digest") and previous.get("digest") != digest)
    result.update(
        {
            "row_count": len(rows),
            "latest_digest": digest,
            "latest_timestamp": snapshot.get("timestamp"),
            "wayback_url": wayback_url(snapshot),
            "changed": changed,
        }
    )

    state.setdefault("papers", {})[key] = {
        "digest": digest,
        "last_checked": now,
        "last_checked_cdx": now_cdx,
        "last_status": "ok",
        "latest_timestamp": snapshot.get("timestamp"),
        "url": paper_url(paper),
        "wayback_url": wayback_url(snapshot),
    }

    if changed:
        change = {
            "previous_digest": previous["digest"],
            "new_digest": digest,
            "latest_timestamp": snapshot.get("timestamp"),
            "wayback_url": wayback_url(snapshot),
        }
        if linear_enabled:
            result["linear"] = emit_linear_alert(
                paper,
                change,
                config=config,
                gateway_config=gateway_config,
            )
        else:
            result["linear"] = {"skipped": True, "reason": "linear-disabled"}
    return result


def default_config() -> dict[str, Any]:
    return {
        "schema_version": "cdx-paper-diff-config/v1",
        "default_from": "19960101",
        "limit": 50,
        "max_error_rate": 0.05,
        "gateway_config": str(DEFAULT_GATEWAY_CONFIG),
        "linear": {
            "enabled": False,
            "create_untracked": False,
            "team_id": DEFAULT_TEAM_ID,
            "priority": 3,
        },
        "papers": [
            {
                "arxiv_id": "2507.21474",
                "linear_identifier": "MIK-3355",
                "note": "MIK-3355 verified digest-diff smoke target.",
            }
        ],
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=DEFAULT_CONFIG)
    parser.add_argument("--state", type=Path, default=DEFAULT_STATE)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--gateway-config", type=Path)
    parser.add_argument("--mock-cdx-file", type=Path)
    parser.add_argument("--linear", action="store_true", help="Enable Linear comments/issue creation for detected changes.")
    parser.add_argument("--no-linear", action="store_true", help="Force-disable Linear writes.")
    parser.add_argument("--strict", action="store_true", help="Exit non-zero when error/no-row rate exceeds max_error_rate.")
    parser.add_argument("--init-config", action="store_true", help="Write a starter config if none exists, then exit.")
    return parser.parse_args()


def main() -> int:
    args = parse_args()

    if args.init_config:
        if args.config.exists():
            print(f"config_exists={args.config}")
            return 0
        write_json(args.config, default_config())
        print(f"config_written={args.config}")
        return 0

    config = read_json(args.config, default_config())
    state = read_json(args.state, {"schema_version": "cdx-paper-diff-state/v1", "papers": {}})
    mock_payloads = read_json(args.mock_cdx_file, None) if args.mock_cdx_file else None
    gateway_config = args.gateway_config or Path(config.get("gateway_config", DEFAULT_GATEWAY_CONFIG)).expanduser()
    now = utc_now()
    now_cdx = cdx_now()
    linear_enabled = bool(config.get("linear", {}).get("enabled", False) or args.linear)
    if args.no_linear:
        linear_enabled = False

    papers = config.get("papers") or []
    if not papers:
        raise CdxPaperDiffError(f"no papers configured in {args.config}")

    results = [
        check_paper(
            paper,
            state=state,
            config=config,
            gateway_config=gateway_config,
            mock_payloads=mock_payloads,
            now=now,
            now_cdx=now_cdx,
            linear_enabled=linear_enabled,
        )
        for paper in papers
    ]

    total = len(results)
    changed = sum(1 for result in results if result.get("changed"))
    failed = sum(1 for result in results if result.get("status") in {"error", "no-cdx-rows"})
    error_rate = failed / max(total, 1)
    report = {
        "schema_version": "cdx-paper-diff-run/v1",
        "checked_at": now,
        "summary": {
            "papers": total,
            "changed": changed,
            "failed_or_empty": failed,
            "error_rate": error_rate,
            "linear_enabled": linear_enabled,
            "fallback_required": error_rate > float(config.get("max_error_rate", 0.05)),
        },
        "results": results,
    }

    write_json(args.state, state)
    report_path = args.report or DEFAULT_REPORT_DIR / f"cdx-paper-diff-run-{now}.json"
    write_json(report_path, report)
    print(json.dumps(report, indent=2, sort_keys=True))
    print(f"report_path={report_path}", file=sys.stderr)

    if args.strict and report["summary"]["fallback_required"]:
        return 2
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except CdxPaperDiffError as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
