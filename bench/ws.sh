# Persistent CDP websocket in a bash coprocess, for latency-sensitive loops.
# Source me, then: ws_open WS_URL; ws_call METHOD [PARAMS_JSON]; ws_close.
# Bash closes a coprocess's own fds in pipeline subshells, so ws_open copies
# them to plain fds, which every subshell inherits. Calls must run one at a
# time.
ws_open() {
  coproc WS { exec websocat -B 268435456 "$1" 2> /dev/null; }
  exec {WS_OUT}>&"${WS[1]}" {WS_IN}<&"${WS[0]}"
  WS_ID=0
}
# Print the reply to one call, skipping events. Chromium writes "id" first.
# grep reads the pipe in blocks; bash read takes one byte per syscall, which
# dominated the timing of large replies such as screenshots. grep may also
# swallow events that follow the reply; no caller needs those. Each id is
# the time in microseconds modulo 2^31, since CDP ids are 32-bit integers.
# A call in a subshell cannot advance a counter in its parent, and a reused
# id let a late reply to a timed-out call pass for the next call's reply.
# WS_TIMEOUT bounds the wait, in seconds.
ws_call() {
  local params=${2:-'{}'} id=$(( ${EPOCHREALTIME/./} % 2147483647 + 1 ))
  jq -cn --argjson i "$id" --arg m "$1" --argjson p "$params" \
    '{id: $i, method: $m, params: $p}' >&"$WS_OUT"
  timeout "${WS_TIMEOUT:-120}" grep -m1 "^{\"id\":$id," <&"$WS_IN"
}
ws_close() {
  exec {WS_OUT}>&- {WS_IN}<&-
  kill "$WS_PID" 2> /dev/null
  wait "$WS_PID" 2> /dev/null
}
# Websocket URL of the first page target on a DevTools port.
ws_page() {
  curl -s "http://127.0.0.1:$1/json/list" |
    jq -r '[.[] | select(.type == "page")][0].webSocketDebuggerUrl'
}
now_ms() { local t=${EPOCHREALTIME/./}; echo $(( t / 1000 )); }
