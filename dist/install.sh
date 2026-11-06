#!/usr/bin/env bash
# Build and install pivot for the current user, autostart it with COSMIC, and bind Alt+Tab.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN_DIR="${HOME}/.local/bin"
CFG="${XDG_CONFIG_HOME:-$HOME/.config}"

echo ":: building (release)"
cargo build --release --locked 2>/dev/null || cargo build --release

install -Dm755 target/release/pivot "$BIN_DIR/pivot"
PIVOT="$BIN_DIR/pivot"
echo ":: installed $PIVOT"

install -Dm644 /dev/stdin "$CFG/autostart/pivot.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Pivot window switcher
Exec=$PIVOT daemon
NoDisplay=true
X-GNOME-Autostart-enabled=true
DESKTOP
echo ":: autostart entry written ($CFG/autostart/pivot.desktop)"

if [ ! -f "$CFG/pivot/config.toml" ]; then
    install -Dm644 dist/config.toml "$CFG/pivot/config.toml"
    echo ":: default config written ($CFG/pivot/config.toml)"
fi

# Custom shortcuts override COSMIC's built-in WindowSwitcher bindings for the same keys.
# Absolute path: ~/.local/bin is often not on the compositor's PATH.
"$PIVOT" setup-shortcuts "$PIVOT"

"$PIVOT" quit 2>/dev/null || true
(setsid "$PIVOT" daemon >/dev/null 2>&1 &)
echo ":: daemon started. Press Alt+Tab."
