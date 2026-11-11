import unittest
from unittest.mock import patch

from zos import BUILD, USB_MARKERS, qemu_command, validate_usb


class UsbTests(unittest.TestCase):
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
