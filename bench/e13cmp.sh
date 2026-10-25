#!/usr/bin/env bash
# Compare a runtime's e13acc.sh outputs against vLLM's: same prompt token
# count, exact text match, and mean length of the shared prefix in chars.
# usage: e13cmp.sh results/e13-acc-vllm.tsv results/e13-acc-OTHER.tsv
awk -F"\t" 'NR == FNR {gsub(/<\|[a-z_]+\|>/, "", $3); tok[$1] = $2; txt[$1] = $3; next}
  {gsub(/<\|[a-z_]+\|>/, "", $3)}
  ($1 in txt) {
    n++; if ($2 == tok[$1]) same_tok++; if ($3 == txt[$1]) exact++
    a = txt[$1]; b = $3; k = 0
    while (k < length(a) && k < length(b) && substr(a, k + 1, 1) == substr(b, k + 1, 1)) k++
    pre += k; len += length(a)
  }
  END {printf "prompts=%d same_prompt_tokens=%d exact=%d mean_shared_prefix_chars=%d of %d\n",
    n, same_tok, exact, pre / n, len / n}' "$1" "$2"
