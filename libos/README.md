# ZOS user-space library

`zos-libos` is a `no_std` workspace crate containing the shared syscall IDs,
errors, bounded-message limit, raw SYSCALL ABI and safe wrappers for
ChannelCreate, Send, Receive, Reply, ThreadCreate, MapMemory, IrqAttach,
IoPortGrant and Exit. DebugWrite provides bounded serial diagnostics.
FramebufferMap returns metadata for a capability-checked boot framebuffer
mapping, only to the initial console process. ConsoleTestFinish is a private
acceptance gate restricted to the TTY in dedicated test images; production
images deny it. StorageTestFinish is similarly restricted to the storage probe
in the two filesystem acceptance kernels; production images deny it.

Send keeps its message/reply buffers borrowed until the blocking call returns.
Receive returns a server-owned reply token and message length; token zero is an
IRQ notification carrying an eight-byte little-endian IRQ number and must not
be replied to. Multiple IRQs may share a channel, coalesced separately by IRQ.
The device owner must drain its device after a notification.
Messages/replies retain the 64-byte default cap. Storage, session and console
processes receive the 1024-byte copied-message capability; the original IPC demo
remains at 64 bytes. Errors are explicit, including Closed, TooLarge, Denied and
Deadlock. Secondary ATA port-range/control grants are restricted to the block
driver; no primary disk ports are granted.

ProcessSpawn/Info/Kill/Reap, MemoryStats, BootProgram and SystemControl are
restricted to the initial process manager. PIDs encode slot generations;
reaped process slots and closed channels can be reused without reviving stale
identifiers. ProcessInfo has a checked 50-byte wire representation and reports
state, system status, resident bytes and exit status. Spawned programs are
single-threaded and lack lifecycle/hardware capabilities. SystemControl sync is
performed by the manager through VFS before invoking reboot/halt.

`reply_live` explicitly logs a canceled outstanding reply when a client exits;
other IPC errors propagate. The manager closes authenticated client-owned VFS
descriptors before reaping a process, preserving FD generations on slot reuse.

The `zos-storage` library supplies the VFS protocol/client wrappers on top of
libos IPC. Read/write RPCs transfer at most 1000 bytes, return actual counts and
use generation-checked, calling-thread-owned descriptors. See the root README
for filesystem format, persistence, path semantics and bounded resources.

See the root README for ABI registers, resource policy and checkpoint limits.
