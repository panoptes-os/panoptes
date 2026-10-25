#!/usr/bin/env bash
# E7 client: N simulated GUI agents in a closed loop against an OpenAI-style
# chat endpoint. Each agent sends STEPS requests. Each request appends one
# new screenshot and the previous reply to that agent's history, the way a
# harness grows its transcript, so step s carries s screenshots. Replies have
# a fixed length (ignore_eos), so every step decodes the same token count.
# Appends "agent step latency_ms prompt_tokens cached_tokens" to LOG.
# Agent i, step s sends image OFFSET + (i-1)*STEPS + (s-1) of results/e7imgs,
# so no two requests in a server's life send the same new image.
# usage: e7sim.sh URL MODEL N STEPS OUT_TOKENS LOG [OFFSET]
set -u
B=/var/tmp/glarb-bench
url=$1 model=$2 n=$3 steps=$4 otok=$5 log=$6 off=${7:-0}
shots=("$B"/results/e7imgs/*.jpg)
tmp=$(mktemp -d "$B/run/e7sim.XXXX")
# A fixed system prompt of about 1,000 tokens, shared by every agent, like a
# harness's tool and action-space description.
{
  echo "You are a GUI agent. You are given a task and screenshots of a"
  echo "1280x720 screen. Output one thought and one action per step."
  for i in $(seq 24); do
    echo "Action $i: click(start_box='(x,y)') clicks the element at pixel"
    echo "(x,y); type(content='...') types text into the focused field;"
    echo "scroll(direction='down') scrolls the page by one screen."
  done
} > "$tmp/sys.txt"
agent() {
  local i=$1 d=$tmp/a$1 s img t0 t1 resp
  mkdir -p "$d"
  jq -n --rawfile sys "$tmp/sys.txt" '[{role: "system", content: $sys}]' \
    > "$d/msgs.json"
  for s in $(seq "$steps"); do
    img=${shots[$(((off + (i - 1) * steps + s - 1) % ${#shots[@]}))]}
    base64 -w0 "$img" > "$d/img.b64"
    jq --rawfile b "$d/img.b64" --arg t "Step $s. What is the next action?" \
      '. + [{role: "user", content: [
        {type: "image_url", image_url: {url: ("data:image/jpeg;base64," + $b)}},
        {type: "text", text: $t}]}]' "$d/msgs.json" > "$d/msgs.new"
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
