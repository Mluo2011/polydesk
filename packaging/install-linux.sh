#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/polydesk"

if [[ ! -x "$BIN" ]]; then
  echo "Building native Linux GPU binary..."
  (cd "$ROOT" && cargo build --release --bin polydesk)
fi

mkdir -p \
  "$HOME/.local/bin" \
  "$HOME/.local/share/applications" \
  "$HOME/.local/share/icons/hicolor/256x256/apps" \
  "$HOME/.local/share/icons/hicolor/scalable/apps"

install -m 755 "$BIN" "$HOME/.local/bin/polydesk"

if [[ -f "$ROOT/assets/polydesk.png" ]]; then
  install -m 644 "$ROOT/assets/polydesk.png" "$HOME/.local/share/icons/hicolor/256x256/apps/polydesk.png"
fi
if [[ -f "$ROOT/assets/polydesk.svg" ]]; then
  install -m 644 "$ROOT/assets/polydesk.svg" "$HOME/.local/share/icons/hicolor/scalable/apps/polydesk.svg"
fi

cat > "$HOME/.local/share/applications/polydesk.desktop" <<EOF
[Desktop Entry]
Version=1.0
Type=Application
Name=PolyDesk
Name[zh_CN]=PolyDesk 交易终端
GenericName=Polymarket Trading Terminal
GenericName[zh_CN]=Polymarket 极速交易终端
Comment=Native Low-Latency Polymarket CLOB V2 Trading Terminal
Comment[zh_CN]=纯本地低延迟 Polymarket CLOB V2 交易终端
Exec=$HOME/.local/bin/polydesk
Icon=polydesk
Terminal=false
Categories=Finance;Network;
Keywords=Polymarket;Trading;Terminal;Crypto;CLOB;
StartupNotify=true
StartupWMClass=PolyDesk
EOF

chmod 644 "$HOME/.local/share/applications/polydesk.desktop"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$HOME/.local/share/applications" || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "Installed PolyDesk as a desktop app in your Applications Menu!"
echo "  Binary : $HOME/.local/bin/polydesk"
echo "  Menu   : $HOME/.local/share/applications/polydesk.desktop"
