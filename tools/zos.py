"""Native Windows ZOS build, image creation, QEMU, and phase acceptance."""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

from mkimage import BUILD, ROOT, create_image, image_path

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
USER_PROGRAMS = ("echo-server", "echo-client", "fault-probe")
READY_MARKER = "ZOS ready: IPC checkpoint"


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


def build(case: str | None = None) -> Path:
    cargo("build", "--locked", "--release", "--package", "zos-demo", user=True)
    args = ["build", "--locked", "--release", "--package", "zos-kernel", "--no-default-features"]
    if case:
        args += ["--features", CASES[case][0]]
    cargo(*args)
    BUILD.mkdir(exist_ok=True)
    destination = BUILD / f"zos-{case or 'normal'}.elf"
    shutil.copyfile(ROOT / "target" / "x86_64-unknown-none" / "release" / "zos-kernel", destination)
    return destination


def image(kernel: Path, case: str | None = None) -> Path:
    limine = TOOLS / "limine"
    destination = image_path(f"zos-{case}.img" if case else "zos.img")
    payloads = {
        "limine.conf": (ROOT / "boot" / "limine.conf").read_bytes(),
        "limine-bios.sys": (limine / "limine-bios.sys").read_bytes(),
        "EFI/BOOT/BOOTX64.EFI": (limine / "BOOTX64.EFI").read_bytes(),
        "boot/zos.elf": kernel.read_bytes(),
    }
    for program in USER_PROGRAMS:
        payloads[f"boot/{program}.elf"] = (
            ROOT / "target" / "user" / "x86_64-unknown-none" / "release" / program
        ).read_bytes()
    create_image(destination, payloads)
    subprocess.run(
        [str(limine / "limine.exe"), "bios-install", str(destination), "1"],
        cwd=ROOT, check=True,
    )
    print(f"Image: {destination}", flush=True)
    return destination


def qemu_command(disk: Path, firmware: str, *, exit_on_shutdown: bool = False,
                 memory_mib: int = 256) -> list[str]:
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
        "-drive", f"file={disk},format=raw,if=ide,snapshot=on",
        "-no-reboot",
    ]
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


def validate_log(text: str, firmware: str, case: str) -> None:
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
    if case in ("boot", "memory", "ipc") and "EXCEPTION:" in text:
        raise RuntimeError("Boot image raised an unexpected exception")
    if case == "ipc":
        validate_ipc(text)


def validate_ipc(text: str) -> None:
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


def accept_case(disk: Path, firmware: str, case: str, timeout: int) -> None:
    log = BUILD / f"{firmware}-{case}.serial.log"
    stderr = BUILD / f"{firmware}-{case}.qemu.log"
    log.unlink(missing_ok=True)
    stderr.unlink(missing_ok=True)
    command = qemu_command(
        disk, firmware, exit_on_shutdown=True, memory_mib=1024 if case == "memory" else 256,
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
    print(f"PASS {firmware}/{case}: serial, framebuffer initialized, timer, expected exit", flush=True)


def screenshot(disk: Path, firmware: str, timeout: int) -> None:
    log = BUILD / f"{firmware}-screen.serial.log"
    stderr = BUILD / f"{firmware}-screen.qemu.log"
    picture = BUILD / f"{firmware}-screen.ppm"
    log.unlink(missing_ok=True)
    stderr.unlink(missing_ok=True)
    picture.unlink(missing_ok=True)
    command = qemu_command(disk, firmware) + [
        "-display", "none", "-monitor", "none", "-serial", f"file:{log}",
        "-qmp", "stdio",
    ]
    with stderr.open("wb") as errors:
        process = subprocess.Popen(
            command, cwd=ROOT, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors
        )
        reader = ThreadPoolExecutor(max_workers=1)
        try:
            deadline = time.monotonic() + timeout
            while True:
                text = log.read_text(encoding="utf-8", errors="replace") if log.exists() else ""
                if READY_MARKER in text:
                    break
                if "KERNEL PANIC" in text or "EXCEPTION:" in text or process.poll() is not None:
                    raise RuntimeError(f"{firmware}: normal kernel failed; inspect {log}")
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"{firmware}: framebuffer capture timed out; inspect {log}")
                time.sleep(0.1)
            for marker in (*COMMON_MARKERS, f"FIRMWARE: {firmware.upper()}"):
                if marker not in text:
                    raise RuntimeError(f"{firmware}: screenshot boot log missing {marker}")
            validate_ipc(text)
            requests = [
                {"execute": "qmp_capabilities", "id": "capabilities"},
                {"execute": "screendump", "arguments": {"filename": str(picture)}, "id": "capture"},
                {"execute": "quit", "id": "quit"},
            ]
            with (BUILD / f"{firmware}-screen.qmp.log").open("wb") as protocol_log:
                def receive() -> dict:
                    try:
                        line = reader.submit(process.stdout.readline).result(timeout=timeout)
                    except TimeoutError as error:
                        raise RuntimeError("QMP response timed out") from error
                    if not line:
                        raise RuntimeError("QMP closed before capture completed")
                    protocol_log.write(line)
                    protocol_log.flush()
                    return json.loads(line)

                if "QMP" not in receive():
                    raise RuntimeError("Missing QMP greeting")
                for request in requests:
                    # Complete capability negotiation before submitting later commands.
                    process.stdin.write(json.dumps(request).encode() + b"\n")
                    process.stdin.flush()
                    while True:
                        response = receive()
                        if "error" in response:
                            raise RuntimeError(f"QMP capture failed: {response['error']}")
                        if response.get("id") == request["id"] and "return" in response:
                            break
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
            reader.shutdown(wait=True)
            if process.stdin is not None:
                process.stdin.close()
            if process.stdout is not None:
                process.stdout.close()
    print(f"CAPTURE {firmware}: {picture} (inspect text visually)", flush=True)


def check() -> None:
    subprocess.run([sys.executable, "-m", "unittest", "discover", "-s", str(ROOT / "tools"),
                    "-p", "test_*.py", "-v"], cwd=ROOT, check=True)
    cargo("fmt", "--all", "--", "--check")
    cargo("clippy", "--locked", "--release", "--package", "zos-demo",
          "--", "-D", "warnings", user=True)
    for case in [None, *CASES]:
        args = ["clippy", "--locked", "--release", "--package", "zos-kernel", "--no-default-features"]
        if case:
            args += ["--features", CASES[case][0]]
        cargo(*args, "--", "-D", "warnings")


def accept(timeout: int) -> None:
    result = BUILD / "phase-4-acceptance.json"
    BUILD.mkdir(exist_ok=True)
    result.unlink(missing_ok=True)
    check()
    for case in CASES:
        disk = image(build(case), case)
        for firmware in ("bios", "uefi"):
            accept_case(disk, firmware, case, timeout)
    normal = image(build())
    for firmware in ("bios", "uefi"):
        screenshot(normal, firmware, timeout)
    result.write_text(json.dumps({
        "phase": 4,
        "automated_checks": "passed",
        "visual_review": "pending: inspect bios-screen.ppm and uefi-screen.ppm",
        "git_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "cases": [f"{firmware}/{case}" for case in CASES for firmware in ("bios", "uefi")],
    }, indent=2) + "\n", encoding="utf-8")
    print("AUTOMATED STEP 4 PASS. Inspect both screenshots before starting Step 5.", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description="ZOS - Zombie OS development tools")
    parser.add_argument("command", choices=["fmt", "build", "image", "run", "debug", "check", "test"])
    parser.add_argument("--firmware", choices=["bios", "uefi"], default="uefi")
    parser.add_argument("--timeout", type=int, default=90)
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    if args.command == "fmt":
        cargo("fmt", "--all")
    elif args.command == "check":
        check()
    elif args.command == "test":
        accept(args.timeout)
    else:
        kernel = build()
        if args.command != "build":
            disk = image(kernel)
            if args.command in ("run", "debug"):
                command = qemu_command(disk, args.firmware) + ["-serial", "stdio"]
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
