#!/usr/bin/env python3
"""Generate Runway's app icons.

Committed as a script rather than as opaque binaries so the icons can be
reviewed, tweaked and regenerated without a design tool. Pure stdlib — no
Pillow, no build-time dependency.

    python3 app/src-tauri/icons/generate.py

The mark is the limit track from the UI: a dark rounded square with three
horizontal bars, filled to different extents and tinted with the same severity
palette the app uses.
"""

import struct
import zlib
from pathlib import Path

HERE = Path(__file__).parent

BG = (23, 26, 33, 255)
CALM = (61, 173, 115, 255)
WATCH = (224, 161, 46, 255)
TRACK = (255, 255, 255, 38)

# (fill fraction, colour) for the three bars, top to bottom.
BARS = [(0.78, CALM), (0.46, WATCH), (0.28, CALM)]


def rounded_rect_alpha(x, y, w, h, radius, px, py):
    """Coverage of a rounded rect at a point, with a little antialiasing."""
    cx = min(max(px, x + radius), x + w - radius)
    cy = min(max(py, y + radius), y + h - radius)
    dx, dy = px - cx, py - cy
    d = (dx * dx + dy * dy) ** 0.5
    if px < x or px > x + w or py < y or py > y + h:
        return 0.0
    return max(0.0, min(1.0, radius - d + 0.5)) if d > 0 else 1.0


def blend(dst, src, alpha):
    return tuple(round(d + (s - d) * alpha) for d, s in zip(dst, src))


def render(size):
    px = [[(0, 0, 0, 0)] * size for _ in range(size)]
    s = size / 32.0  # design grid is 32x32

    for y in range(size):
        for x in range(size):
            a = rounded_rect_alpha(0, 0, size - 1, size - 1, 7 * s, x, y)
            if a > 0:
                px[y][x] = blend((0, 0, 0, 0), BG, a)

    bar_h = max(1, round(3 * s))
    gap = max(1, round(3 * s))
    left = round(6 * s)
    width = size - 2 * left
    top = round(8 * s)

    for index, (fill, colour) in enumerate(BARS):
        y0 = top + index * (bar_h + gap)
        filled = round(width * fill)
        for y in range(y0, min(size, y0 + bar_h)):
            for x in range(left, min(size, left + width)):
                # Round the bar ends so it matches the capsules in the UI.
                r = bar_h / 2.0
                edge = min(x - left, left + width - 1 - x)
                a = 1.0 if edge >= r else max(0.0, min(1.0, edge / r + 0.35))
                colour_here = colour if x - left < filled else TRACK
                base = px[y][x]
                px[y][x] = blend(base, colour_here[:3] + (255,), a * colour_here[3] / 255.0)

    return px


def write_png(path, px):
    size = len(px)
    raw = b"".join(
        b"\x00" + b"".join(struct.pack("4B", *px[y][x]) for x in range(size))
        for y in range(size)
    )

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    path.write_bytes(png)
    return png


def write_ico(path, pngs):
    """ICO with PNG-compressed entries — supported since Windows Vista."""
    count = len(pngs)
    header = struct.pack("<HHH", 0, 1, count)
    offset = 6 + 16 * count
    entries, blobs = b"", b""
    for size, data in pngs:
        entries += struct.pack(
            "<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset
        )
        offset += len(data)
        blobs += data
    path.write_bytes(header + entries + blobs)


def main():
    pngs = {}
    for size in (16, 32, 48, 128, 256, 512):
        pngs[size] = write_png(HERE / f"{size}x{size}.png", render(size))

    # Names Tauri's bundler looks for.
    write_png(HERE / "icon.png", render(512))
    write_png(HERE / "128x128@2x.png", render(256))
    write_ico(HERE / "icon.ico", [(s, pngs[s]) for s in (16, 32, 48, 256)])
    print(f"wrote icons to {HERE}")


if __name__ == "__main__":
    main()
