#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

cargo build --release -p lantern

install -Dm755 target/release/lantern "$HOME/.local/bin/lantern"

for icon in lantern/icons/*.svg; do
  install -Dm644 "$icon" "$HOME/.local/share/icons/hicolor/scalable/status/$(basename "$icon")"
done

install -Dm644 lantern/lantern.desktop "$HOME/.local/share/applications/lantern.desktop"
install -Dm644 lantern/lantern.desktop "$HOME/.config/autostart/lantern.desktop"

echo "Installed. Start it with: setsid -f lantern"
