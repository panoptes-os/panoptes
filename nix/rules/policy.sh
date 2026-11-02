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
# And may not name these anywhere: I/O, printing, the clock, threads, locks,
# atomics, shared or interior-mutable state, unsafe code, closures and trait
# objects taken as arguments, and async.
denied='\bunsafe\b|\bextern\b|\bstatic\s+mut\b|\b(Cell|RefCell|OnceCell|Rc|Arc|Weak|Mutex|RwLock|Condvar|OnceLock|LazyLock)\b|\bAtomic[A-Za-z0-9]+\b|\bdyn\b|\b(Fn|FnMut|FnOnce)\s*\(|\basync\b|\.await\b|\b(Instant|SystemTime)\b|\b(std|core)::(env|fs|io|net|os|process|sync|thread)\b|\b(e?print(ln)?|dbg)!'
# A path in a policy file starts at one of these, or at a policy module, so
# it calls no other crate. Primitive types have paths too, and rustfmt and
# clippy name attributes.
roots='std|core|alloc|self|crate|super|bool|char|str|u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize|f32|f64|rustfmt|clippy'

meta=$(cargo metadata --no-deps --format-version 1 --offline)
code=$(mktemp)
hits=$(mktemp)
trap 'rm -f "$code" "$hits"' EXIT

# rg's matches in $code, into $hits. No match is fine; an error stops the
# check.
search() {
  rg -n -o "$@" "$code" > "$hits" || [ $? = 1 ]
}

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
    # numbers. A test attribute on anything but a module ends nothing.
    awk '
      { sub(/\/\/.*/, "") }
      attr && /^mod / { exit }
      { attr = /^#\[cfg\((all\()?test/; print }
    ' "$file" > "$code"

    search "$denied"
    while IFS=: read -r n what; do
      report "$file:$n" "names \`$what\`"
    done < "$hits"

    search '^\s*(pub(\([a-z]+\))?\s+)?use\s+[A-Za-z_][A-Za-z0-9_:]*'
    while IFS=: read -r n path; do
      path=${path#*use }
      case $path in
        self::* | [A-Z]*) ;;
        crate::* | super::*) ;; # checked below with every crate path
        *) rg -q "$std_use" <<< "$path" || report "$file:$n" "imports \`$path\`" ;;
      esac
    done < "$hits"

    search '\b(crate|super)::([A-Za-z_][A-Za-z0-9_]*|[{*])'
    while IFS=: read -r n path; do
      mod=${path#*::}
      [[ $mod =~ ^($mods)$ ]] || report "$file:$n" "names \`$path\`, which is not a policy module"
    done < "$hits"

    search -r "\$2" '(^|[^A-Za-z0-9_:.])([a-z_][a-z0-9_]*)::[A-Za-z_{*]'
    while IFS=: read -r n root; do
      [[ $root =~ ^($roots|$mods)$ ]] || report "$file:$n" "names \`$root::\`, another crate"
    done < "$hits"
  done
done < <(jq -r --arg root "$PWD/" '.packages[]
  | select(.metadata.panoptes.policy)
  | "\(.manifest_path | ltrimstr($root) | rtrimstr("/Cargo.toml")) \(.metadata.panoptes.policy | join(" "))"' <<< "$meta")

if [ "$fail" = 0 ]; then
  echo "policy-check: ok"
fi
exit "$fail"
