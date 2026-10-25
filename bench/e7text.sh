#!/usr/bin/env bash
# E7d: a local text model for browser agents on the RTX 3090. Serve
# gpt-oss-20b (MXFP4) with vLLM, then run closed-loop agents that send
# accessibility-tree text (e7simtext.sh) at rising concurrency.
# Engine-side means come from deltas of vLLM's Prometheus histograms.
# Appends to results/e7.tsv.
# usage: e7text.sh CONFIG "AGENT_COUNTS" STEPS   CONFIG: mxfp4 | mxfp4-nocache
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
cfg=${1:-mxfp4} counts=${2:-"1 4 8 16"} steps=${3:-6}
port=8000 model=gpt-oss out=$B/results/e7.tsv
args=(--served-model-name "$model" --max-model-len 32768
  --gpu-memory-utilization 0.92 --enable-prompt-tokens-details --port "$port")
case $cfg in
  mxfp4-nocache) args+=(--no-enable-prefix-caching) ;;
esac
slog=$B/logs/e7d-$cfg-server.log
t0=$EPOCHREALTIME
vllm-venv/bin/vllm serve models/gpt-oss-20b "${args[@]}" > "$slog" 2>&1 &
pid=$!
for _ in $(seq 900); do
  curl -sf "localhost:$port/health" > /dev/null && break
  kill -0 "$pid" 2> /dev/null || { echo "e7: server died" >&2; exit 1; }
  sleep 1
done
up_s=$(awk -v a="$t0" -v b="$EPOCHREALTIME" 'BEGIN {printf "%d", b - a}')
kv=$(grep -oE 'KV cache size: [0-9,]+ tokens' "$slog" | tail -1)
gpu=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits)
printf '%s\tserver\tup_s=%s %s gpu_mib=%s\n' "$cfg" "$up_s" "$kv" "$gpu" |
  tee -a "$out"
m() { # sum of a metric's samples across label sets
  curl -s "localhost:$port/metrics" |
    awk -v k="$1" '{n = $1; sub(/\{.*/, "", n); sub(/_total$/, "", n)}
      n == k {s += $2} END {printf "%.6f", s}'
}
mean() { awk -v a="$1" -v b="$2" -v c="$3" -v d="$4" \
  'BEGIN {if (d - c > 0) printf "%d", (b - a) / (d - c) * 1000; else print "-"}'; }
curl -s "localhost:$port/metrics" | grep -oE "^vllm:[a-z_]+" | sort -u \
  > "$B/logs/e7d-$cfg-metrics.txt"
# Warm up the engine: one agent, two steps.
"$B/scripts/e7simtext.sh" "http://127.0.0.1:$port" "$model" 1 2 64 /dev/null 0
off=2
for n in $counts; do
  steplog=$B/logs/e7d-$cfg-n$n.steps
  : > "$steplog"
  names=(vllm:time_to_first_token_seconds vllm:request_prefill_time_seconds
    vllm:request_decode_time_seconds vllm:request_queue_time_seconds)
  declare -A s0 c0
  for k in "${names[@]}"; do s0[$k]=$(m "${k}_sum"); c0[$k]=$(m "${k}_count"); done
  h0=$(m vllm:prefix_cache_hits) q0=$(m vllm:prefix_cache_queries)
  p0=$(m vllm:num_preemptions)
  ( while :; do m vllm:kv_cache_usage_perc; echo; sleep 1; done ) > "$steplog.kv" &
  sampler=$!
  t0=$EPOCHREALTIME
  "$B/scripts/e7simtext.sh" "http://127.0.0.1:$port" "$model" "$n" "$steps" 64 "$steplog" "$off"
  off=$((off + n * steps))
  wall=$(awk -v a="$t0" -v b="$EPOCHREALTIME" 'BEGIN {printf "%.1f", b - a}')
  kill "$sampler"
  row="ttft_ms=$(mean "${s0[vllm:time_to_first_token_seconds]}" "$(m vllm:time_to_first_token_seconds_sum)" "${c0[vllm:time_to_first_token_seconds]}" "$(m vllm:time_to_first_token_seconds_count)")"
  row+=" prefill_ms=$(mean "${s0[vllm:request_prefill_time_seconds]}" "$(m vllm:request_prefill_time_seconds_sum)" "${c0[vllm:request_prefill_time_seconds]}" "$(m vllm:request_prefill_time_seconds_count)")"
  row+=" decode_ms=$(mean "${s0[vllm:request_decode_time_seconds]}" "$(m vllm:request_decode_time_seconds_sum)" "${c0[vllm:request_decode_time_seconds]}" "$(m vllm:request_decode_time_seconds_count)")"
  row+=" queue_ms=$(mean "${s0[vllm:request_queue_time_seconds]}" "$(m vllm:request_queue_time_seconds_sum)" "${c0[vllm:request_queue_time_seconds]}" "$(m vllm:request_queue_time_seconds_count)")"
  hits=$(awk -v a="$h0" -v b="$(m vllm:prefix_cache_hits)" -v c="$q0" -v d="$(m vllm:prefix_cache_queries)" \
    'BEGIN {if (d - c > 0) printf "%.2f", (b - a) / (d - c); else print "-"}')
  pre=$(awk -v a="$p0" -v b="$(m vllm:num_preemptions)" 'BEGIN {printf "%d", b - a}')
  kvmax=$(sort -g "$steplog.kv" | tail -1)
  lat=$(awk '{print $3}' "$steplog" | sort -n | awk '{a[NR] = $1} END {
    printf "step_ms_p50=%d p95=%d max=%d", a[int(NR * .5 + .5)], a[int(NR * .95 + .5)], a[NR]}')
  ptok=$(awk '$4 != "err" {s += $4; c += $5; n++} END {printf "prompt_tok_mean=%d cached_tok_mean=%d", s / n, c / n}' "$steplog")
  errs=$(awk '$4 == "err"' "$steplog" | wc -l)
  printf '%s\tagents=%s steps=%s\twall_s=%s steps_per_s=%s\t%s\t%s\t%s hit_rate=%s kv_max=%s preemptions=%s errors=%s\n' \
    "$cfg" "$n" "$steps" "$wall" \
    "$(awk -v w="$wall" -v t="$((n * steps))" 'BEGIN {printf "%.2f", t / w}')" \
    "$lat" "$ptok" "$row" "$hits" "$kvmax" "$pre" "$errs" | tee -a "$out"
done
kill "$pid"
wait "$pid" 2> /dev/null
