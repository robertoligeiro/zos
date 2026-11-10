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
  restrictions, acceptance-log validation, test VM exit behavior, sequential
  QMP capture negotiation, memory-test RAM sizing, and memory acceptance gates.
  All 12 host tests, Rustfmt, Clippy in all eight configurations, and all 14
  BIOS/UEFI acceptance cases pass. Both normal framebuffer captures have been
  visually inspected at the memory checkpoint.
- Step 3: bitmap physical-frame allocation from Limine's usable regions;
  ZOS-owned page tables; 4 KiB map/unmap/protect; per-process lower halves
  sharing supervisor-only kernel mappings; an 8 MiB coalescing kernel heap
  supporting `Box`/`Vec`; and usable/free/used memory accounting.
- Steps 4-8: deferred. There are no user-space servers, shell, filesystem,
  persistence, or hardware USB support.

The `phase-2` tag preserves the verified boot checkpoint. The `phase-3` memory
checkpoint corresponds to **Step 3 / Phase 2** in the supplied plan; repository
tags follow step numbers. Threads, user-mode execution and IPC are the next step,
not part of this checkpoint.

The frame bitmap and copied page tables are reserved from usable RAM. Bootloader
reclaimable memory stays reserved: boot responses and the initial kernel stack
still live there. The kernel owns its active page-table hierarchy but preserves
Limine's kernel/HHDM/device mappings. Each process owns its root, lower-half page
tables and zero-initialized data frames; destroying an inactive address space
releases them, never the shared kernel half. All memory-manager and heap mutations
run with interrupts disabled under their respective locks on this single CPU.
Memory statistics count initially usable RAM, including ZOS allocations, not
firmware-reserved RAM or the whole installed-memory capacity. The heap is fixed
at 8 MiB for this phase.

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
The kernel pins `spin` version `0.9.9` and uses `linked_list_allocator` `0.10.5`
without its optional locking dependency. `Cargo.lock` records the
verified transitive dependency versions; builds and Clippy use `--locked`.

The image builder uses Python's standard library instead of WSL's `gdisk`,
`mtools`, and `xorriso`. It produces a 128 MiB GPT image with a dedicated BIOS
stage-2 partition and a FAT32 EFI System Partition, then invokes the pinned
`limine.exe bios-install` on **that image file only**. Native Windows commands
below are the primary interface; the Makefile is optional if Windows Make is
already available. `tools/mkimage.sh` is a convenience wrapper for Git Bash.

## Step 3 acceptance gate

`python tools\zos.py test` must exit successfully. It performs:

1. Host unit tests, Rustfmt check, and Clippy with warnings denied for normal,
   boot, divide, page-fault, memory, read-only-write, unmap, and NX-execute test
   configurations (eight configurations total).
2. BIOS and UEFI boots for each of seven isolated test images. Each must
   report the correct firmware, framebuffer initialization, GDT/TSS/IDT, and
   at least 100 timer interrupts; then exit through QEMU's test device with
   host status 33. A panic, unexpected exit, missing marker, or timeout fails.
   Every image checks `Box`/`Vec`, alignment, zeroed user pages, real CR3 switches
   with isolated contents at the same virtual address, shared kernel mappings
   added after process creation, protection flags and leak-free teardown.
   The memory image uses **1 GiB RAM** to hold 100,000 distinct pages at once,
   verify their contents, free them and reject double frees. It also exhausts
   the allocator, checks partially constructed mappings roll back on failure,
   recovers, and verifies heap fragmentation/coalescing and oversized-allocation
   failure without leaks. Three dedicated fault images check the CPU's exact
   page-fault address and error bits for a read-only write, a read after unmap
   (including TLB invalidation), and an instruction fetch from an NX page.
3. Normal BIOS and UEFI boots, each with a framebuffer screenshot. These do
   not enable the test exit device. The harness stops each VM through QMP over
   standard input/output, without opening a network listener.

Outputs are ignored under `build`:

```text
bios-boot.serial.log             uefi-boot.serial.log
bios-divide.serial.log           uefi-divide.serial.log
bios-page-fault.serial.log       uefi-page-fault.serial.log
bios-memory.serial.log          uefi-memory.serial.log
bios-memory-protection.serial.log uefi-memory-protection.serial.log
bios-memory-unmap.serial.log     uefi-memory-unmap.serial.log
bios-memory-execute.serial.log   uefi-memory-execute.serial.log
bios-screen.ppm                 uefi-screen.ppm
phase-3-acceptance.json          (created only after automated success)
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
PAGING ready: ZOS-owned root, shared supervisor kernel half, WP/NX
HEAP ready: 8388608 bytes
HEAP PASS: Box, Vec growth, alignment, deallocation
PAGING PASS: map/unmap/protect, CR3 isolation, shared kernel, teardown
MEMORY ready: usable=... free=... used=... heap_used=0 bytes
PIT ready: 100 Hz
TIMER PASS: ticks=100
ZOS ready: memory checkpoint; no user-space shell yet.
```

UEFI reports `FIRMWARE: UEFI`; resolution and tick count can differ.
The divide test must report `EXCEPTION: DIVIDE ERROR` and
`DIVIDE ACCEPTANCE PASS`. The page-fault test must report its bad address
(`0x400000000000`) and `PAGE FAULT ACCEPTANCE PASS`.

The memory stress case additionally reports `FRAMES PASS: 100000 simultaneous
pages`, `FRAMES OOM PASS`, `HEAP STRESS PASS` and `MEMORY ACCEPTANCE PASS`.
The hardware-fault cases report `MEMORY PROTECTION ACCEPTANCE PASS`,
`MEMORY UNMAP ACCEPTANCE PASS` and `MEMORY EXECUTE ACCEPTANCE PASS`, respectively.
Step 3's automated and visual acceptance gates have passed. Step 4 remains
deferred until this checkpoint is reviewed.

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

QEMU runs a single CPU with 256 MiB RAM using software emulation, except for the
1 GiB physical-memory stress test. UEFI uses the
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
