#!/usr/bin/env bash
# E7d client: like e7sim.sh, but each step appends a text observation (an
# accessibility-tree dump from E2, cut to CHARS bytes) instead of an image.
# Each observation starts with a unique stamp, so no two requests share a
# prefix past the system prompt and their own agent's history.
# usage: e7simtext.sh URL MODEL N STEPS OUT_TOKENS LOG [OFFSET] [CHARS]
set -u
B=/var/tmp/glarb-bench
url=$1 model=$2 n=$3 steps=$4 otok=$5 log=$6 off=${7:-0} chars=${8:-12000}
obs=("$B"/results/obs/*.ax.txt)
tmp=$(mktemp -d "$B/run/e7simtext.XXXX")
{
  echo "You are a browser agent. Each step gives you the page's accessibility"
  echo "tree as role and name lines. Output one thought and one action."
  for i in $(seq 24); do
    echo "Action $i: click(ref) clicks the element with that ref; type(ref,"
    echo "text) types into it; scroll(down) scrolls the page by one screen."
  done
} > "$tmp/sys.txt"
agent() {
  local i=$1 d=$tmp/a$1 s k t0 t1 resp
  mkdir -p "$d"
  jq -n --rawfile sys "$tmp/sys.txt" '[{role: "system", content: $sys}]' > "$d/msgs.json"
  for s in $(seq "$steps"); do
    k=$((off + (i - 1) * steps + s - 1))
    { echo "Observation $k (agent $i, step $s):"; head -c "$chars" "${obs[$((k % ${#obs[@]}))]}"; } > "$d/obs.txt"
    jq --rawfile o "$d/obs.txt" '. + [{role: "user", content: $o}]' "$d/msgs.json" > "$d/msgs.new"
    mv "$d/msgs.new" "$d/msgs.json"
    jq -n --slurpfile m "$d/msgs.json" --arg model "$model" --argjson o "$otok" \
      '{model: $model, messages: $m[0], max_tokens: $o, min_tokens: $o,
        ignore_eos: true, temperature: 0}' > "$d/req.json"
    t0=$EPOCHREALTIME
    resp=$(curl -s -m 900 -H 'content-type: application/json' \
      -d @"$d/req.json" "$url/v1/chat/completions")
    t1=$EPOCHREALTIME
    jq -e .usage <<< "$resp" > /dev/null 2>&1 || resp={}
    jq -c --argjson r "$resp" \
      '. + [{role: "assistant", content: ($r.choices[0].message.content // "")}]' \
      "$d/msgs.json" > "$d/msgs.new"
    mv "$d/msgs.new" "$d/msgs.json"
    printf '%s %s %s %s %s\n' "$i" "$s" \
      "$(awk -v a="$t0" -v b="$t1" 'BEGIN {printf "%d", (b - a) * 1000}')" \
      "$(jq -r '.usage.prompt_tokens // "err"' <<< "$resp")" \
      "$(jq -r '.usage.prompt_tokens_details.cached_tokens // 0' <<< "$resp")" \
      >> "$log"
  done
}
for i in $(seq "$n"); do agent "$i" & done
wait
rm -rf "$tmp"
