#!/usr/bin/env bash
# Wait until the first page target reports document.readyState complete.
# With MODE nav, also wait until it has left the initial about:blank, which
# reports complete before the command-line URL commits.
# usage: waitload.sh PORT [TIMEOUT_S] [nav]
d=$(dirname "$0") port=$1 limit=$(( ${2:-60} * 2 )) mode=${3:-}
expr='document.readyState + " " + location.href'
for _ in $(seq "$limit"); do
  s=$("$d/cdp.sh" "$port" Runtime.evaluate \
    "$(jq -cn --arg e "$expr" '{expression: $e, returnByValue: true}')" |
    jq -r '.result.value // empty' 2> /dev/null)
  case $mode:$s in
    nav:"complete about:blank") ;;
    *:"complete "*) exit 0 ;;
  esac
  sleep 0.5
done
echo "waitload.sh: port $port not complete" >&2
exit 1
