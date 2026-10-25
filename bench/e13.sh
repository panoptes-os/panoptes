#!/usr/bin/env bash
# E13: local inference runtimes for the text-agent workload of E7d.
# gpt-oss-20b (MXFP4) under vLLM, SGLang, or llama.cpp's llama-server;
# closed-loop agents (e13sim.sh) at each count in COUNTS. llama-server is
# restarted per count with --parallel N; the others serve every count from
# one server. Also records cold start, GPU memory, the server tree's RSS,
# its idle CPU, and 20 fixed raw-prompt completions for an exact-match
# check (e13acc.sh). Appends to results/e13.tsv.
# usage: e13.sh RUNTIME "COUNTS" STEPS   RUNTIME: vllm | sglang | llama
#        e13.sh RUNTIME-sleep            sleep and wake test (vllm, sglang)
set -u
export VLLM_USE_FLASHINFER_SAMPLER=0
B=/var/tmp/glarb-bench
cd "$B" || exit 1
rt=$1 counts=${2:-"1 4 8 16 32"} steps=${3:-6}
port=8000 model=gpt-oss out=$B/results/e13.tsv url=http://127.0.0.1:8000
gguf=$B/models/gpt-oss-20b-gguf/gpt-oss-20b-MXFP4.gguf
llama_slot_ctx=20480 llama_ctx_max=${LLAMA_CTX_MAX:-262144}
row() { printf '%s\t%s\n' "${TAG:-$rt}" "$*" | tee -a "$out"; }
gpu_free() {
  for _ in $(seq 120); do
    [ "$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits)" -lt 1000 ] && return 0
    sleep 1
  done
  echo "e13: GPU still busy" >&2; return 1
}
tree() { local p; for p in "$@"; do echo "$p"; tree $(pgrep -P "$p"); done; }
rss_mib() {
  tree "$1" | while read -r p; do awk '/^VmRSS/ {print $2}' "/proc/$p/status" 2> /dev/null; done |
    awk '{s += $1} END {printf "%d", s / 1024}'
}
ticks() {
  tree "$1" | while read -r p; do
    sed 's/^.*) //' "/proc/$p/stat" 2> /dev/null | awk '{print $12 + $13}'
  done | awk '{s += $1} END {print s + 0}'
}
gpu_mib() { nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits; }
m() { # sum of a metric's samples across label sets
  curl -s "$url/metrics" |
    awk -v k="$1" '{n = $1; sub(/\{.*/, "", n); sub(/_total$/, "", n)}
      n == k {s += $2; f = 1} END {if (f) printf "%.6f", s; else print "-"}'
}
kvmetric() {
  case ${rt%-sleep} in
    vllm) echo vllm:kv_cache_usage_perc ;;
    sglang) echo sglang:token_usage ;;
    llama) echo llamacpp:kv_cache_usage_ratio ;;
  esac
}
start() { # PARALLEL [EXTRA...]: start the server; sets pid and up_ms
  local par=$1 ctx; shift
  gpu_free || exit 1
  slog=$B/logs/e13-${TAG:-$rt}-p$par-server.log
  local t0=$EPOCHREALTIME
  case ${rt%-sleep} in
    vllm)
      vllm-venv/bin/vllm serve models/gpt-oss-20b --served-model-name "$model" \
        --max-model-len 32768 --gpu-memory-utilization 0.92 \
        --enable-prompt-tokens-details --port "$port" "$@" > "$slog" 2>&1 & ;;
    sglang)
      export CUDA_HOME=$B/cuda13 LD_LIBRARY_PATH=$B/cuda13/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
      sglang-venv/bin/python -m sglang.launch_server --model-path models/gpt-oss-20b \
        --served-model-name "$model" --port "$port" --context-length 32768 \
        --mem-fraction-static 0.88 --enable-metrics --enable-cache-report --sampling-backend pytorch --reasoning-parser gpt-oss \
        "$@" > "$slog" 2>&1 & ;;
    llama)
      ctx=$((par * llama_slot_ctx)); [ "$ctx" -gt "$llama_ctx_max" ] && ctx=$llama_ctx_max
      LD_LIBRARY_PATH=/opt/cuda/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH} \
        llama.cpp/build/bin/llama-server -m "$gguf" --alias "$model" --port "$port" \
        -ngl 999 -fa on --parallel "$par" ${LLAMA_KV---kv-unified} -c "$ctx" --metrics --jinja ${LLAMA_EXTRA:-} \
        "$@" > "$slog" 2>&1 & ;;
  esac
  pid=$!
  for _ in $(seq 1800); do
    curl -sf -o /dev/null -H 'content-type: application/json' \
      -d "{\"model\":\"$model\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],\"max_tokens\":1}" \
      "$url/v1/chat/completions" && break
    kill -0 "$pid" 2> /dev/null || { echo "e13: $rt server died, see $slog" >&2; exit 1; }
    sleep 0.5
  done
  up_ms=$(awk -v a="$t0" -v b="$EPOCHREALTIME" 'BEGIN {printf "%d", (b - a) * 1000}')
}
stop() { kill "$pid"; wait "$pid" 2> /dev/null; gpu_free; }
measure_idle() {
  local a b; a=$(ticks "$pid"); sleep 30; b=$(ticks "$pid")
  awk -v a="$a" -v b="$b" -v hz="$(getconf CLK_TCK)" 'BEGIN {printf "%.2f", (b - a) / hz / 30 * 100}'
}
run_n() { # N OFFSET
  local n=$1 off=$2 steplog=$B/logs/e13-${TAG:-$rt}-n$1.steps kvm sampler t0 wall kvmax
  : > "$steplog"
  kvm=$(kvmetric)
  ( while :; do m "$kvm"; echo; sleep 1; done ) > "$steplog.kv" &
  sampler=$!
  t0=$EPOCHREALTIME
  scripts/e13sim.sh "$url" "$model" "$n" "$steps" 64 "$steplog" "$off"
  wall=$(awk -v a="$t0" -v b="$EPOCHREALTIME" 'BEGIN {printf "%.1f", b - a}')
  kill "$sampler"
  kvmax=$(grep -v '^-' "$steplog.kv" | sort -g | tail -1)
  row "agents=$n steps=$steps	wall_s=$wall steps_per_s=$(awk -v w="$wall" -v t="$((n * steps))" 'BEGIN {printf "%.2f", t / w}')	$(
    awk '{print $3}' "$steplog" | sort -n | awk '{a[NR] = $1} END {
      printf "step_ms_p50=%d p95=%d max=%d", a[int(NR * .5 + .5)], a[int(NR * .95 + .5)], a[NR]}')	$(
    awk '$4 != "err" {s += $4; c += $5; n++} $4 == "err" {e++} END {
      printf "prompt_tok_mean=%d cached_tok_mean=%d hit_rate=%.2f errors=%d", s / n, c / n, c / s, e}' "$steplog"
    ) kv_max=${kvmax:--} gpu_mib=$(gpu_mib) rss_mib=$(rss_mib "$pid")"
}
case $rt in
  *-sleep)
    case ${rt%-sleep} in
      vllm) export VLLM_SERVER_DEV_MODE=1; start 1 --enable-sleep-mode
        sleep_req() { curl -s -X POST "$url/sleep?level=1"; }
        wake_req() { curl -s -X POST "$url/wake_up"; } ;;
      sglang) start 1 --enable-memory-saver
        sleep_req() { curl -s -X POST -H 'content-type: application/json' -d '{}' "$url/release_memory_occupation"; }
        wake_req() { curl -s -X POST -H 'content-type: application/json' -d '{}' "$url/resume_memory_occupation"; } ;;
    esac
    scripts/e13sim.sh "$url" "$model" 1 2 64 /dev/null 0
    awake=$(gpu_mib)
    t0=$EPOCHREALTIME; sleep_req > /dev/null; t1=$EPOCHREALTIME
    sleep 5; asleep=$(gpu_mib)
    t2=$EPOCHREALTIME; wake_req > /dev/null; t3=$EPOCHREALTIME
    first=$(scripts/e13sim.sh "$url" "$model" 1 1 64 /dev/stdout 1000 | awk '{print $3}')
    row "sleep	up_ms=$up_ms gpu_awake_mib=$awake gpu_asleep_mib=$asleep sleep_ms=$(awk -v a="$t0" -v b="$t1" 'BEGIN {printf "%d", (b-a)*1000}') wake_ms=$(awk -v a="$t2" -v b="$t3" 'BEGIN {printf "%d", (b-a)*1000}') first_step_after_wake_ms=$first"
    stop; exit 0 ;;
esac
off=2
if [ "$rt" = llama ]; then
  for n in $counts; do
    start "$n"
    row "server parallel=$n	up_ms=$up_ms gpu_mib=$(gpu_mib) rss_mib=$(rss_mib "$pid") $(grep -m1 -oE 'n_ctx *= *[0-9]+' "$slog")"
    scripts/e13sim.sh "$url" "$model" 1 2 64 /dev/null 0
    if [ "$n" = 8 ]; then
      row "idle	cpu_pct=$(measure_idle)"
      scripts/e13acc.sh "$url" "$model" "$B/results/e13-acc-${TAG:-$rt}.tsv"
    fi
    run_n "$n" "$off"; off=$((off + n * steps))
    stop
  done
else
  start 0
  row "server	up_ms=$up_ms gpu_mib=$(gpu_mib) rss_mib=$(rss_mib "$pid")"
  scripts/e13sim.sh "$url" "$model" 1 2 64 /dev/null 0
  row "idle	cpu_pct=$(measure_idle)"
  scripts/e13acc.sh "$url" "$model" "$B/results/e13-acc-${TAG:-$rt}.tsv"
  for n in $counts; do run_n "$n" "$off"; off=$((off + n * steps)); done
  stop
fi
