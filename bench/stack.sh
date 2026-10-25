#!/usr/bin/env bash
# One agent desktop stack. Runs as the unit's main process.
# usage: stack.sh VARIANT ID WIDTH HEIGHT DEVTOOLS_PORT URL [CHROME_FLAGS...]
# VARIANT: cage | cage-vnc | xfce | headless
set -u
B=/var/tmp/glarb-bench
. "$B/scripts/env.sh"
v=$1 id=$2 w=$3 h=$4 port=$5 url=$6
shift 6
run=$B/run/$id prof=$B/profiles/$id
rm -rf "$run"
[ "${KEEP:-0}" = 1 ] || rm -rf "$prof"
mkdir -p "$run" "$prof"
chmod 700 "$run"
export XDG_RUNTIME_DIR=$run HOME=$run
case $url in app:*) target=(--app="${url#app:}") ;; *) target=("$url") ;; esac
chrome=(chromium --no-first-run --no-default-browser-check
  --password-store=basic --user-data-dir="$prof"
  --remote-debugging-port="$port" --disable-gpu "$@")
case $v in
cage | cage-vnc)
  export WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1
  vnc=0
  [ "$v" = cage-vnc ] && vnc=$((port + 1000))
  printf '%q ' "${chrome[@]}" --ozone-platform=wayland "${target[@]}" > "$run/chrome.cmd"
  exec cage -- "$B/scripts/inner.sh" "$w" "$h" "$vnc" "$run/chrome.cmd"
  ;;
xfce)
  disp=$((100 + ${id//[!0-9]/}))
  Xvfb ":$disp" -screen 0 "${w}x${h}x24" -nolisten tcp &
  sleep 1
  export DISPLAY=:$disp
  printf '%q ' "${chrome[@]}" --ozone-platform=x11 "--window-size=$w,$h" \
    "$url" > "$run/chrome.cmd"
  conf=$(dirname "$(readlink -f "$(command -v dbus-daemon)")")/../share/dbus-1/session.conf
  exec dbus-run-session --config-file="$conf" -- sh -c "xfce4-session & sleep 1; xfwm4 & xfce4-panel & xfdesktop & xfsettingsd & sleep 3;
    x11vnc -display :$disp -rfbport $((port + 1000)) -localhost -forever \
      -shared -quiet -bg -o $run/x11vnc.log; exec \$(cat $run/chrome.cmd)"
  ;;
headless)
  exec "${chrome[@]}" --headless=new "--window-size=$w,$h" "${target[@]}"
  ;;
esac
