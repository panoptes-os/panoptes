#!/usr/bin/env bash
# report.sh ARM: one row per invoice from panoptes-run/gate/ARM/*/: verified,
# attached, steps, tokens (prompt/cached/completion/reasoning), $, median
# ttft and model call, wall, failed checks, error.
B=/var/tmp/glarb-bench G=$B/panoptes-run/gate/$1
. $B/scripts/loop/jq.sh
printf 'inv\tver\tatt\tsteps\tprompt\tcached\tcompl\treason\tusd\tttft_p50\tcall_p50\twall_s\tfailed\terror\n'
for d in $G/inv-*/; do
  [ -f $d/results.jsonl ] || continue
  jq -s -r --slurpfile r $d/results.jsonl '
    def med: sort | if length == 0 then 0 else .[length/2|floor] end;
    map(select(.step > 0)) as $s | $r[0] as $x |
    [$x.invoice, $x.verified, $x.attached, $x.steps,
     ($s|map(.prompt_tokens//0)|add), ($s|map(.cached_tokens//0)|add),
     ($s|map(.completion_tokens//0)|add), ($s|map(.reasoning_tokens//0)|add),
     ($x.cost_usd*10000|round/10000), ($s|map(.ttft_ms)|med|round), ($s|map(.model_ms)|med|round),
     ($x.wall_ms/1000|round), ($x.failed//[]|join(",")), ($x.error//"")] | @tsv' $d/trace.jsonl
done
