#!/usr/bin/env bash
# E14 kiosk: one native app alone in Cage, with a private session bus and
# AT-SPI bus. Runs as the unit's main process.
# usage: e14app.sh ID APP    APP: kicad | blender | blender-rot | calc | ltspice
# R=gles2 selects wlroots' GLES renderer instead of pixman.
set -u
E=/var/tmp/glarb-bench/e14
. "$E/env.sh"
id=$1 app=$2
run=$E/run/$id
rm -rf "$run"
mkdir -p "$run" "$E/home/${app%%-*}"
chmod 700 "$run"
# HOME persists per app, so first-run wizards are answered once, not per run.
export XDG_RUNTIME_DIR=$run HOME=$E/home/${app%%-*} WLR_BACKENDS=headless
export WLR_RENDERER=${R:-pixman} WLR_LIBINPUT_NO_DEVICES=1 QT_ACCESSIBILITY=1
# Nix's dbus has no system-wide session.conf, so name the one it ships.
conf=$(dirname "$(readlink -f "$(command -v dbus-daemon)")")/../share/dbus-1/session.conf
exec dbus-run-session --config-file="$conf" -- "$E/e14inner.sh" "$app"
