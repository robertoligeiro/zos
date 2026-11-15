"""Build the boot-splash bitmap (assets/zos-splash.bin) from the logo PNG.

    python tools\\mksplash.py                         # assets\\zos-logo.png -> assets\\zos-splash.bin
    python tools\\mksplash.py logo.png splash.bin 768

Uses only the standard library. The result is a square, 2-bits-per-pixel image whose
four shades interpolate between the logo's background and foreground colors:

    offset  size  field
    0       4     magic "ZSPL"
    4       2     width  (little endian)
    6       2     height
    8       16    palette: 4 x u32 little endian, 0x00RRGGBB (shade 0 = background)
    24      ...   pixels, row by row, 4 per byte, first pixel in the high bits
"""

from __future__ import annotations

import struct
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MAGIC = b"ZSPL"
HEADER = 24
SIZE = 768


def paeth(a: int, b: int, c: int) -> int:
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    return a if pa <= pb and pa <= pc else b if pb <= pc else c


def decode_png(data: bytes) -> tuple[int, int, list[bytearray]]:
    """Returns (width, height, rows of RGB bytes). Non-interlaced 8-bit PNGs only."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG file")
    position, idat, header = 8, bytearray(), None
    while position < len(data):
        length, kind = struct.unpack(">I4s", data[position:position + 8])
        body = data[position + 8:position + 8 + length]
        if zlib.crc32(kind + body) & 0xFFFFFFFF != struct.unpack(">I", data[position + 8 + length:position + 12 + length])[0]:
            raise ValueError(f"corrupt PNG chunk {kind!r}")
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
        position += 12 + length
    if header is None:
        raise ValueError("PNG has no header")
    width, height, depth, color, _, _, interlace = header
    channels = {0: 1, 2: 3, 4: 2, 6: 4}.get(color)
    if depth != 8 or channels is None or interlace != 0:
        raise ValueError("only non-interlaced 8-bit gray/RGB/RGBA PNGs are supported")
    raw = zlib.decompress(bytes(idat))
    stride = width * channels
    if len(raw) != height * (stride + 1):
        raise ValueError("PNG pixel data has the wrong size")
    rows, previous = [], bytearray(stride)
    for y in range(height):
        kind = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        if kind == 1:
            for i in range(channels, stride):
                line[i] = (line[i] + line[i - channels]) & 255
        elif kind == 2:
            for i in range(stride):
                line[i] = (line[i] + previous[i]) & 255
        elif kind == 3:
            for i in range(stride):
                left = line[i - channels] if i >= channels else 0
                line[i] = (line[i] + ((left + previous[i]) >> 1)) & 255
        elif kind == 4:
            for i in range(stride):
                left = line[i - channels] if i >= channels else 0
                upper_left = previous[i - channels] if i >= channels else 0
                line[i] = (line[i] + paeth(left, previous[i], upper_left)) & 255
        elif kind != 0:
            raise ValueError(f"unknown PNG filter {kind}")
        previous = line
        if channels == 3:
            rows.append(line)
        elif channels == 4:
            rows.append(bytearray(b for i in range(0, stride, 4) for b in line[i:i + 3]))
        else:  # gray or gray+alpha
            step = channels
            rows.append(bytearray(b for i in range(0, stride, step) for b in (line[i],) * 3))
    return width, height, rows


def scale(width: int, height: int, rows: list[bytearray], size: int) -> list[list[tuple[float, float, float]]]:
    """Box-filter downscale to size x size, returning per-pixel mean RGB."""
    out = []
    for y in range(size):
        y0, y1 = y * height // size, max(y * height // size + 1, (y + 1) * height // size)
        line = []
        for x in range(size):
            x0, x1 = x * width // size, max(x * width // size + 1, (x + 1) * width // size)
            r = g = b = 0
            count = (y1 - y0) * (x1 - x0)
            for yy in range(y0, y1):
                row = rows[yy]
                for xx in range(x0, x1):
                    r += row[xx * 3]
                    g += row[xx * 3 + 1]
                    b += row[xx * 3 + 2]
            line.append((r / count, g / count, b / count))
        out.append(line)
    return out


def luminance(pixel: tuple[float, float, float]) -> float:
    return 0.299 * pixel[0] + 0.587 * pixel[1] + 0.114 * pixel[2]


def build(png: bytes, size: int = SIZE) -> bytes:
    width, height, rows = decode_png(png)
    pixels = scale(width, height, rows, size)
    flat = [p for line in pixels for p in line]
    # Background = the most common luminance bucket; foreground = the brightest 0.5%.
    buckets: dict[int, list[tuple[float, float, float]]] = {}
    for pixel in flat:
        buckets.setdefault(int(luminance(pixel) // 8), []).append(pixel)
    background_pixels = max(buckets.values(), key=len)
    ordered = sorted(flat, key=luminance)
    foreground_pixels = ordered[-max(1, len(ordered) // 200):]
    mean = lambda items: tuple(sum(p[i] for p in items) / len(items) for i in range(3))
    bg, fg = mean(background_pixels), mean(foreground_pixels)
    span = luminance(fg) - luminance(bg)
    if span < 32:
        raise ValueError("the logo has too little contrast for a splash")
    palette = [tuple(round(bg[i] + (fg[i] - bg[i]) * level / 3) for i in range(3)) for level in range(4)]
    body = bytearray()
    for line in pixels:
        levels = [min(3, max(0, round((luminance(p) - luminance(bg)) / span * 3))) for p in line]
        levels += [0] * (-len(levels) % 4)
        for i in range(0, len(levels), 4):
            body.append(levels[i] << 6 | levels[i + 1] << 4 | levels[i + 2] << 2 | levels[i + 3])
    header = MAGIC + struct.pack("<HH", size, size)
    for r, g, b in palette:
        header += struct.pack("<I", r << 16 | g << 8 | b)
    return bytes(header + body)


def main() -> None:
    source = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "assets" / "zos-logo.png"
    destination = Path(sys.argv[2]) if len(sys.argv) > 2 else ROOT / "assets" / "zos-splash.bin"
    size = int(sys.argv[3]) if len(sys.argv) > 3 else SIZE
    data = build(source.read_bytes(), size)
    destination.write_bytes(data)
    palette = struct.unpack_from("<4I", data, 8)
    print(f"{destination}: {size}x{size}, {len(data):,} bytes, palette " + " ".join(f"{c:06x}" for c in palette))


if __name__ == "__main__":
    main()
