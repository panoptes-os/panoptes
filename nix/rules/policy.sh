# shellcheck shell=bash
# policy-check: the policy files that each crate lists in
# [package.metadata.panoptes] name only plain data, std's collections and
# each other: policy 2 and 3 in docs/design/verification.md. Their test
# modules are not checked. Run it from the workspace root.
set -euo pipefail

fail=0
report() {
  echo "$1: $2" >&2
  fail=1
}

# A policy file may import these, other policy modules of its crate, and
# its own enums' variants.
std_use='^(std|core|alloc)::(borrow|cmp|collections|convert|fmt|hash|iter|mem|num|ops|option|result|str|string|vec|time::Duration)\b'
# And may not name these anywhere: I/O, the clock, threads, locks, atomics,
# shared or interior-mutable state, unsafe code, closures and trait objects
# taken as arguments, and async.
denied='\bunsafe\b|\bextern\b|\bstatic\s+mut\b|\b(Cell|RefCell|OnceCell|Rc|Arc|Weak|Mutex|RwLock|Condvar|OnceLock|LazyLock)\b|\bAtomic[A-Za-z0-9]+\b|\bdyn\b|\b(Fn|FnMut|FnOnce)\s*\(|\basync\b|\.await\b|\bpanoptes_sync\b|\bsync::|\b(Instant|SystemTime)\b|\b(std|core)::(env|fs|io|net|os|process|sync|thread)\b|\b(libc|nix|rustix|tokio)::'

meta=$(cargo metadata --no-deps --format-version 1 --offline)
code=$(mktemp)
trap 'rm -f "$code"' EXIT

# One line per crate: its directory, then its policy files.
while read -r dir files; do
  read -ra list <<< "$files"
  # The module names the files give, which may name each other.
  mods=$(for f in "${list[@]}"; do basename "$f" .rs; done | paste -sd '|' -)
  for f in "${list[@]}"; do
    file=$dir/$f
    [ -f "$file" ] || {
      report "$file" "listed as policy, but missing"
      continue
    }
    # The code before the test module, without comments; lines keep their
    # numbers.
    awk '/^#\[cfg\((all\()?test/ { exit } { sub(/\/\/.*/, ""); print }' "$file" > "$code"

    while IFS=: read -r n what; do
      report "$file:$n" "names \`$what\`"
    done < <(rg -n -o "$denied" "$code" || true)

    while IFS=: read -r n path; do
      path=${path#*use }
      case $path in
        self::* | [A-Z]*) ;;
        crate::* | super::*) ;; # checked below with every crate path
        *) rg -q "$std_use" <<< "$path" || report "$file:$n" "imports \`$path\`" ;;
      esac
    done < <(rg -n -o '^\s*(pub(\([a-z]+\))?\s+)?use\s+[A-Za-z_][A-Za-z0-9_:]*' "$code" || true)

    while IFS=: read -r n path; do
      mod=${path#*::}
      [[ $mod =~ ^($mods)$ ]] || report "$file:$n" "names \`$path\`, which is not a policy module"
    done < <(rg -n -o '\b(crate|super)::[A-Za-z_][A-Za-z0-9_]*' "$code" || true)
  done
done < <(jq -r --arg root "$PWD/" '.packages[]
  | select(.metadata.panoptes.policy)
  | "\(.manifest_path | ltrimstr($root) | rtrimstr("/Cargo.toml")) \(.metadata.panoptes.policy | join(" "))"' <<< "$meta")

if [ "$fail" = 0 ]; then
  echo "policy-check: ok"
fi
exit "$fail"
