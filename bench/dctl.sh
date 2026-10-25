#!/usr/bin/env bash
# dctl.sh TOOL [JSON]: one call to the discovery desk; prints result text.
B=/var/tmp/glarb-bench; S=${SOCK:-$B/panoptes-run/disc/desk.sock}
r=$($B/panoptes/target/debug/panoptesctl desk "$S" "$1" "${2:-{\}}")
if [ "$1" = ax_tree ]; then printf '%s\n' "$r" | jq -r '.result.tree // .error'
else printf '%s\n' "$r" | jq -c 'del(.image)'; fi
