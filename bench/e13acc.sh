#!/usr/bin/env bash
# E13 accuracy probe: 20 fixed raw Harmony prompts, one per E2 tree dump,
# sent to /v1/completions with greedy decoding and 64 tokens, so every
# runtime sees the same token ids without its own chat template. Writes one
# line per prompt: index, prompt tokens, and the completion as JSON.
# usage: e13acc.sh URL MODEL OUT
set -u
B=/var/tmp/glarb-bench
url=$1 model=$2 out=$3
obs=("$B"/results/obs/*.ax.txt)
: > "$out"
for i in $(seq 0 19); do
  p=$(printf '<|start|>system<|message|>You are a browser agent. Reasoning: low<|end|><|start|>user<|message|>Pick the next action for this page.\n%s<|end|><|start|>assistant' \
    "$(head -c 3000 "${obs[$((i * 5 % ${#obs[@]}))]}")")
  jq -n --arg p "$p" --arg m "$model" \
    '{model: $m, prompt: $p, max_tokens: 64, temperature: 0, ignore_eos: true}' |
    curl -s -m 300 -H 'content-type: application/json' -d @- "$url/v1/completions" |
    jq -r --arg i "$i" '[$i, (.usage.prompt_tokens // "err" | tostring), (.choices[0].text // "" | tojson)] | @tsv' >> "$out"
done
