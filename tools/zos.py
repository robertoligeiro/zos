"""Native Windows ZOS build, image creation, QEMU, and phase acceptance."""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import struct
import sys
import time

from mkimage import BUILD, ROOT, create_image, image_path
from mkfs import (ZOS_FIRST, ZOS_PARTITION_TYPE, data_path, ensure_disk, format_disk, format_gpt_disk,
                  outside_zos_sha256, validate_disk, volume_image)
from zosfs import load as load_volume

TOOLS = ROOT / ".tools"
CASES = {
    "boot": ("test-boot", "BOOT ACCEPTANCE PASS"),
    "divide": ("test-divide", "DIVIDE ACCEPTANCE PASS"),
    "page-fault": ("test-page-fault", "PAGE FAULT ACCEPTANCE PASS"),
    "memory": ("test-memory", "MEMORY ACCEPTANCE PASS"),
    "memory-protection": ("test-memory-protection", "MEMORY PROTECTION ACCEPTANCE PASS"),
    "memory-unmap": ("test-memory-unmap", "MEMORY UNMAP ACCEPTANCE PASS"),
    "memory-execute": ("test-memory-execute", "MEMORY EXECUTE ACCEPTANCE PASS"),
    "ipc": ("test-ipc", "IPC ACCEPTANCE PASS"),
    "console": ("test-console", "CONSOLE ACCEPTANCE PASS"),
    "fs-create": ("test-fs-create", "FS CREATE PASS:"),
    "fs-read": ("test-fs-read", "FS REBOOT PASS:"),
    "shell": ("test-shell", "SHELL ACCEPTANCE PASS"),
}
COMMON_MARKERS = ("ZOS - Zombie OS", "ZOS booting", "FRAMEBUFFER ready:",
                  "GDT/TSS/IDT ready", "PAGING ready:", "HEAP ready:",
                  "HEAP PASS:", "PAGING PASS:", "MEMORY ready:",
                  "PIT ready: 100 Hz", "TIMER PASS:")
MEMORY_MARKERS = ("FRAMES PASS: 100000 simultaneous pages", "FRAMES OOM PASS:",
                  "HEAP STRESS PASS:")
IPC_MARKERS = (
    "RING3 ready: SYSCALL/SYSRET", "ELF VALIDATION PASS:",
    "client: syscall validation and denied capabilities verified",
    "client: cross-process echo and priority inheritance verified",
    "client: cross-page buffers and channel closure verified",
    "worker: receive/reply and stale token rejection verified",
    "server: IRQ notification and port grant verified",
    "peer: timer preemption and SSE context verified",
    "SCHEDULER PASS:", "IPC PASS:", "IRQ/IO PASS:", "USER ISOLATION PASS:",
    "PROCESS CLEANUP PASS:",
)
USER_PROGRAMS = ("echo-server", "echo-client", "fault-probe",
                 "console-server", "input-server", "tty-server",
                 "blkdev-server", "fs-simple-server", "vfs-server", "storage-probe",
                 "procmgr-server", "shell-server", "zos-command")
READY_MARKER = "ZOS ready: console checkpoint"
CONSOLE_MARKERS = ("CONSOLE PASS: cursor, colors, backspace, scrolling",
                   "PS2 ready: set-1 keyboard IRQ1, 3-byte mouse IRQ12",
                   "CAPABILITIES PASS: framebuffer, PS/2 ports and IRQs isolated", READY_MARKER)
CONSOLE_LINES = ["zombie", "zombie!", "zombie!", "ZOS", "ab!c", "~finish"]
STORAGE_READY = "ZOS ready: persistent filesystem and VFS verified"
STORAGE_MARKERS = ("BLOCK ready: secondary ATA", "FS ready: checkpoint=", "VFS ready: root mount")
SHELL_READY = "SHELL ready: interactive ZOS shell"
SHELL_PROOFS = (
    "PROCESS CAPABILITIES PASS:", "PROCESS REUSE PASS:", "PROCESS STATUS PASS:",
    "PROCESS CLEANUP PASS: kill blocked IPC", "PATH SPAWN PASS:",
    "SHELL SESSION PASS:", "SHELL CLEANUP PASS:",
)
USB_MARKERS = (
    "PCI XHCI PASS:", "XHCI ready:", "USB ENUM PASS: slot=1 port=3 Storage",
    "USB ENUM PASS: slot=2 port=5 Keyboard", "USB ENUM PASS: slot=3 port=6 Mouse",
    "USB SCSI PASS:", "USB ready:", "USB CAPABILITIES PASS:",
    "CAPABILITIES PASS: framebuffer, USB MMIO/DMA and IRQs isolated",
    "BLOCK ready: USB bulk-only SCSI", "FS ready: checkpoint=", "VFS ready: root mount",
    "CONSOLE PASS: cursor, colors, backspace, scrolling", READY_MARKER,
    "USB STORAGE CAPABILITY PASS: shell cannot issue raw blocks",
)


def environment() -> dict[str, str]:
    env = os.environ.copy()
    env["CARGO_HOME"] = str(TOOLS / "cargo")
    env["RUSTUP_HOME"] = str(TOOLS / "rustup")
    env["PATH"] = os.pathsep.join(
        [str(TOOLS / "cargo" / "bin"), str(TOOLS / "mingw64" / "bin"), env["PATH"]]
    )
    return env


def user_environment() -> dict[str, str]:
    env = environment()
    env["CARGO_TARGET_DIR"] = str(ROOT / "target" / "user")
    env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join([
        "-C", "link-arg=-Tservers/demo/linker.ld",
        # Match the prebuilt freestanding core's LTO model. All user ELF symbols
        # lie below 2 GiB, which is also supported by the kernel code model.
        "-C", "relocation-model=static", "-C", "code-model=kernel", "-C", "no-redzone=yes",
    ])
    return env


def cargo(*args: str, user: bool = False) -> None:
    executable = TOOLS / "cargo" / "bin" / "cargo.exe"
    if not executable.is_file():
        raise RuntimeError("Rust unavailable. Run tools\\bootstrap.ps1 after allowing it in sandbox policy.")
    subprocess.run([str(executable), *args], cwd=ROOT,
                   env=user_environment() if user else environment(), check=True)


def build(case: str | None = None, *, usb: bool = False, diagnostic: bool = False) -> Path:
    if diagnostic and case:
        raise ValueError("Diagnostics cannot be combined with another boot test")
    cargo("build", "--locked", "--release", "--package", "zos-demo",
          "--package", "zos-console", "--package", "zos-storage", "--package", "zos-session", "--package", "zos-wm", user=True)
    BUILD.mkdir(exist_ok=True)
    program = ROOT / "target" / "user" / "x86_64-unknown-none" / "release" / "zos-command"
    (BUILD / "zos-command.elf").write_bytes(compact_elf(program.read_bytes()))
    args = ["build", "--locked", "--release", "--package", "zos-kernel", "--no-default-features"]
    features = ([CASES[case][0]] if case else []) + (
        ["usb-diagnostic"] if diagnostic else ["usb"] if usb else [])
    if features:
        args += ["--features", ",".join(features)]
    cargo(*args)
    BUILD.mkdir(exist_ok=True)
    destination = BUILD / ("zos-diagnostic.elf" if diagnostic else f"zos-{'usb-' if usb else ''}{case or 'normal'}.elf")
    shutil.copyfile(ROOT / "target" / "x86_64-unknown-none" / "release" / "zos-kernel", destination)
    return destination


def compact_elf(data: bytes) -> bytes:
    if len(data) < 64 or data[:7] != b"\x7fELF\x02\x01\x01":
        raise ValueError("Invalid bundled program ELF")
    offset = struct.unpack_from("<Q", data, 32)[0]
    count = struct.unpack_from("<H", data, 56)[0]
    end = offset + count * 56
    if count == 0 or end > len(data):
        raise ValueError("Invalid program header table")
    for i in range(count):
        location = offset + i * 56
        if struct.unpack_from("<I", data, location)[0] == 1:
            file_offset = struct.unpack_from("<Q", data, location + 8)[0]
            size = struct.unpack_from("<Q", data, location + 32)[0]
            end = max(end, file_offset + size)
    if end > len(data) or end > 65536:
        raise ValueError("Bundled executable exceeds the filesystem's 64 KiB file limit")
    result = bytearray(data[:end])
    result[40:48] = bytes(8)
    result[58:64] = bytes(6)
    return bytes(result)


def image(kernel: Path, case: str | None = None) -> Path:
    limine = TOOLS / "limine"
    destination = image_path(f"zos-{case}.img" if case else "zos.img")
    configuration = {"diagnostic": "limine-diagnostic.conf", "gui": "limine-gui.conf",
                     "usb-gui": "limine-gui.conf"}.get(case, "limine.conf")
    payloads = {
        "limine.conf": (ROOT / "boot" / configuration).read_bytes(),
        "limine-bios.sys": (limine / "limine-bios.sys").read_bytes(),
        "EFI/BOOT/BOOTX64.EFI": (limine / "BOOTX64.EFI").read_bytes(),
        "boot/zos.elf": kernel.read_bytes(),
    }
    # The desktop entry loads the window manager in place of the text console server.
    programs = ("console-server", "input-server") if case == "diagnostic" else (
        tuple(p for p in USER_PROGRAMS if p != "console-server") + ("wm-server",) if case in ("gui", "usb-gui")
        else USER_PROGRAMS)
    for program in programs:
        source = BUILD / "zos-command.elf" if program == "zos-command" else (
            ROOT / "target" / "user" / "x86_64-unknown-none" / "release" / program
        )
        payloads[f"boot/{program}.elf"] = source.read_bytes()
    create_image(destination, payloads)
    subprocess.run(
        [str(limine / "limine.exe"), "bios-install", str(destination), "1"],
        cwd=ROOT, check=True,
    )
    print(f"Image: {destination}", flush=True)
    if case == "diagnostic":
        package = BUILD / "usb-diagnostic-files"
        hashes = {}
        for name, data in payloads.items():
            if name == "limine-bios.sys":
                continue
            target = package / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            hashes[name.replace("/", "\\")] = hashlib.sha256(data).hexdigest()
        (package / "manifest.json").write_text(json.dumps({
            "kind": "zos-usb-diagnostic", "storage_read_only": True, "files": hashes,
        }, indent=2) + "\n", encoding="utf-8")
        print(f"UEFI USB package: {package}", flush=True)
    if case == "usb":
        usb_normal_package(payloads)
    return destination


def boot_menu() -> bytes:
    """A Limine config with two entries built from the normal and desktop configs: the
    desktop first (the default), then the plain command line."""
    command_line = (ROOT / "boot" / "limine.conf").read_text(encoding="ascii").replace("\r\n", "\n")
    desktop = (ROOT / "boot" / "limine-gui.conf").read_text(encoding="ascii").replace("\r\n", "\n")
    header, command_entry = command_line.split("/ZOS - Zombie OS", 1)
    _, desktop_entry = desktop.split("/ZOS - Zombie OS (desktop)", 1)
    if "timeout: 0" not in header:
        raise RuntimeError("boot/limine.conf no longer starts with 'timeout: 0'")
    header = header.replace("timeout: 0", "timeout: 5\ndefault_entry: 1")
    # Diagnostic: the command line plus a marker module. The kernel then keeps its boot
    # log on screen and the console draws nothing, so a hang shows how far boot got.
    marker = "    module_path: boot():/boot/echo-server.elf\n    module_cmdline: screenlog\n"
    diagnostic = command_entry.rstrip("\n").replace("    resolution:", marker + "    resolution:", 1)
    if marker not in diagnostic:
        raise RuntimeError("boot/limine.conf has no resolution line to put the marker before")
    menu = (header + "/ZOS - Desktop (GUI)" + desktop_entry.rstrip("\n") + "\n\n"
            + "/ZOS - Command line" + command_entry.rstrip("\n") + "\n\n"
            + "/ZOS - Diagnostic (show boot log)" + diagnostic + "\n")
    return menu.encode("ascii")


def usb_normal_package(payloads: dict[str, bytes]) -> Path:
    """The files for a bootable USB stick with the interactive shell and a writable ZOS
    volume. prepare-usb.ps1 -Normal copies the boot files to a FAT32 partition and writes
    zos-volume.img into a separate GPT partition of type ZOS_PARTITION_TYPE."""
    package = BUILD / "usb-normal-files"
    shutil.rmtree(package, ignore_errors=True)
    hashes = {}
    # Boot menu: the desktop (default after 5 seconds) or the plain command line.
    payloads = dict(payloads)
    payloads["limine.conf"] = boot_menu()
    payloads["boot/wm-server.elf"] = (ROOT / "target" / "user" / "x86_64-unknown-none" / "release"
                                      / "wm-server").read_bytes()
    for name, data in payloads.items():
        if name == "limine-bios.sys":
            continue
        target = package / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        hashes[name.replace("/", "\\")] = hashlib.sha256(data).hexdigest()
    volume = volume_image()
    (package / "zos-volume.img").write_bytes(volume)
    (package / "manifest.json").write_text(json.dumps({
        "kind": "zos-usb-normal", "storage_read_only": False, "files": hashes,
        "volume": {"file": "zos-volume.img", "bytes": len(volume),
                   "sha256": hashlib.sha256(volume).hexdigest(),
                   "gpt_type": str(ZOS_PARTITION_TYPE)},
    }, indent=2) + "\n", encoding="utf-8")
    print(f"Normal USB package: {package}", flush=True)
    return package


def qemu_command(disk: Path, firmware: str, *, exit_on_shutdown: bool = False,
                 memory_mib: int = 256, data_disk: Path | None = None,
                 usb: bool = False) -> list[str]:
    directory = TOOLS / "qemu"
    executable = directory / "qemu-system-x86_64.exe"
    if not executable.is_file():
        raise RuntimeError("QEMU unavailable. Run tools\\bootstrap.ps1 first.")
    bios = next(directory.rglob("bios-256k.bin"), None)
    if bios is None:
        raise RuntimeError("QEMU firmware directory is missing bios-256k.bin")
    command = [
        str(executable), "-L", str(bios.parent), "-accel", "tcg", "-machine", "pc",
        "-cpu", "max", "-smp", "1", "-m", f"{memory_mib}M", "-nic", "none",
        "-drive", f"file={disk},format=raw,if=ide,index=0,snapshot=on",
    ]
    if exit_on_shutdown:
        command.append("-no-reboot")
    if usb:
        command += ["-device", "qemu-xhci,id=xhci",
                    "-device", "usb-kbd,id=zos-usb-kbd,bus=xhci.0,port=1",
                    "-device", "usb-mouse,id=zos-usb-mouse,bus=xhci.0,port=2"]
    if data_disk is not None:
        if data_disk.resolve() == disk.resolve() or data_disk.resolve() != data_path(data_disk.name):
            raise ValueError("Data disk must be a separate generated volume inside build")
        validate_disk(data_disk)
        if usb:
            command += ["-drive", f"file={data_disk},format=raw,if=none,id=usb-data,cache=writeback,snapshot=off",
                        "-device", "usb-storage,id=zos-usb-disk,bus=xhci.0,port=3,drive=usb-data"]
        else:
            command += ["-drive", f"file={data_disk},format=raw,if=ide,index=2,cache=writeback,snapshot=off"]
    if not exit_on_shutdown:
        command.append("-no-shutdown")
    if firmware == "uefi":
        code = next(directory.rglob("edk2-x86_64-code.fd"), None)
        variables = next(directory.rglob("edk2-i386-vars.fd"), None)
        if code is None or variables is None:
            raise RuntimeError("QEMU package lacks the EDK2 x86-64 code/i386 vars firmware pair")
        runtime_vars = BUILD / f"uefi-vars-{disk.stem}.fd"
        shutil.copyfile(variables, runtime_vars)
        command += [
            "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={code}",
            "-drive", f"if=pflash,format=raw,unit=1,file={runtime_vars}",
        ]
    return command


def validate_log(text: str, firmware: str, case: str, *, usb: bool = False) -> None:
    expected = (*COMMON_MARKERS, f"FIRMWARE: {firmware.upper()}", CASES[case][1])
    if case == "memory":
        expected += MEMORY_MARKERS
    if case == "ipc":
        expected += IPC_MARKERS
    missing = [marker for marker in expected if marker not in text]
    if missing:
        raise RuntimeError(f"Missing serial markers: {', '.join(missing)}")
    ticks = re.search(r"TIMER PASS: ticks=(\d+)", text)
    if ticks is None or int(ticks.group(1)) < 100:
        raise RuntimeError("Timer did not report at least 100 interrupts")
    if "KERNEL PANIC" in text:
        raise RuntimeError("Kernel panic appeared in acceptance log")
    if case == "divide" and "EXCEPTION: DIVIDE ERROR" not in text:
        raise RuntimeError("Divide test did not report the expected CPU exception")
    if case in ("page-fault", "memory-protection", "memory-unmap", "memory-execute") \
            and "EXCEPTION: PAGE FAULT" not in text:
        raise RuntimeError("Page-fault test did not report the expected CPU exception")
    if case in ("boot", "memory", "ipc", "console", "fs-create", "fs-read") and "EXCEPTION:" in text:
        raise RuntimeError("Boot image raised an unexpected exception")
    if case == "ipc":
        validate_ipc(text)
    if case == "console":
        validate_console(text)
    if case == "shell":
        validate_shell(text, usb=usb)
    if case.startswith("fs-"):
        validate_storage(text)
        for marker in ("STORAGE CLEANUP PASS:", "FILESYSTEM ACCEPTANCE PASS"):
            if marker not in text:
                raise RuntimeError(f"Missing filesystem proof: {marker}")
        if case == "fs-create":
            for marker in ("FD OWNER PASS:", "STORAGE CAPABILITIES PASS:", "VFS CRUD PASS:",
                           "8197 patterned bytes durably committed"):
                if marker not in text:
                    raise RuntimeError(f"Missing storage create proof: {marker}")
        elif "exact data and metadata survived QEMU restart" not in text:
            raise RuntimeError("Read phase did not verify exact reboot persistence")


def validate_ipc(text: str) -> None:
    text = text.split("Starting ring-3 console", 1)[0]
    for marker in IPC_MARKERS:
        if marker not in text:
            raise RuntimeError(f"Missing IPC proof: {marker}")
    expected_faults = ("PAGE FAULT", "GENERAL PROTECTION", "INVALID OPCODE",
                       "GENERAL PROTECTION", "INVALID RETURN")
    faults = re.findall(r"^USER FAULT: tid=\d+ kind=(.*?) address=", text, re.MULTILINE)
    if sorted(faults) != sorted(expected_faults):
        raise RuntimeError("Expected exactly five isolated user faults")
    if text.count("peer: timer preemption and SSE context verified") != 2:
        raise RuntimeError("Both preempted peer threads must verify their SSE state")
    if text.index("client: cross-process echo") < text.rindex("USER FAULT:"):
        raise RuntimeError("Echo must complete after all isolated faults")
    exits = re.findall(r"^USER EXIT: tid=\d+ status=(-?\d+)$", text, re.MULTILINE)
    if len(exits) != 6 or any(status != "0" for status in exits):
        raise RuntimeError("All six non-faulting threads must exit successfully")


def validate_storage(text: str) -> None:
    validate_ipc(text)
    for marker in (*CONSOLE_MARKERS, *STORAGE_MARKERS):
        if marker not in text:
            raise RuntimeError(f"Missing storage startup proof: {marker}")
    services = text.split("Starting ring-3 console", 1)[-1]
    if any(marker in services for marker in ("USER FAULT:", "SERVICE FAILURE", "KERNEL PANIC",
                                            "IO ERROR", "ASSERTION FAILED")):
        raise RuntimeError("Storage or console service failed")


def validate_usb(text: str) -> None:
    for marker in USB_MARKERS:
        if marker not in text:
            raise RuntimeError(f"Missing USB proof: {marker}")
    services = text.split("Starting ring-3 console", 1)[-1]
    if any(marker in services for marker in (
            "PS2 ready:", "BLOCK ready: secondary ATA", "USB DISK ERROR",
            "USB DMA ERROR", "USB INITIALIZATION FAILURE", "USB RUNTIME FAILURE", "XHCI HOST ERROR")):
        raise RuntimeError("USB service failed or fell back to legacy hardware")


def validate_shell(text: str, *, usb: bool = False) -> None:
    validate_ipc(text)
    hardware = USB_MARKERS if usb else (*CONSOLE_MARKERS, *STORAGE_MARKERS)
    if usb:
        validate_usb(text)
    for marker in (*hardware, SHELL_READY, *SHELL_PROOFS):
        if marker not in text:
            raise RuntimeError(f"Missing shell proof: {marker}")
    services = text.split("Starting ring-3 console", 1)[-1]
    faults = re.findall(r"^USER FAULT: tid=(\d+) kind=(.*?) address=", services, re.MULTILINE)
    if faults != [("9", "INVALID OPCODE")]:
        raise RuntimeError("Shell test requires exactly its contained command fault")
    if any(marker in services for marker in ("SERVICE FAILURE", "KERNEL PANIC", "EXCEPTION:",
                                             "IO ERROR", "ASSERTION FAILED")):
        raise RuntimeError("Shell or supporting service failed")
    for tid, status in re.findall(r"^USER EXIT: tid=(\d+) status=(-?\d+)$", services, re.MULTILINE):
        if int(tid) <= 8 and status != "0":
            raise RuntimeError("A system service exited unsuccessfully")

def validate_console(text: str) -> None:
    validate_ipc(text)
    for marker in (*CONSOLE_MARKERS, "TTY PASS: real keys, editing, modifiers and history",
                   "CONSOLE CLEANUP PASS:", "CONSOLE ACCEPTANCE PASS",
                   "MOUSE: x=592 y=354 buttons=1", "MOUSE: x=592 y=354 buttons=0"):
        if marker not in text:
            raise RuntimeError(f"Missing console proof: {marker}")
    if re.findall(r"TTY LINE: ([^\r\n]*)", text) != CONSOLE_LINES:
        raise RuntimeError("Actual typed/edited/history lines do not match console script")
    if "CONSOLE SERVICE FAILURE" in text or "KERNEL PANIC" in text or "EXCEPTION:" in text:
        raise RuntimeError("Console service or kernel failed")


class Qmp:
    """Bounded, sequential QMP over pipes; no listening socket."""

    def __init__(self, process, path: Path, timeout: int):
        self.process = process
        self.timeout = timeout
        self.reader = ThreadPoolExecutor(max_workers=1)
        self.log = path.open("wb")
        self.sequence = 0

    def receive(self) -> dict:
        try:
            line = self.reader.submit(self.process.stdout.readline).result(timeout=self.timeout)
        except TimeoutError as error:
            raise RuntimeError("QMP response timed out") from error
        if not line:
            raise RuntimeError("QMP closed before command completed")
        self.log.write(line)
        self.log.flush()
        return json.loads(line)

    def negotiate(self) -> None:
        if "QMP" not in self.receive():
            raise RuntimeError("Missing QMP greeting")
        self.execute("qmp_capabilities", identity="capabilities")

    def execute(self, command: str, arguments: dict | None = None,
                *, identity: str | None = None):
        self.sequence += 1
        request = {"execute": command, "id": identity or str(self.sequence)}
        if arguments is not None:
            request["arguments"] = arguments
        self.process.stdin.write(json.dumps(request).encode() + b"\n")
        self.process.stdin.flush()
        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
            response = self.receive()
            if "error" in response:
                raise RuntimeError(f"QMP {command} failed: {response['error']}")
            if response.get("id") == request["id"] and "return" in response:
                return response["return"]
        raise RuntimeError(f"QMP {command} timed out")

    def close(self) -> None:
        # Kill/wait the VM before closing so a timed-out pipe reader cannot hang.
        self.reader.shutdown(wait=True)
        self.log.close()


def wait_serial(process, log: Path, marker: str, timeout: int, *, start: int = 0) -> str:
    deadline = time.monotonic() + timeout
    while True:
        text = log.read_text(encoding="utf-8", errors="replace") if log.exists() else ""
        current = text[start:]
        if any(failure in current for failure in ("KERNEL PANIC", "EXCEPTION:", "SERVICE FAILURE", "X64 Exception Type")):
            raise RuntimeError(f"Guest failed; inspect {log}")
        if marker in current:
            return text
        if process.poll() is not None or time.monotonic() >= deadline:
            raise RuntimeError(f"Waiting for {marker!r} failed; inspect {log}")
        time.sleep(0.01)

def diagnostic_command(disk: Path, firmware: str, case: str = "normal") -> list[str]:
    command = qemu_command(disk, firmware, usb=case != "no-controller")
    storage_cases = ("normal", "read-only-media", "usb2", "superspeed", "nec", "hub", "integrated", "no-ps2")
    if case in storage_cases:
        if case == "normal":
            index = command.index("-drive")
            del command[index:index + 2]
        # Bundled EDK2's USB mass-storage driver faults on a write-protected medium.
        protection = "snapshot=on" if firmware == "uefi" and case != "read-only-media" else "readonly=on"
        command += ["-drive", f"file={disk},format=raw,if=none,id=diagnostic-boot,{protection}"]
        if case in storage_cases and case != "read-only-media":
            command += ["-device", "usb-storage,id=zos-usb-disk,bus=xhci.0,port=3,drive=diagnostic-boot,bootindex=1"]
    if case in ("empty", "integrated"):
        for prefix in ("usb-kbd,", "usb-mouse,"):
            index = next(i for i, value in enumerate(command) if value.startswith(prefix))
            del command[index - 1:index + 1]
    if case == "unsupported":
        command += ["-device", "usb-tablet,id=unsupported,bus=xhci.0,port=4"]
    if case == "usb2":
        command[command.index("qemu-xhci,id=xhci")] = "qemu-xhci,id=xhci,p3=0"
    if case == "nec":
        command[command.index("qemu-xhci,id=xhci")] = "nec-usb-xhci,id=xhci"
    if case == "no-ps2":
        command[command.index("pc")] = "pc,i8042=off"
    if case == "hub":
        for i, value in enumerate(command):
            if value.startswith(("usb-kbd,", "usb-mouse,", "usb-storage,")):
                for port in (1, 2, 3):
                    value = value.replace(f"port={port},", f"port=1.{port},")
                if value.endswith("port=1"):
                    value = value[:-1] + "1.1"
                elif value.endswith("port=2"):
                    value = value[:-1] + "1.2"
                command[i] = value
        index = command.index("qemu-xhci,id=xhci") + 1
        command[index:index] = ["-device", "usb-hub,id=zos-hub,bus=xhci.0,port=1"]
    return command


def diagnostic_case(disk: Path, firmware: str, timeout: int, case: str = "normal") -> None:
    stem = f"{firmware}-diagnostic-{case}"
    log, picture = BUILD / f"{stem}.serial.log", BUILD / f"{stem}.ppm"
    log.unlink(missing_ok=True)
    picture.unlink(missing_ok=True)
    before = hashlib.sha256(disk.read_bytes()).hexdigest()
    command = diagnostic_command(disk, firmware, case) + [
        "-display", "none", "-monitor", "none", "-serial", f"file:{log}", "-qmp", "stdio"]
    with (BUILD / f"{stem}.qemu.log").open("wb") as errors:
        process = subprocess.Popen(command, cwd=ROOT, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors)
        qmp = Qmp(process, BUILD / f"{stem}.qmp.log", timeout)
        try:
            qmp.negotiate()
            if case == "read-only-media":
                # Insert after ExitBootServices, before the one-second PIT test ends.
                wait_serial(process, log, "PIT ready: 100 Hz", timeout)
                qmp.execute("stop")
                qmp.execute("device_add", {"driver": "usb-storage", "id": "zos-usb-disk",
                            "bus": "xhci.0", "port": "3", "drive": "diagnostic-boot"})
                qmp.execute("cont")
            expected = "DIAGNOSTIC FAILED: " if case == "no-controller" else "DIAGNOSTIC READY:"
            text = wait_serial(process, log, expected, timeout)
            if case != "no-controller":
                for marker in ("XHCI CAP: version=", "XHCI RAW: hcs1=",
                               "XHCI DMA: allocating controller rings",
                               "DIAGNOSTIC READ ONLY PASS:", "XHCI running:"):
                    if marker not in text:
                        raise RuntimeError(f"Missing diagnostic proof {marker}; inspect {log}")
                if text.index("XHCI RAW: hcs1=") > text.index("XHCI DMA: allocating"):
                    raise RuntimeError("Controller capabilities were not reported before DMA allocation")
                if case != "empty":
                    qmp.execute("send-key", {"keys": [{"type": "qcode", "data": "a"}], "hold-time": 20})
                    wait_serial(process, log, "DIAGNOSTIC KEY: Character(97)", timeout)
                    qmp.execute("input-send-event", {"events": [
                        {"type": "rel", "data": {"axis": "x", "value": 20}},
                        {"type": "rel", "data": {"axis": "y", "value": -10}}]})
                    wait_serial(process, log, "MOUSE: x=532 y=374", timeout)
                if case in ("normal", "read-only-media", "usb2", "superspeed", "nec", "hub", "integrated", "no-ps2"):
                    text = wait_serial(process, log, "USB READ PASS:", timeout)
                    if "USB DISK: sectors=262144 block=512" not in text:
                        raise RuntimeError("Diagnostics did not read the non-16MiB USB boot disk")
                    if case == "normal":
                        qmp.execute("device_del", {"id": "zos-usb-disk"})
                        wait_serial(process, log, "DIAGNOSTIC OFFLINE: Io", timeout)
                if case == "superspeed":
                    if "speed-id=4 Super:" not in text:
                        raise RuntimeError("SuperSpeed test did not enumerate a SuperSpeed device")
                if case == "usb2":
                    if "speed-id=3 High:" not in text or " Super:" in text:
                        raise RuntimeError("USB2 test did not force high-speed storage")
                if case == "nec" and "id=0x01941033" not in text:
                    raise RuntimeError("Non-QEMU PCI controller ID was not exercised")
                if case == "hub":
                    for marker in ("USB HUB READY:", "route=0x1 ", "route=0x2 ", "route=0x3 "):
                        if marker not in text:
                            raise RuntimeError(f"Missing hub proof {marker}")
                if case == "integrated":
                    if "INTEGRATED PS2: keyboard=Ok(()) mouse=Ok(())" not in text or "USB HID KEY PASS:" in text:
                        raise RuntimeError("Integrated input did not use the PS/2 path")
                if case == "no-ps2" and "INTEGRATED PS2 UNAVAILABLE:" not in text:
                    raise RuntimeError("Absent i8042 was not handled explicitly")
                if case == "unsupported":
                    wait_serial(process, log, "get-configuration: Unsupported", timeout)
            text = log.read_text(encoding="utf-8")
            for marker in ("SHELL ready:", "FS ready:", "BLOCK ready:", "USER FAULT:", "KERNEL PANIC"):
                if marker in text:
                    raise RuntimeError(f"Unexpected diagnostic activity {marker}; inspect {log}")
            qmp.execute("screendump", {"filename": str(picture)})
            qmp.execute("quit")
            if process.wait(timeout=timeout) != 0 or not picture.read_bytes().startswith(b"P6\n"):
                raise RuntimeError(f"Diagnostic capture/shutdown failed; inspect {log}")
        except RuntimeError:
            if process.poll() is None:
                qmp.execute("screendump", {"filename": str(picture)})
            raise
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            qmp.close()
            process.stdin.close()
            process.stdout.close()
    if hashlib.sha256(disk.read_bytes()).hexdigest() != before:
        raise RuntimeError("Read-only diagnostic changed its boot disk")
    (BUILD / f"{stem}.data.json").write_text(json.dumps({
        "image": disk.name, "before_sha256": before, "after_sha256": before,
        "usb_medium": "snapshot overlay" if case == "normal" and firmware == "uefi"
            else "read-only" if case in ("normal", "read-only-media", "usb2", "superspeed", "nec", "hub", "integrated", "no-ps2") else "none",
        "qemu_fully_exited": True,
    }, indent=2) + "\n", encoding="utf-8")
    print(f"PASS {firmware}/diagnostic-{case}: explicit diagnostics, image preserved", flush=True)


def console_case(disk: Path, firmware: str, timeout: int) -> None:
    log = BUILD / f"{firmware}-console.serial.log"
    stderr = BUILD / f"{firmware}-console.qemu.log"
    picture = BUILD / f"{firmware}-console.ppm"
    for path in (log, stderr, picture):
        path.unlink(missing_ok=True)
    command = qemu_command(disk, firmware, exit_on_shutdown=True) + [
        "-display", "none", "-monitor", "none", "-serial", f"file:{log}",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04", "-qmp", "stdio",
    ]
    with stderr.open("wb") as errors:
        process = subprocess.Popen(command, cwd=ROOT, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=errors)
        qmp = Qmp(process, BUILD / f"{firmware}-console.qmp.log", timeout)
        try:
            qmp.negotiate()
            wait_serial(process, log, READY_MARKER, timeout)
            keys = 0

            def event_key(code, down):
                return {"type": "key", "data": {"down": down,
                        "key": {"type": "qcode", "data": code}}}

            def key(code, *, shift=False, counted=True, finish=False):
                nonlocal keys
                events = [event_key(code, True), event_key(code, False)]
                if shift:
                    events = [event_key("shift", True), *events, event_key("shift", False)]
                qmp.execute("input-send-event", {"events": events})
                if counted and not finish:
                    keys += 1
                    wait_serial(process, log, f"TTY KEY: {keys}\n", timeout)

            # Backspace, Home/End, Delete, insertion, modifier release and history.
            qmp.execute("input-send-event", {"events": [
                event_key(code, down) for code in "zxombiex" for down in (True, False)
            ]})
            keys += len("zxombiex")
            wait_serial(process, log, f"TTY KEY: {keys}\n", timeout)
            for code in ("backspace", "home", "right", "delete", "end", "ret"):
                key(code)
            key("up"); key("1", shift=True); key("ret")
            key("x"); key("up"); key("down"); key("backspace"); key("up"); key("ret")
            key("caps_lock", counted=False)
            for code in "zos":
                key(code)
            key("ret"); key("caps_lock", counted=False)
            key("a"); key("c"); key("left"); key("b")
            key("1", shift=True); key("end"); key("ret")
            qmp.execute("input-send-event", {"events": [
                {"type": "rel", "data": {"axis": "x", "value": 80}},
                {"type": "rel", "data": {"axis": "y", "value": -30}},
                {"type": "btn", "data": {"button": "left", "down": True}},
            ]})
            wait_serial(process, log, "MOUSE: x=592 y=354 buttons=1", timeout)
            qmp.execute("input-send-event", {"events": [
                {"type": "btn", "data": {"button": "left", "down": False}},
            ]})
            wait_serial(process, log, "MOUSE: x=592 y=354 buttons=0", timeout)
            qmp.execute("screendump", {"filename": str(picture)})
            if not picture.read_bytes().startswith(b"P6\n"):
                raise RuntimeError("Console framebuffer capture failed")
            key("grave_accent", shift=True)
            for code in "finish":
                key(code)
            key("ret", finish=True)
            code = process.wait(timeout=timeout)
            if code != 33:
                raise RuntimeError(f"{firmware}/console expected exit 33, got {code}; inspect {log}")
            validate_log(log.read_text(encoding="utf-8", errors="replace"), firmware, "console")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            qmp.close()
            process.stdin.close()
            process.stdout.close()
    print(f"PASS {firmware}/console: real keyboard/mouse, editing/history, rendering, cleanup", flush=True)


class ShellKeys:
    def __init__(self, process, qmp: Qmp, log: Path, timeout: int):
        self.process, self.qmp, self.log, self.timeout = process, qmp, log, timeout
        self.keys = self.lines = self.start = 0

    def key(self, code: str, *, shift: bool = False, ctrl: bool = False, wait: bool = True) -> None:
        codes = (["shift"] if shift else []) + (["ctrl"] if ctrl else []) + [code]
        self.qmp.execute("send-key", {"keys": [{"type": "qcode", "data": c} for c in codes], "hold-time": 5})
        if wait:
            time.sleep(0.015)
        self.keys += 1
        if wait:
            wait_serial(self.process, self.log, f"SHELL KEY: {self.keys}\n", self.timeout, start=self.start)

    def type(self, text: str) -> None:
        plain = {" ": "spc", "/": "slash", ".": "dot", ";": "semicolon", "'": "apostrophe",
                 "-": "minus", "\\": "backslash", "=": "equal", ",": "comma",
                 "[": "bracket_left", "]": "bracket_right"}
        shifted = {'"': "apostrophe", ">": "dot", "&": "7", "~": "grave_accent",
                   "_": "minus", "!": "1", ":": "semicolon", "$": "4", "|": "backslash",
                   "<": "comma", "#": "3", "*": "8", "?": "slash", "{": "bracket_left",
                   "}": "bracket_right", "(": "9", ")": "0", "%": "5", "@": "2", "^": "6", "+": "equal"}
        for char in text:
            if char in shifted:
                self.key(shifted[char], shift=True)
            elif char.isupper():
                self.key(char.lower(), shift=True)
            elif char in plain:
                self.key(plain[char])
            elif char.isascii() and char.isalnum():
                self.key(char)
            else:
                raise ValueError(f"Missing keyboard test mapping for {char!r}")

    def enter(self, statuses: list[int]) -> None:
        before = re.findall(r"SHELL STATUS: (-?\d+)", self.log.read_text(encoding="utf-8"))
        self.key("ret")
        self.lines += 1
        text = wait_serial(self.process, self.log, f"SHELL LINE DONE: {self.lines}\n", self.timeout, start=self.start)
        after = re.findall(r"SHELL STATUS: (-?\d+)", text)
        actual = [int(n) for n in after[len(before):]]
        if actual != statuses:
            raise RuntimeError(f"Shell statuses {actual} != {statuses}; inspect {self.log}")

    def line(self, text: str, statuses: list[int]) -> None:
        self.type(text)
        self.enter(statuses)


def shell_case(disk: Path, firmware: str, timeout: int, *, usb: bool = False, gpt: bool = False) -> None:
    """With gpt, the USB disk is a 64 MiB stick: the ZOS volume sits inside a GPT
    partition, and every byte outside that partition must come out unchanged."""
    flavor = "usb-gpt-shell" if gpt else "usb-shell" if usb else "shell"
    data = (format_gpt_disk if gpt else format_disk)(f"data-{flavor}-test-{firmware}.img", force=True)
    outside = outside_zos_sha256(data) if gpt else None
    hashes = []
    for stage in ("session", "reboot"):
        log = BUILD / f"{firmware}-{flavor}-{stage}.serial.log"
        log.unlink(missing_ok=True)
        command = qemu_command(disk, firmware, exit_on_shutdown=True, data_disk=data, usb=usb) + [
            "-display", "none", "-monitor", "none", "-serial", f"file:{log}",
            "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04", "-qmp", "stdio",
        ]
        with (BUILD / f"{firmware}-{flavor}-{stage}.qemu.log").open("wb") as errors:
            process = subprocess.Popen(command, cwd=ROOT, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors)
            qmp = Qmp(process, BUILD / f"{firmware}-{flavor}-{stage}.qmp.log", timeout)
            try:
                qmp.negotiate()
                wait_serial(process, log, SHELL_READY, timeout)
                keys = ShellKeys(process, qmp, log, timeout)
                if stage == "session":
                    keys.line("mkdir a; cd a; echo hi > f; cat f; ps; free", [0] * 6)
                    keys.line("pwd", [0])
                    keys.line('echo "a; > b" >> f; cat f', [0, 0])
                    keys.line("cp f g; mv g renamed; cat renamed", [0, 0, 0])
                    keys.line("touch empty; rm empty; mkdir sub; rmdir sub", [0] * 4)
                    keys.line("ls > listing; cat listing", [0, 0])
                    keys.line("cp f f; /bin/cat f > f; cat f", [1, 1, 0])
                    keys.line('echo "with spaces" > "two words"; cat "two words"', [0, 0])
                    keys.line('echo "unterminated', [])
                    keys.line("uname; help", [0, 0])
                    # Shell 2.0 language. Output is checked through files (`cat` prints a file in
                    # one piece), and every command's exact exit status is asserted.
                    def shown(text: str) -> bool:
                        return f"SHELL OUT: {text}" in log.read_text(encoding="utf-8")
                    keys.line("FOO=bar; echo $FOO \"x $FOO\" '$FOO' \\$FOO > o; cat o", [0, 0, 0])
                    if not shown("bar x bar $FOO $FOO\n"):
                        raise RuntimeError("Variable expansion or quoting is wrong")
                    keys.line("false || echo or-ok > o; true && echo and-ok >> o; false && echo never >> o; "
                              "echo after >> o; cat o", [1, 0, 0, 0, 1, 0, 0])
                    if not shown("or-ok\nand-ok\nafter\n"):
                        raise RuntimeError("&& / || short-circuit semantics are wrong")
                    keys.line("! false; ! true; false || ! false", [0, 1, 1, 0])
                    keys.line("echo a > o # a comment ; echo hidden >> o", [0])
                    keys.line("cat o", [0])
                    if not shown("a\n") or shown("a\nhidden"):
                        raise RuntimeError("A comment was executed")
                    keys.line("echo ${FOO}${FOO} > o; cat o", [0, 0])
                    if not shown("barbar\n"):
                        raise RuntimeError("${NAME} expansion is wrong")
                    keys.line("echo a | cat", [])
                    keys.line("echo hi < f", [])
                    keys.line("echo ${NOPE", [1])
                    # Environment: only exported variables (and per-command prefixes) are inherited.
                    keys.line("export X=1 Y; printenv X > o; printenv Y >> o; printenv FOO; cat o", [0, 0, 0, 1, 0])
                    if not shown("1\n\n"):
                        raise RuntimeError("Exported variables were not inherited")
                    keys.line("Z=temp printenv Z > o; printenv Z >> o; env > e; cat o", [0, 1, 0, 0])
                    if not shown("temp\n"):
                        raise RuntimeError("A command-prefix assignment was not inherited")
                    keys.line("cat e", [0])
                    if not shown("PATH=/bin\nHOME=/\nX=1\nY=\n"):
                        raise RuntimeError("env output is wrong")
                    keys.line("unset X; printenv X", [0, 1])
                    # Filename patterns: sorted matches, quoted patterns are literal, no match keeps the text.
                    keys.line("mkdir g; cd g; touch b.txt; touch a.txt; touch c.log", [0] * 5)
                    keys.line("echo *.txt > o; echo '*.txt' >> o; echo nomatch* >> o; ls *.log >> o; "
                              "echo [ab].txt >> o; cat o", [0] * 6)
                    if not shown("a.txt b.txt\n*.txt\nnomatch*\nc.log\na.txt b.txt\n"):
                        raise RuntimeError("Filename pattern expansion is wrong")
                    keys.line("cd ..; echo g/*.log > o; cat o; cd /a", [0] * 4)
                    if not shown("g/c.log\n"):
                        raise RuntimeError("A pattern with a directory part is wrong")
                    keys.line("kill 3", [1])
                    keys.line("spin &", [0])
                    pid = re.findall(r"SHELL BACKGROUND: (\d+)", log.read_text(encoding="utf-8"))[-1]
                    keys.line("ps", [0])
                    if "SHELL OUT: spin" not in log.read_text(encoding="utf-8"):
                        raise RuntimeError("Background process was missing from ps")
                    keys.line(f"kill {pid}; wait {pid}", [0, 0])
                    keys.type("echx")
                    keys.key("backspace")
                    keys.type("o edited")
                    keys.enter([0])
                    keys.key("up")
                    keys.enter([0])
                    keys.key("l", ctrl=True)
                    keys.line("cd /; cat /a/f; ps; free", [0] * 4)
                else:
                    keys.line('cat /a/f; cat /a/renamed; cat "/a/two words"; ps; free', [0] * 5)
                    text = log.read_text(encoding="utf-8")
                    if text.count("SHELL OUT: hi\na; > b\n") != 2 or "SHELL OUT: with spaces\n" not in text:
                        raise RuntimeError("Shell file contents did not survive full QEMU exit/relaunch")
                if usb:
                    if stage == "session":
                        keys.type("echo abxc")
                        keys.key("home")
                        keys.key("end")
                        keys.key("left")
                        keys.key("left")
                        keys.key("delete")
                        keys.key("a", ctrl=True)
                        keys.key("e", ctrl=True)
                        keys.enter([0])
                        keys.type("echo ")
                        qmp.execute("send-key", {"keys": [{"type": "qcode", "data": "caps_lock"}], "hold-time": 5})
                        time.sleep(0.05)
                        keys.type("caps")
                        qmp.execute("send-key", {"keys": [{"type": "qcode", "data": "caps_lock"}], "hold-time": 5})
                        time.sleep(0.05)
                        keys.enter([0])
                        if "SHELL OUT: abc\n" not in log.read_text(encoding="utf-8") or "SHELL OUT: CAPS\n" not in log.read_text(encoding="utf-8"):
                            raise RuntimeError("USB navigation/modifier decoding failed")
                        keys.type("cp /a/f /a/inputcopy")
                        before = len(re.findall(r"SHELL STATUS:", log.read_text(encoding="utf-8")))
                        keys.key("ret", wait=False)
                        keys.lines += 1
                        keys.key("e", wait=False)
                        text = wait_serial(process, log, f"SHELL LINE DONE: {keys.lines}\n", timeout)
                        actual = re.findall(r"SHELL STATUS: (-?\d+)", text)[before:]
                        if actual != ["0"]:
                            raise RuntimeError(f"USB concurrent copy failed: {actual}")
                        wait_serial(process, log, f"SHELL KEY: {keys.keys}\n", timeout)
                        keys.type("cho queued > /a/inputproof")
                        keys.enter([0])
                        keys.line("cat /a/inputproof; cat /a/inputcopy", [0, 0])
                        text = log.read_text(encoding="utf-8")
                        if "SHELL OUT: queued\n" not in text:
                            raise RuntimeError("USB queued input was lost during storage activity")
                        wait_serial(process, log, "USB INPUT BACKPRESSURE PASS:", timeout)
                    else:
                        keys.line("cat /a/inputproof; cat /a/inputcopy", [0, 0])
                        if "SHELL OUT: queued\n" not in log.read_text(encoding="utf-8"):
                            raise RuntimeError("USB concurrent-input file did not survive restart")
                    qmp.execute("input-send-event", {"events": [
                        {"type": "rel", "data": {"axis": "x", "value": 80}},
                        {"type": "rel", "data": {"axis": "y", "value": -30}},
                        {"type": "btn", "data": {"button": "left", "down": True}},
                    ]})
                    wait_serial(process, log, "MOUSE: x=592 y=354 buttons=1", timeout)
                    qmp.execute("input-send-event", {"events": [
                        {"type": "btn", "data": {"button": "left", "down": False}},
                    ]})
                    wait_serial(process, log, "MOUSE: x=592 y=354 buttons=0", timeout)
                    text = log.read_text(encoding="utf-8")
                    proofs = ("USB COMMAND RING WRAP PASS:", "USB DMA CAPABILITY PASS:",
                              "USB BULK RING WRAP PASS:", "USB EVENT RING WRAP PASS",
                              "USB HID MOUSE PASS:")
                    if stage == "session":
                        proofs += ("USB HID RING WRAP PASS: Keyboard",)
                    for proof in proofs:
                        if proof not in text:
                            raise RuntimeError(f"Missing USB acceptance proof: {proof}")
                picture = BUILD / f"{firmware}-{flavor}-{stage}.ppm"
                qmp.execute("screendump", {"filename": str(picture)})
                keys.type("~finish")
                keys.key("ret", wait=False)
                if process.wait(timeout=timeout) != 33:
                    raise RuntimeError(f"Shell acceptance exit failed; inspect {log}")
                validate_log(log.read_text(encoding="utf-8"), firmware, "shell", usb=usb)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
                qmp.close()
                process.stdin.close()
                process.stdout.close()
        hashes.append(hashlib.sha256(data.read_bytes()).hexdigest())
        if gpt and outside_zos_sha256(data) != outside:
            raise RuntimeError(f"{flavor} wrote outside the ZOS partition during the {stage} stage")
        print(f"PASS {firmware}/{flavor}-{stage}: real keys, commands, process lifecycle and clean exit", flush=True)
    shell_power_case(disk, firmware, data, timeout, usb=usb, gpt=gpt)
    if gpt and outside_zos_sha256(data) != outside:
        raise RuntimeError(f"{flavor} wrote outside the ZOS partition during reboot/halt")
    (BUILD / f"{firmware}-{flavor}.data.json").write_text(json.dumps({
        "data_image": data.name, "session_sha256": hashes[0], "reboot_sha256": hashes[1],
        "full_qemu_exit_between_sessions": True,
        "exact_reboot_contents_verified": ["/a/f", "/a/renamed", "/a/two words"],
        "reboot_and_halt_verified": True,
    }, indent=2) + "\n", encoding="utf-8")


def usb_ps2_case(disk: Path, firmware: str, timeout: int) -> None:
    """USB storage on a GPT stick but no USB keyboard or mouse: the built-in i8042
    keyboard and mouse (how most laptops are wired) must drive the shell."""
    data = format_gpt_disk(f"data-usb-ps2-{firmware}.img", force=True)
    outside = outside_zos_sha256(data)
    log = BUILD / f"{firmware}-usb-ps2.serial.log"
    log.unlink(missing_ok=True)
    command = qemu_command(disk, firmware, exit_on_shutdown=True, data_disk=data, usb=True)
    for prefix in ("usb-kbd,", "usb-mouse,"):
        index = next(i for i, value in enumerate(command) if value.startswith(prefix))
        del command[index - 1:index + 1]
    command += ["-display", "none", "-monitor", "none", "-serial", f"file:{log}",
                "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04", "-qmp", "stdio"]
    with (BUILD / f"{firmware}-usb-ps2.qemu.log").open("wb") as errors:
        process = subprocess.Popen(command, cwd=ROOT, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors)
        qmp = Qmp(process, BUILD / f"{firmware}-usb-ps2.qmp.log", timeout)
        try:
            qmp.negotiate()
            wait_serial(process, log, SHELL_READY, timeout)
            text = log.read_text(encoding="utf-8")
            if "INTEGRATED PS2: kbd=true mouse=true" not in text:
                raise RuntimeError("Built-in PS/2 devices were not initialized in USB mode")
            if "Keyboard" in "".join(re.findall(r"USB ENUM PASS:.*", text)):
                raise RuntimeError("A USB keyboard was present; the PS/2 path was not isolated")
            keys = ShellKeys(process, qmp, log, timeout)
            keys.line("mkdir p; cd p; echo ps2 > f; cat f; pwd", [0] * 5)
            keys.line("cp f g; cat g; ls", [0, 0, 0])
            keys.line("dmesg > /k.log; ls /", [0, 0])
            keys.type("echo abxc")
            keys.key("left")
            keys.key("left")
            keys.key("delete")
            keys.enter([0])
            text = log.read_text(encoding="utf-8")
            if "SHELL OUT: ps2\n" not in text or "SHELL OUT: abc\n" not in text:
                raise RuntimeError("PS/2 keyboard input did not reach the shell")
            qmp.execute("input-send-event", {"events": [
                {"type": "rel", "data": {"axis": "x", "value": 80}},
                {"type": "rel", "data": {"axis": "y", "value": -30}},
                {"type": "btn", "data": {"button": "left", "down": True}},
            ]})
            mark = wait_serial(process, log, "buttons=1", timeout).rfind("buttons=1")
            qmp.execute("input-send-event", {"events": [
                {"type": "btn", "data": {"button": "left", "down": False}},
            ]})
            wait_serial(process, log, "buttons=0", timeout, start=mark)
            keys.type("~finish")
            keys.key("ret", wait=False)
            if process.wait(timeout=timeout) != 33:
                raise RuntimeError(f"PS/2 shell acceptance exit failed; inspect {log}")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            qmp.close()
            process.stdin.close()
            process.stdout.close()
    if outside_zos_sha256(data) != outside:
        raise RuntimeError("PS/2 shell wrote outside the ZOS partition")
    # The saved logs must be readable from the host, straight off the stick image.
    volume = load_volume(data, ZOS_FIRST)
    boot_log = volume.read(volume.lookup("/boot.log")).decode("ascii", "replace")
    for expected in ("INTEGRATED PS2: kbd=true mouse=true", "USB WINDOW: base=16384", "FS ready: checkpoint="):
        if expected not in boot_log:
            raise RuntimeError(f"/boot.log on the stick lacks {expected!r}")
    shell_log = volume.read(volume.lookup("/k.log")).decode("ascii", "replace")
    if "SHELL LINE DONE: 2" not in shell_log or "USB WINDOW: base=16384" not in shell_log:
        raise RuntimeError("dmesg > /k.log did not save the running log")
    print(f"PASS {firmware}/usb-ps2: built-in keyboard and mouse drive the shell on USB storage", flush=True)


def shell_power_case(disk: Path, firmware: str, data: Path, timeout: int, *, usb: bool = False,
                     gpt: bool = False) -> None:
    flavor = "usb-gpt-shell" if gpt else "usb-shell" if usb else "shell"
    log = BUILD / f"{firmware}-{flavor}-power.serial.log"
    log.unlink(missing_ok=True)
    command = qemu_command(disk, firmware, data_disk=data, usb=usb) + [
        "-display", "none", "-monitor", "none", "-serial", f"file:{log}", "-qmp", "stdio",
    ]
    with (BUILD / f"{firmware}-{flavor}-power.qemu.log").open("wb") as errors:
        process = subprocess.Popen(command, cwd=ROOT, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors)
        qmp = Qmp(process, BUILD / f"{firmware}-{flavor}-power.qmp.log", timeout)
        try:
            qmp.negotiate()
            wait_serial(process, log, SHELL_READY, timeout)
            keys = ShellKeys(process, qmp, log, timeout)
            keys.type("reboot")
            start = len(log.read_text(encoding="utf-8"))
            keys.key("ret", wait=False)
            wait_serial(process, log, SHELL_READY, timeout, start=start)
            text = log.read_text(encoding="utf-8")
            if text.count("ZOS REBOOT: filesystem synced") != 1 or text.count(SHELL_READY) != 2:
                raise RuntimeError("Reboot command did not restart the guest")
            keys = ShellKeys(process, qmp, log, timeout)
            keys.start = len(text)
            keys.type("halt")
            keys.key("ret", wait=False)
            wait_serial(process, log, "ZOS HALTED: filesystem synced", timeout, start=keys.start)
            time.sleep(0.05)
            registers = qmp.execute("human-monitor-command", {"command-line": "info registers"})
            if not isinstance(registers, str) or "HLT=1" not in registers or "CPL=0" not in registers:
                raise RuntimeError("Halt command did not halt the CPU in ring 0")
            qmp.execute("quit")
            if process.wait(timeout=timeout) != 0:
                raise RuntimeError("Power-control QEMU did not exit cleanly")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            qmp.close()
            process.stdin.close()
            process.stdout.close()
    print(f"PASS {firmware}/{flavor}-power: real reboot and ring-0 halted CPU, storage synchronized", flush=True)


def usb_failure_case(disk: Path, firmware: str, timeout: int, case: str) -> None:
    # A stick without any ZOS partition must be refused without a single write.
    if case == "no-zos-partition":
        data = format_gpt_disk(f"data-usb-{case}-{firmware}.img", force=True, zos=False)
    elif case == "unformatted-zos-partition":
        data = format_gpt_disk(f"data-usb-{case}-{firmware}.img", force=True, formatted=False)
    else:
        data = format_disk(f"data-usb-{case}-{firmware}.img", force=True)
    log = BUILD / f"{firmware}-usb-{case}.serial.log"
    log.unlink(missing_ok=True)
    command = qemu_command(disk, firmware, data_disk=data, usb=case != "no-controller")
    if case == "missing-disk":
        index = next(i for i, part in enumerate(command) if part.startswith("usb-storage,"))
        del command[index - 1:index + 1]
    elif case == "unsupported-hid":
        index = next(i for i, part in enumerate(command) if part.startswith("usb-mouse,"))
        command[index] = "usb-tablet,id=zos-usb-mouse,bus=xhci.0,port=2"
    elif case not in ("no-controller", "unplug-disk", "no-zos-partition", "unformatted-zos-partition"):
        raise ValueError(f"Unknown USB failure case: {case}")
    command += ["-display", "none", "-monitor", "none", "-serial", f"file:{log}", "-qmp", "stdio"]
    before = hashlib.sha256(data.read_bytes()).hexdigest()
    with (BUILD / f"{firmware}-usb-{case}.qemu.log").open("wb") as errors:
        process = subprocess.Popen(command, cwd=ROOT, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors)
        qmp = Qmp(process, BUILD / f"{firmware}-usb-{case}.qmp.log", timeout)
        try:
            qmp.negotiate()
            if case == "unplug-disk":
                wait_serial(process, log, SHELL_READY, timeout)
                before = hashlib.sha256(data.read_bytes()).hexdigest()
                qmp.execute("device_del", {"id": "zos-usb-disk"})
                expected = "USB RUNTIME FAILURE: Io"
            elif case == "unsupported-hid":
                # Generic-PC policy: an unsupported peripheral is skipped; the
                # supported keyboard and storage must still reach the shell.
                text = wait_serial(process, log, SHELL_READY, timeout)
                if "USB INITIALIZATION FAILURE" in text:
                    raise RuntimeError("Unsupported peripheral aborted USB initialization")
                if "USB ENUM PASS" not in text or "Storage" not in text or "Keyboard" not in text:
                    raise RuntimeError(f"Supported devices missing after skipping peripheral; inspect {log}")
                expected = None
            else:
                expected = "USB INITIALIZATION FAILURE: " + ("Io" if case == "no-controller" else "Unsupported")
            if expected is not None:
                deadline = time.monotonic() + timeout
                while True:
                    text = log.read_text(encoding="utf-8") if log.exists() else ""
                    if expected in text and "USER EXIT: tid=2 status=" in text:
                        break
                    if process.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError(f"Missing expected USB failure {expected!r}; inspect {log}")
                    time.sleep(0.01)
            if case not in ("unplug-disk", "unsupported-hid") and SHELL_READY in text:
                raise RuntimeError("Unsupported USB configuration incorrectly reached the shell")
            if "USER FAULT: tid=2" in text:
                raise RuntimeError("USB error handling faulted instead of exiting explicitly")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            qmp.close()
            process.stdin.close()
            process.stdout.close()
    if case == "unsupported-hid":
        print(f"PASS {firmware}/usb-{case}: unsupported peripheral skipped; keyboard and storage reach the shell", flush=True)
        return
    after = hashlib.sha256(data.read_bytes()).hexdigest()
    if before != after:
        raise RuntimeError(f"USB failure case changed its data volume: {case}")
    print(f"PASS {firmware}/usb-{case}: explicit failure, no fallback or disk writes", flush=True)


def accept_case(disk: Path, firmware: str, case: str, timeout: int) -> None:
    if case == "shell":
        shell_case(disk, firmware, timeout)
        return
    if case == "console":
        console_case(disk, firmware, timeout)
        return
    log = BUILD / f"{firmware}-{case}.serial.log"
    stderr = BUILD / f"{firmware}-{case}.qemu.log"
    log.unlink(missing_ok=True)
    stderr.unlink(missing_ok=True)
    data_disk = None
    before = None
    if case.startswith("fs-"):
        name = f"data-test-{firmware}.img"
        if case == "fs-create":
            data_disk = format_disk(name, force=True)
        else:
            data_disk = data_path(name)
            validate_disk(data_disk)
        before = hashlib.sha256(data_disk.read_bytes()).hexdigest()
    command = qemu_command(
        disk, firmware, exit_on_shutdown=True, memory_mib=1024 if case == "memory" else 256,
        data_disk=data_disk,
    ) + [
        "-display", "none", "-monitor", "none", "-serial", f"file:{log}",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
    ]
    with stderr.open("wb") as errors:
        process = subprocess.Popen(command, cwd=ROOT, stdout=errors, stderr=errors)
        try:
            code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired as error:
            process.kill()
            process.wait()
            raise RuntimeError(f"{firmware}/{case} timed out; inspect {log} and {stderr}") from error
    text = log.read_text(encoding="utf-8", errors="replace") if log.exists() else ""
    if code != 33:
        raise RuntimeError(
            f"{firmware}/{case}: expected debug-exit 33, got {code}; inspect {log} and {stderr}"
        )
    validate_log(text, firmware, case)
    if data_disk is not None:
        after = hashlib.sha256(data_disk.read_bytes()).hexdigest()
        if (case == "fs-create" and before == after) or (case == "fs-read" and before != after):
            raise RuntimeError("Filesystem persistence image did not have the expected write/read behavior")
        (BUILD / f"{firmware}-{case}.data.json").write_text(json.dumps({
            "data_image": data_disk.name, "before_sha256": before, "after_sha256": after,
            "qemu_fully_exited": True,
        }, indent=2) + "\n", encoding="utf-8")
    print(f"PASS {firmware}/{case}: serial, framebuffer initialized, timer, expected exit", flush=True)


def read_ppm(path: Path) -> tuple[int, int, bytes]:
    data = path.read_bytes()
    magic, size, depth, pixels = data.split(b"\n", 3)
    width, height = (int(value) for value in size.split())
    if magic != b"P6" or depth != b"255" or len(pixels) != width * height * 3:
        raise RuntimeError(f"{path.name} is not a valid QEMU framebuffer dump")
    return width, height, pixels


def color_box(width: int, height: int, pixels: bytes, rgb: int, tolerance: int = 2):
    """Bounding box (x0, y0, x1, y1), inclusive, of the pixels matching 0xRRGGBB, or None."""
    want = ((rgb >> 16) & 255, (rgb >> 8) & 255, rgb & 255)
    x0 = y0 = 10 ** 9
    x1 = y1 = -1
    for y in range(height):
        row = pixels[y * width * 3:(y + 1) * width * 3]
        for x in range(width):
            r, g, b = row[x * 3], row[x * 3 + 1], row[x * 3 + 2]
            if abs(r - want[0]) <= tolerance and abs(g - want[1]) <= tolerance and abs(b - want[2]) <= tolerance:
                x0, y0, x1, y1 = min(x0, x), min(y0, y), max(x1, x), max(y1, y)
    return (x0, y0, x1, y1) if x1 >= 0 else None


def count_color_in(width: int, pixels: bytes, box: tuple[int, int, int, int], rgb: int, tolerance: int = 2) -> int:
    """Pixels matching 0xRRGGBB inside the inclusive box (x0, y0, x1, y1)."""
    want = ((rgb >> 16) & 255, (rgb >> 8) & 255, rgb & 255)
    x0, y0, x1, y1 = box
    found = 0
    for y in range(y0, y1 + 1):
        for x in range(x0, x1 + 1):
            i = (y * width + x) * 3
            found += all(abs(pixels[i + k] - want[k]) <= tolerance for k in range(3))
    return found


def count_color(pixels: bytes, rgb: int, tolerance: int = 2) -> int:
    want = bytes(((rgb >> 16) & 255, (rgb >> 8) & 255, rgb & 255))
    return sum(1 for i in range(0, len(pixels), 3)
               if all(abs(pixels[i + k] - want[k]) <= tolerance for k in range(3)))


# Window-manager colors (servers/wm/src/wm.rs).
WM_DESKTOP, WM_FRAME, WM_TITLE_ACTIVE, WM_HELP_BACKGROUND, WM_TERMINAL_TEXT = 0x1c3f6e, 0xc8c8c8, 0x0a5c36, 0xf4f4e8, 0x90ee90


def gui_case(disk: Path, firmware: str, timeout: int, *, usb: bool = False) -> None:
    """Boots the desktop image and drives it with real keyboard and mouse input,
    checking the window manager's log and the actual framebuffer pixels. With usb, the
    keyboard, mouse and the data disk are USB devices instead of PS/2 and ATA."""
    flavor = "usb-gui" if usb else "gui"
    data = format_disk(f"data-{flavor}-{firmware}.img", force=True)
    log = BUILD / f"{firmware}-{flavor}.serial.log"
    log.unlink(missing_ok=True)
    command = qemu_command(disk, firmware, exit_on_shutdown=True, data_disk=data, usb=usb) + [
        "-display", "none", "-monitor", "none", "-serial", f"file:{log}",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04", "-qmp", "stdio"]
    shots = []
    with (BUILD / f"{firmware}-{flavor}.qemu.log").open("wb") as errors:
        process = subprocess.Popen(command, cwd=ROOT, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors)
        qmp = Qmp(process, BUILD / f"{firmware}-{flavor}.qmp.log", timeout)
        try:
            qmp.negotiate()
            wait_serial(process, log, SHELL_READY, timeout)
            text = wait_serial(process, log, "WM ready:", timeout)

            def shot(name: str):
                path = BUILD / f"{firmware}-{flavor}-{name}.ppm"
                qmp.execute("screendump", {"filename": str(path)}, identity=f"shot-{name}")
                shots.append(path)
                return read_ppm(path)

            def alt(key: str, *, wait_for: str) -> None:
                mark = len(log.read_text(encoding="utf-8", errors="replace"))
                qmp.execute("send-key", {"keys": [{"type": "qcode", "data": "alt"},
                                                  {"type": "qcode", "data": key}], "hold-time": 5})
                wait_serial(process, log, wait_for, timeout, start=mark)

            def pointer_to(x: int, y: int) -> tuple[int, int]:
                """Moves the pointer to a screen position in small steps; returns the final position."""
                current = re.findall(r"MOUSE: x=(\d+) y=(\d+)", log.read_text(encoding="utf-8", errors="replace"))
                px, py = (int(v) for v in current[-1]) if current else (512, 384)
                while (px, py) != (x, y):
                    dx, dy = max(-100, min(100, x - px)), max(-100, min(100, y - py))
                    mark = len(log.read_text(encoding="utf-8", errors="replace"))
                    qmp.execute("input-send-event", {"events": [
                        {"type": "rel", "data": {"axis": "x", "value": dx}},
                        {"type": "rel", "data": {"axis": "y", "value": dy}}]})
                    wait_serial(process, log, "MOUSE:", timeout, start=mark)
                    px, py = px + dx, py + dy
                return px, py

            def button(down: bool) -> None:
                mark = len(log.read_text(encoding="utf-8", errors="replace"))
                qmp.execute("input-send-event", {"events": [{"type": "btn", "data": {"button": "left", "down": down}}]})
                wait_serial(process, log, "MOUSE:", timeout, start=mark)

            def windows() -> dict[int, tuple[str, int, int, int, int, int]]:
                """The latest 'WM WIN' line per window slot: state, x, y, w, h, focus."""
                latest = {}
                for slot, state, x, y, w, h, focus in re.findall(
                        r"WM WIN (\d+) (\w+) (-?\d+),(-?\d+) (\d+)x(\d+) focus=(\d)", log.read_text(encoding="utf-8", errors="replace")):
                    latest[int(slot)] = (state, int(x), int(y), int(w), int(h), int(focus))
                return latest

            # 1. The desktop: terminal window with the shell banner, taskbar, background.
            width, height, pixels = shot("desktop")
            if (width, height) != (1024, 768):
                raise RuntimeError(f"Unexpected desktop mode {width}x{height}")
            box = color_box(width, height, pixels, WM_TITLE_ACTIVE)
            if box is None or box[0] != 43 or box[1] != 39 or box[3] != 58 or not 680 <= box[2] <= 683:
                raise RuntimeError(f"Terminal title bar is not where expected: {box}")
            for (x, y), rgb, what in (((900, 300), WM_DESKTOP, "desktop"), ((1000, 760), WM_FRAME, "taskbar")):
                got = tuple(pixels[(y * width + x) * 3:(y * width + x) * 3 + 3])
                want = ((rgb >> 16) & 255, (rgb >> 8) & 255, rgb & 255)
                if any(abs(a - b) > 2 for a, b in zip(got, want)):
                    raise RuntimeError(f"The {what} color at ({x},{y}) is {got}, expected {want}")
            if count_color(pixels, WM_TERMINAL_TEXT) < 200:
                raise RuntimeError("No shell text is visible in the terminal window")

            # 2. The shell works inside the window.
            keys = ShellKeys(process, qmp, log, timeout)
            keys.line("echo gui > o; cat o", [0, 0])
            if "SHELL OUT: gui\n" not in log.read_text(encoding="utf-8", errors="replace"):
                raise RuntimeError("A shell command typed into the terminal window did not run")

            # 3. Alt+H opens the help window, centered near the top.
            alt("h", wait_for="WM WIN 1 Normal")
            width, height, pixels = shot("help")
            box = color_box(width, height, pixels, WM_HELP_BACKGROUND)
            if box != (295, 83, 728, 356):
                raise RuntimeError(f"Help window content is not where expected: {box}")
            if windows()[1][5] != 1 or windows()[0][5] != 0:
                raise RuntimeError(f"Focus did not move to the new window: {windows()}")
            # The new window has its own task button, with its title drawn in black.
            second_button = (212, 743, 359, 764)
            if count_color_in(width, pixels, second_button, 0x000000) < 20:
                raise RuntimeError("The help window has no task button in the taskbar")

            # 4. Drag the help window by its title bar with the mouse.
            pointer_to(400, 72)
            button(True)
            pointer_to(500, 132)
            button(False)
            moved = windows()[1]
            if moved[1:3] != (292 + 100, 60 + 60):
                raise RuntimeError(f"Dragging moved the help window to {moved[1:3]}, expected (392, 120)")
            width, height, pixels = shot("dragged")
            box = color_box(width, height, pixels, WM_HELP_BACKGROUND)
            if box != (395, 143, 828, 416):
                raise RuntimeError(f"Help window pixels did not follow the drag: {box}")

            # 5. Click-to-focus the terminal, then Alt+Tab back to help.
            pointer_to(150, 300)
            button(True)
            button(False)
            if windows()[0][5] != 1 or windows()[1][5] != 0:
                raise RuntimeError(f"Clicking the terminal did not focus it: {windows()}")
            alt("tab", wait_for="WM WIN")
            if windows()[1][5] != 1:
                raise RuntimeError(f"Alt+Tab did not move the focus to the help window: {windows()}")

            # 6. Minimize help with Alt+M; it disappears and its task button restores it.
            alt("m", wait_for="WM WIN 1 Minimized")
            width, height, pixels = shot("minimized")
            if color_box(width, height, pixels, WM_HELP_BACKGROUND) is not None:
                raise RuntimeError("A minimized window is still drawn")
            if count_color_in(width, pixels, second_button, 0x909090) < 10:
                raise RuntimeError("The minimized window button is not greyed out")
            pointer_to(230, 752)
            button(True)
            button(False)
            if windows()[1][0] != "Normal" or windows()[1][5] != 1:
                raise RuntimeError(f"The task button did not restore the window: {windows()}")

            # 7. Alt+Tab to the terminal and maximize it: its title bar spans the screen.
            alt("tab", wait_for="WM WIN")
            alt("x", wait_for="WM WIN 0 Maximized")
            width, height, pixels = shot("maximized")
            box = color_box(width, height, pixels, WM_TITLE_ACTIVE)
            if box is None or box[0] != 3 or box[1] != 3 or box[2] < 1018:
                raise RuntimeError(f"A maximized window does not fill the work area: {box}")
            alt("x", wait_for="WM WIN 0 Normal")

            # 8. The terminal still takes keystrokes after all that.
            keys.line("cat o", [0])
            keys.type("~finish")
            keys.key("ret", wait=False)
            if process.wait(timeout=timeout) != 33:
                raise RuntimeError(f"Desktop acceptance exit failed; inspect {log}")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            qmp.close()
            process.stdin.close()
            process.stdout.close()
    if any(marker in text for marker in ("SERVICE FAILURE", "KERNEL PANIC", "EXCEPTION:")):
        raise RuntimeError("A service failed while the desktop was running")
    print(f"PASS {firmware}/{flavor}: desktop, terminal, help window, mouse drag, focus, minimize/restore, maximize", flush=True)


def splash_case(disk: Path, firmware: str, timeout: int) -> None:
    """Boots the normal build, freezes the VM as soon as the console is up (before the
    shell banner can replace the logo) and inspects the real framebuffer pixels."""
    data = format_disk(f"data-splash-{firmware}.img", force=True)
    log = BUILD / f"{firmware}-splash.serial.log"
    picture = BUILD / f"{firmware}-splash.ppm"
    log.unlink(missing_ok=True)
    picture.unlink(missing_ok=True)
    command = qemu_command(disk, firmware, data_disk=data) + [
        "-display", "none", "-monitor", "none", "-serial", f"file:{log}", "-qmp", "stdio"]
    with (BUILD / f"{firmware}-splash.qemu.log").open("wb") as errors:
        process = subprocess.Popen(command, cwd=ROOT, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors)
        qmp = Qmp(process, BUILD / f"{firmware}-splash.qmp.log", timeout)
        try:
            qmp.negotiate()
            wait_serial(process, log, "CONSOLE PASS: cursor", timeout)
            qmp.execute("stop", identity="stop")
            if SHELL_READY in log.read_text(encoding="utf-8", errors="replace"):
                raise RuntimeError("Splash capture was too late: the shell banner was already up")
            qmp.execute("screendump", {"filename": str(picture)}, identity="capture")
            qmp.execute("quit", identity="quit")
            process.wait(timeout=timeout)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            qmp.close()
            process.stdin.close()
            process.stdout.close()
    width, height, pixels = read_ppm(picture)
    at = lambda x, y: tuple(pixels[(y * width + x) * 3:(y * width + x) * 3 + 3])
    boot_blue = (0x01, 0x1d, 0x94)
    for x, y in ((4, 4), (width - 5, height - 5), (4, height - 5), (width - 5, 4)):
        if any(abs(a - b) > 4 for a, b in zip(at(x, y), boot_blue)):
            raise RuntimeError(f"Splash background at ({x},{y}) is {at(x, y)}, expected {boot_blue}")
    white = green = 0
    for index in range(0, len(pixels), 3):
        r, g, b = pixels[index], pixels[index + 1], pixels[index + 2]
        white += r > 200 and g > 200 and b > 200
        green += g > 200 and r < 190 and b < 190  # the kernel/console text color
    if white < 20000:
        raise RuntimeError(f"The logo is missing from the boot screen ({white} white pixels)")
    if green:
        raise RuntimeError(f"Kernel or console text leaked onto the boot screen ({green} green pixels)")
    print(f"PASS {firmware}/splash: logo on the real framebuffer ({width}x{height}, {white} logo pixels, no text)", flush=True)


def screenshot(disk: Path, firmware: str, timeout: int, *, data_disk: Path | None = None, usb: bool = False) -> None:
    flavor = "usb-screen" if usb else "screen"
    log = BUILD / f"{firmware}-{flavor}.serial.log"
    stderr = BUILD / f"{firmware}-{flavor}.qemu.log"
    picture = BUILD / f"{firmware}-{flavor}.ppm"
    log.unlink(missing_ok=True)
    stderr.unlink(missing_ok=True)
    picture.unlink(missing_ok=True)
    command = qemu_command(disk, firmware, data_disk=data_disk or ensure_disk("data-usb.img" if usb else "data.img"), usb=usb) + [
        "-display", "none", "-monitor", "none", "-serial", f"file:{log}",
        "-qmp", "stdio",
    ]
    with stderr.open("wb") as errors:
        process = subprocess.Popen(
            command, cwd=ROOT, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors
        )
        qmp = Qmp(process, BUILD / f"{firmware}-{flavor}.qmp.log", timeout)
        try:
            deadline = time.monotonic() + timeout
            while True:
                text = log.read_text(encoding="utf-8", errors="replace") if log.exists() else ""
                if SHELL_READY in text:
                    break
                if "KERNEL PANIC" in text or "EXCEPTION:" in text or "SERVICE FAILURE" in text or process.poll() is not None:
                    raise RuntimeError(f"{firmware}: normal kernel failed; inspect {log}")
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"{firmware}: framebuffer capture timed out; inspect {log}")
                time.sleep(0.1)
            for marker in (*COMMON_MARKERS, f"FIRMWARE: {firmware.upper()}"):
                if marker not in text:
                    raise RuntimeError(f"{firmware}: screenshot boot log missing {marker}")
            validate_ipc(text)
            if usb:
                validate_usb(text)
            else:
                validate_storage(text)
                for marker in CONSOLE_MARKERS:
                    if marker not in text:
                        raise RuntimeError(f"{firmware}: normal console missing {marker}")
            qmp.negotiate()
            qmp.execute("screendump", {"filename": str(picture)}, identity="capture")
            qmp.execute("quit", identity="quit")
            process.wait(timeout=timeout)
            pixels = picture.read_bytes()
            if not pixels.startswith(b"P6\n") or len(pixels) < 1024:
                raise RuntimeError("Invalid QEMU framebuffer dump")
            if process.returncode != 0:
                raise RuntimeError("Screenshot QEMU did not shut down cleanly")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            qmp.close()
            if process.stdin is not None:
                process.stdin.close()
            if process.stdout is not None:
                process.stdout.close()
    print(f"CAPTURE {firmware}: {picture} (inspect text visually)", flush=True)


def check() -> None:
    subprocess.run([sys.executable, "-m", "unittest", "discover", "-s", str(ROOT / "tools"),
                    "-p", "test_*.py", "-v"], cwd=ROOT, check=True)
    cargo("fmt", "--all", "--", "--check")
    cargo("test", "--locked", "--package", "zos-console", "--lib",
          "--package", "zos-storage", "--package", "zos-session", "--package", "zos-wm",
          "--target", "x86_64-pc-windows-gnu")
    cargo("clippy", "--locked", "--release", "--package", "zos-demo",
          "--package", "zos-console", "--package", "zos-storage", "--package", "zos-session",
          "--package", "zos-wm", "--", "-D", "warnings", user=True)
    for case in [None, *CASES]:
        args = ["clippy", "--locked", "--release", "--package", "zos-kernel", "--no-default-features"]
        if case:
            args += ["--features", CASES[case][0]]
        cargo(*args, "--", "-D", "warnings")
    for features in ("usb", "usb,test-shell", "usb-diagnostic"):
        cargo("clippy", "--locked", "--release", "--package", "zos-kernel",
              "--no-default-features", "--features", features, "--", "-D", "warnings")


def source_hashes() -> dict[str, str]:
    files = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard"], cwd=ROOT, text=True,
    ).splitlines()
    return {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest() for name in files}


def accept(timeout: int, *, usb: bool = False, diagnostic: bool = False) -> None:
    usb = usb or diagnostic
    phase = 9 if diagnostic else 8 if usb else 7
    result = BUILD / f"phase-{phase}-acceptance.json"
    BUILD.mkdir(exist_ok=True)
    result.unlink(missing_ok=True)
    tested_sources = source_hashes()
    user_volumes = {name: hashlib.sha256((BUILD / name).read_bytes()).hexdigest()
                    for name in ("data.img", "data-usb.img") if (BUILD / name).exists()}
    check()
    for case in CASES:
        disk = image(build(case), case)
        for firmware in ("bios", "uefi"):
            accept_case(disk, firmware, case, timeout)
    normal = image(build())
    data = format_disk("data-normal-acceptance.img", force=True)
    for firmware in ("bios", "uefi"):
        screenshot(normal, firmware, timeout, data_disk=data)
        splash_case(normal, firmware, timeout)
    usb_cases = []
    # The desktop: window manager in place of the text console, driven by real input.
    gui_disk = image(build("shell"), "gui")
    for firmware in ("bios", "uefi"):
        gui_case(gui_disk, firmware, timeout)
        usb_cases.append(f"{firmware}/gui")
    if usb:
        usb_gui_disk = image(build("shell", usb=True), "usb-gui")
        for firmware in ("bios", "uefi"):
            gui_case(usb_gui_disk, firmware, timeout, usb=True)
            usb_cases.append(f"{firmware}/usb-gui")
        disk = image(build("shell", usb=True), "usb-shell")
        for firmware in ("bios", "uefi"):
            shell_case(disk, firmware, timeout, usb=True)
            usb_cases.extend(f"{firmware}/usb-shell-{stage}" for stage in ("session", "reboot", "power"))
            shell_case(disk, firmware, timeout, usb=True, gpt=True)
            usb_cases.extend(f"{firmware}/usb-gpt-shell-{stage}" for stage in ("session", "reboot", "power"))
            usb_ps2_case(disk, firmware, timeout)
            usb_cases.append(f"{firmware}/usb-ps2")
            for case in ("no-controller", "missing-disk", "unsupported-hid", "unplug-disk", "no-zos-partition",
                         "unformatted-zos-partition"):
                usb_failure_case(disk, firmware, timeout, case)
                usb_cases.append(f"{firmware}/usb-{case}")
        normal_usb = image(build(usb=True), "usb")
        data_usb = format_disk("data-usb-normal-acceptance.img", force=True)
        for firmware in ("bios", "uefi"):
            screenshot(normal_usb, firmware, timeout, data_disk=data_usb, usb=True)
            usb_cases.append(f"{firmware}/usb-normal")
    if diagnostic:
        disk = image(build(diagnostic=True), "diagnostic")
        for firmware in ("bios", "uefi"):
            for case in ("normal", "read-only-media", "no-storage", "empty", "unsupported", "no-controller",
                         "usb2", "superspeed", "nec", "hub", "integrated", "no-ps2"):
                diagnostic_case(disk, firmware, timeout, case)
                usb_cases.append(f"{firmware}/diagnostic-{case}")
    if any(hashlib.sha256((BUILD / name).read_bytes()).hexdigest() != digest
           for name, digest in user_volumes.items()):
        raise RuntimeError("Acceptance modified an existing user data volume")
    if source_hashes() != tested_sources:
        raise RuntimeError("Source files changed during acceptance; rerun against the final revision")
    result.write_text(json.dumps({
        "phase": phase,
        "automated_checks": "passed",
        "visual_review": "pending: inspect normal, console and shell captures in BIOS and UEFI",
        "git_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "source_sha256": tested_sources,
        "preserved_user_volume_sha256": user_volumes,
        "physical_hardware": "not tested",
        "cases": [f"{firmware}/{case}" for case in CASES for firmware in ("bios", "uefi")] + usb_cases,
    }, indent=2) + "\n", encoding="utf-8")
    print(f"AUTOMATED STEP {phase} PASS. Inspect normal, console and shell screenshots.", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description="ZOS - Zombie OS development tools")
    parser.add_argument("command", choices=["fmt", "build", "image", "run", "debug", "check", "test", "mkfs"])
    parser.add_argument("--firmware", choices=["bios", "uefi"], default="uefi")
    parser.add_argument("--timeout", type=int, default=90)
    parser.add_argument("--data-image", help="Dedicated volume name inside build (default: data.img or data-usb.img)")
    parser.add_argument("--usb", action="store_true", help="QEMU xHCI USB keyboard/mouse and data disk")
    parser.add_argument("--diagnostic", action="store_true", help="Read-only Intel/QEMU USB diagnostics; no filesystem or shell")
    parser.add_argument("--force", action="store_true", help="Explicitly overwrite the selected data volume (mkfs only)")
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    if args.force and args.command != "mkfs":
        parser.error("--force is only valid with mkfs")
    if args.diagnostic and (args.usb or args.data_image is not None or args.command == "mkfs"):
        parser.error("--diagnostic cannot be combined with --usb, --data-image or mkfs")
    args.data_image = args.data_image or ("data-usb.img" if args.usb else "data.img")
    if args.command == "mkfs":
        print(f"Formatted: {format_disk(args.data_image, force=args.force)}")
    elif args.command == "fmt":
        cargo("fmt", "--all")
    elif args.command == "check":
        check()
    elif args.command == "test":
        accept(args.timeout, usb=args.usb, diagnostic=args.diagnostic)
    else:
        kernel = build(usb=args.usb, diagnostic=args.diagnostic)
        if args.command != "build":
            disk = image(kernel, "diagnostic" if args.diagnostic else "usb" if args.usb else None)
            if args.command in ("run", "debug"):
                command = (diagnostic_command(disk, args.firmware) if args.diagnostic else
                    qemu_command(disk, args.firmware, data_disk=ensure_disk(args.data_image),
                                 usb=args.usb)) + ["-serial", "stdio"]
                if args.command == "debug":
                    command += ["-s", "-S", "-d", "int,cpu_reset",
                                "-D", str(BUILD / "qemu-debug.log")]
                subprocess.run(command, cwd=ROOT, check=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"ZOS ERROR: {error}", file=sys.stderr)
        sys.exit(1)
