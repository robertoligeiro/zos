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

`procmgr`, `vfs`, `fs-simple`, `blkdev`, `console`, `input` and `shell` remain
future work. There is no interactive terminal or shell in Step 4.
