#!/usr/bin/env bash
# Run bench steps one at a time once WAIT_PID exits. Steps come from
# STEPS_FILE, one shell command per line, read as they are appended; the
# runner waits at the end of the file until a line reads END. Each step logs
# to logs/q-N.log; logs/queue.log records start, end, and exit status.
# usage: queue.sh WAIT_PID STEPS_FILE
B=/var/tmp/glarb-bench
cd "$B/scripts" || exit 1
while kill -0 "$1" 2> /dev/null; do sleep 10; done
exec 3< "$2"
n=0
while :; do
  if ! IFS= read -r step <&3; then sleep 20; continue; fi
  case $step in END) break ;; '' | '#'*) continue ;; esac
  n=$((n + 1))
  echo "$(date +%T) start $n: $step" >> "$B/logs/queue.log"
  bash -c "$step" < /dev/null > "$B/logs/q-$n.log" 2>&1
  echo "$(date +%T) end $n: status $?" >> "$B/logs/queue.log"
done
echo "$(date +%T) queue done" >> "$B/logs/queue.log"
