import struct
import unittest

from zos import compact_elf, validate_shell, SHELL_PROOFS
import test_tools


class SessionToolsTests(unittest.TestCase):
    def test_compact_elf_preserves_load_bytes_and_removes_only_nonloaded_tail(self):
        data = bytearray(5000)
        data[:7] = b"\x7fELF\x02\x01\x01"
        struct.pack_into("<Q", data, 32, 64)
        struct.pack_into("<Q", data, 40, 4500)
        struct.pack_into("<H", data, 56, 1)
        struct.pack_into("<HHH", data, 58, 64, 2, 1)
        struct.pack_into("<I", data, 64, 1)
        struct.pack_into("<Q", data, 72, 4096)
        struct.pack_into("<Q", data, 96, 32)
        data[4096:4128] = bytes(range(32))
        result = compact_elf(data)
        self.assertEqual(len(result), 4128)
        self.assertEqual(result[4096:4128], bytes(range(32)))
        self.assertEqual(result[64:120], data[64:120])
        self.assertEqual(result[40:48], bytes(8))
        self.assertEqual(result[58:64], bytes(6))

    def test_compact_elf_rejects_truncation_and_oversized_load_segments(self):
        for data in (b"", bytes(63), bytes(128)):
            with self.assertRaises(ValueError):
                compact_elf(data)
        data = bytearray(70000)
        data[:7] = b"\x7fELF\x02\x01\x01"
        struct.pack_into("<Q", data, 32, 64)
        struct.pack_into("<H", data, 56, 1)
        struct.pack_into("<I", data, 64, 1)
        struct.pack_into("<Q", data, 72, 4096)
        struct.pack_into("<Q", data, 96, 64000)
        with self.assertRaises(ValueError):
            compact_elf(data)
        struct.pack_into("<Q", data, 72, 100000)
        with self.assertRaises(ValueError):
            compact_elf(data)

    def test_shell_requires_all_proofs_and_only_the_expected_command_fault(self):
        valid = test_tools.AcceptanceHarnessTests().valid_log("BIOS", "shell")
        validate_shell(valid)
        for marker in SHELL_PROOFS:
            with self.subTest(marker=marker), self.assertRaises(RuntimeError):
                validate_shell(valid.replace(marker, ""))
        for broken in (
            valid.replace("tid=9 kind=INVALID OPCODE", "tid=6 kind=INVALID OPCODE"),
            valid + "\nUSER FAULT: tid=9 kind=PAGE FAULT address=0",
            valid + "\nUSER EXIT: tid=8 status=5000",
            valid + "\nSESSION SERVICE FAILURE",
            valid + "\nFILESYSTEM IO ERROR",
        ):
            with self.subTest(log=broken), self.assertRaises(RuntimeError):
                validate_shell(broken)


if __name__ == "__main__":
    unittest.main()
