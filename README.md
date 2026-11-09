# ZOS - Zombie OS

A single-core x86-64 microkernel project following the supplied
`MICROKERNEL_OS_PLAN.md`. Rust, Limine, BIOS + UEFI, and serial-first debugging.

## Current checkpoint

- Step 1: workspace and local Git repository created; tagged `phase-1`.
- Step 0: portable Rust, MinGW, QEMU, and Limine installed and validated.
- Step 2: compiled and boot-tested in **both BIOS and UEFI**. Includes COM1,
  framebuffer text using an embedded bitmap font expanded to 8x16 pixels,
  GDT/kernel-user segments/TSS, exception IDT, and a 100 Hz PIT with PIC forwarding.
- Host tests: GPT CRCs, primary/backup structures, FAT32 geometry/mirrors,
  bootloader long filenames, file cluster chains, EFI paths, output-path
  restrictions, acceptance-log validation, test VM exit behavior, and sequential
  QMP capture negotiation. Rustfmt and Clippy pass in all four configurations.
  Boot, divide-by-zero, and page-fault acceptance tests pass in both firmware
  modes; both normal framebuffer captures have been visually inspected.
- Step 3: **not started**. Memory remains bootloader-owned. There is no frame
  allocator, owned page-table API, per-process address space, or kernel heap yet.
- Steps 4-8: deferred. There are no user-space servers, shell, filesystem,
  persistence, or hardware USB support.

The `phase-2` tag records the verified boot checkpoint. The page-fault test is an early
exception-handler regression, not as evidence of Step 3 memory management.

## Windows setup (no WSL or administrator installation)

Prerequisites: Windows x86-64, Python 3.10+ and Git on PATH, internet access,
and sandbox permission to execute this project's tools and downloaded binaries.
All downloaded dependencies stay in ignored `.tools`; the scripts do not
change the system PATH, install drivers, or run installers.

After updating sandbox policy to permit `tools\bootstrap.ps1`, run from this
repository in PowerShell:

```powershell
.\tools\bootstrap.ps1
python tools\zos.py fmt
python tools\zos.py test
```

Do not bypass the sandbox with elevation or alternate script execution.
If another project path is blocked, allow that path in sandbox policy first.
The formatting command is useful after editing Rust source; the committed
source already passes the formatting gate.

The setup uses a project-local GNU-hosted Rust toolchain plus portable MinGW
for host-side procedural macros, avoiding a Visual Studio C++ installation.
Pins: Rust `nightly-2025-03-01`; Limine binary commit
`ee5d29cd0a8034612dcd1df3f00052480db785c5` on `v9.x-binary`;
Limine Rust bindings `0.6.5`, explicitly requesting protocol base revision 3;
QEMU Windows build `20260811`; MinGW `16.2.0-rt_v14-rev1`; 7-Zip `26.06`.
The kernel pins the non-yanked `spin` version `0.9.9`. `Cargo.lock` records the
verified transitive dependency versions; builds and Clippy use `--locked`.

The image builder uses Python's standard library instead of WSL's `gdisk`,
`mtools`, and `xorriso`. It produces a 128 MiB GPT image with a dedicated BIOS
stage-2 partition and a FAT32 EFI System Partition, then invokes the pinned
`limine.exe bios-install` on **that image file only**. Native Windows commands
below are the primary interface; the Makefile is optional if Windows Make is
already available. `tools/mkimage.sh` is a convenience wrapper for Git Bash.

## Step 2 acceptance gate

`python tools\zos.py test` must exit successfully. It performs:

1. Host unit tests, Rustfmt check, and Clippy with warnings denied for normal,
   boot-test, divide-test, and page-fault-test configurations.
2. BIOS and UEFI boots for each of the three isolated test images. Each must
   report the correct firmware, framebuffer initialization, GDT/TSS/IDT, and
   at least 100 timer interrupts; then exit through QEMU's test device with
   host status 33. A panic, unexpected exit, missing marker, or timeout fails.
3. Normal BIOS and UEFI boots, each with a framebuffer screenshot. These do
   not enable the test exit device. The harness stops each VM through QMP over
   standard input/output, without opening a network listener.

Outputs are ignored under `build`:

```text
bios-boot.serial.log             uefi-boot.serial.log
bios-divide.serial.log           uefi-divide.serial.log
bios-page-fault.serial.log       uefi-page-fault.serial.log
bios-screen.ppm                 uefi-screen.ppm
phase-2-acceptance.json          (created only after automated success)
zos.img                         (normal image, never a deliberate-fault image)
```

Inspect both screenshots to confirm the ZOS banner, readable text, and timer
line. The JSON explicitly records visual review as pending. Alternatively,
verify the screen interactively, closing each QEMU window before the next:

```powershell
python tools\zos.py run --firmware bios
python tools\zos.py run --firmware uefi
```

Expected normal serial/screen lines include:

```text
ZOS - Zombie OS
ZOS booting
FRAMEBUFFER ready: 1024x768, embedded 8x16 font
FIRMWARE: BIOS
GDT/TSS/IDT ready
PIT ready: 100 Hz
TIMER PASS: ticks=100
ZOS ready: boot checkpoint; no user-space shell yet.
```

UEFI reports `FIRMWARE: UEFI`; resolution and tick count can differ.
The divide test must report `EXCEPTION: DIVIDE ERROR` and
`DIVIDE ACCEPTANCE PASS`. The page-fault test must report its bad address
(`0x400000000000`) and `PAGE FAULT ACCEPTANCE PASS`.

This checkpoint has passed automated acceptance and visual confirmation.
Step 3 remains deferred until the boot checkpoint is reviewed. Its next gate is frame allocation/freeing
across 100,000 pages (use enough emulated RAM), map/unmap/protect and address-space
isolation checks, a kernel `Vec` allocation, and readable page-fault diagnostics.

## Development commands

```powershell
python tools\zos.py build
python tools\zos.py image
python tools\zos.py check
python tools\zos.py debug --firmware bios
python -m unittest discover -s tools -p "test_*.py" -v
```

Debug starts QEMU paused with its GDB stub on port 1234; use a compatible GDB
with `target remote :1234`. Interrupt/reset diagnostics go to
`build\qemu-debug.log`; serial remains enabled.

QEMU runs a single CPU with 256 MiB RAM using software emulation. UEFI uses the
QEMU package's EDK2 code and a fresh per-image copy of its variable store.
Disk access is restricted to generated image files; QEMU uses snapshot mode.
No tools here enumerate, format, or write a physical drive.

## Repository and sharing

This is a local repository on `main`; no GitHub remote is configured and
nothing has been published. Tool downloads, generated disk images, logs,
screenshots, and Cargo target output are ignored.

`build\zos-source.bundle`, when supplied with this checkpoint, contains the
committed source and local tags, not dependencies or runtime validation.
It can be shared and cloned without GitHub:

```powershell
git clone .\zos-source.bundle zos
```

Before creating a later source bundle:

```powershell
git bundle create build\zos-source.bundle --all
git bundle verify build\zos-source.bundle
```
