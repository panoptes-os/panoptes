#!/usr/bin/env bash
# Memory of one cgroup: the kernel's charge and the summed PSS of its
# processes. Run as root. usage: mem.sh CGROUP_DIR [breakdown]
cg=$1
st() { awk -v k="$1" '$1==k{v=$2} END{print v+0}' "$cg/memory.stat"; }
mapfile -t pids < <(find "$cg" -name cgroup.procs -exec cat {} +)
pss=0
for p in "${pids[@]}"; do
  v=$(awk '/^Pss:/{print $2}' "/proc/$p/smaps_rollup" 2>/dev/null)
  pss=$((pss + ${v:-0}))
done
mib() { echo $(( $1 >> 20 )); }
printf 'current=%s anon=%s file=%s shmem=%s kernel=%s zswap=%s zswapped=%s pss=%s procs=%s\n' \
  "$(mib "$(cat "$cg/memory.current")")" "$(mib "$(st anon)")" "$(mib "$(st file)")" \
  "$(mib "$(st shmem)")" "$(mib "$(st kernel)")" "$(mib "$(st zswap)")" \
  "$(mib "$(st zswapped)")" "$((pss >> 10))" "${#pids[@]}"
[ "${2:-}" = breakdown ] || exit 0
for p in "${pids[@]}"; do
  v=$(awk '/^Pss:/{print $2}' "/proc/$p/smaps_rollup" 2>/dev/null) || continue
  role=$(tr '\0 ' '\n\n' < "/proc/$p/cmdline" |
    grep -oE -- '^--(type|utility-sub-type)=[A-Za-z.-]+' | sed 's/^--//' | paste -sd' ')
  printf '%s\t%s\n' "${v:-0}" "$(cat "/proc/$p/comm")${role:+ $role}"
done | awk -F'\t' '{s[$2]+=$1; n[$2]++}
  END {for (k in s) printf "%8.1f MiB %3d  %s\n", s[k]/1024, n[k], k}' | sort -rn
