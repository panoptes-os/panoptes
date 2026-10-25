#!/bin/sh
# usage: mcp-check.sh REPLIES.jsonl: check the replies to mcp-session.sh.
set -eu
f=$1
proto=$(jq -r 'select(.id == 1) | .result.protocolVersion' "$f")
names=$(jq -r 'select(.id == 2) | [.result.tools[].name] | join(",")' "$f")
shot=$(jq -r 'select(.id == 3) | .result.content | map(.type + ":" + (.mimeType // "")) | join(",")' "$f")
text=$(jq -r 'select(.id == 3) | .result.content[0].text' "$f")
printf 'mcp: protocol %s\nmcp: tools %s\nmcp: screenshot content %s\nmcp: %s\n' "$proto" "$names" "$shot" "$text" >&2
[ -n "$proto" ] && [ "$proto" != null ] || { echo "FAIL: no initialize reply" >&2; exit 1; }
case $names in *screenshot*) ;; *) echo "FAIL: tools/list lacks screenshot" >&2; exit 1 ;; esac
case $shot in *image:image/jpeg*) ;; *) echo "FAIL: screenshot returned no image" >&2; exit 1 ;; esac
