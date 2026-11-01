#!/usr/bin/env bash
# Build the OKF conformance oracle once.
# Pinned to openwiki@0.7.2 — the version measured in the SP comparison
# benchmarks (/tmp/ow-bench provenance: npm install openwiki@^0.7.2).
# Cached: re-run is a no-op when the CLI entry exists.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
DEST="$HERE/openwiki"
PIN="0.7.2"
CLI="$DEST/node_modules/openwiki/dist/cli/cli.js"
if [ -f "$CLI" ]; then echo "oracle cached (openwiki@$PIN)"; exit 0; fi
mkdir -p "$DEST"
cat > "$DEST/package.json" <<EOF
{"dependencies": {"openwiki": "$PIN"}}
EOF
npm install --prefix "$DEST" --no-audit --no-fund
node -e "console.log('oracle:', require('$DEST/node_modules/openwiki/package.json').version)"
