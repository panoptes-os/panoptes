# shellcheck shell=bash
# mutants-check DIFF: cargo-mutants on the lines DIFF changes in the policy
# and threads files that each crate lists in [package.metadata.panoptes]. A
# mutant of a policy file must fail the crate's tests. A mutant of a threads
# file must fail those tests or its loom models: policy 7, threads 12 and 13
# in docs/design/verification.md. Run it from the workspace root;
# .cargo/mutants.toml lists what is skipped.
set -euo pipefail
diff=$1
jobs=${MUTANTS_JOBS:-2}
out=${MUTANTS_OUT:-target/mutants}

# What the run in directory $1 put in its lists $2..., sorted. A run that
# found no mutants writes no lists.
recorded() {
  local dir=$1/mutants.out
  shift
  for name in "$@"; do
    [ ! -f "$dir/$name.txt" ] || cat "$dir/$name.txt"
  done | sort -u
}

# Run one half of a threads check. cargo-mutants exits 2 if it missed a
# mutant and 3 if one timed out: results that the other half may answer. Any
# other failure ends the check's success.
half() {
  local code=0
  "$@" || code=$?
  case $code in
    0 | 2 | 3) ;;
    *)
      echo "cargo-mutants exited $code" >&2
      status=1
      ;;
  esac
}

meta=$(cargo metadata --no-deps --format-version 1 --offline)
status=0
while IFS=$'\t' read -r pkg layer files; do
  read -ra list <<< "$files"
  args=(--in-diff "$diff" --package "$pkg" --jobs "$jobs" --no-shuffle)
  dir=$out/$pkg-$layer
  for f in "${list[@]}"; do
    [ -f "$f" ] || {
      echo "$f: listed in [package.metadata.panoptes], but missing" >&2
      exit 1
    }
    args+=(--file "$f")
  done
  mkdir -p "$dir/tests" "$dir/loom"
  echo "== $pkg $layer: ${list[*]}"
  case $layer in
    policy) cargo mutants "${args[@]}" --output "$dir/tests" || status=1 ;;
    threads)
      half cargo mutants "${args[@]}" --output "$dir/tests"
      half env RUSTFLAGS="--cfg panoptes_loom" CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true \
        cargo mutants "${args[@]}" --output "$dir/loom" --profile release -- loom_models
      # Missed by one half, and not caught by the other. A timeout is a catch.
      survivors=$({
        comm -23 <(recorded "$dir/tests" missed) <(recorded "$dir/loom" caught timeout)
        comm -23 <(recorded "$dir/loom" missed) <(recorded "$dir/tests" caught timeout)
      } | sort -u)
      if [ -n "$survivors" ]; then
        echo "Missed by both the tests and the loom models:"
        echo "$survivors"
        status=1
      fi
      ;;
  esac
done < <(jq -r --arg root "$PWD/" '.packages[] | . as $p
  | ($p.manifest_path | ltrimstr($root) | rtrimstr("Cargo.toml")) as $dir
  | ("policy", "threads") as $layer
  | select($p.metadata.panoptes[$layer])
  | [$p.name, $layer, ([$p.metadata.panoptes[$layer][] | $dir + .] | join(" "))]
  | @tsv' <<< "$meta")
exit "$status"
