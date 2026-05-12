#!/usr/bin/env python3
"""Refresh portfolio.json for url-insight cross-map (§4).

Source: gh repo list MikkoParkkola → owned + active + strategic filter.
Enrichment: local CLAUDE.md/README tagline, 30d commit count, clone path.
Output: ~/.claude/data/url-insight/portfolio.json

Run weekly (or when `generated_at > 14d old`).
"""
import json, subprocess, pathlib, re, datetime, sys

HOME = pathlib.Path("/Users/mikko/github")
OUT = pathlib.Path("/Users/mikko/.claude/data/url-insight/portfolio.json")
CUTOFF = "2026-03-01"  # pushedAt ≥ CUTOFF counts as "active"
DROP = {
    # Non-strategic (sites, profile README)
    "MikkoParkkola", "revaluator-legal",
    "revaluator-website", "domain-tracker",
}
DESIGN_CONTEXT_REPOS = {"parkkola-website"}


def extract_tagline(repo_name: str) -> str | None:
    """Prefer CLAUDE.md, fall back to README.md. Skip status/code-fence noise."""
    for fname in ("CLAUDE.md", "README.md"):
        p = HOME / repo_name / fname
        if not p.exists():
            continue
        try:
            text = p.read_text(errors="replace")[:4000]
        except Exception:
            continue
        # Prefer bolded line that isn't a status marker
        for line in text.splitlines()[:30]:
            m = re.match(r"^\s*\*\*(.+?)\*\*\s*$", line)
            if m:
                cand = m.group(1).strip()
                if 15 < len(cand) < 200 and not cand.lower().startswith(("status", "version", "this file")):
                    return cand
        # First real prose line
        for line in text.splitlines():
            s = line.strip()
            if not s or s.startswith(("#", "[!", "![", "<!--", "```", ">", "---", "- ", "* ", "|", "**Status")):
                continue
            if 20 < len(s) < 240:
                return s
    return None


def commits_30d(repo_name: str) -> int:
    p = HOME / repo_name
    if not (p / ".git").exists():
        return 0
    try:
        out = subprocess.check_output(
            ["git", "-C", str(p), "log", "--since=30 days ago", "--oneline"],
            text=True, stderr=subprocess.DEVNULL)
        return len([l for l in out.splitlines() if l.strip()])
    except subprocess.CalledProcessError:
        return 0


def has_design_context(repo_name: str) -> bool:
    p = HOME / repo_name / ".claude" / "skills" / f"{repo_name}-context" / "SKILL.md"
    return p.exists()


def main() -> int:
    try:
        gh_raw = subprocess.check_output(
            ["gh", "repo", "list", "MikkoParkkola", "--limit", "200", "--json",
             "name,description,isArchived,isFork,pushedAt,visibility,primaryLanguage"],
            text=True)
    except subprocess.CalledProcessError as e:
        print(f"gh repo list failed: {e}", file=sys.stderr)
        return 1

    repos = json.loads(gh_raw)
    active = [r for r in repos
              if not r["isArchived"]
              and not r["isFork"]
              and ((r["pushedAt"] or "") >= CUTOFF or r["name"] in DESIGN_CONTEXT_REPOS)
              and r["name"] not in DROP]

    enriched = []
    for r in active:
        name = r["name"]
        path = HOME / name
        enriched.append({
            "name": name,
            "path": str(path) if path.exists() else None,
            "description": r.get("description") or "",
            "tagline": extract_tagline(name) or (r.get("description") or ""),
            "visibility": r.get("visibility"),
            "language": (r.get("primaryLanguage") or {}).get("name") if r.get("primaryLanguage") else None,
            "pushed_at": r.get("pushedAt"),
            "commits_30d": commits_30d(name),
            "has_claude_md": (path / "CLAUDE.md").exists(),
            "has_design_context": has_design_context(name),
            "local_clone": path.exists(),
        })

    enriched.sort(key=lambda e: -e["commits_30d"])

    payload = {
        "generated_at": datetime.datetime.now(datetime.UTC).isoformat(),
        "source": "gh repo list MikkoParkkola + local ~/github enrichment",
        "filter": (f"owned (isFork=false) + not archived + "
                   f"(pushedAt >= {CUTOFF} or design-context tracked) + strategic only"),
        "count": len(enriched),
        "repos": enriched,
    }
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(payload, indent=2))
    design_context_count = sum(1 for e in enriched if e["has_design_context"])
    print(f"WROTE {OUT} — {len(enriched)} repos "
          f"({sum(1 for e in enriched if e['has_claude_md'])} w/ CLAUDE.md, "
          f"{design_context_count} w/ design context)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
