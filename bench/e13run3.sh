#!/usr/bin/env bash
# E13 third queue: after the llama.cpp sweep, rerun SGLang with CUDA_HOME
# on the venv's CUDA 13.4 toolkit, then its sleep test.
B=/var/tmp/glarb-bench
cd "$B" || exit 1
log() { echo "$(date +%T) $*" >> logs/e13-run.log; }
step() { log "start $*"; scripts/e13.sh "$@" >> logs/e13-run.log 2>&1; log "end $* rc=$?"; }
until grep -q 'queue done' logs/e13-run.log; do sleep 5; done
step sglang "1 4 8 16 32" 6
step sglang-sleep
log "queue3 done"
