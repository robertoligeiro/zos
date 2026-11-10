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
  QMP capture negotiation, memory-test RAM sizing, user ELF packaging, separate
  user linker flags, and memory/IPC/console acceptance gates. Rust host tests
  exercise keyboard modifiers/releases, mouse packet decoding, editor/history
  boundaries, scrolling, colors, cursor pixels and pointer restoration.
  The Step 5 gate contains 17 Python tests, five Rust tests, Rustfmt, Clippy in
  eleven configurations, and 18 BIOS/UEFI acceptance cases.
- Step 3: bitmap physical-frame allocation from Limine's usable regions;
  ZOS-owned page tables; 4 KiB map/unmap/protect; per-process lower halves
  sharing supervisor-only kernel mappings; an 8 MiB coalescing kernel heap
  supporting `Box`/`Vec`; and usable/free/used memory accounting.
- Step 4: ring-3 ELF processes, per-thread kernel stacks, preemptive priority
  scheduling with round-robin peers, synchronous IPC with transitive priority
  inheritance, checked syscalls, `libos`, and an echo server/client demonstration.
- Step 5: separate ring-3 framebuffer console, PS/2 keyboard/mouse input and
  line-editing TTY servers. Normal boots present an interactive **input demo**.
- Steps 6-8: deferred. There is no command shell, filesystem, filesystem
  persistence, or hardware USB support.

The `phase-2` tag preserves the verified boot checkpoint. The `phase-3` memory
checkpoint preserves **Step 3 / Phase 2** in the supplied plan. The
`phase-4` tag corresponds to **Step 4 / Phase 3**; `phase-5` corresponds to
**Step 5 / Phase 4**. Repository tags follow step numbers. The command shell
is Step 7, after disk/filesystem work in Step 6.

The frame bitmap and copied page tables are reserved from usable RAM. Bootloader
reclaimable memory stays reserved: boot responses and the initial kernel stack
still live there. The kernel owns its active page-table hierarchy but preserves
Limine's kernel/HHDM/device mappings. Each process owns its root, lower-half page
tables and zero-initialized data frames; destroying an inactive address space
releases them, never the shared kernel half or borrowed device frames. The console
alone can map the boot framebuffer as writable, user-accessible, NX device pages;
page-table ownership bits prevent returning device memory to the RAM allocator.
All memory-manager and heap mutations
run with interrupts disabled under their respective locks on this single CPU.
Memory statistics count initially usable RAM, including ZOS allocations, not
firmware-reserved RAM or the whole installed-memory capacity. The heap is fixed
at 8 MiB for this phase.

User processes enter initially through IRETQ, issue SYSCALL, and return from
syscalls through SYSRETQ. Timer switches restore full general-register and
x87/SSE state; AVX/XSAVE execution is deliberately disabled until its state is
supported. Each thread has a 32 KiB kernel stack, a 64 KiB user stack separated
from other stacks by an unmapped guard gap, and a priority from 0 (highest) to 31.
Kernel dispatch runs with interrupts disabled and never suspends a kernel call
stack. An independent idle stack permits safe final address-space/stack cleanup.
User faults terminate only the offending thread; other threads continue.

The syscall ABI uses RAX for the call/result and RDI/RSI/RDX/R10/R8 for arguments;
Receive also returns message length in RDX. The nine planned syscalls are
implemented, plus bounded serial `DebugWrite`, capability-checked `FramebufferMap`,
and a test-only `ConsoleTestFinish`. Small messages and replies are copied, at most **64 bytes**;
oversized buffers return an error, never truncated data. Large shared-message
mapping is deferred, as allowed by the plan's start-with-copy decision. Send
blocks until a server-owned, generation-checked Reply or an explicit channel
closure error. Dependency cycles are rejected; donation propagates transitively
while queued and while awaiting a reply. All user buffers are checked page by
page and copied through kernel physical aliases, including cross-page buffers.

The regression demo's echo-server alone can attach **IRQ0 and port 0x80**.
After it finishes and releases its resources, the input server alone can attach
**IRQ1/IRQ12 and ports 0x60/0x64**. IrqAttach delivers coalesced notifications
containing the actual IRQ number through an owned channel, reserving reply token
0 for notifications. The input driver drains controller data in user space;
the kernel only forwards IRQs and handles PIC masks/cascade/EOI. IoPortGrant
enables exact approved ports in the requesting thread's TSS I/O bitmap; IOPL
remains zero and all other ports stay denied. MapMemory supplies individual
zeroed pages in a restricted user range,
rejects writable-executable mappings and overlaps, and caps dynamic allocations
at 128 pages per process. This boot demonstration allows 32 thread IDs,
16 channel IDs and 8 stacks per process; IDs/stacks are not recycled until final
demo teardown. It is not yet a general-purpose process manager.

Six freestanding user ELFs are packaged as Limine modules. The loader checks
ELF64/x86-64 headers, file bounds/overflow, page-separated load segments,
executable entry points, overlap, image limits and W^X. User builds use their
own lower-half linker script and `target\user` artifacts. Their code model
matches the prebuilt freestanding Rust core for LTO compatibility, with all ELF
symbols below 2 GiB. Step 5 reuses the already pinned bitmap-font dependency.

The console owns the framebuffer after kernel boot output hands it off; serial
diagnostics remain enabled. It renders an 8x16 cursor, real scrolling, RGB colors,
backspace, and a mouse crosshair that turns red while a button is pressed.
The current grid supports 64-128 columns and 2-48 rows; the boot image requests
1024x768 RGB32. The input driver selects native scan-code set 1 with controller
translation disabled and a standard three-byte PS/2 mouse. Initialization and
controller drains are bounded and fail explicitly. Keyboard support is US ASCII
with Shift/Caps Lock, extended arrows/Home/End/Delete, and Ctrl+A/E/L.
The TTY supports insertion, Backspace/Delete, cursor movement, and eight history
entries with draft restoration. Lines are limited to **48 ASCII characters**;
extra characters report `TTY LIMIT` on serial without corrupting the editor.
Enter echoes `You typed: ...`; it does not parse or execute commands.
Click inside QEMU to focus keyboard/mouse input; Ctrl+Alt releases mouse capture.

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

## Step 5 acceptance gate

`python tools\zos.py test` must exit successfully. It performs:

1. Host unit tests, Rustfmt check, and Clippy with warnings denied for normal,
   boot, divide, page-fault, memory, read-only-write, unmap, and NX-execute test
   configurations plus IPC and console (ten kernel configurations), and both user
   packages (eleven configurations total). Five Rust host tests cover driver,
   editor and renderer behavior alongside the 17 Python harness/image tests.
2. BIOS and UEFI boots for each of nine isolated test images. Each must
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
   The IPC image runs real ring-3 code in seven address spaces: an echo server,
   client, and five disposable fault probes. It checks cross-process messages,
   blocked request/reply, priority donation ahead of a medium-priority competitor,
   syscall-free peer loops requiring timer preemption, SSE state preservation,
   IRQ notification, allowed/denied I/O, invalid pointers and priorities,
   cross-page copies, small-buffer errors, stale/forged reply tokens, deadlock
   rejection, and closure while queued or awaiting a reply. Supervisor writes,
   CLI, UD2, denied OUT and a noncanonical syscall stack must be contained;
   the echo must complete after these faults. All six surviving threads must
   exit successfully. Final frame/heap totals must return exactly to baseline.
3. The console image additionally receives actual QMP keyboard and mouse events:
   Backspace, arrows, insertion, Home/End/Delete, history recall, Shift/Caps Lock,
   mouse movement and button press/release. Expected text and exact pointer
   coordinates must match. It captures the edited screen, then terminates all
   three services and verifies exact frame/heap recovery, including borrowed
   framebuffer mappings. Its finish syscall is denied in production images.
4. Normal BIOS and UEFI boots, each with a framebuffer screenshot. These do
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
bios-ipc.serial.log              uefi-ipc.serial.log
bios-console.serial.log          uefi-console.serial.log
bios-screen.ppm                 uefi-screen.ppm
bios-console.ppm                uefi-console.ppm
phase-5-acceptance.json          (created only after automated success)
zos.img                         (normal image, never a deliberate-fault image)
```

Inspect all four screenshots to confirm a readable ZOS banner, input-demo
instructions, prompt/cursor, correctly edited/echoed text and mouse crosshair.
Earlier IPC/isolation diagnostics remain in the serial log.
The JSON explicitly records visual review as pending. Alternatively,
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
RING3 ready: SYSCALL/SYSRET, per-thread kernel stacks, FXSAVE
ELF VALIDATION PASS: ...
USER FAULT: ...                  (five intentional, contained probes)
client: cross-process echo and priority inheritance verified
SCHEDULER PASS: ...
IPC PASS: ...
IRQ/IO PASS: ...
USER ISOLATION PASS: 5 faults contained; echo completed afterwards
PROCESS CLEANUP PASS: all address spaces, threads, channels released
CONSOLE PASS: cursor, colors, backspace, scrolling
PS2 ready: set-1 keyboard IRQ1, 3-byte mouse IRQ12
CAPABILITIES PASS: framebuffer, PS/2 ports and IRQs isolated
ZOS ready: console checkpoint; input demo, not a shell.
```

UEFI reports `FIRMWARE: UEFI`; resolution and tick count can differ.
The divide test must report `EXCEPTION: DIVIDE ERROR` and
`DIVIDE ACCEPTANCE PASS`. The page-fault test must report its bad address
(`0x400000000000`) and `PAGE FAULT ACCEPTANCE PASS`.

The memory stress case additionally reports `FRAMES PASS: 100000 simultaneous
pages`, `FRAMES OOM PASS`, `HEAP STRESS PASS` and `MEMORY ACCEPTANCE PASS`.
The hardware-fault cases report `MEMORY PROTECTION ACCEPTANCE PASS`,
`MEMORY UNMAP ACCEPTANCE PASS` and `MEMORY EXECUTE ACCEPTANCE PASS`, respectively.
The IPC image additionally reports `IPC ACCEPTANCE PASS`. The console image
reports `TTY PASS`, `CONSOLE CLEANUP PASS` and `CONSOLE ACCEPTANCE PASS`.
Normal boots run the IPC regression demo, then keep the console/input/TTY
services running at an editable input prompt. Review this checkpoint before
implementing Step 6; the Step 7 command shell is not present yet.

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
