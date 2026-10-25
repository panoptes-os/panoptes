#!/usr/bin/env bash
# E7c: park the engine itself. Serve UI-TARS with sleep mode on, then time
# sleep at LEVEL (1 moves weights to host RAM and drops the KV cache), the
# GPU and host memory while asleep, wake-up, and the first request after
# it. Uses images 190 and up, which e7.sh never sends. Appends to
# results/e7.tsv.
# usage: e7sleep.sh [LEVEL]
set -u
# FlashInfer JIT-compiles its sampler at engine start, and this host's nvcc
# (CUDA 12.9) rejects the default gcc 15. Requests are greedy, so vLLM's
# PyTorch sampler gives the same tokens.
export VLLM_USE_FLASHINFER_SAMPLER=0
B=/var/tmp/glarb-bench
cd "$B" || exit 1
# A previous engine's worker can outlive its API server for a moment; wait
# until the GPU is free, so this server's free-memory check passes.
for _ in $(seq 120); do
  [ "$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits)" -lt 1000 ] && break
  sleep 1
done
lvl=${1:-1} port=8000 model=ui-tars out=$B/results/e7.tsv
slog=$B/logs/e7-sleep$lvl-server.log
VLLM_SERVER_DEV_MODE=1 vllm-venv/bin/vllm serve models/ui-tars-1.5-7b \
  --served-model-name "$model" --max-model-len 16384 \
  --gpu-memory-utilization 0.92 --limit-mm-per-prompt '{"image":8}' \
  --enable-prompt-tokens-details --enable-sleep-mode --port "$port" \
  > "$slog" 2>&1 &
pid=$!
for _ in $(seq 900); do
  curl -sf "localhost:$port/health" > /dev/null && break
  kill -0 "$pid" 2> /dev/null || { echo "e7sleep: server died" >&2; exit 1; }
  sleep 1
done
gpu() { nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits; }
avail() { awk '/^MemAvailable/ {print int($2 / 1024)}' /proc/meminfo; }
ms() { awk -v a="$1" -v b="$2" 'BEGIN {printf "%d", (b - a) * 1000}'; }
one() { # one fresh single-step request; prints its latency in ms
  local l; l=$(mktemp "$B/run/e7sleep.XXXX")
  "$B/scripts/e7sim.sh" "http://127.0.0.1:$port" "$model" 1 1 64 "$l" "$1"
  awk '{print $3}' "$l"; rm -f "$l"
}
one 190 > /dev/null
warm=$(one 191)
g0=$(gpu) a0=$(avail)
t0=$EPOCHREALTIME
code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "localhost:$port/sleep?level=$lvl")
t1=$EPOCHREALTIME
sleep 5
g1=$(gpu) a1=$(avail)
t2=$EPOCHREALTIME
curl -s -o /dev/null -X POST "localhost:$port/wake_up"
t3=$EPOCHREALTIME
g2=$(gpu)
first=$(one 192)
printf 'sleep\tlevel=%s http=%s\tsleep_ms=%s wake_ms=%s\tgpu_mib awake=%s asleep=%s woken=%s\thost_avail_drop_mib=%s\tstep_ms warm=%s first_after_wake=%s\n' \
  "$lvl" "$code" "$(ms "$t0" "$t1")" "$(ms "$t2" "$t3")" "$g0" "$g1" "$g2" \
  "$((a0 - a1))" "$warm" "$first" | tee -a "$out"
kill "$pid"
wait "$pid" 2> /dev/null
