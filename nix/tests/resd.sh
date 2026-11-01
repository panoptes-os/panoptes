# panoptes-resd end to end, as root: a kiosk holding 96 MiB of shmem is
# discovered, frozen on release, parked into zswap, woken on acquire,
# kept frozen across a restart, evicted when emptied, and forgotten when
# removed. A desk may name only its own agent.

root=/sys/fs/cgroup/panoptes.slice
sock=/run/panoptes/resd.sock
a=$root/a

fail() {
  echo "FAIL: $*" >&2
  journalctl -u resd --no-pager -n 50 >&2 || true
  exit 1
}
resd() { panoptesctl resd "$sock" "$1"; }
# The agent's status row, or null.
agent() { resd '{"op":"status"}' | jq -c --arg n "$1" '[.agents[] | select(.agent == $n)][0]'; }
state() { agent "$1" | jq -r '.state // "none"'; }
wait_state() {
  for _ in $(seq 200); do
    [ "$(state "$1")" = "$2" ] && return 0
    sleep 0.1
  done
  fail "$1 is $(state "$1"), not $2"
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
# Wait until resd reports a reclaim of a; sets park to its stat. A
# restarted resd reports none until its own first reclaim.
wait_park() {
  for _ in $(seq 200); do
    park=$(agent a | jq -c '.last_park')
    [ -n "$park" ] && [ "$park" != null ] && return 0
    sleep 0.1
  done
  fail "no reclaim reported"
}

echo "== setup"
swap=/dev/disk/by-id/virtio-empty0
[ -e "$swap" ] || swap=/dev/vdb
mkswap "$swap" > /dev/null || fail "mkswap $swap"
swapon "$swap" || fail "swapon $swap"
[ "$(cat /sys/module/zswap/parameters/enabled)" = Y ] || fail "zswap off"
# systemd enables only the controllers its own units use; the root cgroup
# is exempt from the no-internal-process rule, so it can enable more.
echo "+cpu +memory" > /sys/fs/cgroup/cgroup.subtree_control || fail "root controllers"
mkdir -p "$root"
echo "+cpu +memory" > "$root/cgroup.subtree_control" || fail "slice controllers"
mkdir -p "$a"
echo "+cpu +memory" > "$a/cgroup.subtree_control" || fail "agent controllers"
mkdir -p "$a/desk" "$a/kiosk"
setsid sh -c "echo \$\$ > $a/kiosk/cgroup.procs
  head -c 96M /dev/zero > /dev/shm/kiosk-a
  exec sleep infinity" < /dev/null > /dev/null 2>&1 &
# The eviction step kills it; no job notice.
disown
for _ in $(seq 100); do
  [ "$(stat -c %s /dev/shm/kiosk-a 2> /dev/null)" = $((96 << 20)) ] && break
  sleep 0.1
done
systemctl start resd
for _ in $(seq 100); do [ -S "$sock" ] && break; sleep 0.1; done

echo "== discovery"
wait_state a starting

echo "== release freezes"
out=$(resd '{"op":"release","agent":"a"}') || fail "release: $out"
[ "$(jq -r .state <<< "$out")" = waiting ] || fail "release: $out"
expect_frozen 1
[ "$(cat "$a/kiosk/cpu.weight")" = 100 ] || fail "idle weight"

echo "== a long wait parks into zswap"
wait_state a parked
wait_park
before=$(jq .before <<< "$park")
after=$(jq .after <<< "$park")
[ "$before" -ge $((96 << 20)) ] || fail "kiosk held only $before bytes"
[ "$after" -lt $((before / 4)) ] || fail "park freed too little: $park"
for _ in $(seq 50); do
  [ "$(jq -r .a /run/panoptes/resd.state)" = parked ] && break
  sleep 0.1
done
[ "$(jq -r .a /run/panoptes/resd.state)" = parked ] || fail "state file: $(cat /run/panoptes/resd.state)"

echo "== acquire wakes it"
out=$(resd '{"op":"acquire","agent":"a"}') || fail "acquire: $out"
[ "$(jq -r .from <<< "$out")" = parked ] || fail "acquire: $out"
[ "$(jq -r .state <<< "$out")" = acting ] || fail "acquire: $out"
expect_frozen 0
[ "$(cat "$a/kiosk/cpu.weight")" = 10000 ] || fail "acting weight"
cmp -n $((96 << 20)) /dev/zero /dev/shm/kiosk-a || fail "the kiosk's memory changed"

echo "== a desk names only its own agent"
as_desk() {
  sh -c "echo \$\$ > $a/desk/cgroup.procs
    exec setpriv --reuid=nobody --regid=nogroup --clear-groups panoptesctl resd $sock '$1'"
}
as_nobody() {
  setpriv --reuid=nobody --regid=nogroup --clear-groups panoptesctl resd "$sock" "$1"
}
out=$(as_desk '{"op":"release"}') || fail "desk release: $out"
expect_frozen 1
out=$(as_desk '{"op":"acquire","agent":"b"}') && fail "a desk named an agent: $out"
jq -e '.error == "only root names an agent"' <<< "$out" > /dev/null || fail "$out"
out=$(as_nobody '{"op":"acquire"}') && fail "a stranger acquired: $out"
jq -e '.error == "not a desk of this root"' <<< "$out" > /dev/null || fail "$out"

echo "== a restart keeps frozen kiosks frozen"
systemctl stop resd
expect_frozen 1
systemctl start resd
for _ in $(seq 100); do [ -S "$sock" ] && break; sleep 0.1; done
# park_after may have passed before the stop.
s=$(state a)
[ "$s" = waiting ] || [ "$s" = parked ] || fail "recovered a frozen kiosk as $s"
expect_frozen 1
wait_state a parked
wait_park
systemctl restart resd
for _ in $(seq 100); do [ -S "$sock" ] && break; sleep 0.1; done
wait_state a parked
expect_frozen 1
# The state file records parked before the reclaim runs, so recovery
# reclaims again.
wait_park

echo "== an emptied kiosk is evicted"
resd '{"op":"acquire","agent":"a"}' > /dev/null || fail "acquire before kill"
echo 1 > "$a/kiosk/cgroup.kill"
wait_state a evicted
out=$(resd '{"op":"acquire","agent":"a"}') && fail "acquired an evicted kiosk: $out"
jq -e '.error == "evicted"' <<< "$out" > /dev/null || fail "$out"

echo "== a removed agent is forgotten"
rm -f /dev/shm/kiosk-a
rmdir "$a/kiosk" "$a/desk" "$a"
wait_state a none

echo "resd-check: ok"
