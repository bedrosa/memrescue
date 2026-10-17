#!/usr/bin/env python3
"""Genera el icono original de MemRescue (app/src/MemRescue/Assets/MemRescue.ico).

Diseño propio: cuadrado redondeado azul petróleo, plato de disco claro y una cruz de rescate
naranja en el centro. Solo biblioteca estándar; salida determinista.
Uso: python -I tools/make_icon.py <salida.ico>
"""
from __future__ import annotations

import struct
import sys
import zlib
from pathlib import Path

BG = (15, 92, 110)        # azul petróleo
PLATTER = (226, 236, 240)  # plato
RIM = (150, 178, 188)      # borde del plato
CROSS = (242, 135, 33)     # naranja de rescate


def blend(dst, src, a):
    return tuple(round(d * (1 - a) + s * a) for d, s in zip(dst, src))


def render(size: int) -> bytes:
    ss = 4  # supermuestreo
    n = size * ss
    c = (n - 1) / 2
    r_corner = n * 0.18
    r_plat, r_rim = n * 0.40, n * 0.43
    arm_w, arm_l = n * 0.075, n * 0.20
    acc = [[0.0, 0.0, 0.0, 0.0] for _ in range(size * size)]
    for y in range(n):
        for x in range(n):
            # Cuadrado redondeado.
            dx = max(abs(x - c) - (c - r_corner), 0)
            dy = max(abs(y - c) - (c - r_corner), 0)
            if dx * dx + dy * dy > r_corner * r_corner:
                continue
            col = BG
            d2 = (x - c) ** 2 + (y - c) ** 2
            if d2 <= r_rim * r_rim:
                col = RIM
            if d2 <= r_plat * r_plat:
                col = PLATTER
            if (abs(x - c) <= arm_w and abs(y - c) <= arm_l) or (abs(y - c) <= arm_w and abs(x - c) <= arm_l):
                col = CROSS
            px = acc[(y // ss) * size + x // ss]
            px[0] += col[0]
            px[1] += col[1]
            px[2] += col[2]
            px[3] += 255
    k = ss * ss
    raw = bytearray()
    for y in range(size):
        raw.append(0)
        for x in range(size):
            r, g, b, a = acc[y * size + x]
            cov = a / 255
            if cov:
                raw += bytes((round(r / cov), round(g / cov), round(b / cov), round(a / k)))
            else:
                raw += b"\x00\x00\x00\x00"
    def chunk(t: bytes, d: bytes) -> bytes:
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b"")


def main(out: str) -> int:
    sizes = [16, 24, 32, 48, 64, 256]
    images = [render(s) for s in sizes]
    header = struct.pack("<HHH", 0, 1, len(sizes))
    offset = 6 + 16 * len(sizes)
    entries = b""
    for s, png in zip(sizes, images):
        entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(png), offset)
        offset += len(png)
    path = Path(out)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(header + entries + b"".join(images))
    print(f"{path} ({path.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
