"""Runs inside the terminal under test, driven by bench/suite.py.

usage: python3 bench/probe.py DATA_DIR OUT_DIR

Works through the suite's phases and records, in OUT_DIR:
  marks.tsv    phase boundaries (`name<TAB>unix time`), so the suite can cut
               its CPU/memory samples of the terminal process into phases
  results.tsv  measurements (`kind<TAB>name<TAB>value`)
  done         written last
"""
import os
import signal
import subprocess
import sys
import termios
import time
import tty

ASCII_LINE = b"the quick brown fox jumps over the lazy dog while the terminal renders every glyph quickly\r\n"


def mark(out, name):
    with open(os.path.join(out, "marks.tsv"), "a") as f:
        f.write(f"{name}\t{time.time():.6f}\n")


def record(out, kind, name, value):
    with open(os.path.join(out, "results.tsv"), "a") as f:
        f.write(f"{kind}\t{name}\t{value}\n")


def write_all(fd, data):
    while data:
        data = data[os.write(fd, data):]


def query_latency(n, flood=None):
    """Round trips of a cursor position report (CSI 6n -> CSI row;col R), in
    seconds. The query is answered only after everything written before it
    has been processed, so under flood this includes draining the backlog.

    The flood is a separate `cat` loop writing as fast as the terminal
    reads, sharing nothing with the queries: a lock between the two would
    starve whichever side is slower, and a query landing inside plain text
    is harmless."""
    fd_in, fd_out = sys.stdin.fileno(), sys.stdout.fileno()
    saved = termios.tcgetattr(fd_in)
    tty.setraw(fd_in)
    flooder = None
    if flood:
        flooder = subprocess.Popen(["/bin/sh", "-c", 'while :; do cat "$0"; done', flood],
                                   stdout=fd_out, start_new_session=True)
        time.sleep(0.2)  # let the backlog build
    times = []
    try:
        for _ in range(n):
            start = time.perf_counter()
            write_all(fd_out, b"\x1b[6n")
            reply = b""
            while not reply.endswith(b"R"):
                reply += os.read(fd_in, 64)
            times.append(time.perf_counter() - start)
            time.sleep(0.005 if not flood else 0.02)
    finally:
        if flooder:
            os.killpg(flooder.pid, signal.SIGTERM)
            flooder.wait()
        termios.tcsetattr(fd_in, termios.TCSAFLUSH, saved)
    return times


def cat(path):
    """Writes a file to the terminal; returns seconds until the terminal took it all."""
    write_all(sys.stdout.fileno(), b"\x1b[0m\x1b[r\x1b[H\x1b[2J")
    start = time.perf_counter()
    pid = os.fork()
    if pid == 0:
        os.execvp("cat", ["cat", path])
    os.waitpid(pid, 0)
    return time.perf_counter() - start


def session(data, out):
    os.makedirs(out, exist_ok=True)
    fd = sys.stdout.fileno()
    rows, cols = os.get_terminal_size()
    record(out, "grid", "size", f"{cols}x{rows}")
    time.sleep(3)  # settle after startup

    mark(out, "idle_start")
    time.sleep(10)
    mark(out, "idle_end")

    for t in query_latency(200):
        record(out, "latency", "idle", f"{t:.6f}")

    # Memory after a known amount of output, counted from launch.
    ascii_path = os.path.join(data, "ascii.txt")
    size = os.path.getsize(ascii_path)
    written = 0
    for target in (140, 260):
        while written < target * 1048576 - size // 2:
            cat(ascii_path)
            written += size
        record(out, "output", f"mem{target}", written)
        mark(out, f"mem{target}_start")
        time.sleep(2)
        mark(out, f"mem{target}_end")

    mark(out, "load_start")
    for name in ("ascii", "truecolor", "unicode", "cursor_4m"):
        path = os.path.join(data, name + ".txt")
        for _ in range(3):
            secs = cat(path)
            record(out, "throughput", name, f"{os.path.getsize(path) / 1048576 / secs:.2f}")
    mark(out, "load_end")

    for t in query_latency(50, flood=ascii_path):
        record(out, "latency", "flood", f"{t:.6f}")
    write_all(fd, b"\x1b[0m\x1b[H\x1b[2J")
    time.sleep(1)

    # Steady output: 250 lines a second, like a busy log tail.
    mark(out, "steady_start")
    start = time.perf_counter()
    for i in range(2500):
        delay = start + i / 250 - time.perf_counter()
        if delay > 0:
            time.sleep(delay)
        write_all(fd, b"%6d " % i + ASCII_LINE)
    mark(out, "steady_end")

    with open(os.path.join(out, "done"), "w"):
        pass


if __name__ == "__main__":
    session(sys.argv[1], sys.argv[2])
