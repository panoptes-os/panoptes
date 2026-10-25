#!/usr/bin/env bash
# E9: memory of the agent loop itself, idle at its first screen. Each loop
# runs in a pty (script), under a scratch HOME so no real config is read or
# written, in its own scope in probe.slice. No credentials, so this is a
# floor: a working session adds history, tool output, and MCP servers.
# Appends to results/e9.tsv. usage: e9.sh [SETTLE_S]
set -u
B=/var/tmp/glarb-bench P=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
cd "$B/scripts" && . ./env.sh
settle=${1:-20} out=$B/results/e9.tsv
pkg() { NIXPKGS_ALLOW_UNFREE=1 nix build --impure --no-link --print-out-paths "$P#$1"; }
node=$(pkg nodejs)/bin/node codex=$(pkg codex)/bin/codex claude=$(pkg claude-code)/bin/claude
declare -A cmd=(
  [node-floor]="$node -e 'setInterval(() => {}, 1000)'"
  [codex]="$codex"
  [claude-code]="$claude"
)
for name in node-floor codex claude-code; do
  home=$(mktemp -d "$B/run/e9home.XXXX")
  unit=e9-$name cg=/sys/fs/cgroup/probe.slice/e9-$name.scope
  sleep $((settle + 30)) | sudo systemd-run -q --scope --unit="$unit" \
    --slice=probe.slice --uid=user --gid=user \
    env HOME="$home" TERM=xterm-256color \
    script -qfc "stty cols 120 rows 40; exec ${cmd[$name]}" /dev/null \
    > "$B/logs/e9-$name.out" 2>&1 &
  sleep "$settle"
  if [ -d "$cg" ]; then
    m=$(sudo ./mem.sh "$cg")
    thr=$(sudo find "$cg" -name cgroup.threads -exec cat {} + | wc -l)
    printf '%s\t%s threads=%s\n' "$name" "$m" "$thr" | tee -a "$out"
    sudo ./mem.sh "$cg" breakdown | sed -n '2,6p' | sed "s/^/  $name /"
  else
    echo "e9: $name exited early; see logs/e9-$name.out" >&2
  fi
  sudo systemctl kill "$unit.scope" 2> /dev/null
  wait
  rm -rf "$home"
done
