#!/usr/bin/env python3
"""Generates the web/PWA icons (pure Python, no dependencies): web/icons/*.png. Same glyph as android/res/drawable/ic_launcher_fg.xml."""
import struct, zlib, os, math
OUT = os.path.join(os.path.dirname(__file__), "..", "web", "icons")
L = [((7, 4), (10, 17)), ((7, 17), (17, 20))]            # rectangles of the "L"
STAR = [(18, 3), (19, 5.2), (21.2, 6.2), (19, 7.2), (18, 9.4), (17, 7.2), (14.8, 6.2), (17, 5.2)]
C0, C1 = (0x2E, 0x1B, 0xD0), (0x1F, 0x09, 0x97)

def inside_poly(x, y, poly):
    c = False
    j = len(poly) - 1
    for i in range(len(poly)):
        xi, yi = poly[i]; xj, yj = poly[j]
        if (yi > y) != (yj > y) and x < (xj - xi) * (y - yi) / (yj - yi) + xi:
            c = not c
        j = i
    return c

def rrect(x, y, w, h, r):
    dx = abs(x - w / 2) - (w / 2 - r); dy = abs(y - h / 2) - (h / 2 - r)
    return math.hypot(max(dx, 0), max(dy, 0)) + min(max(dx, dy), 0) - r <= 0

def render(size, scale, rounded, filename):
    ss = 2
    s = size * scale / 17.0
    cx, cy = 14.1, 11.5
    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            acc = [0.0, 0.0, 0.0, 0.0]
            for sy in range(ss):
                for sx in range(ss):
                    x = px + (sx + .5) / ss; y = py + (sy + .5) / ss
                    if rounded and not rrect(x, y, size, size, size * 0.22):
                        continue
                    t = (x + y) / (2 * size)
                    col = tuple(C0[i] + (C1[i] - C0[i]) * t for i in range(3))
                    gx = (x - size / 2) / s + cx; gy = (y - size / 2) / s + cy
                    fg = any(a[0] <= gx <= b[0] and a[1] <= gy <= b[1] for a, b in L) or (14 <= gx <= 22 and 3 <= gy <= 9.5 and inside_poly(gx, gy, STAR))
                    if fg:
                        col = (255, 255, 255)
                    acc[0] += col[0]; acc[1] += col[1]; acc[2] += col[2]; acc[3] += 255
            n = ss * ss
            a = acc[3] / n
            if acc[3] == 0:
                row += bytes((0, 0, 0, 0))
            else:
                row += bytes((int(acc[0] / acc[3] * 255), int(acc[1] / acc[3] * 255), int(acc[2] / acc[3] * 255), int(a)))
        rows.append(b"\x00" + bytes(row))
    raw = b"".join(rows)
    def chunk(t, d):
        c = struct.pack(">I", len(d)) + t + d
        return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
    os.makedirs(OUT, exist_ok=True)
    open(os.path.join(OUT, filename), "wb").write(png)

render(512, 0.62, True, "icon-512.png")
render(192, 0.62, True, "icon-192.png")
render(180, 0.62, False, "apple-touch-icon.png")
render(512, 0.46, False, "maskable-512.png")   # full-bleed, glyph inside the 80 % safe zone
print("icons written to", os.path.normpath(OUT))
