#!/usr/bin/env bash
# E13 queue: one inference server on the GPU at a time. Waits for the
# SGLang venv before its runs. Logs each step to logs/e13-run.log.
# usage: nohup setsid e13run.sh > /dev/null 2>&1 &
B=/var/tmp/glarb-bench
cd "$B" || exit 1
log() { echo "$(date +%T) $*" >> logs/e13-run.log; }
step() { log "start $*"; scripts/e13.sh "$@" >> logs/e13-run.log 2>&1; log "end $* rc=$?"; }
step vllm "8" 6
step vllm-sleep
step llama "1 4 8 16 32" 6
for _ in $(seq 3600); do [ -e run/e13-sglang.ok ] || [ -e run/e13-sglang.failed ] && break; sleep 2; done
if [ -e run/e13-sglang.ok ]; then
  step sglang "1 4 8 16 32" 6
  step sglang-sleep
else
  log "sglang install failed; see logs/e13-sglang.log"
fi
log "queue done"
