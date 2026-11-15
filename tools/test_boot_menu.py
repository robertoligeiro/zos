import re
import unittest

from mkimage import ROOT
from zos import boot_menu


class BootMenuTests(unittest.TestCase):
    def setUp(self):
        self.text = boot_menu().decode("ascii")
        header, *entries = re.split(r"(?m)^/", self.text)
        self.header = header
        self.entries = {entry.splitlines()[0]: entry for entry in entries}

    def test_header_waits_five_seconds_and_defaults_to_the_desktop(self):
        self.assertIn("timeout: 5", self.header)
        self.assertNotIn("timeout: 0", self.text)
        self.assertIn("default_entry: 1", self.header)
        self.assertIn("serial: yes", self.header)

    def test_two_entries_in_order_desktop_first(self):
        self.assertEqual(list(self.entries), ["ZOS - Desktop (GUI)", "ZOS - Command line"])

    def test_the_desktop_loads_the_window_manager_as_the_console_server(self):
        desktop, plain = self.entries["ZOS - Desktop (GUI)"], self.entries["ZOS - Command line"]
        self.assertIn("module_path: boot():/boot/wm-server.elf\n    module_cmdline: console-server", desktop)
        self.assertNotIn("/boot/console-server.elf", desktop)
        self.assertIn("module_path: boot():/boot/console-server.elf\n    module_cmdline: console-server", plain)
        self.assertNotIn("wm-server", plain)

    def test_every_other_module_and_the_kernel_are_identical(self):
        def modules(entry):
            return [line.strip() for line in entry.splitlines() if "module_" in line or line.strip().startswith("path:")]
        desktop = modules(self.entries["ZOS - Desktop (GUI)"])
        plain = modules(self.entries["ZOS - Command line"])
        self.assertEqual(len(desktop), len(plain))
        differing = [(a, b) for a, b in zip(desktop, plain) if a != b]
        self.assertEqual(differing, [("module_path: boot():/boot/wm-server.elf", "module_path: boot():/boot/console-server.elf")])
        for entry in self.entries.values():
            self.assertIn("path: boot():/boot/zos.elf", entry)
            self.assertIn("resolution: 1024x768x32", entry)

    def test_the_source_configs_are_unchanged_by_building_the_menu(self):
        normal = (ROOT / "boot" / "limine.conf").read_text(encoding="ascii")
        self.assertIn("timeout: 0", normal, "QEMU tests rely on the instant-boot config")
        self.assertIn("wm-server", (ROOT / "boot" / "limine-gui.conf").read_text(encoding="ascii"))


if __name__ == "__main__":
    unittest.main()