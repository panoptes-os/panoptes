#!/usr/bin/env bash
# One line per E4 row: page, mode, zswap allocator/compressor, writeback,
# charge before and parked, what zswap holds compressed and the original
# size it stands for (ratio), the global pool's real growth (pool, from
# debugfs, newer rows only), swap growth, pages written back to the swap
# device, reclaim and thaw times, first actions after thaw (ms from thaw),
# and the charge and swap after resume. Cold relaunch rows print as they
# are. Fields: $5 before, $6 freeze and reclaim, $7 parked, $8 after hold,
# $9 resume timings, $10 resumed.
awk -F'\t' '
function kv(s, k,   r) { r = " " s; if (r !~ "[[ ]" k "=") return "-"; sub(".*[[ ]" k "=", "", r); sub(/[ \]].*/, "", r); return r }
$2 == "cold-relaunch" { printf "%-4s cold-relaunch  %s\n", $1, $3; next }
{
  z = kv($7, "zswap") + 0; zd = kv($7, "zswapped") + 0
  ratio = (z > 0) ? sprintf("%.1f", zd / z) : "-"
  wbp = kv($7, "zswpwb"); wbm = (wbp == "-") ? "-" : int(wbp * 4 / 1024)
  printf "%-4s %-22s %-13s wb=%s cur %3s -> %3s (anon %3s file %3s) zswap %3s holds %3s (x%s) pool %3s swap +%s wrote %s  reclaim %4s ms  thaw %s cdp %3s grim %3s scroll %3s next %3s  resumed %3s swap %s\n",
    $1, $2, kv($4, "zswap"), kv($4, "wb"), kv($5, "current"), kv($7, "current"),
    kv($7, "anon"), kv($7, "file"), z, zd, ratio, kv($7, "pool"),
    kv($7, "swap") - kv($5, "swap"), wbm, kv($6, "reclaim_ms"), kv($9, "thaw_ms"),
    kv($9, "first_cdp_ms"), kv($9, "first_grim_ms"), kv($9, "scroll_shot_ms"),
    kv($9, "next_scroll_shot_ms"), kv($10, "current"), kv($10, "swap")
}' /var/tmp/glarb-bench/results/e4.tsv
