"""Generates terminal throughput workloads (modelled on alacritty/vtebench).

usage: python3 bench/gen.py OUT_DIR [MiB]
"""
import os
import random
import sys

out = sys.argv[1]
size = int(sys.argv[2] if len(sys.argv) > 2 else 16) * 1024 * 1024
os.makedirs(out, exist_ok=True)
rng = random.Random(42)
COLS, ROWS = 120, 40
WORDS = "the quick brown fox jumps over lazy dog terminal render glyph cursor scroll".split()


def ascii_line():
    return " ".join(rng.choice(WORDS) for _ in range(rng.randint(3, 16))) + "\n"


def long_line():
    return "".join(chr(rng.randint(0x21, 0x7E)) for _ in range(1000)) + "\n"


UNICODE = "中文日本語한국어αβγδ€→✓λ≠éüñçøå" + "😀🚀🦀🎉👋"


def unicode_line():
    return "".join(rng.choice(UNICODE) for _ in range(50)) + "\n"


def truecolor_line():
    cells = []
    for _ in range(COLS):
        r, g, b = rng.randrange(256), rng.randrange(256), rng.randrange(256)
        cells.append(f"\x1b[38;2;{r};{g};{b};48;2;{b};{r};{g}m{chr(rng.randint(0x41, 0x5A))}")
    return "".join(cells) + "\x1b[0m\n"


def cursor_motion():
    return "".join(
        f"\x1b[{rng.randint(1, ROWS)};{rng.randint(1, COLS)}H{chr(rng.randint(0x41, 0x7A))}" for _ in range(64)
    )


def scroll_region():
    return f"\x1b[{ROWS - 5};1H" + ascii_line()


WORKLOADS = {
    "ascii": (ascii_line, "", ""),
    "long_lines": (long_line, "", ""),
    "unicode": (unicode_line, "", ""),
    "truecolor": (truecolor_line, "", ""),
    "cursor_motion": (cursor_motion, "\x1b[2J", "\x1b[H\x1b[2J"),
    "scroll_region": (scroll_region, f"\x1b[5;{ROWS - 5}r", "\x1b[r\x1b[H\x1b[2J"),
}

for name, (gen, prefix, suffix) in WORKLOADS.items():
    path = os.path.join(out, name + ".txt")
    with open(path, "wb") as f:
        f.write(prefix.encode())
        written = 0
        while written < size:
            chunk = gen().encode()
            f.write(chunk)
            written += len(chunk)
        f.write(suffix.encode())
    print(f"{name:14} {os.path.getsize(path) / 1048576:.1f} MiB")
