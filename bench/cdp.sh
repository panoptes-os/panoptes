#!/usr/bin/env bash
# One Chrome DevTools Protocol call; prints .result, or .error on failure.
# usage: cdp.sh PORT METHOD [PARAMS_JSON] [page|browser|WS_URL]
. "$(dirname "$0")/env.sh"
port=$1 method=$2 params=${3:-'{}'} target=${4:-page}
case $target in
  page) ws=$(curl -s "http://127.0.0.1:$port/json/list" |
    jq -r '[.[] | select(.type == "page")][0].webSocketDebuggerUrl') ;;
  browser) ws=$(curl -s "http://127.0.0.1:$port/json/version" |
    jq -r .webSocketDebuggerUrl) ;;
  *) ws=$target ;;
esac
jq -cn --arg m "$method" --argjson p "$params" '{id: 1, method: $m, params: $p}' |
  timeout 60 websocat -n1 -B 268435456 "$ws" | jq -c '.result // .error'
