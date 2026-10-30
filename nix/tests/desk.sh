# desk-check RUN_DIR: drive a running panoptes-desk over MCP on
# RUN_DIR/desk.sock and check what wev (Wayland) and xev (X11) receive.
run=$1
sock=$run/desk.sock

fail() {
  echo "FAIL: $*" >&2
  echo "--- last reply" >&2
  cut -c1-2000 reply.json >&2 || true
  for f in "$run"/logs/*; do
    echo "--- $f" >&2
    tail -n 40 "$f" >&2
  done
  journalctl -u desk --no-pager -n 80 >&2 || true
  exit 1
}

cd "$(mktemp -d)" || exit 1
coproc MCP { socat - "UNIX-CONNECT:$sock"; }
id=0

# rpc METHOD PARAMS_JSON: one JSON-RPC request; the reply goes to reply.json.
rpc() {
  id=$((id + 1))
  jq -nc --argjson id "$id" --arg m "$1" --argjson p "$2" \
    '{jsonrpc: "2.0", id: $id, method: $m, params: $p}' >&"${MCP[1]}"
  IFS= read -r -t 300 line <&"${MCP[0]}" || fail "no reply to $1"
  printf '%s\n' "$line" > reply.json
  [ "$(jq .id reply.json)" = "$id" ] || fail "reply to $1 has the wrong id"
}

# call TOOL [ARGS_JSON]: a tool call that must succeed. Its result goes to
# result.json and its screenshot, if any, to shot.png. Apps draw slowly in a
# VM without KVM, so the screen may take up to 30 s to settle.
call() {
  rpc tools/call "$(jq -nc --arg n "$1" --argjson a "${2:-"{}"}" \
    '{name: $n, arguments: ({settle_ms: 30000} + $a)}')"
  [ "$(jq '.result.isError' reply.json)" != true ] ||
    fail "$1 failed: $(jq -r '.result.content[0].text' reply.json)"
  jq -r '.result.content[0].text' reply.json | jq .result > result.json
  rm -f shot.png
  if [ "$(jq -r '.result.content[1].type' reply.json)" = image ]; then
    [ "$(jq -r '.result.content[1].mimeType' reply.json)" = image/png ] || fail "$1: not a PNG"
    jq -r '.result.content[1].data' reply.json | base64 -d > shot.png
  fi
}

# refuse TOOL ARGS_JSON PATTERN: a tool call that must fail with PATTERN.
refuse() {
  rpc tools/call "$(jq -nc --arg n "$1" --argjson a "$2" '{name: $n, arguments: $a}')"
  [ "$(jq '.result.isError' reply.json)" = true ] || fail "$1 $2 succeeded"
  jq -r '.result.content[0].text' reply.json | grep -Eq "$3" ||
    fail "$1 $2 failed without /$3/"
}

# is JQ_EXPR: the last result satisfies JQ_EXPR.
is() { jq -e "$1" result.json > /dev/null || fail "result is not $1"; }

# logged FILE RE: within 30 s, FILE has a line matching RE.
logged() {
  for _ in $(seq 300); do
    grep -Eq "$2" "$1" && return 0
    sleep 0.1
  done
  fail "$(basename "$1") never logged /$2/"
}

# logfile RE: within 30 s, the app log that has a line matching RE.
logfile() {
  for _ in $(seq 300); do
    grep -El "$1" "$run"/logs/* 2> /dev/null && return 0
    sleep 0.1
  done
  fail "no app log matches /$1/"
}

full_screen() { file shot.png | grep -q 'PNG image data, 1024 x 768' || fail "screenshot: $(file shot.png)"; }

echo "== MCP session"
rpc initialize '{"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "desk-check", "version": "0"}}'
jq -e .result.protocolVersion reply.json > /dev/null || fail "initialize"
echo '{"jsonrpc": "2.0", "method": "notifications/initialized"}' >&"${MCP[1]}"
rpc tools/list '{}'
for t in screenshot windows click move drag scroll key type launch focus close kill wait; do
  jq -e --arg t "$t" '[.result.tools[].name] | index($t)' reply.json > /dev/null || fail "no tool $t"
done

echo "== the empty desktop"
call screenshot
full_screen
is '.width == 1024 and .height == 768'
base64 -d "$run/thumb.b64" > thumb.png
file thumb.png | grep -q 'PNG image data, 102 x 76' || fail "thumbnail: $(file thumb.png)"
call windows
is '.windows == [] and (.xwayland | test("^:[0-9]+$"))'

echo "== launch failures"
refuse launch '{"argv": ["false"]}' 'exited \(exit code 1\) before opening a window'
refuse launch '{"argv": ["no-such-program"]}' 'launch no-such-program'

echo "== wev, a Wayland window"
call launch '{"argv": ["stdbuf", "-oL", "wev"], "timeout_ms": 120000}'
full_screen
wev_pid=$(jq .pid result.json)
is '.windows.opened | length == 1'
is ".windows.opened[0] | .app_id == \"wev\" and .pid == $wev_pid and .x == 0 and .y == 0
  and .width == 1024 and .height == 768 and .focused"
wev=$(jq '.windows.opened[0].id' result.json)
is ".windows.focused == $wev"
wevlog=$(logfile 'wl_keyboard')

call click '{"x": 300, "y": 200}'
is '.timed_out == false and .settled_ms >= 300'
full_screen
logged "$wevlog" 'x, y: 300\.000000, 200\.000000'
logged "$wevlog" 'button: 272 \(left\), state: 1 \(pressed\)'
logged "$wevlog" 'button: 272 \(left\), state: 0 \(released\)'

call key '{"key": "ctrl+shift+t", "screenshot": false}'
[ ! -e shot.png ] || fail "screenshot despite screenshot: false"
logged "$wevlog" 'depressed: 00000005'
logged "$wevlog" 'sym: T +\(84\)'

call type '{"text": "é中\n", "screenshot": false}'
logged "$wevlog" "utf8: 'é'"
logged "$wevlog" "utf8: '中'"
logged "$wevlog" 'sym: Return'

call scroll '{"x": 500, "y": 400, "dy": 2, "screenshot": false}'
logged "$wevlog" 'discrete: 2|value120: 240'

call drag '{"x": 100, "y": 100, "to_x": 400, "to_y": 300, "screenshot": false}'
logged "$wevlog" 'x, y: 400\.000000, 300\.000000'

echo "== xev, an X11 window"
call launch '{"argv": ["stdbuf", "-oL", "xev"], "timeout_ms": 120000}'
xev_pid=$(jq .pid result.json)
is ".windows.opened | length == 1 and .[0].pid == $xev_pid and .[0].focused"
xev=$(jq '.windows.opened[0].id' result.json)
xevlog=$(logfile 'Outer window')

call click '{"x": 300, "y": 200, "screenshot": false}'
logged "$xevlog" 'ButtonPress event'
logged "$xevlog" '\(300,200\), root:\(300,200\)'
logged "$xevlog" 'button 1,'

call key '{"key": "ctrl+shift+t", "screenshot": false}'
logged "$xevlog" 'state 0x5, keycode 28 \(keysym 0x54, T\)'

# ñ is not bound yet, so typing it replaces the keymap while xev has focus.
call type '{"text": "é中ñ", "screenshot": false}'
logged "$xevlog" '\(keysym 0xe9, eacute\)'
logged "$xevlog" '\(keysym 0x1004e2d, U4E2D\)'
logged "$xevlog" '\(keysym 0xf1, ntilde\)'

echo "== focus, close, kill"
call focus "{\"window\": $wev}"
is ".windows.focused == $wev"
call windows
is ".windows | map(select(.id == $xev)) | .[0].focused == false"

call close "{\"window\": $wev}"
call wait '{"app_id": "wev", "closed": true, "timeout_ms": 60000}'
is '.met'

call kill "{\"window\": $xev}"
call wait "{\"pid\": $xev_pid, \"closed\": true, \"timeout_ms\": 60000}"
is '.met'
call windows
is '.windows == []'

call wait '{"app_id": "nothing", "timeout_ms": 500, "screenshot": false}'
is '.met == false'
refuse focus '{"window": 999}' 'no window 999'
refuse click '{"x": 1024, "y": 0}' 'off the 1024x768 screen'
refuse wait '{"app_id": "wev", "closed": "true"}' 'closed must be true or false'

echo "desk-check: ok"
