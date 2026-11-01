# shellcheck shell=bash
# rule-copies-check: the rules in docs/design/verification.md and the
# agents' copy in docs/agents/verification.md have the same numbers, in
# the same sections. Run it from the repository root.
set -euo pipefail

# Each rule section's numbers, as "policy 1 2 3 ...": a rule is a line
# that starts with its number.
numbers() {
  awk '
    /^## / { sec = "" }
    /^## Policy/ { sec = "policy" }
    /^## Threads/ { sec = "threads" }
    /^## I\/O/ { sec = "io" }
    /^## tokio/ { sec = "tokio" }
    sec != "" && /^[0-9]+\. / { n[sec] = n[sec] " " $1 + 0 }
    END { for (s in n) print s n[s] }
  ' "$1" | sort
}

human=docs/design/verification.md
agents=docs/agents/verification.md
if ! diff <(numbers "$human") <(numbers "$agents"); then
  echo "rule-copies-check: $human and $agents number their rules differently" >&2
  exit 1
fi
for s in policy threads io tokio; do
  numbers "$human" | grep -q "^$s " || {
    echo "rule-copies-check: no $s rules in $human" >&2
    exit 1
  }
done
echo "rule-copies-check: ok"
