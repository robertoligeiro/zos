import unittest
import hashlib
import json
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from zos import BUILD, USB_MARKERS, build, diagnostic_command, image, qemu_command, validate_usb


class UsbTests(unittest.TestCase):
    def test_diagnostic_kernel_cannot_combine_test_features(self):
        with patch("zos.cargo") as cargo, self.assertRaises(ValueError):
            build("shell", diagnostic=True)
        cargo.assert_not_called()

    def test_diagnostic_usb_boot_is_separate_from_read_only_media_test(self):
        disk = BUILD / "zos-diagnostic.img"
        base = ["qemu", "-drive", "boot,if=ide", "-device", "qemu-xhci,id=xhci"]
        for firmware, protection in (("bios", "readonly=on"), ("uefi", "snapshot=on")):
            with self.subTest(firmware=firmware), patch("zos.qemu_command", return_value=base.copy()):
                command = diagnostic_command(disk, firmware)
                self.assertNotIn("boot,if=ide", command)
                self.assertTrue(any(f"id=diagnostic-boot,{protection}" in part for part in command))
                self.assertTrue(any("bootindex=1" in part for part in command))
        with patch("zos.qemu_command", return_value=base.copy()):
            command = diagnostic_command(disk, "uefi", "read-only-media")
        self.assertIn("boot,if=ide", command)
        self.assertTrue(any("id=diagnostic-boot,readonly=on" in part for part in command))
        self.assertFalse(any(part.startswith("usb-storage,") for part in command))
        self.assertFalse(any("snapshot=on" in part for part in command))

    def test_usb_package_contains_only_diagnostic_boot_files_and_hashes(self):
        BUILD.mkdir(exist_ok=True)
        with TemporaryDirectory(dir=BUILD, prefix="package-test-") as directory:
            root = Path(directory)
            with patch("zos.BUILD", root), patch("zos.create_image"), patch("zos.subprocess.run"), \
                    patch.object(Path, "read_bytes", return_value=b"fixture"):
                image(root / "kernel.elf", "diagnostic")
            package = root / "usb-diagnostic-files"
            manifest = json.loads((package / "manifest.json").read_text())
            self.assertEqual(manifest["kind"], "zos-usb-diagnostic")
            self.assertTrue(manifest["storage_read_only"])
            self.assertEqual(set(manifest["files"]), {
                "limine.conf", r"EFI\BOOT\BOOTX64.EFI", r"boot\zos.elf",
                r"boot\console-server.elf", r"boot\input-server.elf"})
            for name, digest in manifest["files"].items():
                self.assertEqual(hashlib.sha256((package / name).read_bytes()).hexdigest(), digest)

    def test_usb_disk_is_not_attached_as_ata(self):
        data = BUILD / "data-usb.img"
        with patch("zos.validate_disk"):
            command = qemu_command(BUILD / "zos-usb.img", "bios", data_disk=data, usb=True)
        self.assertIn("qemu-xhci,id=xhci", command)
        self.assertIn("usb-kbd,id=zos-usb-kbd,bus=xhci.0,port=1", command)
        self.assertIn("usb-mouse,id=zos-usb-mouse,bus=xhci.0,port=2", command)
        self.assertIn("usb-storage,id=zos-usb-disk,bus=xhci.0,port=3,drive=usb-data", command)
        data_drive = next(part for part in command if part.startswith(f"file={data},"))
        self.assertIn("if=none,id=usb-data", data_drive)
        self.assertIn("snapshot=off", data_drive)
        self.assertNotIn("if=ide", data_drive)

    def test_usb_proofs_cannot_be_replaced_with_legacy_boot(self):
        text = "\n".join(USB_MARKERS)
        validate_usb(text)
        for marker in USB_MARKERS:
            with self.subTest(missing=marker), self.assertRaises(RuntimeError):
                validate_usb(text.replace(marker, ""))
        for error in ("PS2 ready:", "BLOCK ready: secondary ATA", "USB DISK ERROR: Io",
                      "USB RUNTIME FAILURE: Io", "USB INITIALIZATION FAILURE: Io", "XHCI HOST ERROR"):
            with self.subTest(error=error), self.assertRaises(RuntimeError):
                validate_usb(text + "\n" + error)


if __name__ == "__main__":
    unittest.main()
