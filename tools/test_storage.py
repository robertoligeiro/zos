import hashlib
from pathlib import Path
import struct
import tempfile
import unittest
import uuid
from unittest.mock import patch
import zlib

from mkimage import BUILD
from mkfs import BANKS, SECTOR, SECTORS, data_path, ensure_disk, format_disk, metadata, validate_disk
from zos import qemu_command, validate_log
import test_tools


class StorageToolsTests(unittest.TestCase):
    def setUp(self):
        self.name = f"data-unittest-{uuid.uuid4().hex}.img"
        self.path = data_path(self.name)

    def tearDown(self):
        self.path.unlink(missing_ok=True)

    def test_format_geometry_root_bitmap_and_checksums(self):
        format_disk(self.name)
        self.assertEqual(self.path.stat().st_size, SECTORS * SECTOR)
        header, body = metadata()
        with self.path.open("rb") as stream:
            superblock = stream.read(SECTOR)
            self.assertEqual(struct.unpack_from("<8sIIII", superblock), (b"ZOSFS001", 1, SECTORS, 4096, 64))
            self.assertEqual(zlib.crc32(superblock[:508]), struct.unpack_from("<I", superblock, 508)[0])
            stream.seek(BANKS[0] * SECTOR)
            self.assertEqual(stream.read(SECTOR), header)
            self.assertEqual(stream.read(len(body)), body)
            stream.seek(BANKS[1] * SECTOR)
            self.assertEqual(stream.read(SECTOR), bytes(SECTOR))
        self.assertEqual(body[0], 255)
        self.assertEqual(body[512], 1)
        self.assertEqual(body[513:], bytes(len(body) - 513))

    def test_existing_data_is_unchanged_without_explicit_force(self):
        format_disk(self.name)
        with self.path.open("r+b") as stream:
            stream.seek(64 * SECTOR)
            stream.write(b"persistent bytes")
        before = hashlib.sha256(self.path.read_bytes()).digest()
        self.assertEqual(ensure_disk(self.name), self.path)
        self.assertEqual(hashlib.sha256(self.path.read_bytes()).digest(), before)
        with self.assertRaises(FileExistsError):
            format_disk(self.name)
        self.assertEqual(hashlib.sha256(self.path.read_bytes()).digest(), before)
        format_disk(self.name, force=True)
        self.assertNotEqual(hashlib.sha256(self.path.read_bytes()).digest(), before)

    def test_corrupt_existing_image_is_rejected_not_reformatted(self):
        self.path.write_bytes(b"corrupt existing volume")
        with self.assertRaises(ValueError):
            ensure_disk(self.name)
        self.assertEqual(self.path.read_bytes(), b"corrupt existing volume")
        format_disk(self.name, force=True)
        with self.path.open("r+b") as stream:
            stream.seek(BANKS[0] * SECTOR + 508)
            stream.write(bytes(4))
        with self.assertRaises(ValueError):
            validate_disk(self.path)

    def test_only_separate_generated_data_volumes_are_allowed(self):
        for name in ("zos.img", "../data.img", "C:\\data.img", "data-x,cache=unsafe.img", "data x.img"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                data_path(name)
        format_disk(self.name)
        with tempfile.TemporaryDirectory(dir=BUILD) as directory:
            tools = Path(directory)
            qemu = tools / "qemu"
            qemu.mkdir()
            (qemu / "qemu-system-x86_64.exe").touch()
            (qemu / "bios-256k.bin").touch()
            with patch("zos.TOOLS", tools):
                command = qemu_command(BUILD / "zos.img", "bios", data_disk=self.path)
                with self.assertRaises(ValueError):
                    qemu_command(self.path, "bios", data_disk=self.path)
        drives = [command[i + 1] for i, arg in enumerate(command) if arg == "-drive"]
        self.assertIn("index=0,snapshot=on", drives[0])
        self.assertIn("index=2,cache=writeback,snapshot=off", drives[1])

    def test_storage_acceptance_requires_reboot_and_capability_proofs(self):
        fixture = test_tools.AcceptanceHarnessTests()
        for case in ("fs-create", "fs-read"):
            text = fixture.valid_log("BIOS", case)
            validate_log(text, "bios", case)
            for marker in ("STORAGE CLEANUP PASS:", "VFS ready: root mount", "FILESYSTEM ACCEPTANCE PASS"):
                with self.subTest(case=case, marker=marker), self.assertRaises(RuntimeError):
                    validate_log(text.replace(marker, ""), "bios", case)
            with self.assertRaises(RuntimeError):
                validate_log(text + "\nUSER FAULT: tid=5 kind=PAGE FAULT address=0x1", "bios", case)
