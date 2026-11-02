"""Full terminal comparison: startup, memory, CPU, throughput, latency.

usage: python3 -I bench/suite.py [ROUNDS] [STARTUPS]

Every terminal runs with its shipped defaults apart from a 120x40 grid and
Menlo 14 (kitty with --config NONE, Ghostty with --config-default-files=false),
and terminals take turns round by round so they share system conditions.

The suite samples the terminal's own process (not its shell or children)
every 50 ms through proc_pid_rusage: CPU time, resident set (RSS) and
physical footprint. macOS has no PSS; physical footprint is its closest
equivalent (what Activity Monitor reports as Memory). bench/probe.py runs
inside each terminal, does the work and marks the phases.

Results: bench/results/suite/raw/ROUND/TERMINAL/ and summary.md.
"""
import ctypes
import os
import shutil
import statistics
import subprocess
import sys
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DATA = os.path.join(ROOT, "bench", "data")
OUT = os.path.join(ROOT, "bench", "results", "suite")
PROBE = os.path.join(ROOT, "bench", "probe.py")

TERMINALS = {
    "mightty": (
        [os.path.join(ROOT, "target", "release", "mightty"), "--config-default-files=false", "--window-width=120", "--window-height=40",
         "--font-family=Menlo", "--font-size=14", "-e"],
        {},
    ),
    "ghostty": (
        ["/Applications/Ghostty.app/Contents/MacOS/ghostty", "--config-default-files=false",
         "--window-width=120", "--window-height=40", "--font-family=Menlo", "--font-size=14",
         "--quit-after-last-window-closed=true", "--confirm-close-surface=false", "-e"],
        {},
    ),
    "kitty": (
        ["/Applications/kitty.app/Contents/MacOS/kitty", "--config", "NONE", "--single-instance=no",
         "-o", "remember_window_size=no", "-o", "initial_window_width=120c", "-o", "initial_window_height=40c",
         "-o", "font_family=Menlo", "-o", "font_size=14", "-o", "confirm_os_window_close=0",
         "-o", "macos_quit_when_last_window_closed=yes"],
        {},
    ),
}

# ---- process sampling ----

_lib = ctypes.CDLL("/usr/lib/libSystem.B.dylib")


class _Timebase(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


class _RusageV2(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [
        (name, ctypes.c_uint64)
        for name in ("user_time", "system_time", "pkg_idle_wkups", "interrupt_wkups", "pageins",
                     "wired_size", "resident_size", "phys_footprint", "proc_start_abstime",
                     "proc_exit_abstime", "child_user_time", "child_system_time",
                     "child_pkg_idle_wkups", "child_interrupt_wkups", "child_pageins",
                     "child_elapsed_abstime", "diskio_bytesread", "diskio_byteswritten")
    ]


_tb = _Timebase()
_lib.mach_timebase_info(ctypes.byref(_tb))


def rusage(pid):
    """(cpu seconds, rss bytes, footprint bytes), or None once the process is gone.
    CPU times come in mach ticks on Apple silicon, hence the timebase."""
    ru = _RusageV2()
    if _lib.proc_pid_rusage(pid, 2, ctypes.byref(ru)) != 0:
        return None
    return (ru.user_time + ru.system_time) * _tb.numer / _tb.denom / 1e9, ru.resident_size, ru.phys_footprint


class Sampler(threading.Thread):
    def __init__(self, pid, interval=0.05):
        super().__init__(daemon=True)
        self.pid, self.interval, self.samples = pid, interval, []
        self.stop = threading.Event()

    def run(self):
        while not self.stop.is_set():
            r = rusage(self.pid)
            if r is None:
                break
            self.samples.append((time.time(), *r))
            time.sleep(self.interval)


def window(samples, start, end):
    """CPU % (100 = one core) per interval, RSS and footprint in MiB, within [start, end]."""
    inside = [s for s in samples if start <= s[0] <= end]
    cpu = [100 * (b[1] - a[1]) / (b[0] - a[0]) for a, b in zip(inside, inside[1:]) if b[0] > a[0]]
    return cpu, [s[2] / 1048576 for s in inside], [s[3] / 1048576 for s in inside]


# ---- running terminals ----

def launch(name, command):
    argv, env = TERMINALS[name]
    return subprocess.Popen(argv + command, env={**os.environ, **env}, cwd=ROOT,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def finish(proc, grace=3):
    """Lets the terminal quit by itself after its command exits, or ends it."""
    try:
        proc.wait(grace)
    except subprocess.TimeoutExpired:
        proc.terminate()
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def wait_for(path, timeout):
    deadline = time.time() + timeout
    while not os.path.exists(path):
        if time.time() > deadline:
            raise TimeoutError(path)
        time.sleep(0.01)


def startup(name, scratch):
    """Seconds from launch until the terminal's command runs in a ready PTY."""
    mark = os.path.join(scratch, f"{name}.started")
    if os.path.exists(mark):
        os.remove(mark)
    start = time.time()
    proc = launch(name, ["/usr/bin/touch", mark])
    wait_for(mark, 30)
    elapsed = os.stat(mark).st_mtime - start
    finish(proc)
    return elapsed


def session(name, round_dir):
    out = os.path.join(round_dir, name)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    proc = launch(name, ["/usr/bin/python3", "-I", PROBE, DATA, out])
    sampler = Sampler(proc.pid)
    sampler.start()
    try:
        wait_for(os.path.join(out, "done"), 900)
    finally:
        sampler.stop.set()
        sampler.join()
        finish(proc)
    with open(os.path.join(out, "samples.tsv"), "w") as f:
        for s in sampler.samples:
            f.write("%.6f\t%.6f\t%d\t%d\n" % s)
    return out


def load_session(out):
    samples = [tuple(map(float, l.split("\t"))) for l in open(os.path.join(out, "samples.tsv"))]
    marks = dict(l.rstrip("\n").split("\t") for l in open(os.path.join(out, "marks.tsv")))
    marks = {k: float(v) for k, v in marks.items()}
    results = [l.rstrip("\n").split("\t") for l in open(os.path.join(out, "results.tsv"))]
    return samples, marks, results


# ---- summary ----

def pct(values, p):
    values = sorted(values)
    if not values:
        return float("nan")
    k = (len(values) - 1) * p / 100
    lo = int(k)
    hi = min(lo + 1, len(values) - 1)
    return values[lo] + (values[hi] - values[lo]) * (k - lo)


def summarize(names, startups, sessions):
    m = {n: {} for n in names}
    for n in names:
        m[n]["startup"] = statistics.median(startups[n]) * 1000
        acc = {}
        for out in sessions[n]:
            samples, marks, results = load_session(out)
            for phase in ("idle", "load", "steady", "mem140", "mem260"):
                cpu, rss, fp = window(samples, marks[phase + "_start"], marks[phase + "_end"])
                acc.setdefault(phase, ([], [], []))
                for dst, src in zip(acc[phase], (cpu, rss, fp)):
                    dst.extend(src)
            for kind, key, value in results:
                if kind in ("throughput", "latency"):
                    acc.setdefault((kind, key), []).append(float(value))
        m[n]["acc"] = acc
    return m


def table(names, m):
    lines = []

    def row(label, fmt, values, better=None):
        cells = [fmt.format(v) for v in values]
        if better and len(values) > 1:
            # Bold the winner, and anything that ties with it at this precision.
            best = fmt.format((max if better == "high" else min)(values))
            cells = [f"**{c}**" if c == best else c for c in cells]
        lines.append(f"| {label} | " + " | ".join(cells) + " |")

    lines.append("| | " + " | ".join(names) + " |")
    lines.append("|---|" + "---|" * len(names))
    acc = {n: m[n]["acc"] for n in names}
    lines.append("| **Startup** | |" + " |" * (len(names) - 1))
    row("launch → command running (median)", "{:.0f} ms", [m[n]["startup"] for n in names], "low")
    for phase, title in (("idle", "Idle (10 s)"), ("load", "Under load (throughput runs)")):
        lines.append(f"| **{title}** | |" + " |" * (len(names) - 1))
        row("CPU, avg", "{:.1f} %", [statistics.mean(acc[n][phase][0]) for n in names], "low")
        row("RSS, median", "{:.0f} MiB", [statistics.median(acc[n][phase][1]) for n in names], "low")
        row("footprint (≈PSS), median", "{:.0f} MiB", [statistics.median(acc[n][phase][2]) for n in names], "low")
    lines.append("| **Distribution under load** | |" + " |" * (len(names) - 1))
    for i, what, unit in ((0, "CPU", "%"), (1, "RSS", "MiB"), (2, "footprint", "MiB")):
        fmt = "{:.0f} " + unit
        row(f"{what} avg", fmt, [statistics.mean(acc[n]["load"][i]) for n in names], "low")
        for p in (50, 75, 90, 95):
            row(f"{what} P{p}", fmt, [pct(acc[n]["load"][i], p) for n in names], "low")
    lines.append("| **Memory after output** | |" + " |" * (len(names) - 1))
    for target in (140, 260):
        row(f"RSS after ~{target} MB", "{:.0f} MiB", [statistics.median(acc[n][f"mem{target}"][1]) for n in names], "low")
        row(f"footprint after ~{target} MB", "{:.0f} MiB", [statistics.median(acc[n][f"mem{target}"][2]) for n in names], "low")
    lines.append("| **Throughput** (median) | |" + " |" * (len(names) - 1))
    for key, label in (("ascii", "plain ASCII"), ("truecolor", "SGR-colour heavy"), ("unicode", "CJK / Unicode"),
                       ("cursor_4m", "random cursor addressing (4 MB)")):
        row(label, "{:.0f} MiB/s", [statistics.median(acc[n][("throughput", key)]) for n in names], "high")
    lines.append("| **Response latency** (DSR round trip) | |" + " |" * (len(names) - 1))
    for key, label in (("idle", "idle"), ("flood", "under flood")):
        row(f"{label}, median", "{:.3f} ms", [1000 * statistics.median(acc[n][("latency", key)]) for n in names], "low")
        row(f"{label}, P95", "{:.3f} ms", [1000 * pct(acc[n][("latency", key)], 95) for n in names], "low")
    lines.append("| **Steady 250 lines/s** | |" + " |" * (len(names) - 1))
    row("CPU, avg", "{:.1f} %", [statistics.mean(acc[n]["steady"][0]) for n in names], "low")
    return "\n".join(lines)


def prepare_data():
    if not os.path.isdir(DATA):
        subprocess.run([sys.executable, "-I", os.path.join(ROOT, "bench", "gen.py"), DATA, "16"], check=True)
    cursor = os.path.join(DATA, "cursor_4m.txt")
    if not os.path.exists(cursor):
        with open(os.path.join(DATA, "cursor_motion.txt"), "rb") as f:
            data = f.read(4 * 1048576 + 64)
        cut = data.rfind(b"\x1b", 0, 4 * 1048576 + 1)  # end on a sequence boundary
        with open(cursor, "wb") as f:
            f.write(data[:cut])


def main():
    rounds = int(sys.argv[1]) if len(sys.argv) > 1 else 3
    starts = int(sys.argv[2]) if len(sys.argv) > 2 else 10
    names = [n for n, (argv, _) in TERMINALS.items() if os.path.exists(argv[0])]
    prepare_data()
    os.makedirs(OUT, exist_ok=True)
    scratch = os.path.join(OUT, "tmp")
    os.makedirs(scratch, exist_ok=True)

    startups = {n: [] for n in names}
    for i in range(starts):
        for n in names:
            startups[n].append(startup(n, scratch))
        print(f"startup {i + 1}/{starts}: " + ", ".join(f"{n} {startups[n][-1] * 1000:.0f} ms" for n in names), flush=True)

    sessions = {n: [] for n in names}
    for r in range(rounds):
        for n in names:
            t = time.time()
            sessions[n].append(session(n, os.path.join(OUT, "raw", f"round{r + 1}")))
            print(f"round {r + 1}/{rounds}: {n} session done in {time.time() - t:.0f} s", flush=True)

    summary = table(names, summarize(names, startups, sessions))
    with open(os.path.join(OUT, "summary.md"), "w") as f:
        f.write(summary + "\n")
    print(summary)


if __name__ == "__main__":
    main()
