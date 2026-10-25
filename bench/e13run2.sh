#!/usr/bin/env bash
# E13 second queue: after the running SGLang sweep exits, run the SGLang
# sleep test and the llama.cpp sweep. Logs to logs/e13-run.log.
B=/var/tmp/glarb-bench
cd "$B" || exit 1
log() { echo "$(date +%T) $*" >> logs/e13-run.log; }
step() { log "start $*"; scripts/e13.sh "$@" >> logs/e13-run.log 2>&1; log "end $* rc=$?"; }
while pgrep -f 'e13.sh sglang' > /dev/null; do sleep 5; done
log "end sglang sweep (first queue)"
step sglang-sleep
step llama "1 4 8 16 32" 6
log "queue done"
