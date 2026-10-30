#!/bin/sh
# Runs inside the terminal under test: cats each workload and records how
# long the terminal took to consume it.
# usage: bench/run.sh DATA_DIR OUT_FILE [RUNS]
data=$1
out=$2
runs=${3:-3}

tmp="$out.tmp"
printf 'grid\t%s\n' "$(stty size)" > "$tmp"
for file in "$data"/*.txt; do
  name=$(basename "$file" .txt)
  i=1
  while [ "$i" -le "$runs" ]; do
    printf '\033[0m\033[r\033[H\033[2J'
    # cat writes to the terminal; only the timing goes to a file.
    perl -MTime::HiRes=time -e '$s = time; system("cat", $ARGV[0]); open my $t, ">", $ARGV[1]; printf $t "%.4f", time - $s' "$file" "$tmp.time"
    secs=$(cat "$tmp.time")
    printf '%s\t%s\t%s\t%s\n' "$name" "$i" "$secs" "$(wc -c < "$file" | tr -d ' ')" >> "$tmp"
    i=$((i + 1))
  done
done
printf '\033[0m\033[r\033[H\033[2J'
rm -f "$tmp.time"
mv "$tmp" "$out"
