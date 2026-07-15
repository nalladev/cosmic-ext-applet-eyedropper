#!/usr/bin/env bash
# Installs a downloaded release tarball's contents into the current user's
# local prefix (no root required). Run this from inside the extracted
# tarball directory.
set -e

mkdir -p ~/.local/bin \
    "${XDG_DATA_HOME:-~/.local/share}/applications" \
    "${XDG_DATA_HOME:-~/.local/share}/appdata" \
    "${XDG_DATA_HOME:-~/.local/share}/icons/hicolor/scalable/apps"

cp ./cosmic-ext-applet-eyedropper ~/.local/bin/
cp ./*.desktop "${XDG_DATA_HOME:-~/.local/share}/applications/"
cp ./*.metainfo.xml "${XDG_DATA_HOME:-~/.local/share}/appdata/"
cp ./*.svg "${XDG_DATA_HOME:-~/.local/share}/icons/hicolor/scalable/apps/io.github.nalladev.CosmicExtAppletEyedropper-symbolic.svg"

echo "Installed. Restart cosmic-panel (pkill cosmic-panel) then add the applet."
