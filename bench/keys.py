"""Runs inside mightty for its keypress latency probe.

usage: MIGHTTY_KEY_LATENCY=OUT mightty -e python3 -I bench/keys.py [idle|stream|flood]

Prints every key it reads in the bottom-left cell, which the probe watches
for, and `.` there once ready. Above that line, `stream` writes 250 lines/s
(a busy log tail) and `flood` writes as fast as the terminal reads, so keys
are timed while the window is busy drawing output. Exits when the terminal
closes.
"""
import os
import sys
import threading
import time
import tty

LINE = b"the quick brown fox jumps over the lazy dog while the terminal renders every glyph quickly"


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "idle"
    if mode not in ("idle", "stream", "flood"):
        sys.exit(__doc__)
    fd_in, fd_out = sys.stdin.fileno(), sys.stdout.fileno()
    tty.setraw(fd_in)
    rows = os.get_terminal_size(fd_out).lines
    lock = threading.Lock()

    def write(data):
        with lock:
            while data:
                data = data[os.write(fd_out, data):]

    # Output scrolls in rows 1..rows-1; the last row holds the echo.
    write(b"\x1b[?25l\x1b[2J\x1b[1;%dr\x1b[%d;1H." % (rows - 1, rows))
    to_output = b"\x1b[%d;1H\n" % (rows - 1)

    def stream():
        start, i = time.perf_counter(), 0
        while True:
            if mode == "stream":
                delay = start + i / 250 - time.perf_counter()
                if delay > 0:
                    time.sleep(delay)
                write(to_output + b"%8d " % i + LINE)
                i += 1
            else:
                write(b"".join(to_output + b"%8d " % (i + n) + LINE for n in range(64)))
                i += 64

    if mode != "idle":
        threading.Thread(target=stream, daemon=True).start()
    while True:
        key = os.read(fd_in, 1)
        if not key or key == b"\x03":
            break
        write(b"\x1b[%d;1H" % rows + key)


if __name__ == "__main__":
    try:
        main()
    except OSError:
        pass  # the terminal closed
