# shellcheck shell=bash
# mutants-check DIFF: cargo-mutants on the lines DIFF changes in the policy
# and threads files that each crate lists in [package.metadata.panoptes].
# Policy files must fail the crate's tests and threads files its loom
# models: policy 7 and 8, threads 12 and 13 in docs/design/verification.md.
# Run it from the workspace root; .cargo/mutants.toml lists what is skipped.
set -euo pipefail
diff=$1
jobs=${MUTANTS_JOBS:-2}
out=${MUTANTS_OUT:-target/mutants}

meta=$(cargo metadata --no-deps --format-version 1 --offline)
status=0
while IFS=$'\t' read -r pkg layer files; do
  read -ra list <<< "$files"
  args=(--in-diff "$diff" --package "$pkg" --jobs "$jobs" --no-shuffle
    --output "$out/$pkg-$layer")
  for f in "${list[@]}"; do args+=(--file "$f"); done
  mkdir -p "$out/$pkg-$layer"
  echo "== $pkg $layer: ${list[*]}"
  case $layer in
    policy) cargo mutants "${args[@]}" || status=1 ;;
    threads)
      RUSTFLAGS="--cfg panoptes_loom" cargo mutants "${args[@]}" \
        --profile release -- loom_models || status=1
      ;;
  esac
done < <(jq -r --arg root "$PWD/" '.packages[] | . as $p
  | ($p.manifest_path | ltrimstr($root) | rtrimstr("Cargo.toml")) as $dir
  | ("policy", "threads") as $layer
  | select($p.metadata.panoptes[$layer])
  | [$p.name, $layer, ([$p.metadata.panoptes[$layer][] | $dir + .] | join(" "))]
  | @tsv' <<< "$meta")
exit "$status"
