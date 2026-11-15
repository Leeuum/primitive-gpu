#!/bin/bash
# check.sh - Run -check for every shape mode on both GPU paths and both scores, and say PASS or FAIL against check_baseline.tsv.
# Usage: ./check.sh          compare with the saved numbers
#        ./check.sh --save   save the current numbers as the new reference
# The run fails when a row has more mismatching candidates, or more wrong batch winners, than the saved row.
set -euo pipefail
cd "$(dirname "$0")"
BIN=${BIN:-target/release/primitive-gpu}
N=${N:-10}
MODES=${MODES:-"1 2 3 4 5 6 7 8"}
RASTERS=${RASTERS:-"gpu cpu"}
METRICS=${METRICS:-"squared plain"}
PIC=${PIC:-owl}
BASE=check_baseline.tsv
new=$(mktemp)
trap 'rm -f "$new"' EXIT
printf "metric\traster\tmode\tcandidates\tdiffer\tmax_diff\twinner_differs\tscore\n" >"$new"
for e in $METRICS; do
for r in $RASTERS; do
  for m in $MODES; do
    out=$("$BIN" -i "../examples/$PIC.png" -o /tmp/check_out.png -n "$N" -m "$m" -seed 1 -j 8 -v -check -raster "$r" -metric "$e")
    cand=$(sed -n 's/^check: \([0-9]*\) candidates.*/\1/p' <<<"$out")
    differ=$(sed -n 's/^  score differs from exact: \([0-9]*\) candidates.*/\1/p' <<<"$out")
    maxd=$(sed -n 's/.*max |diff| \([0-9]*\),.*/\1/p' <<<"$out")
    win=$(sed -n 's/^  batch winner differs: \([0-9]*\) of.*/\1/p' <<<"$out")
    score=$(grep "^$N: " <<<"$out" | sed 's/.*score=\([0-9.]*\).*/\1/')
    printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$e" "$r" "$m" "$cand" "$differ" "$maxd" "$win" "$score" >>"$new"
  done
done
done
if [ "${1:-}" = "--save" ]; then
  cp "$new" "$BASE"
  column -t "$BASE"
  echo "Saved to $BASE"
  exit 0
fi
column -t "$new"
[ -f "$BASE" ] || { echo "FAIL: no $BASE. Run ./check.sh --save first."; exit 1; }
bad=$(awk -F'\t' 'NR==FNR { k=$1 FS $2 FS $3; d[k]=$5; w[k]=$7; next }
  FNR>1 { k=$1 FS $2 FS $3
    if (!(k in d)) next
    if ($5=="" || $5+0 > d[k]+0 || $7+0 > w[k]+0)
      printf "  metric=%s raster=%s mode=%s: differ %s (saved %s), wrong winners %s (saved %s)\n", $1, $2, $3, $5, d[k], $7, w[k] }' "$BASE" "$new")
if [ -n "$bad" ]; then
  echo "FAIL: worse than $BASE"
  echo "$bad"
  exit 1
fi
echo "PASS"
