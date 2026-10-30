"""Summarizes bench/results/*.tsv: median time per workload, throughput, and
how each terminal compares with the baseline (default: ghostty).

usage: python3 bench/compare.py [RESULTS_DIR] [BASELINE]
"""
import glob
import os
import statistics
import sys

results_dir = sys.argv[1] if len(sys.argv) > 1 else "bench/results"
baseline = sys.argv[2] if len(sys.argv) > 2 else "ghostty"

data = {}  # terminal -> workload -> (median seconds, bytes)
for path in sorted(glob.glob(os.path.join(results_dir, "*.tsv"))):
    terminal = os.path.basename(path)[:-4]
    runs = {}
    with open(path) as f:
        for line in f:
            fields = line.rstrip("\n").split("\t")
            if len(fields) == 4:
                runs.setdefault(fields[0], []).append((float(fields[2]), int(fields[3])))
    data[terminal] = {w: (statistics.median(t for t, _ in r), r[0][1]) for w, r in runs.items()}

terminals = sorted(data, key=lambda t: (t != "mightty", t != baseline, t))
workloads = sorted({w for t in data.values() for w in t})
print(f"{'workload':14}" + "".join(f"{t:>22}" for t in terminals))
for w in workloads:
    row = f"{w:14}"
    base = data.get(baseline, {}).get(w)
    for t in terminals:
        if w not in data[t]:
            row += f"{'-':>22}"
            continue
        secs, size = data[t][w]
        cell = f"{size / 1048576 / secs:7.0f} MiB/s"
        if base and t != baseline:
            cell += f" ({base[0] / secs:4.2f}x)"
        row += f"{cell:>22}"
    print(row)
print(f"\nmedian of runs; (Nx) = speed relative to {baseline}, >1 is faster")
