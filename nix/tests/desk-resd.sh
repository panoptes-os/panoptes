# desk-resd-check: panoptes-desk with --resd against a running
# panoptes-resd, as root. Between calls the kiosk is frozen, a long wait
# parks it into zswap, and the next call wakes it and reaches the app. When
# resd is gone, the desk thaws the kiosk itself.

cg=/sys/fs/cgroup
root=$cg/panoptes.slice
a=$root/a
run=/run/desk
sock=$run/desk.sock
resd_sock=/run/panoptes/resd.sock

fail() {
  echo "FAIL: $*" >&2
  echo "--- last reply" >&2
  cut -c1-2000 reply.json >&2 || true
  journalctl -u desk -u resd --no-pager -n 80 >&2 || true
  exit 1
}

# resd's view: the agent's status row, or null.
resd() { panoptesctl resd "$resd_sock" "$1"; }
agent() { resd '{"op":"status"}' | jq -c --arg n "$1" '[.agents[] | select(.agent == $n)][0]'; }
state() { agent "$1" | jq -r '.state // "none"'; }
wait_state() {
  for _ in $(seq 200); do
    [ "$(state "$1")" = "$2" ] && return 0
    sleep 0.1
  done
  fail "$1 is $(state "$1"), not $2"
}
# Between calls: waiting, or parked once park_after has passed, which a
# slow builder can reach before the first poll.
wait_idle() {
  for _ in $(seq 200); do
    case $(state a) in waiting | parked) return 0 ;; esac
    sleep 0.1
  done
  fail "a is $(state a) between calls"
}
# Wait until resd reports a's reclaim, which ends after a is parked; sets
# park to its stat.
wait_park() {
  for _ in $(seq 200); do
    park=$(agent a | jq -c '.last_park')
    [ -n "$park" ] && [ "$park" != null ] && return 0
    sleep 0.1
  done
  fail "no reclaim reported"
}
freeze() { cat "$a/kiosk/cgroup.freeze"; }
frozen() { awk '$1 == "frozen" { print $2 }' "$a/kiosk/cgroup.events"; }
expect_frozen() {
  for _ in $(seq 50); do
    [ "$(freeze)" = "$1" ] && [ "$(frozen)" = "$1" ] && return 0
    sleep 0.1
  done
  fail "cgroup.freeze $(freeze), frozen $(frozen); wanted $1"
}

# The desk's MCP session, as desk.sh drives it.
rpc() {
  id=$((id + 1))
  jq -nc --argjson id "$id" --arg m "$1" --argjson p "$2" \
    '{jsonrpc: "2.0", id: $id, method: $m, params: $p}' >&"${MCP[1]}"
  IFS= read -r -t 300 line <&"${MCP[0]}" || fail "no reply to $1"
  printf '%s\n' "$line" > reply.json
  [ "$(jq .id reply.json)" = "$id" ] || fail "reply to $1 has the wrong id"
}
call() {
  rpc tools/call "$(jq -nc --arg n "$1" --argjson a "${2:-"{}"}" \
    '{name: $n, arguments: ({settle_ms: 30000} + $a)}')"
  [ "$(jq '.result.isError' reply.json)" != true ] ||
    fail "$1 failed: $(jq -r '.result.content[0].text' reply.json)"
  jq -r '.result.content[0].text' reply.json | jq .result > result.json
}
logged() {
  for _ in $(seq 300); do
    grep -Eq "$2" "$1" && return 0
    sleep 0.1
  done
  fail "$(basename "$1") never logged /$2/"
}
logfile() {
  for _ in $(seq 300); do
    grep -El "$1" "$run"/logs/* 2> /dev/null && return 0
    sleep 0.1
  done
  fail "no app log matches /$1/"
}

echo "== setup"
mkswap /dev/vdb > /dev/null || fail "mkswap"
swapon /dev/vdb || fail "swapon"
[ "$(cat /sys/module/zswap/parameters/enabled)" = Y ] || fail "zswap off"
# systemd enables only the controllers its own units use; the root cgroup
# is exempt from the no-internal-process rule, so it can enable more.
echo "+cpu +memory +pids" > "$cg/cgroup.subtree_control" || fail "root controllers"
mkdir -p "$a"
echo "+cpu +memory +pids" > "$root/cgroup.subtree_control" || fail "slice controllers"
systemctl start resd
for _ in $(seq 100); do [ -S "$resd_sock" ] && break; sleep 0.1; done
# The desk moves itself into a/desk and starts Cage in a/kiosk.
systemd-run --unit=desk --setenv=PATH=/run/current-system/sw/bin \
  panoptes-desk --cgroup "$a" --run-dir "$run" --resd "$resd_sock"
for _ in $(seq 600); do [ -S "$sock" ] && break; sleep 0.1; done
[ -S "$sock" ] || fail "no desk socket"

cd "$(mktemp -d)" || exit 1
coproc MCP { socat - "UNIX-CONNECT:$sock"; }
id=0
rpc initialize '{"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "desk-resd-check", "version": "0"}}'
echo '{"jsonrpc": "2.0", "method": "notifications/initialized"}' >&"${MCP[1]}"

echo "== between calls the kiosk is frozen"
call launch '{"argv": ["stdbuf", "-oL", "wev"], "timeout_ms": 120000}'
jq -e '.windows.opened[0].app_id == "wev"' result.json > /dev/null || fail "no wev window"
wevlog=$(logfile 'wl_keyboard')
wait_idle
expect_frozen 1

echo "== a long wait parks it, and the next call wakes it"
wait_state a parked
wait_park
[ "$(jq '.before > .after' <<< "$park")" = true ] || fail "park freed nothing: $park"
call click '{"x": 300, "y": 200}'
logged "$wevlog" 'x, y: 300\.000000, 200\.000000'
resd '{"op":"status","since":0}' |
  jq -e '[.events[] | select(.agent == "a" and .from == "parked" and .to == "acting")] | length > 0' \
    > /dev/null || fail "resd never woke a from parked"
wait_idle
expect_frozen 1

echo "== without resd the desk thaws the kiosk itself"
systemctl stop resd
expect_frozen 1
call click '{"x": 310, "y": 210}'
logged "$wevlog" 'x, y: 310\.000000, 210\.000000'
expect_frozen 0

echo "desk-resd-check: ok"
