#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build clipper and wire it up for your user account. No sudo required —
# everything lands in ~/.local/bin, ~/.config/systemd/user, ~/.config/autostart.
#
# Undo:  bash uninstall.sh   (or see the notes at the bottom of this file)
set -euo pipefail
cd "$(dirname "$0")"

echo "== building (release) =="
cargo build --release

BIN="$HOME/.local/bin"
mkdir -p "$BIN" "$HOME/.config/systemd/user" "$HOME/.config/autostart" "$HOME/.config/clipper"

echo "== installing binaries to $BIN =="
install -m 755 target/release/clipctl "$BIN/clipctl"
install -m 755 target/release/voice-clipper "$BIN/voice-clipper"
# Old name from before the GUI was renamed; keeping it around confuses people.
rm -f "$BIN/clipgui"

echo "== phrases file =="
if [ -f "$HOME/.config/clipper/phrases.txt" ]; then
    echo "keeping your existing ~/.config/clipper/phrases.txt"
else
    "$BIN/clipctl" init
fi

echo "== systemd user service (the always-on listener) =="
# The wrapper must exist before the unit that runs it.
install -m 755 systemd/clipper-listen "$BIN/clipper-listen"
install -m 644 systemd/clipper.service "$HOME/.config/systemd/user/clipper.service"
systemctl --user daemon-reload
systemctl --user enable --now clipper

echo "== icons =="
# The taskbar and app launcher look these up by name (Icon=voice-clipper).
# The icon is a raster the user supplied (assets/voice-clipper.png), so the
# theme entries are just resized copies of it. `-background none` keeps the
# alpha channel; without it ImageMagick flattens onto white.
magick -background none assets/voice-clipper.png -resize 256x256 /tmp/vc256.png
magick -background none assets/voice-clipper.png -resize 512x512 /tmp/vc512.png
# The window icon is embedded at compile time with include_bytes!, so refresh it
# from the same artwork here; the committed gui/assets/icon.rgba is only the
# fallback for a machine that cannot run magick.
magick -background none assets/voice-clipper.png -resize 64x64 -depth 8 rgba:gui/assets/icon.rgba
# Fail loudly rather than shipping a white box again.
if [ "$(magick /tmp/vc256.png -format '%[fx:a]' info:)" != "0" ]; then
    echo "   WARNING: the rendered icon is not transparent at its corner" >&2
fi
install -D -m 644 /tmp/vc256.png "$HOME/.local/share/icons/hicolor/256x256/apps/voice-clipper.png"
install -D -m 644 /tmp/vc512.png "$HOME/.local/share/icons/hicolor/512x512/apps/voice-clipper.png"
# A stale scalable SVG would win over these PNGs in Plasma, so it goes away.
rm -f "$HOME/.local/share/icons/hicolor/scalable/apps/voice-clipper.svg"
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

echo "== autostart entry (the power-button window) =="
# @HOME@ is substituted here rather than using a desktop field code, because
# `%h` is not a valid Exec field code and desktop-file-validate rejects it.
render_desktop() {
    sed "s|@HOME@|$HOME|g" desktop/voice-clipper.desktop
}
render_desktop > "$HOME/.config/autostart/voice-clipper.desktop"
# Same file, also registered as a launcher so Plasma can match the window to an
# application (WM_CLASS=voice-clipper -> StartupWMClass), which is what puts it
# in the taskbar.
mkdir -p "$HOME/.local/share/applications"
render_desktop > "$HOME/.local/share/applications/voice-clipper.desktop"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
rm -f "$HOME/.config/autostart/clipgui.desktop"

echo
echo "== health check =="
"$BIN/clipctl" doctor || true

echo
echo "Done. Try:  clipctl clip        (saves a clip immediately)"
echo "            systemctl --user status clipper"
echo "            journalctl --user -u clipper -f"
echo
echo "To undo all of this:"
echo "  systemctl --user disable --now clipper"
echo "  rm ~/.config/systemd/user/clipper.service ~/.config/autostart/clipgui.desktop"
echo "  rm ~/.local/bin/clipctl ~/.local/bin/voice-clipper"
echo "  systemctl --user daemon-reload"
