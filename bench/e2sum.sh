#!/usr/bin/env bash
# E2 summary: per resolution, the mean over pages of the agent's charge and
# each capture path's median latency and size; then per page, the size of
# the text observations at 1280x720.
awk -F'\t' '
function kv(s, k,   r) { if (s !~ k "=") return 0; r = s; sub(".*" k "=", "", r); sub(/[ \]].*/, "", r); return r + 0 }
{ r = $1; if (!(r in seen)) { seen[r] = 1; order[++nr] = r }
  n[r]++; cur[r] += kv($4, "current"); pss[r] += kv($4, "pss")
  for (f = 5; f <= NF; f++) { split($f, kvs, " "); for (j in kvs) { split(kvs[j], p, "="); v[r, p[1]] += p[2] } }
  if (r == "1280x720") { obsrow[$3] = sprintf("%-5s ax_nodes=%d ax_txt=%.1f KB text=%.1f KB", $3, kv($5, "ax_nodes"), kv($5, "ax_txt_b") / 1024, kv($5, "text_b") / 1024) }
}
END {
  printf "%-10s %5s %5s | %-14s %-14s | %-14s %-14s %-14s | noop\n", "res", "cur", "pss", "grim png", "grim jpg", "cdp png", "cdp jpg", "cdp webp"
  for (i = 1; i <= nr; i++) { r = order[i]; k = n[r]
    printf "%-10s %5d %5d |", r, cur[r] / k, pss[r] / k
    split("grim_png grim_jpg cdp_png cdp_jpg cdp_webp", fs, " ")
    for (j = 1; j <= 5; j++) printf " %3d ms %5.0f KB", v[r, fs[j] "_ms"] / k, v[r, fs[j] "_b"] / k / 1024
    printf " | %d\n", v[r, "cdp_noop_ms"] / k
  }
  for (pg in obsrow) print obsrow[pg]
}' /var/tmp/glarb-bench/results/e2.tsv
