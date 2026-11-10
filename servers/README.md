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
- `tty-server`: 48-character ASCII insertion/editing, eight-entry history with
  draft restoration, and input echo; no command parsing or execution.

Normal boot runs and tears down the regression demo first, then starts these
three persistent services in priority order (console 0, input 1, TTY 2). Their
fixed boot channels 1/2/3 are established by that deterministic order before
input delivery is enabled by the TTY handshake; there is no registry yet.
Synchronous IPC and priority inheritance apply to both key and rendering paths.
Boot kernel framebuffer output is disabled at handoff; COM1 stays enabled.

Pure decoding/editor/rendering logic has five host Rust tests. QMP-driven
BIOS/UEFI tests inject actual PS/2 keyboard/mouse events, check edited lines and
pointer coordinates, capture the framebuffer, and verify driver/process cleanup.
The `~finish` line has special meaning only in the dedicated test image;
normal boots just echo it like any other input.

`procmgr`, `vfs`, `fs-simple`, `blkdev` and `shell` remain future work.
