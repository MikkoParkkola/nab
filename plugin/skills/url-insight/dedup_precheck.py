#!/usr/bin/env python3
"""URL canonicalization and dedup helpers for the url-insight skill."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from datetime import UTC, date, datetime
from typing import Any
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

STRIP_QUERY_PARAMS = {
    "fbclid",
    "gclid",
    "ref",
    "source",
    "utm_campaign",
    "utm_content",
    "utm_medium",
    "utm_source",
    "utm_term",
}
ROI_RE = re.compile(r"\bROI:?\s*([0-9]+(?:\.[0-9]+)?x|low|medium|high)\b", re.IGNORECASE)
ISSUE_RE = re.compile(r"\b[A-Z]+-\d+\b")


@dataclass(frozen=True)
class DedupHit:
    identifier: str
    title: str
    url: str
    verdict: str
    roi: str

    def message(self) -> str:
        return (
            f"Already triaged: {self.identifier}, verdict={self.verdict}, "
            f"ROI={self.roi} — {self.url}"
        )


def canonicalize_url(raw_url: str) -> str:
    """Return the stable URL form used before any fetch work starts."""

    parsed = urlsplit(raw_url.strip())
    scheme = (parsed.scheme or "https").lower()
    hostname = (parsed.hostname or "").lower()
    if not hostname:
        raise ValueError(f"URL is missing host: {raw_url!r}")

    netloc = hostname
    if parsed.port and not (
        (scheme == "http" and parsed.port == 80) or (scheme == "https" and parsed.port == 443)
    ):
        netloc = f"{hostname}:{parsed.port}"

    path = parsed.path or "/"
    if path != "/":
        path = path.rstrip("/")
    query_pairs = sorted(
        [
            (key, value)
            for key, value in parse_qsl(parsed.query, keep_blank_values=True)
            if not _strip_query_param(key)
        ]
    )
    query = urlencode(query_pairs, doseq=True)
    return urlunsplit((scheme, netloc, path, query, ""))


def _strip_query_param(key: str) -> bool:
    lowered = key.lower()
    return lowered in STRIP_QUERY_PARAMS or lowered.startswith("utm_")


def canonical_hash(canonical_url: str) -> str:
    return hashlib.sha256(canonical_url.encode("utf-8")).hexdigest()


def dedup_key(canonical_url: str, created_on: date | None = None) -> str:
    day = created_on or datetime.now(UTC).date()
    return f"url-{canonical_hash(canonical_url)}-{day.isoformat()}"


def search_terms(canonical_url: str) -> list[str]:
    parsed = urlsplit(canonical_url)
    terms = _domain_terms(parsed.hostname or "")
    path_parts = [part for part in parsed.path.split("/") if part]
    if len(path_parts) >= 2 and parsed.hostname == "github.com":
        terms.append("/".join(path_parts[:2]))
    elif path_parts:
        terms.append(path_parts[-1])
    return [term for term in terms if term]


def build_hebb_pin_body(
    *,
    original_url: str,
    canonical_url: str,
    linear_identifier: str,
    verdict: str,
    roi: str,
    run_artifact_path: str,
) -> str:
    return (
        f"url={original_url}\n"
        f"canonical_url={canonical_url}\n"
        f"canonical_hash={canonical_hash(canonical_url)}\n"
        f"linear={linear_identifier}\n"
        f"verdict={verdict}\n"
        f"roi={roi}\n"
        f"run_artifact_path={run_artifact_path}"
    )


def extract_linear_hits(canonical_url: str, payload: Mapping[str, Any]) -> list[DedupHit]:
    nodes = _issue_nodes(payload)
    hits: list[DedupHit] = []
    for node in nodes:
        title = str(node.get("title") or "")
        haystack = f"{title} {node.get('description') or ''} {node.get('url') or ''}".lower()
        if not _matches_linear_terms(canonical_url, haystack):
            continue
        identifier = str(node.get("identifier") or _identifier_from_text(haystack) or "")
        if not identifier:
            continue
        hits.append(
            DedupHit(
                identifier=identifier,
                title=title,
                url=str(node.get("url") or ""),
                verdict=str(node.get("verdict") or "HUMAN-REVIEW"),
                roi=str(node.get("roi") or _roi_from_title(title) or "unknown"),
            )
        )
    return hits


def _matches_linear_terms(canonical_url: str, haystack: str) -> bool:
    parsed = urlsplit(canonical_url)
    domain_terms = _domain_terms(parsed.hostname or "")
    path_parts = [part.lower() for part in parsed.path.split("/") if part]
    if not domain_terms:
        return False
    if parsed.hostname == "github.com" and len(path_parts) >= 2:
        return f"{path_parts[0]}/{path_parts[1]}" in haystack
    domain_matches = any(term.lower() in haystack for term in domain_terms)
    path_matches = all(part in haystack for part in path_parts[-1:])
    return domain_matches and path_matches


def _domain_terms(hostname: str) -> list[str]:
    if not hostname:
        return []
    hostname = hostname.lower()
    terms = [hostname]
    if hostname.startswith("www."):
        terms.append(hostname[4:])
    labels = hostname[4:].split(".") if hostname.startswith("www.") else hostname.split(".")
    if len(labels) >= 2:
        terms.append(labels[-2])
    return list(dict.fromkeys(terms))


def _issue_nodes(payload: Mapping[str, Any]) -> list[Mapping[str, Any]]:
    if isinstance(payload.get("issues"), list):
        return [node for node in payload["issues"] if isinstance(node, Mapping)]
    nodes = (
        payload.get("data", {}).get("team", {}).get("issues", {}).get("nodes", [])
        if isinstance(payload.get("data"), Mapping)
        else []
    )
    return [node for node in nodes if isinstance(node, Mapping)]


def _identifier_from_text(text: str) -> str | None:
    match = ISSUE_RE.search(text.upper())
    return match.group(0) if match else None


def _roi_from_title(title: str) -> str | None:
    match = ROI_RE.search(title)
    return match.group(1) if match else None


def inspect_url(url: str, linear_payload: Mapping[str, Any] | None = None) -> dict[str, Any]:
    canonical_url = canonicalize_url(url)
    hits = extract_linear_hits(canonical_url, linear_payload or {})
    return {
        "original_url": url,
        "canonical_url": canonical_url,
        "canonical_hash": canonical_hash(canonical_url),
        "dedup_key_prefix": f"url-{canonical_hash(canonical_url)}",
        "hebb_recall_query": canonical_hash(canonical_url),
        "hebb_project": "url-insight",
        "linear_search_terms": search_terms(canonical_url),
        "dedup_hit": hits[0].__dict__ if hits else None,
        "short_circuit": bool(hits),
        "message": hits[0].message() if hits else "No prior triage found; proceed with fetch.",
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Prepare url-insight dedup pre-check metadata.")
    parser.add_argument("--url", required=True)
    parser.add_argument(
        "--linear-json",
        type=argparse.FileType("r", encoding="utf-8"),
        help="Optional Linear search JSON fixture/result for offline hit extraction.",
    )
    parser.add_argument("--json", action="store_true", help="Emit JSON instead of text.")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    linear_payload = json.load(args.linear_json) if args.linear_json else None
    report = inspect_url(args.url, linear_payload=linear_payload)
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(report["message"])
        print(f"canonical_url: {report['canonical_url']}")
        print(f"canonical_hash: {report['canonical_hash']}")
        print(f"linear_search_terms: {', '.join(report['linear_search_terms'])}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
