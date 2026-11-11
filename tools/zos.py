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
from mkfs import data_path, ensure_disk, format_disk, validate_disk

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
          "--package", "zos-console", "--package", "zos-storage", "--package", "zos-session", user=True)
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
    payloads = {
        "limine.conf": (ROOT / "boot" / ("limine-diagnostic.conf" if case == "diagnostic" else "limine.conf")).read_bytes(),
        "limine-bios.sys": (limine / "limine-bios.sys").read_bytes(),
        "EFI/BOOT/BOOTX64.EFI": (limine / "BOOTX64.EFI").read_bytes(),
        "boot/zos.elf": kernel.read_bytes(),
    }
    for program in (("console-server", "input-server") if case == "diagnostic" else USER_PROGRAMS):
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
    return destination


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
    if case in ("normal", "read-only-media"):
        if case == "normal":
            index = command.index("-drive")
            del command[index:index + 2]
        # Bundled EDK2's USB mass-storage driver faults on a write-protected medium.
        protection = "snapshot=on" if case == "normal" and firmware == "uefi" else "readonly=on"
        command += ["-drive", f"file={disk},format=raw,if=none,id=diagnostic-boot,{protection}"]
        if case == "normal":
            command += ["-device", "usb-storage,id=zos-usb-disk,bus=xhci.0,port=3,drive=diagnostic-boot,bootindex=1"]
    if case == "empty":
        for prefix in ("usb-kbd,", "usb-mouse,"):
            index = next(i for i, value in enumerate(command) if value.startswith(prefix))
            del command[index - 1:index + 1]
    if case == "unsupported":
        command += ["-device", "usb-tablet,id=unsupported,bus=xhci.0,port=4"]
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
                for marker in ("DIAGNOSTIC READ ONLY PASS:", "XHCI running:"):
                    if marker not in text:
                        raise RuntimeError(f"Missing diagnostic proof {marker}; inspect {log}")
                if case != "empty":
                    qmp.execute("send-key", {"keys": [{"type": "qcode", "data": "a"}], "hold-time": 20})
                    wait_serial(process, log, "DIAGNOSTIC KEY: Character(97)", timeout)
                    qmp.execute("input-send-event", {"events": [
                        {"type": "rel", "data": {"axis": "x", "value": 20}},
                        {"type": "rel", "data": {"axis": "y", "value": -10}}]})
                    wait_serial(process, log, "MOUSE: x=532 y=374", timeout)
                if case in ("normal", "read-only-media"):
                    text = wait_serial(process, log, "USB READ PASS:", timeout)
                    if "USB DISK: sectors=262144 block=512" not in text:
                        raise RuntimeError("Diagnostics did not read the non-16MiB USB boot disk")
                    if case == "normal":
                        qmp.execute("device_del", {"id": "zos-usb-disk"})
                        wait_serial(process, log, "DIAGNOSTIC OFFLINE: Io", timeout)
                if case == "unsupported":
                    wait_serial(process, log, "unsupported device", timeout)
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
            else "read-only" if case in ("normal", "read-only-media") else "none",
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
                 "-": "minus", "\\": "backslash", "=": "equal"}
        shifted = {'"': "apostrophe", ">": "dot", "&": "7", "~": "grave_accent",
                   "_": "minus", "!": "1", ":": "semicolon"}
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


def shell_case(disk: Path, firmware: str, timeout: int, *, usb: bool = False) -> None:
    flavor = "usb-shell" if usb else "shell"
    data = format_disk(f"data-{flavor}-test-{firmware}.img", force=True)
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
        print(f"PASS {firmware}/{flavor}-{stage}: real keys, commands, process lifecycle and clean exit", flush=True)
    shell_power_case(disk, firmware, data, timeout, usb=usb)
    (BUILD / f"{firmware}-{flavor}.data.json").write_text(json.dumps({
        "data_image": data.name, "session_sha256": hashes[0], "reboot_sha256": hashes[1],
        "full_qemu_exit_between_sessions": True,
        "exact_reboot_contents_verified": ["/a/f", "/a/renamed", "/a/two words"],
        "reboot_and_halt_verified": True,
    }, indent=2) + "\n", encoding="utf-8")


def shell_power_case(disk: Path, firmware: str, data: Path, timeout: int, *, usb: bool = False) -> None:
    flavor = "usb-shell" if usb else "shell"
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
    elif case not in ("no-controller", "unplug-disk"):
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
            else:
                expected = "USB INITIALIZATION FAILURE: " + ("Io" if case == "no-controller" else "Unsupported")
            deadline = time.monotonic() + timeout
            while True:
                text = log.read_text(encoding="utf-8") if log.exists() else ""
                if expected in text and "USER EXIT: tid=2 status=" in text:
                    break
                if process.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError(f"Missing expected USB failure {expected!r}; inspect {log}")
                time.sleep(0.01)
            if case != "unplug-disk" and SHELL_READY in text:
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
          "--package", "zos-storage", "--package", "zos-session",
          "--target", "x86_64-pc-windows-gnu")
    cargo("clippy", "--locked", "--release", "--package", "zos-demo",
          "--package", "zos-console", "--package", "zos-storage", "--package", "zos-session",
          "--", "-D", "warnings", user=True)
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
    usb_cases = []
    if usb:
        disk = image(build("shell", usb=True), "usb-shell")
        for firmware in ("bios", "uefi"):
            shell_case(disk, firmware, timeout, usb=True)
            usb_cases.extend(f"{firmware}/usb-shell-{stage}" for stage in ("session", "reboot", "power"))
            for case in ("no-controller", "missing-disk", "unsupported-hid", "unplug-disk"):
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
            for case in ("normal", "read-only-media", "no-storage", "empty", "unsupported", "no-controller"):
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
