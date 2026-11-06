#!/usr/bin/env bash
set -euo pipefail
CFG="${XDG_CONFIG_HOME:-$HOME/.config}"
PIVOT="$HOME/.local/bin/pivot"
"$PIVOT" quit 2>/dev/null || true
"$PIVOT" remove-shortcuts "$PIVOT" 2>/dev/null || true
rm -f "$PIVOT" "$CFG/autostart/pivot.desktop"
echo ":: uninstalled (config kept in $CFG/pivot)"
