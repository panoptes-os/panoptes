#!/usr/bin/env bash
# Cage's client: size the headless output, optionally serve VNC, run Chromium.
# usage: inner.sh WIDTH HEIGHT VNC_PORT|0 CHROME_CMD_FILE
w=$1 h=$2 vnc=$3 cmd=$4
wlr-randr --output HEADLESS-1 --custom-mode "${w}x${h}" 2>> "${cmd%/*}/randr.log"
[ "$vnc" != 0 ] && wayvnc 127.0.0.1 "$vnc" &
eval "exec $(cat "$cmd")"
