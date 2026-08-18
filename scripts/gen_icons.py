#!/usr/bin/env python3
"""Generate original LDM app icons (PNG only, stdlib).

Draws a simple, original mark: a rounded square with a download arrow into
a tray. No external dependencies (zlib + struct are stdlib).
"""
import struct
import zlib
import os


def png_chunk(tag, data):
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


def write_png(path, size, pixels):
    """pixels: list of rows, each row a list of (r,g,b,a)."""
    raw = b""
    for row in pixels:
        raw += b"\x00"  # filter type 0
        for r, g, b, a in row:
            raw += bytes((r, g, b, a))
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(png_chunk(b"IHDR", ihdr))
        f.write(png_chunk(b"IDAT", zlib.compress(raw, 9)))
        f.write(png_chunk(b"IEND", b""))


def rounded_rect_mask(size, radius):
    """Mask of rounded-rect interior (True inside)."""
    mask = [[False] * size for _ in range(size)]
    for y in range(size):
        for x in range(size):
            # distance from nearest corner center
            cx = min(x, size - 1 - x)
            cy = min(y, size - 1 - y)
            if cx < radius and cy < radius:
                dx = radius - cx
                dy = radius - cy
                inside = dx * dx + dy * dy <= radius * radius
            else:
                inside = True
            mask[y][x] = inside
    return mask


def lerp(a, b, t):
    return int(a + (b - a) * t)


def make_icon(size):
    """Background: vertical gradient indigo->blue. Foreground: white download
    arrow into a tray, centered."""
    mask = rounded_rect_mask(size, max(2, size // 5))
    px = [[(0, 0, 0, 0)] * size for _ in range(size)]
    # gradient colors
    c_top = (79, 70, 229)   # indigo-500
    c_bot = (37, 99, 235)   # blue-600

    cx = size // 2
    # geometry in fractions of size
    shaft_w = max(2, size // 10)         # arrow shaft width
    head_w = max(3, size // 4)           # arrow head width
    head_h = max(3, size // 5)           # arrow head height
    stem_top = int(size * 0.18)
    stem_bot = int(size * 0.52)
    tray_y = int(size * 0.72)
    tray_h = max(2, size // 14)
    tray_w = int(size * 0.66)

    for y in range(size):
        t = y / (size - 1)
        bg = (lerp(c_top[0], c_bot[0], t), lerp(c_top[1], c_bot[1], t), lerp(c_top[2], c_bot[2], t))
        for x in range(size):
            if not mask[y][x]:
                continue
            # default background
            r, g, b = bg
            a = 255
            # arrow stem (vertical bar)
            if stem_top <= y <= stem_bot and abs(x - cx) <= shaft_w // 2:
                r = g = b = 255
            # arrow head (triangle)
            local = y - stem_bot
            if 0 <= local < head_h:
                half = int(head_w * local / head_h) // 2
                if abs(x - cx) <= half:
                    r = g = b = 255
            # tray (rounded bar)
            if tray_y <= y <= tray_y + tray_h and abs(x - cx) <= tray_w // 2:
                r = g = b = 255
            px[y][x] = (r, g, b, a)
    return px


def main():
    out = os.path.join(os.path.dirname(__file__), "..", "app", "src-tauri", "icons")
    os.makedirs(out, exist_ok=True)
    for size, name in [
        (32, "32x32.png"),
        (128, "128x128.png"),
        (256, "128x128@2x.png"),
        (512, "icon.png"),
    ]:
        write_png(os.path.join(out, name), size, make_icon(size))
        print(f"wrote {name} ({size}x{size})")
    # Also a square icon for the repo / README / desktop entry.
    repo_icon = os.path.join(os.path.dirname(__file__), "..", "assets", "icon-512.png")
    os.makedirs(os.path.dirname(repo_icon), exist_ok=True)
    write_png(repo_icon, 512, make_icon(512))
    print(f"wrote {repo_icon}")


if __name__ == "__main__":
    main()
