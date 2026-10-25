#!/usr/bin/env bash
# E13 fourth queue, after queue 3: llama.cpp with a larger prefill batch,
# first with the unified KV cache, then with one cache per slot.
B=/var/tmp/glarb-bench
cd "$B" || exit 1
log() { echo "$(date +%T) $*" >> logs/e13-run.log; }
step() { log "start $*"; scripts/e13.sh "$@" >> logs/e13-run.log 2>&1; log "end $* rc=$?"; }
until grep -q 'queue3 done' logs/e13-run.log; do sleep 5; done
TAG=llama-ub2048 LLAMA_EXTRA="-b 4096 -ub 2048" step llama "8" 6
TAG=llama-ub2048-slots LLAMA_KV="" LLAMA_EXTRA="-b 4096 -ub 2048" LLAMA_CTX_MAX=327680 step llama "8 16" 6
log "queue4 done"
