#!/bin/bash
# bench.sh - Time primitive-gpu over all modes, seeds and pictures; print one tab-separated row per run.
# Usage: ./bench.sh [binary] [extra flags...]   (default binary: target/release/primitive-gpu)
set -euo pipefail
cd "$(dirname "$0")"
BIN=${1:-target/release/primitive-gpu}
shift || true
N=${N:-50}
MODES=${MODES:-"0 1 2 3 4 5 6 7 8"}
SEEDS=${SEEDS:-"1 2 3 4 5"}
PICS=${PICS:-"monalisa owl"}
printf "pic\tmode\tseed\tscore\twall_ms\tcand_per_s\trand_ms\trand_prep\trand_draw_thr\trand_upload\trand_wait\tclimb_ms\tclimb_prep\tclimb_draw_thr\tclimb_upload\tclimb_wait\tbytes_cand_climb\n"
for pic in $PICS; do
  for m in $MODES; do
    for s in $SEEDS; do
      out=$("$BIN" -i "../examples/$pic.png" -o /tmp/bench_out.png -n "$N" -m "$m" -seed "$s" -vv "$@")
      score=$(grep "^$N: " <<<"$out" | sed 's/.*score=\([0-9.]*\).*/\1/')
      r=($(grep "^  random " <<<"$out"))
      c=($(grep "^  climb " <<<"$out"))
      tot=$(grep "^  commit" <<<"$out" | sed 's/.*total wall \([0-9.]*\) ms, candidates \([0-9]*\).*/\1 \2/')
      read -r wall cand <<<"$tot"
      cps=$(awk -v c="$cand" -v w="$wall" 'BEGIN{printf "%.0f", c/(w/1000)}')
      # columns: phase wall prepare make draw pack draw/thr upload gpu-wait other cand/s bytes/cand
      printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" \
        "$pic" "$m" "$s" "$score" "$wall" "$cps" \
        "${r[1]}" "${r[2]}" "${r[6]}" "${r[7]}" "${r[8]}" \
        "${c[1]}" "${c[2]}" "${c[6]}" "${c[7]}" "${c[8]}" "${c[11]}"
    done
  done
done
