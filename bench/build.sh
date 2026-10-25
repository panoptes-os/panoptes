#!/usr/bin/env bash
# build.sh: clippy -D warnings, tests, release build; full log in logs/build.log.
set -o pipefail
cd /var/tmp/glarb-bench/panoptes || exit 1
nix develop path:. -c bash -c 'cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo build --release --workspace' \
  > /var/tmp/glarb-bench/logs/build.log 2>&1
rc=$?; echo "build exit $rc"
if [ $rc -ne 0 ]; then rg -n -A12 '^(error|warning)' /var/tmp/glarb-bench/logs/build.log | head -120; else rg '^test result|running' /var/tmp/glarb-bench/logs/build.log; fi
exit $rc
