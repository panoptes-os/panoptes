#!/usr/bin/env bash
# Start one agent stack as a transient unit in glarb.slice, then wait for
# its DevTools port. usage: start.sh VARIANT ID W H PORT URL [CHROME_FLAGS...]
B=/var/tmp/glarb-bench
id=$2 port=$5
sudo systemd-run --quiet --unit="glarb-$id" --slice=glarb.slice \
  -p User=user -p Group=user -p Delegate=yes -p KillMode=control-group \
  --setenv=KEEP="${KEEP:-0}" ${EXTRA_PROPS:-} \
  "$B/scripts/stack.sh" "$@"
for _ in $(seq 150); do
  curl -sf "http://127.0.0.1:$port/json/version" > /dev/null && exit 0
  sleep 0.1
done
echo "start.sh: DevTools port $port never answered for $id" >&2
exit 1
