import re
import struct
import unittest
import zlib

import mksplash
from mkimage import ROOT


def png(width: int, height: int, rows: list[bytes], channels: int = 3, filters: list[int] | None = None) -> bytes:
    """Encode raw rows as a PNG, applying the given per-row filter types."""
    filters = filters or [0] * height
    out, previous = bytearray(), bytes(width * channels)
    for row, kind in zip(rows, filters):
        out.append(kind)
        for i, value in enumerate(row):
            left = row[i - channels] if i >= channels else 0
            up = previous[i]
            upper_left = previous[i - channels] if i >= channels else 0
            predictor = [0, left, up, (left + up) // 2, mksplash.paeth(left, up, upper_left)][kind]
            out.append((value - predictor) & 255)
        previous = row

    def chunk(kind: bytes, body: bytes) -> bytes:
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)

    color = {1: 0, 3: 2, 4: 6}[channels]
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, color, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(out))) + chunk(b"IEND", b""))


BLUE, WHITE = (1, 29, 148), (254, 255, 254)


class SplashTests(unittest.TestCase):
    def test_decoder_handles_every_png_filter(self):
        rows = [bytes((x * 7 + y * 13 + c * 5) & 255 for x in range(6) for c in range(3)) for y in range(5)]
        for filters in ([0, 1, 2, 3, 4], [4, 4, 4, 4, 4], [3, 2, 1, 0, 4]):
            width, height, decoded = mksplash.decode_png(png(6, 5, rows, filters=filters))
            self.assertEqual((width, height), (6, 5))
            self.assertEqual([bytes(r) for r in decoded], rows, filters)

    def test_decoder_handles_gray_and_rgba_and_rejects_unsupported_files(self):
        gray = [bytes([10, 20, 30]), bytes([40, 50, 60])]
        _, _, decoded = mksplash.decode_png(png(3, 2, gray, channels=1))
        self.assertEqual(bytes(decoded[0]), bytes([10, 10, 10, 20, 20, 20, 30, 30, 30]))
        rgba = [bytes([1, 2, 3, 255, 4, 5, 6, 0])]
        _, _, decoded = mksplash.decode_png(png(2, 1, rgba, channels=4))
        self.assertEqual(bytes(decoded[0]), bytes([1, 2, 3, 4, 5, 6]))
        for bad in (b"", b"not a png", png(1, 1, [b"\x00\x00\x00"])[:-5]):
            with self.assertRaises((ValueError, struct.error, zlib.error)):
                mksplash.decode_png(bad)
        corrupt = bytearray(png(2, 2, [bytes(6), bytes(6)]))
        corrupt[20] ^= 0xFF  # inside IHDR: checksum no longer matches
        with self.assertRaises(ValueError):
            mksplash.decode_png(bytes(corrupt))

    def test_build_makes_a_four_shade_bitmap_between_background_and_foreground(self):
        size = 16
        rows = []
        for y in range(size):
            row = bytearray()
            for x in range(size):
                row += bytes(WHITE if 4 <= x < 12 and 4 <= y < 12 else BLUE)
            rows.append(bytes(row))
        data = mksplash.build(png(size, size, rows), size=8)
        self.assertEqual(data[:4], b"ZSPL")
        self.assertEqual(struct.unpack_from("<HH", data, 4), (8, 8))
        palette = struct.unpack_from("<4I", data, 8)
        self.assertEqual(palette[0], 1 << 16 | 29 << 8 | 148)
        self.assertEqual(palette[3], 254 << 16 | 255 << 8 | 254)
        self.assertEqual(len(data), mksplash.HEADER + 8 * 2)
        body = data[mksplash.HEADER:]

        def shade(x, y):
            return body[y * 2 + x // 4] >> (6 - 2 * (x % 4)) & 3
        # The 16x16 source halves to 8x8: its white square (4..12) becomes pixels 2..5.
        for x, y, expected in ((0, 0, 0), (1, 1, 0), (2, 2, 3), (5, 5, 3), (3, 4, 3), (6, 6, 0), (7, 7, 0)):
            self.assertEqual(shade(x, y), expected, (x, y))
        # Colors that interpolate evenly between the two ends.
        for level in range(4):
            expected = [round(c0 + (c1 - c0) * level / 3) for c0, c1 in zip(BLUE, WHITE)]
            got = [palette[level] >> 16 & 255, palette[level] >> 8 & 255, palette[level] & 255]
            self.assertEqual(got, expected)

    def test_build_rejects_low_contrast_images_and_pads_odd_widths(self):
        flat = [bytes(BLUE) * 8 for _ in range(8)]
        with self.assertRaises(ValueError):
            mksplash.build(png(8, 8, flat), size=8)
        rows = [bytes((WHITE if x == y else BLUE)[c] for x in range(10) for c in range(3)) for y in range(10)]
        data = mksplash.build(png(10, 10, rows), size=5)
        self.assertEqual(len(data), mksplash.HEADER + 5 * 2)

    def test_committed_splash_matches_the_logo_and_the_kernel_boot_color(self):
        logo = ROOT / "assets" / "zos-logo.png"
        committed = (ROOT / "assets" / "zos-splash.bin").read_bytes()
        # Regenerating from the committed logo gives exactly the committed bitmap.
        self.assertEqual(mksplash.build(logo.read_bytes()), committed)
        palette = struct.unpack_from("<4I", committed, 8)
        kernel = (ROOT / "kernel" / "src" / "framebuffer.rs").read_text(encoding="utf-8")
        match = re.search(r"BOOT_BACKGROUND: \(u32, u32, u32\) = \((0x[0-9a-f]+), (0x[0-9a-f]+), (0x[0-9a-f]+)\)", kernel)
        self.assertIsNotNone(match, "kernel boot background constant not found")
        red, green, blue = (int(group, 16) for group in match.groups())
        self.assertEqual(palette[0], red << 16 | green << 8 | blue)


if __name__ == "__main__":
    unittest.main()
