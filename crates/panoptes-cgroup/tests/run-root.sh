#!/usr/bin/env bash
# Run the kernel tests as root in a transient, delegated systemd scope that
# holds only the test binary. Usage: tests/run-root.sh (from the workspace).
set -euo pipefail
bin=$(cargo test -p panoptes-cgroup --test root --no-run --message-format=json 2>/dev/null |
  sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | tail -1)
[ -x "$bin" ] || { echo "test binary not found" >&2; exit 1; }
exec sudo systemd-run --scope --quiet --unit="panoptes-b0-test-$$" \
  -p Delegate=yes -p MemoryAccounting=yes \
  "$bin" --ignored --test-threads=1 --nocapture
