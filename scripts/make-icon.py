#!/usr/bin/env python3
"""Draws mightty's app icon: a prompt chevron and cursor on a dark rounded
square. Writes a 1024x1024 PNG; scripts/make-icon.sh turns it into
assets/mightty.icns. Standard library only.

usage: make-icon.py OUT.png
"""
import math
import struct
import sys
import zlib

SIZE = 1024
# Apple's icon grid: the shape is 824 points with a 100-point margin.
MARGIN, RADIUS = 100, 185
TOP, BOTTOM = (0x2E, 0x33, 0x40), (0x13, 0x15, 0x1B)
ACCENT = (0x7A, 0xA2, 0xF7)
CURSOR = (0xE6, 0xE8, 0xEE)
EDGE = (0xFF, 0xFF, 0xFF, 0.08)


def rounded_rect(x, y, cx, cy, hw, hh, r):
    """Signed distance to a rounded rectangle centred at (cx, cy)."""
    qx, qy = abs(x - cx) - hw + r, abs(y - cy) - hh + r
    outside = math.hypot(max(qx, 0.0), max(qy, 0.0))
    return outside + min(max(qx, qy), 0.0) - r


def segment(x, y, ax, ay, bx, by, r):
    """Signed distance to a capsule from (ax, ay) to (bx, by), radius r."""
    px, py, dx, dy = x - ax, y - ay, bx - ax, by - ay
    t = max(0.0, min(1.0, (px * dx + py * dy) / (dx * dx + dy * dy)))
    return math.hypot(px - dx * t, py - dy * t) - r


def coverage(distance):
    return max(0.0, min(1.0, 0.5 - distance))


def over(base, color, alpha):
    return tuple(b + (c - b) * alpha for b, c in zip(base, color))


def pixel(x, y):
    half = SIZE / 2 - MARGIN
    shape = rounded_rect(x, y, SIZE / 2, SIZE / 2, half, half, RADIUS)
    alpha = coverage(shape)
    if alpha == 0.0:
        return (0, 0, 0, 0)
    t = (y - MARGIN) / (SIZE - 2 * MARGIN)
    color = over(TOP, BOTTOM, max(0.0, min(1.0, t)))
    # A faint rim, as on Apple's dark icons.
    color = over(color, EDGE[:3], EDGE[3] * coverage(abs(shape + 3) - 3))
    # The chevron: two strokes meeting at the right.
    stroke = 34
    tip = (470, 512)
    chevron = min(segment(x, y, 300, 362, *tip, stroke), segment(x, y, 300, 662, *tip, stroke))
    color = over(color, ACCENT, coverage(chevron))
    # The cursor: a bar to the right of the chevron, on the baseline.
    bar = rounded_rect(x, y, 640, 640, 105, 26, 14)
    color = over(color, CURSOR, coverage(bar))
    return (*color, alpha * 255)


def write_png(path, rows):
    raw = b"".join(b"\x00" + bytes(round(v) for px in row for v in px) for row in rows)
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    header = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    write_png(sys.argv[1], [[pixel(x + 0.5, y + 0.5) for x in range(SIZE)] for y in range(SIZE)])


if __name__ == "__main__":
    main()
