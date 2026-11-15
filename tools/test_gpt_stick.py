from pathlib import Path
import re
import struct
import unittest
import uuid

from mkfs import (BASIC_DATA_TYPE, SECTOR, SECTORS, STICK_SECTORS, ZOS_FIRST, ZOS_LAST, ZOS_PARTITION_TYPE,
                  data_path, format_gpt_disk, outside_zos_sha256, validate_disk, validate_gpt_disk)
from mkimage import ROOT, crc


class GptStickTests(unittest.TestCase):
    def setUp(self):
        self.name = f"data-unittest-gpt-{uuid.uuid4().hex}.img"
        self.path = data_path(self.name)

    def tearDown(self):
        self.path.unlink(missing_ok=True)

    def entries(self):
        with self.path.open("rb") as stream:
            stream.seek(2 * SECTOR)
            data = stream.read(128 * 128)
        return [data[i * 128:(i + 1) * 128] for i in range(128) if data[i * 128:i * 128 + 16] != bytes(16)]

    def test_layout_has_a_decoy_and_the_zos_partition_at_a_nonzero_offset(self):
        format_gpt_disk(self.name)
        self.assertEqual(self.path.stat().st_size, STICK_SECTORS * SECTOR)
        kinds = [uuid.UUID(bytes_le=entry[:16]) for entry in self.entries()]
        self.assertEqual(kinds, [BASIC_DATA_TYPE, ZOS_PARTITION_TYPE])
        first, last = struct.unpack_from("<QQ", self.entries()[1], 32)
        self.assertEqual((first, last), (ZOS_FIRST, ZOS_LAST))
        self.assertEqual(last - first + 1, SECTORS)
        validate_disk(self.path)

    def test_stick_without_a_zos_partition_is_valid_but_has_none(self):
        format_gpt_disk(self.name, zos=False)
        self.assertEqual([uuid.UUID(bytes_le=e[:16]) for e in self.entries()], [BASIC_DATA_TYPE])
        validate_gpt_disk(self.path)

    def test_backup_gpt_matches_and_checksums_are_valid(self):
        format_gpt_disk(self.name)
        data = self.path.read_bytes()
        for lba, entries_lba in ((1, 2), (STICK_SECTORS - 1, STICK_SECTORS - 33)):
            header = bytearray(data[lba * SECTOR:(lba + 1) * SECTOR])
            stored = struct.unpack_from("<I", header, 16)[0]
            struct.pack_into("<I", header, 16, 0)
            self.assertEqual(crc(header[:92]), stored)
            self.assertEqual(struct.unpack_from("<Q", header, 72)[0], entries_lba)
            array = data[entries_lba * SECTOR:entries_lba * SECTOR + 128 * 128]
            self.assertEqual(crc(array), struct.unpack_from("<I", header, 88)[0])
            self.assertEqual(struct.unpack_from("<Q", header, 48)[0], STICK_SECTORS - 34)

    def test_outside_digest_ignores_the_zos_partition_only(self):
        format_gpt_disk(self.name)
        before = outside_zos_sha256(self.path)
        with self.path.open("r+b") as stream:
            for lba in (ZOS_FIRST, ZOS_FIRST + 5000, ZOS_LAST):
                stream.seek(lba * SECTOR)
                stream.write(b"inside")
        self.assertEqual(outside_zos_sha256(self.path), before)
        for lba in (0, 1, 2, 2048, ZOS_FIRST - 1, ZOS_LAST + 1, STICK_SECTORS - 1):
            with self.path.open("r+b") as stream:
                stream.seek(lba * SECTOR + 100)
                original = stream.read(1)
                stream.seek(lba * SECTOR + 100)
                stream.write(bytes([original[0] ^ 1]))
            self.assertNotEqual(outside_zos_sha256(self.path), before, f"sector {lba}")
            with self.path.open("r+b") as stream:
                stream.seek(lba * SECTOR + 100)
                stream.write(original)
            self.assertEqual(outside_zos_sha256(self.path), before)

    def test_refuses_to_overwrite_without_force_and_rejects_corruption(self):
        format_gpt_disk(self.name)
        with self.assertRaises(FileExistsError):
            format_gpt_disk(self.name)
        with self.path.open("r+b") as stream:
            stream.seek(2 * SECTOR + 40)
            original = stream.read(1)[0]
            stream.seek(2 * SECTOR + 40)
            stream.write(bytes([original ^ 0x01]))
        with self.assertRaises(ValueError):
            validate_gpt_disk(self.path)

    def test_zos_partition_guid_matches_the_kernel_side_constant(self):
        source = (ROOT / "servers" / "console" / "src" / "usb" / "gpt.rs").read_text(encoding="utf-8")
        match = re.search(r"ZOS_DATA_GUID: \[u8; 16\] = \[(.*?)\];", source, re.S)
        rust = bytes(int(value, 16) for value in re.findall(r"0x([0-9a-f]{2})", match.group(1)))
        self.assertEqual(rust, ZOS_PARTITION_TYPE.bytes_le)


if __name__ == "__main__":
    unittest.main()
