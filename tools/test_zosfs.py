from pathlib import Path
import struct
import tempfile
import unittest
import zlib

from mkfs import ZOS_FIRST, volume_image
from zosfs import BANKS, BLOCK, SECTOR, Volume, VolumeError, load


def with_files(files: dict[str, bytes]) -> bytes:
    """A volume image holding /dir and the given files directly under it, built with
    the same layout fs.rs writes (bitmap, 64-byte inodes, copy-on-write banks)."""
    image = bytearray(volume_image())
    body = bytearray(9 * SECTOR)
    bitmap = bytearray(512)
    bitmap[0] = 255
    nodes = [(1, 0, 0, [], "")]
    nodes.append((1, 0, 0, [], "dir"))
    next_block = 8
    payloads = {}
    for name, content in files.items():
        blocks = []
        for start in range(0, len(content), BLOCK):
            blocks.append(next_block)
            payloads[next_block] = content[start:start + BLOCK]
            bitmap[next_block // 8] |= 1 << (next_block % 8)
            next_block += 1
        nodes.append((2, 1, len(content), blocks, name))
    body[:512] = bitmap
    for index, (kind, parent, size, blocks, name) in enumerate(nodes):
        record = bytearray(64)
        record[0], record[1] = kind, len(name)
        struct.pack_into("<HI", record, 2, parent, size)
        struct.pack_into("<16H", record, 8, *(blocks + [0] * (16 - len(blocks))))
        record[40:40 + len(name)] = name.encode()
        body[512 + index * 64:512 + (index + 1) * 64] = record
    header = bytearray(512)
    struct.pack_into("<8sQI", header, 0, b"ZOSBANK1", 5, zlib.crc32(body))
    struct.pack_into("<I", header, 508, zlib.crc32(header[:508]))
    # A newer valid bank (generation 5) wins over the formatter's generation-1 bank.
    image[BANKS[1] * SECTOR:(BANKS[1] + 1) * SECTOR] = header
    image[(BANKS[1] + 1) * SECTOR:(BANKS[1] + 10) * SECTOR] = body
    for block, data in payloads.items():
        image[block * BLOCK:block * BLOCK + len(data)] = data
    return bytes(image)


class ZosFsTests(unittest.TestCase):
    def test_empty_formatted_volume_lists_nothing(self):
        volume = Volume(volume_image())
        self.assertEqual(volume.generation, 1)
        self.assertEqual(volume.listing("/"), [])

    def test_reads_files_across_blocks_and_picks_the_newest_checkpoint(self):
        log = bytes(range(256)) * 40  # 10240 bytes: three blocks
        volume = Volume(with_files({"boot.log": log, "note": b"hi\n"}))
        self.assertEqual(volume.generation, 5)
        self.assertEqual(volume.read(volume.lookup("/dir/boot.log")), log)
        self.assertEqual(volume.read(volume.lookup("/dir/note")), b"hi\n")
        self.assertEqual(volume.listing("/"), ["   <dir>  dir/"])
        self.assertEqual(volume.listing("/dir"), [f"{len(log):>8}  boot.log", "       3  note"])
        with self.assertRaises(VolumeError):
            volume.lookup("/dir/missing")

    def test_rejects_foreign_or_damaged_images(self):
        good = bytearray(volume_image())
        with self.assertRaises(VolumeError):
            Volume(bytes(len(good)))
        damaged = bytearray(good)
        damaged[BANKS[0] * SECTOR + 20] ^= 1  # bank header checksum
        with self.assertRaises(VolumeError):
            Volume(bytes(damaged))
        with self.assertRaises(VolumeError):
            Volume(bytes(good[:SECTOR]))

    def test_loads_a_volume_at_an_offset_inside_a_disk_image(self):
        with tempfile.TemporaryDirectory() as directory:
            disk = Path(directory) / "disk.img"
            disk.write_bytes(bytes(ZOS_FIRST * SECTOR) + with_files({"boot.log": b"abc"}) + bytes(4096))
            volume = load(disk, ZOS_FIRST)
            self.assertEqual(volume.read(volume.lookup("/dir/boot.log")), b"abc")
            with self.assertRaises(VolumeError):
                load(disk, 0)


if __name__ == "__main__":
    unittest.main()
