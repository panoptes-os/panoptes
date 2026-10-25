# Summarize results/e1b.tsv: mean of reps per set and page for
# current, anon, pss, purged current, procs. Run on the server.
awk -F'\t' '
function val(s, k,   r) { r = s; sub(".*" k "=", "", r); sub(/[ \]].*/, "", r); return r + 0 }
{ k = $2 SUBSEP $3; n[k]++
  cur[k] += val($5, "current"); an[k] += val($5, "anon"); pss[k] += val($5, "pss")
  pc[k] += val($6, "current"); pr[k] = val($5, "procs")
  if (!(($2) in seen)) { seen[$2] = 1; order[++ns] = $2 } }
END {
  split("blank gh wd wiki", pg, " ")
  printf "%-7s", "set"; for (j = 1; j <= 4; j++) printf "  %-17s", pg[j]; printf "  procs\n"
  for (i = 1; i <= ns; i++) { s = order[i]; printf "%-7s", s
    for (j = 1; j <= 4; j++) { k = s SUBSEP pg[j]
      if (n[k]) printf "  %4d %4d/%4d   ", cur[k]/n[k], an[k]/n[k], pc[k]/n[k]; else printf "  %-17s", "-" }
    printf "  %d\n", pr[s SUBSEP "gh"] }
}' /var/tmp/glarb-bench/results/e1b.tsv
