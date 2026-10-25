#!/usr/bin/env bash
# Download the E7 models with retries, and mark each one done once it is
# complete; a model already marked done is skipped. The first attempt used
# Xet and died with a 401 from the Xet server after 2 h on this link; plain
# HTTP resumes partial files. hf takes one pattern per --exclude.
# usage: nohup setsid dl.sh > logs/hf-download.log 2>&1 &
B=/var/tmp/glarb-bench V=$B/vllm-venv/bin
cd "$B" || exit 1
export HF_HUB_DISABLE_XET=1
get() { # REPO DIR [ARGS...]
  local repo=$1 dir=$2 i; shift 2
  [ -e "models/$dir.done" ] && return 0
  for i in 1 2 3 4 5 6; do
    "$V/hf" download "$repo" --local-dir "models/$dir" "$@" &&
      { touch "models/$dir.done"; return 0; }
    echo "dl: $repo attempt $i failed" >&2; sleep 10
  done
  return 1
}
rm -f models/dl.failed
get ByteDance-Seed/UI-TARS-1.5-7B ui-tars-1.5-7b &&
  get openai/gpt-oss-20b gpt-oss-20b --exclude "original/*" --exclude "metal/*" ||
  touch models/dl.failed
