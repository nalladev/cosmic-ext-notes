#!/usr/bin/env bash
# Installs a downloaded release tarball's contents into the current user's
# local prefix (no root required). Run this from inside the extracted
# tarball directory.
set -e

mkdir -p ~/.local/bin \
    "${XDG_DATA_HOME:-~/.local/share}/applications" \
    "${XDG_DATA_HOME:-~/.local/share}/appdata" \
    "${XDG_DATA_HOME:-~/.local/share}/icons/hicolor/scalable/apps"

cp ./cosmic-notes ~/.local/bin/
cp ./*.desktop "${XDG_DATA_HOME:-~/.local/share}/applications/"
cp ./*.metainfo.xml "${XDG_DATA_HOME:-~/.local/share}/appdata/"
cp ./*.svg "${XDG_DATA_HOME:-~/.local/share}/icons/hicolor/scalable/apps/com.github.nalladev.cosmic-notes.svg"

echo "Installed. Launch cosmic-notes from the application menu."
