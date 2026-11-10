#!/bin/sh
# Interleaved comparison: alternates terminals round by round so they all
# see the same system conditions, then summarizes medians.
# usage: bench/compare.sh [ROUNDS] [RUNS_PER_ROUND]
# Needs a running mightty (for `mightty ctl spawn`); Ghostty and kitty are
# used when installed.
set -eu
cd "$(dirname "$0")/.."
rounds=${1:-5}
runs=${2:-3}
root=$PWD
out=bench/results/interleaved
mkdir -p "$out"
rm -f "$out"/*.tsv "$out"/*.part
[ -d bench/data ] || python3 -I bench/gen.py bench/data 16
mightty=$root/target/release/mightty
kitty=/Applications/kitty.app/Contents/MacOS/kitty

wait_for() {
  i=0
  while [ ! -f "$1" ] && [ $i -lt 1200 ]; do sleep 0.5; i=$((i + 1)); done
}

for round in $(seq 1 "$rounds"); do
  echo "round $round/$rounds"
  part=$root/$out/mightty.part
  # One string, which the pane's shell splits: quoted for paths with spaces.
  pane=$("$mightty" ctl spawn -- "'$root/bench/run.sh' '$root/bench/data' '$part' $runs")
  wait_for "$part"
  "$mightty" ctl close "$pane" >/dev/null
  cat "$part" >> "$out/mightty.tsv" && rm "$part"

  if [ -d /Applications/Ghostty.app ]; then
    part=$root/$out/ghostty.part
    open -na /Applications/Ghostty.app --args --window-width=120 --window-height=40 \
      --font-family=Menlo --font-size=14 --quit-after-last-window-closed=true \
      --confirm-close-surface=false -e "$root/bench/run.sh" "$root/bench/data" "$part" "$runs"
    wait_for "$part"
    cat "$part" >> "$out/ghostty.tsv" && rm "$part"
  fi

  if [ -x "$kitty" ]; then
    part=$root/$out/kitty.part
    # Shipped defaults (no user config), same grid and font as the others.
    "$kitty" --config NONE --single-instance=no -o remember_window_size=no \
      -o initial_window_width=120c -o initial_window_height=40c \
      -o font_family=Menlo -o font_size=14 -o confirm_os_window_close=0 \
      -o macos_quit_when_last_window_closed=yes \
      "$root/bench/run.sh" "$root/bench/data" "$part" "$runs" >/dev/null 2>&1 &
    wait_for "$part"
    wait
    cat "$part" >> "$out/kitty.tsv" && rm "$part"
  fi
done
python3 -I bench/compare.py "$out"
