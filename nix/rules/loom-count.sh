# shellcheck shell=bash
# loom-count-check LOG: every crate that uses panoptes-sync, and the facade
# itself, ran at least one loom model in LOG, the output of the loom check's
# `cargo test --lib --bins -- loom_models`. A check that runs no model
# passes too: threads 13 in docs/design/verification.md. Run it from the
# workspace root.
set -euo pipefail
log=$1

# Test binaries are named for their crate, with - as _.
ran=$(awk '
  /Running unittests src\/(lib|main)\.rs/ {
    if (match($0, /deps\/[a-z0-9_]+-[0-9a-f]+\)/)) crate = substr($0, RSTART + 5, RLENGTH - 5)
    sub(/-[0-9a-f]+\)$/, "", crate)
    next
  }
  /^running [0-9]+ tests?$/ && crate != "" { n[crate] += $2; crate = "" }
  END { for (c in n) print c, n[c] }
' "$log")

fail=0
for manifest in crates/*/Cargo.toml; do
  name=$(awk -F'"' '/^name = / { print $2; exit }' "$manifest")
  [ "$name" = panoptes-sync ] || grep -q '^panoptes-sync[ .=]' "$manifest" || continue
  crate=${name//-/_}
  n=$(awk -v c="$crate" '$1 == c { print $2 }' <<< "$ran")
  if [ "${n:-0}" -gt 0 ]; then
    echo "loom-count-check: $name ran $n"
  else
    echo "loom-count-check: $name uses panoptes-sync but ran no loom model" >&2
    fail=1
  fi
done
exit "$fail"
