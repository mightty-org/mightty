#!/usr/bin/env python3
"""Draws mightty's app icon: a wizard on a terminal screen. The wizard's face
is a block cursor whose eyes are the prompt `>_`, under a violet hat and over
a white beard. Writes a 1024x1024 PNG; scripts/make-icon.sh turns it into
assets/mightty.icns. With --wizard, writes just the wizard on a transparent
square, for the website. Standard library only.

usage: make-icon.py [--wizard] OUT.png
"""
import math
import struct
import sys
import zlib

SIZE = 1024
SUB = 5  # sub-scanlines per pixel when filling paths


def hex_color(h):
    return tuple(int(h[i:i + 2], 16) for i in (1, 3, 5))


def gradient(stops, t):
    """Colour at t along stops [(offset, '#rrggbb')], clamped to the ends."""
    t = max(0.0, min(1.0, t))
    for (o0, c0), (o1, c1) in zip(stops, stops[1:]):
        if t <= o1:
            f = (t - o0) / (o1 - o0)
            return tuple(a + (b - a) * f for a, b in zip(hex_color(c0), hex_color(c1)))
    return hex_color(stops[-1][1])


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


def flatten(start, segments, steps=24):
    """Points along a path of ('L', p), ('Q', c, p) and ('C', c1, c2, p)."""
    points = [start]
    for kind, *ps in segments:
        (x0, y0) = points[-1]
        if kind == "L":
            points.append(ps[0])
            continue
        for i in range(1, steps + 1):
            t, u = i / steps, 1 - i / steps
            if kind == "Q":
                (cx, cy), (x, y) = ps
                points.append((u * u * x0 + 2 * u * t * cx + t * t * x, u * u * y0 + 2 * u * t * cy + t * t * y))
            else:
                (ax, ay), (bx, by), (x, y) = ps
                points.append((u ** 3 * x0 + 3 * u * u * t * ax + 3 * u * t * t * bx + t ** 3 * x,
                               u ** 3 * y0 + 3 * u * u * t * ay + 3 * u * t * t * by + t ** 3 * y))
    return points


def stroke(points, half):
    """Outline of a polyline stroked half on each side, with round caps."""
    left, right = [], []
    for i, (x, y) in enumerate(points):
        (ax, ay), (bx, by) = points[max(i - 1, 0)], points[min(i + 1, len(points) - 1)]
        dx, dy = bx - ax, by - ay
        n = math.hypot(dx, dy)
        nx, ny = -dy / n * half, dx / n * half
        left.append((x + nx, y + ny))
        right.append((x - nx, y - ny))

    def cap(x, y, nx, ny):
        return [(x + nx * math.cos(a) - ny * math.sin(a), y + ny * math.cos(a) + nx * math.sin(a))
                for a in (math.pi * k / 12 for k in range(1, 12))]
    (ex, ey), (lx, ly) = points[-1], left[-1]
    (sx, sy), (rx, ry) = points[0], right[0]
    return left + cap(ex, ey, lx - ex, ly - ey) + right[::-1] + cap(sx, sy, rx - sx, ry - sy)


def fill(points):
    """Pixel coverage of a closed polygon (even-odd): {(x, y): coverage}."""
    edges = list(zip(points, points[1:] + points[:1]))
    top = math.floor(min(y for _, y in points))
    bottom = math.ceil(max(y for _, y in points))
    cover = {}
    for py in range(top, bottom):
        row = {}
        for s in range(SUB):
            sy = py + (s + 0.5) / SUB
            xs = sorted(x0 + (sy - y0) * (x1 - x0) / (y1 - y0)
                        for (x0, y0), (x1, y1) in edges if (y0 <= sy) != (y1 <= sy))
            for a, b in zip(xs[::2], xs[1::2]):
                ia, ib = math.floor(a), math.floor(b)
                if ia == ib:
                    row[ia] = row.get(ia, 0.0) + (b - a) / SUB
                    continue
                row[ia] = row.get(ia, 0.0) + (ia + 1 - a) / SUB
                for x in range(ia + 1, ib):
                    row[x] = row.get(x, 0.0) + 1 / SUB
                row[ib] = row.get(ib, 0.0) + (b - ib) / SUB
        for x, c in row.items():
            cover[(x, py)] = min(1.0, c)
    return cover


def field(distance, x0, y0, x1, y1):
    """Pixel coverage of a signed-distance shape within a bounding box."""
    cover = {}
    for y in range(y0, y1):
        for x in range(x0, x1):
            c = coverage(distance(x + 0.5, y + 0.5))
            if c > 0.0:
                cover[(x, y)] = c
    return cover


def bounds(points):
    xs, ys = [x for x, _ in points], [y for _, y in points]
    return min(xs), min(ys), max(xs), max(ys)


class Canvas:
    """Premultiplied RGBA, composited with 'over'."""

    def __init__(self):
        self.px = [[0.0, 0.0, 0.0, 0.0] for _ in range(SIZE * SIZE)]

    def paint(self, cover, color):
        """color(x, y) -> (r, g, b, alpha) at a pixel centre."""
        for (x, y), c in cover.items():
            if 0 <= x < SIZE and 0 <= y < SIZE:
                r, g, b, a = color(x + 0.5, y + 0.5)
                a *= c
                p = self.px[y * SIZE + x]
                p[0] = r * a + p[0] * (1 - a)
                p[1] = g * a + p[1] * (1 - a)
                p[2] = b * a + p[2] * (1 - a)
                p[3] = 255 * a + p[3] * (1 - a)

    def rows(self):
        for y in range(SIZE):
            row = []
            for p in self.px[y * SIZE:(y + 1) * SIZE]:
                a = p[3] / 255
                row.append((p[0] / a, p[1] / a, p[2] / a, p[3]) if a else (0, 0, 0, 0))
            yield row


def solid(h, alpha=1.0):
    c = (*hex_color(h), alpha)
    return lambda x, y: c


def vertical(stops, y0, y1, alpha=1.0):
    return lambda x, y: (*gradient(stops, (y - y0) / (y1 - y0)), alpha)


def diagonal(stops, box):
    """A top-left to bottom-right gradient across a bounding box."""
    x0, y0, x1, y1 = box
    return lambda x, y: (*gradient(stops, ((x - x0) / (x1 - x0) + (y - y0) / (y1 - y0)) / 2), 1.0)


def draw(canvas):
    # Apple's icon grid: the shape is 824 points with a 100-point margin.
    c, half = SIZE / 2, SIZE / 2 - 100

    # A soft drop shadow under the frame.
    shadow = {}
    for y in range(80, 1000):
        for x in range(60, 964):
            d = rounded_rect(x + 0.5, y + 0.5, c, c + 12, half, half, 185)
            a = 0.35 * 0.5 * math.erfc(d / (14 * math.sqrt(2)))
            if a > 0.002 and rounded_rect(x + 0.5, y + 0.5, c, c, half, half, 185) > -1:
                shadow[(x, y)] = a
    canvas.paint(shadow, solid("#000000"))

    # The brushed-metal frame and its bright rim.
    canvas.paint(field(lambda x, y: rounded_rect(x, y, c, c, half, half, 185), 100, 100, 924, 924),
                 vertical([(0, "#F1F2F6"), (0.5, "#C9CCD5"), (1, "#9A9EAA")], 100, 924))
    canvas.paint(field(lambda x, y: abs(rounded_rect(x, y, c, c, 409, 409, 182)) - 3, 100, 100, 924, 924),
                 solid("#FFFFFF", 0.6))

    # The screen, with a dark edge.
    screen = lambda x, y: rounded_rect(x, y, c, c, 362, 362, 138)
    canvas.paint(field(screen, 150, 150, 874, 874), vertical([(0, "#1C2240"), (1, "#0A0D18")], 150, 874))
    canvas.paint(field(lambda x, y: abs(screen(x, y)) - 3, 145, 145, 879, 879), solid("#000000", 0.45))

    wizard(canvas)

    # A faint shine across the top of the glass.
    canvas.paint(field(lambda x, y: rounded_rect(x, y, c, 300, 362, 150, 138), 150, 150, 874, 450),
                 lambda x, y: (255, 255, 255, 0.10 * (1 - (y - 150) / 300)))


# The square around the wizard, (x, y, side), for --wizard.
WIZARD_BOX = (100, 171, 450)


def wizard(canvas):
    # The wizard sits in the top-left of the screen, like Terminal's prompt.
    ox, oy = -25, -50

    def move(points):
        return [(x + ox, y + oy) for x, y in points]

    # The face: a block cursor.
    canvas.paint(field(lambda x, y: rounded_rect(x, y, 350 + ox, 484 + oy, 100, 86, 22), 220, 340, 480, 550),
                 vertical([(0, "#A6F0FF"), (1, "#5CCDEB")], 398 + oy, 570 + oy))
    # The eyes: the prompt `>_`.
    eye = 7.5
    canvas.paint(field(lambda x, y: min(segment(x, y, 290 + ox, 448 + oy, 322 + ox, 472 + oy, eye),
                                        segment(x, y, 322 + ox, 472 + oy, 290 + ox, 496 + oy, eye)),
                       250, 380, 310, 460), solid("#141A30"))
    canvas.paint(field(lambda x, y: segment(x, y, 376 + ox, 494 + oy, 428 + ox, 494 + oy, eye),
                       340, 430, 415, 460), solid("#141A30"))

    # The beard, as wide as the face, with a moustache curve along its top.
    beard = move(flatten((248, 538), [("C", (248, 600), (290, 640), (350, 664)),
                                      ("C", (410, 640), (452, 600), (452, 538)),
                                      ("Q", (405, 582), (350, 564)), ("Q", (295, 582), (248, 538))]))
    _, top, _, bottom = bounds(beard)
    canvas.paint(fill(beard), vertical([(0, "#FFFFFF"), (1, "#D3D8E4")], top, bottom))

    # The hat, drawn on a 1024 grid and squashed to sit on the face.
    def hat(points):
        return [(350 + ox + 0.72 * (x - 560), 398 + oy + 0.56 * (y - 760)) for x, y in points]

    cone = hat(flatten((410, 756), [("C", (460, 690), (500, 620), (530, 540)),
                                    ("C", (552, 484), (604, 456), (676, 470)),
                                    ("C", (632, 492), (612, 528), (614, 570)),
                                    ("C", (618, 650), (650, 710), (710, 756))]))
    canvas.paint(fill(cone), diagonal([(0, "#9478FF"), (1, "#4523C9")], bounds(cone)))
    shine = hat(flatten((530, 540), [("C", (552, 484), (604, 456), (676, 470)),
                                     ("C", (632, 492), (612, 528), (614, 570)),
                                     ("C", (596, 552), (560, 540), (530, 540))]))
    canvas.paint(fill(shine), solid("#B7A3FF", 0.35))
    band = hat(stroke(flatten((440, 736), [("Q", (560, 758), (682, 736))]), 9))
    canvas.paint(fill(band), solid("#FFD36E"))
    brim = hat([(560 + 200 * math.cos(a), 760 + 34 * math.sin(a)) for a in (math.tau * k / 96 for k in range(96))])
    canvas.paint(fill(brim), solid("#3A1D9E"))
    star = hat(flatten((560, 616), [("Q", (560, 642), (586, 642)), ("Q", (560, 642), (560, 668)),
                                    ("Q", (560, 642), (534, 642)), ("Q", (560, 642), (560, 616))]))
    canvas.paint(fill(star), solid("#FFD36E"))


def write_png(path, rows, size=SIZE):
    raw = b"".join(b"\x00" + bytes(round(v) for px in row for v in px) for row in rows)
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main():
    args = sys.argv[1:]
    only_wizard = args[:1] == ["--wizard"]
    if only_wizard:
        args = args[1:]
    if len(args) != 1:
        sys.exit(__doc__)
    canvas = Canvas()
    if only_wizard:
        wizard(canvas)
        x, y, side = WIZARD_BOX
        rows = (row[x:x + side] for row in list(canvas.rows())[y:y + side])
        write_png(args[0], rows, side)
    else:
        draw(canvas)
        write_png(args[0], canvas.rows())


if __name__ == "__main__":
    main()
