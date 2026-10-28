# shellcheck shell=bash
# Container entry, under tini: claim the container's cgroup subtree for
# panoptes-resd, check what parking needs from the host, start resd, then
# run the command.
#
# /sys/fs/cgroup/
# ├─ init/            this script, tini, and the command
# └─ panoptes.slice/  one directory per agent; panoptesctl spawn makes them
#    └─ <agent>/{desk,kiosk}
set -eu

cg=/sys/fs/cgroup
[ -f "$cg/cgroup.controllers" ] || { echo "panoptes: needs cgroup v2" >&2; exit 1; }

# A cgroup with processes in it cannot enable controllers for its children,
# so everything already here moves to init/ first, as docker-in-docker does.
# The list is read whole first: reading cgroup.procs while moving its
# processes out skips entries.
mkdir -p "$cg/init"
pids=$(cat "$cg/cgroup.procs")
for pid in $pids; do
  echo "$pid" > "$cg/init/cgroup.procs" 2> /dev/null || true
done
for c in cpu memory pids; do
  echo "+$c" > "$cg/cgroup.subtree_control" 2> /dev/null ||
    echo "panoptes: cannot enable the $c controller" >&2
done
mkdir -p "$cg/panoptes.slice"
for c in cpu memory pids; do
  echo "+$c" > "$cg/panoptes.slice/cgroup.subtree_control" 2> /dev/null || true
done

# Parking compresses kiosks into zswap with writeback off; without zswap and
# swap it falls back to freezing, which saves CPU but not memory.
z=/sys/module/zswap/parameters
if [ "$(cat "$z/enabled" 2> /dev/null)" != Y ]; then
  echo "panoptes: zswap is off on the host; parked agents keep their memory" >&2
else
  echo "panoptes: zswap $(cat "$z/zpool") $(cat "$z/compressor")," \
    "pool cap $(cat "$z/max_pool_percent")%" >&2
fi
swap=$(awk '/^SwapTotal/ {print int($2 / 1048576)}' /proc/meminfo)
[ "$swap" -gt 0 ] || echo "panoptes: no swap on the host; parking needs swap slots" >&2

mkdir -p /run/panoptes
# PANOPTES_RESD_ARGS is a list of flags, split on purpose.
# shellcheck disable=SC2086
panoptes-resd --root "$cg/panoptes.slice" --socket /run/panoptes/resd.sock \
  --state /run/panoptes/resd.state ${PANOPTES_RESD_ARGS:-} &

exec "$@"
