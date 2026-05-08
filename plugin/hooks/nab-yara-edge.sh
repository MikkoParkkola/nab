#!/usr/bin/env bash
set -euo pipefail

payload="$(cat || true)"

if printf '%s' "${payload}" | grep -Eiq 'nab([_-]mcp)?|nab fetch|mcp__.*nab.*fetch|NAB_YARA_BYPASS'; then
  cat <<'EOF'
nab-yara-edge WARN: nab fetch-time YARA-X scanning remains enforced by nab.
This plugin hook is a non-blocking edge warning stub for MIK-3390, not the full enforcement path.
Do not set NAB_YARA_BYPASS=1 except as an audited emergency operator escape hatch.
EOF
fi

exit 0
