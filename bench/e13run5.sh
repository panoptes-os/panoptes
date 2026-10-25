#!/usr/bin/env bash
# E13 fifth queue, after queue 4: SGLang again (libcudart.so link added to
# the CUDA 13 home), its sleep test, and llama.cpp's best config with the
# host-RAM prompt cache off.
B=/var/tmp/glarb-bench
cd "$B" || exit 1
log() { echo "$(date +%T) $*" >> logs/e13-run.log; }
step() { log "start $*"; scripts/e13.sh "$@" >> logs/e13-run.log 2>&1; log "end $* rc=$?"; }
true
TAG=sglang step sglang "1 4 8 16 32" 6
step sglang-sleep
TAG=llama-ub2048-slots-nocacheram LLAMA_KV="" LLAMA_EXTRA="-b 4096 -ub 2048 --cache-ram 0" LLAMA_CTX_MAX=327680 step llama "8" 6
log "queue5 done"
