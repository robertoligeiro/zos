import json
import struct
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch
import zlib

from mkimage import (
    BIOS_END, BIOS_START, BUILD, ESP_END, ESP_START, SECTOR, SECTORS,
    create_image, image_path, long_entries, short_name,
)
from zos import (
    CASES, COMMON_MARKERS, IPC_MARKERS, MEMORY_MARKERS, READY_MARKER, USER_PROGRAMS,
    image, qemu_command, screenshot, user_environment, validate_log,
)


class ImageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.path = image_path("unittest-layout.img")
        cls.payload = bytes(range(256)) * 9
        create_image(cls.path, {
            "limine.conf": b"ZOS test configuration",
            "limine-bios.sys": cls.payload,
            "EFI/BOOT/BOOTX64.EFI": b"mock EFI executable",
            "boot/zos.elf": b"mock kernel",
            "boot/echo-server.elf": b"mock server",
            "boot/echo-client.elf": b"mock client",
            "boot/fault-probe.elf": b"mock fault probe",
        })
        cls.stream = cls.path.open("rb")
        cls.stream.seek(ESP_START * SECTOR)
        cls.boot = cls.stream.read(SECTOR)
        cls.fat_sectors = struct.unpack_from("<I", cls.boot, 36)[0]
        cls.data_start = ESP_START + 32 + 2 * cls.fat_sectors
        cls.stream.seek((ESP_START + 32) * SECTOR)
        cls.fat = cls.stream.read(cls.fat_sectors * SECTOR)

    @classmethod
    def tearDownClass(cls):
        cls.stream.close()
        cls.path.unlink()

    def read_at(self, offset, count):
        self.stream.seek(offset)
        return self.stream.read(count)

    def chain(self, cluster):
        seen = set()
        result = bytearray()
        while cluster < 0x0FFFFFF8:
            self.assertNotIn(cluster, seen, "FAT cluster chain loops")
            self.assertGreaterEqual(cluster, 2)
            seen.add(cluster)
            result.extend(self.read_at((self.data_start + cluster - 2) * SECTOR, SECTOR))
            cluster = struct.unpack_from("<I", self.fat, cluster * 4)[0] & 0x0FFFFFFF
        return bytes(result)

    def directory(self, cluster):
        result = {}
        long_parts = {}
        expected_checksum = None
        for offset in range(0, len(data := self.chain(cluster)), 32):
            entry = data[offset:offset + 32]
            if entry[0] == 0:
                break
            if entry[11] == 0x0F:
                if entry[0] & 0x40:
                    long_parts = {}
                    expected_checksum = entry[13]
                self.assertEqual(entry[13], expected_checksum)
                long_parts[entry[0] & 0x1F] = entry[1:11] + entry[14:26] + entry[28:32]
                continue
            if long_parts:
                checksum = 0
                for byte in entry[:11]:
                    checksum = (((checksum & 1) << 7) + (checksum >> 1) + byte) & 0xFF
                self.assertEqual(checksum, expected_checksum)
                encoded = b"".join(long_parts[index] for index in sorted(long_parts))
                name = encoded.decode("utf-16le").split("\0", 1)[0]
            else:
                name = entry[:8].decode().strip()
                extension = entry[8:11].decode().strip()
                if extension:
                    name += "." + extension
            first = (struct.unpack_from("<H", entry, 20)[0] << 16) | struct.unpack_from("<H", entry, 26)[0]
            result[name] = (first, struct.unpack_from("<I", entry, 28)[0], entry[11])
            long_parts = {}
        return result

    def test_primary_and_backup_gpt_crc_and_partition_extents(self):
        self.assertEqual(self.path.stat().st_size, SECTORS * SECTOR)
        self.assertEqual(self.read_at(510, 2), b"\x55\xaa")
        self.assertEqual(self.read_at(450, 1), b"\xee")
        for current, backup in [(1, SECTORS - 1), (SECTORS - 1, 1)]:
            header = bytearray(self.read_at(current * SECTOR, 92))
            self.assertEqual(header[:8], b"EFI PART")
            self.assertEqual(struct.unpack_from("<QQ", header, 24), (current, backup))
            expected = struct.unpack_from("<I", header, 16)[0]
            struct.pack_into("<I", header, 16, 0)
            self.assertEqual(zlib.crc32(header), expected)
            table_lba = struct.unpack_from("<Q", header, 72)[0]
            entries = self.read_at(table_lba * SECTOR, 16384)
            self.assertEqual(zlib.crc32(entries), struct.unpack_from("<I", header, 88)[0])
            self.assertEqual(struct.unpack_from("<QQ", entries, 32), (BIOS_START, BIOS_END))
            self.assertEqual(struct.unpack_from("<QQ", entries, 160), (ESP_START, ESP_END))

    def test_fat32_geometry_backups_and_mirrored_tables(self):
        self.assertEqual(self.boot[510:512], b"\x55\xaa")
        self.assertEqual(self.boot[82:90], b"FAT32   ")
        self.assertEqual(struct.unpack_from("<H", self.boot, 11)[0], SECTOR)
        self.assertEqual(self.boot[13], 1)
        total = struct.unpack_from("<I", self.boot, 32)[0]
        clusters = total - 32 - 2 * self.fat_sectors
        self.assertGreaterEqual(clusters, 65525)
        self.assertGreaterEqual(len(self.fat) // 4, clusters + 2)
        self.assertEqual(self.read_at((ESP_START + 6) * SECTOR, SECTOR), self.boot)
        self.assertEqual(self.read_at((ESP_START + 32 + self.fat_sectors) * SECTOR, len(self.fat)), self.fat)
        info = self.read_at((ESP_START + 1) * SECTOR, SECTOR)
        self.assertEqual(struct.unpack_from("<I", info, 0)[0], 0x41615252)
        self.assertEqual(struct.unpack_from("<I", info, 484)[0], 0x61417272)
        self.assertEqual(info, self.read_at((ESP_START + 7) * SECTOR, SECTOR))

    def test_bootloader_long_filenames_and_multi_cluster_payload(self):
        root = self.directory(2)
        cluster, length, attributes = root["limine-bios.sys"]
        self.assertEqual(attributes, 0x20)
        self.assertEqual(self.chain(cluster)[:length], self.payload)
        cluster, length, _ = root["limine.conf"]
        self.assertEqual(self.chain(cluster)[:length], b"ZOS test configuration")

    def test_uefi_boot_path_and_directory_parents(self):
        root = self.directory(2)
        efi = self.directory(root["EFI"][0])
        self.assertEqual(efi[".."][0], 0)
        boot = self.directory(efi["BOOT"][0])
        self.assertEqual(boot[".."][0], root["EFI"][0])
        cluster, length, _ = boot["BOOTX64.EFI"]
        self.assertEqual(self.chain(cluster)[:length], b"mock EFI executable")
        kernel_dir = self.directory(root["boot"][0])
        cluster, length, _ = kernel_dir["zos.elf"]
        self.assertEqual(self.chain(cluster)[:length], b"mock kernel")
        for program, payload in (
            ("echo-server", b"mock server"), ("echo-client", b"mock client"),
            ("fault-probe", b"mock fault probe"),
        ):
            cluster, length, _ = kernel_dir[f"{program}.elf"]
            self.assertEqual(self.chain(cluster)[:length], payload)

    def test_image_targets_cannot_escape_build(self):
        for name in ("../disk.img", "..\\disk.img", "C:\\disk.img", "C:disk.img", "disk.bin", ""):
            with self.subTest(name=name), self.assertRaises(ValueError):
                image_path(name)
        self.assertEqual(image_path("zos.img"), (BUILD / "zos.img").resolve())

    def test_long_name_aliases_are_unique(self):
        used = set()
        first = short_name("limine-bios.sys", used)
        second = short_name("limine-bios-cd.sys", used)
        self.assertNotEqual(first, second)
        entries = long_entries("limine-bios.sys", first)
        self.assertEqual(len(entries), 64)
        self.assertEqual(entries[0], 0x42)
        self.assertEqual(entries[32], 1)


class AcceptanceHarnessTests(unittest.TestCase):
    def test_screenshot_negotiates_stdio_qmp_before_capture_and_quit(self):
        BUILD.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=BUILD) as directory:
            output = Path(directory)
            serial_text = "\n".join(
                (self.valid_log("BIOS", "ipc"), READY_MARKER)
            )
            replies = iter([
                b'{"QMP": {}}\n',
                b'{"return": {}, "id": "capabilities"}\n',
                b'{"return": {}, "id": "capture"}\n',
                b'{"return": {}, "id": "quit"}\n',
            ])
            process = Mock(returncode=0)
            process.poll.return_value = 0
            read_count = 0
            commands = []

            def receive():
                nonlocal read_count
                read_count += 1
                return next(replies)

            def send(payload):
                request = json.loads(payload)
                self.assertEqual(read_count, len(commands) + 1)
                commands.append(request["id"])
                if request["id"] == "capture":
                    Path(request["arguments"]["filename"]).write_bytes(
                        b"P6\n32 32\n255\n" + bytes(32 * 32 * 3)
                    )

            process.stdout.readline.side_effect = receive
            process.stdin.write.side_effect = send

            def launch_process(*args, **kwargs):
                (output / "bios-screen.serial.log").write_text(serial_text, encoding="utf-8")
                return process

            with patch("zos.BUILD", output), patch("zos.qemu_command", return_value=["qemu"]), \
                    patch("zos.subprocess.Popen", side_effect=launch_process) as launch:
                screenshot(BUILD / "zos.img", "bios", 5)
            self.assertEqual(commands, ["capabilities", "capture", "quit"])
            self.assertEqual(launch.call_args.args[0][-2:], ["-qmp", "stdio"])
            process.kill.assert_not_called()

    def test_guest_shutdown_exits_acceptance_vm_but_preserves_interactive_vm(self):
        BUILD.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=BUILD) as directory:
            tools = Path(directory)
            qemu = tools / "qemu"
            qemu.mkdir()
            (qemu / "qemu-system-x86_64.exe").touch()
            (qemu / "bios-256k.bin").touch()
            with patch("zos.TOOLS", tools):
                normal = qemu_command(BUILD / "zos.img", "bios")
                acceptance = qemu_command(BUILD / "zos-boot.img", "bios", exit_on_shutdown=True)
                memory = qemu_command(BUILD / "zos-memory.img", "bios", memory_mib=1024)
            self.assertIn("-no-shutdown", normal)
            self.assertNotIn("-no-shutdown", acceptance)
            self.assertIn("-no-reboot", acceptance)
            self.assertEqual(normal[normal.index("-m") + 1], "256M")
            self.assertEqual(memory[memory.index("-m") + 1], "1024M")

    def valid_log(self, firmware, case):
        result = "\n".join((*COMMON_MARKERS, f"FIRMWARE: {firmware}", CASES[case][1]))
        result = result.replace("TIMER PASS:", "TIMER PASS: ticks=100")
        if case == "divide":
            result += "\nEXCEPTION: DIVIDE ERROR"
        elif case in ("page-fault", "memory-protection", "memory-unmap", "memory-execute"):
            result += "\nEXCEPTION: PAGE FAULT"
        elif case == "memory":
            result += "\n" + "\n".join(MEMORY_MARKERS)
        elif case == "ipc":
            result += "\n" + "\n".join(
                marker for marker in IPC_MARKERS
                if marker != "client: cross-process echo and priority inheritance verified"
            )
            for index, kind in enumerate(("PAGE FAULT", "GENERAL PROTECTION", "INVALID OPCODE",
                                          "GENERAL PROTECTION", "INVALID RETURN"), 3):
                result += f"\nUSER FAULT: tid={index} kind={kind} address=0x400000"
            result += "\nclient: cross-process echo and priority inheritance verified"
            result += "\npeer: timer preemption and SSE context verified"
            for index in range(6):
                result += f"\nUSER EXIT: tid={index + 1} status=0"
        return result

    def test_ipc_requires_isolation_then_echo_and_both_preempted_peers(self):
        valid = self.valid_log("BIOS", "ipc")
        for marker in IPC_MARKERS:
            with self.subTest(marker=marker), self.assertRaises(RuntimeError):
                validate_log(valid.replace(marker, ""), "bios", "ipc")
        for invalid in (
            valid.replace("kind=INVALID RETURN", "kind=GENERAL PROTECTION"),
            valid + "\nUSER FAULT: tid=99 kind=PAGE FAULT address=0",
            valid.replace("status=0", "status=101", 1),
            valid + "\nEXCEPTION: GENERAL PROTECTION",
            valid.replace("\npeer: timer preemption and SSE context verified", "", 1),
        ):
            with self.subTest(log=invalid), self.assertRaises(RuntimeError):
                validate_log(invalid, "bios", "ipc")

    def test_user_flags_override_kernel_linker_and_use_separate_artifacts(self):
        env = user_environment()
        flags = env["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
        self.assertIn("link-arg=-Tservers/demo/linker.ld", flags)
        self.assertNotIn("link-arg=-Tkernel/linker.ld", flags)
        self.assertEqual(Path(env["CARGO_TARGET_DIR"]).parts[-2:], ("target", "user"))

    def test_image_packages_every_configured_user_module(self):
        with patch("zos.Path.read_bytes", return_value=b"ELF payload"), \
                patch("zos.create_image") as create, patch("zos.subprocess.run"):
            image(Path("kernel.elf"), "ipc")
        payloads = create.call_args.args[1]
        for program in USER_PROGRAMS:
            self.assertEqual(payloads[f"boot/{program}.elf"], b"ELF payload")

    def test_expected_markers_for_each_firmware_and_case(self):
        for firmware in ("BIOS", "UEFI"):
            for case in CASES:
                with self.subTest(firmware=firmware, case=case):
                    validate_log(self.valid_log(firmware, case), firmware.lower(), case)

    def test_missing_timer_wrong_firmware_and_panics_fail(self):
        valid = self.valid_log("BIOS", "boot")
        for invalid in (
            valid.replace("TIMER PASS:", ""),
            valid.replace("ticks=100", "ticks=0"),
            valid.replace("FIRMWARE: BIOS", "FIRMWARE: UEFI"),
            valid + "\nKERNEL PANIC",
            valid + "\nEXCEPTION: GENERAL PROTECTION",
        ):
            with self.subTest(log=invalid), self.assertRaises(RuntimeError):
                validate_log(invalid, "bios", "boot")

    def test_memory_requires_all_stress_checks_and_no_unexpected_faults(self):
        valid = self.valid_log("BIOS", "memory")
        for marker in (*MEMORY_MARKERS, "PAGING PASS:", "HEAP PASS:"):
            with self.subTest(marker=marker), self.assertRaises(RuntimeError):
                validate_log(valid.replace(marker, ""), "bios", "memory")
        with self.assertRaises(RuntimeError):
            validate_log(valid + "\nEXCEPTION: PAGE FAULT", "bios", "memory")

    def test_memory_fault_cases_require_expected_exception_and_their_own_marker(self):
        for case in ("memory-protection", "memory-unmap", "memory-execute"):
            valid = self.valid_log("UEFI", case)
            for invalid in (
                valid.replace("EXCEPTION: PAGE FAULT", ""),
                valid.replace(CASES[case][1], CASES["page-fault"][1]),
            ):
                with self.subTest(case=case), self.assertRaises(RuntimeError):
                    validate_log(invalid, "uefi", case)


if __name__ == "__main__":
    unittest.main()
