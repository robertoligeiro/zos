# ZOS user-space servers

`demo` contains three `no_std` ring-3 ELFs: echo-server, echo-client and a
disposable fault probe. The host runner builds them with a lower-half linker
script and packages them as Limine modules. The kernel loader gives each
process a separate address space; ThreadCreate shares its parent's space.

The server starts at priority 0 and client at 1 so the demonstration's known
channel IDs are established before lower-priority programs run. A worker at 20
inherits its blocked client's priority, ahead of a competitor at 10. Two peers
at 10 must preempt each other without syscalls while retaining XMM0 state.
Five fault probes run before the echo completes. The full sequence and expected
proof markers are validated under both BIOS and UEFI.

`console` contains three additional `no_std` ring-3 ELFs:

- `console-server`: framebuffer text rendering, cursor, scroll, RGB colors,
  backspace and mouse crosshair; owns the only user framebuffer mapping.
- `input-server`: bounded PS/2 controller initialization, set-1 keyboard
  decoding and standard three-byte mouse packets; owns ports 0x60/0x64 and
  IRQ1/IRQ12. It forwards decoded keys to TTY and mouse motion to console.
- `tty-server`: 120-character ASCII insertion/editing, eight-entry history with
  draft restoration, and input echo; no command parsing or execution.

Boot runs and tears down the regression demo first, then starts console/input
and either the normal shell or regression TTY (priorities 0/1/2). Their
fixed boot channels 1/2/3 are established by that deterministic order before
input delivery is enabled by the TTY handshake; there is no registry yet.
Synchronous IPC and priority inheritance apply to both key and rendering paths.
Boot kernel framebuffer output is disabled at handoff; COM1 stays enabled.

Pure decoding/editor/rendering logic has host Rust tests. QMP-driven
BIOS/UEFI tests inject actual PS/2 keyboard/mouse events, check edited lines and
pointer coordinates, capture the framebuffer, and verify driver/process cleanup.
The `~finish` line has special meaning only in the dedicated test image;
normal boots do not interpret it as a test control.

`storage` adds four ring-3 ELFs after the console services:

- `blkdev-server`: priority 3/channel 4, exact secondary ATA port grants,
  bounded polling, one-sector LBA28 read/write RPCs and cache flush.
- `fs-simple-server`: priority 4/channel 5, checksummed geometry, inodes,
  bitmap, hierarchy and copy-on-write checkpoints. Accepts only VFS requests.
- `vfs-server`: priority 5/channel 6, root mount table, normalized absolute
  paths, owned generation handles, bounded file operations and explicit errors.
- `storage-probe`: priority 6, exercises persistence through public VFS IPC.
  Normal boots preserve/read welcome.txt and exit; isolated creation/read tests
  validate CRUD, ownership, backend/device isolation and exact reboot contents.

The FS server is the block driver's only accepted caller. Those identities come
from kernel-issued reply tokens; fixed boot channel/thread IDs are an explicit
checkpoint limitation; a dynamic registry remains deferred. Storage and
session/console processes have 1024-byte copied IPC; the demo remains at 64 bytes.
The console test still starts only its original three services.

Rust host storage tests cover crash/torn-write recovery, preserved backup
data, corruption, limits, hierarchy, VFS descriptor ownership and protocol bounds.
Real BIOS/UEFI tests format disposable volumes, write, stop QEMU completely and
read from the same image in a new VM. The normal data volume remains persistent.

`session` adds `procmgr-server`, `shell-server` and the compact `zos-command`
ELF. The manager starts at priority 7/TID 8, creates channel 7, seeds missing
`/bin` files and authenticates shell (TID 3) and child startup requests. Its IRQ0
notifications drive exit/fault polling, VFS descriptor cleanup and safe reaping.
The normal shell creates channel 3 before storage startup, then waits for the
manager handshake before enabling input. Fixed TID/channel distinctions matter:
VFS owner-cleanup trusts manager **TID 8**, not its channel 7.

The shell has a bounded pure parser for quotes/escapes/`;`/`>`/`>>`/`&`, builtins
for filesystem/process/memory/control operations, and VFS-path executable loading
for external commands. Each single-thread child receives a checked invocation
and opens its own redirection FD. Generational PIDs, reply tokens, channels and
FDs prevent stale operations after slot reuse. System services cannot be killed.
At most 24 jobs retain status until wait. Existing `/bin` executables are
preserved, not automatically replaced by newer boot modules.

BIOS/UEFI shell acceptance checks repeated spawn/wait, fault and kill status,
blocked IPC/channel reuse, leaked-descriptor cleanup, copied executable launch,
real keyboard commands, exact contents across full QEMU exit/relaunch, guest
reboot and CPU halt. `~finish` is privileged only in dedicated test images.

With the kernel's `usb` feature, the same input process (TID/channel 2) owns
QEMU xHCI MMIO and bounded DMA pages, enumerates direct-root HID boot devices
and a bulk-only/SCSI disk, and polls event rings on IRQ0. Its block RPCs accept
only block-service TID 4. The existing block service proxies USB sector/flush
requests instead of using ATA; FS/VFS and all fixed service identities remain
unchanged. No extra ELF or driver thread is introduced.

Input uses `SendReady` and a bounded event queue to avoid waiting on a busy
shell while that shell's filesystem call needs the same USB transport.
The timer may notify both input and manager. USB images grant no PS/2 or ATA
ports; normal images retain the legacy drivers. QEMU USB acceptance covers
real keys/mouse, concurrent copying/input, ring wraps, full-restart persistence,
power controls, explicit unsupported/disconnect errors and complete teardown.
Physical hardware, hotplug recovery, hubs and UAS remain unvalidated/unsupported;
the driver is trusted for DMA because there is no IOMMU.

The `usb-diagnostic` kernel feature instead starts **only console and input**.
Its separate boot configuration packages those two ELFs and skips the regression
demo, filesystem and shell. The input driver reports controller/port/device
state to the framebuffer as well as serial, supports bounded scratchpad setup
and reuses disabled-slot DMA allocations while skipping unsupported devices.
Its transport-level read-only allowlist and unconditional block-RPC rejection
apply even when no storage device was enumerated. The normal USB/shell identity
and writable 16 MiB volume contracts are unchanged. Intel `8086:4ded` is gated
to this diagnostic kernel, and physical-controller validation is pending.
