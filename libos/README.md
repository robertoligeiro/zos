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
Messages/replies retain the 64-byte default cap. Only initial storage processes
receive the 1024-byte copied-message capability; existing console/IPC limits and
tests are unchanged. Errors are explicit, including Closed, TooLarge, Denied and
Deadlock. Secondary ATA port-range/control grants are restricted to the block
driver; no primary disk ports are granted.

The `zos-storage` library supplies the VFS protocol/client wrappers on top of
libos IPC. Read/write RPCs transfer at most 1000 bytes, return actual counts and
use generation-checked, calling-thread-owned descriptors. See the root README
for filesystem format, persistence, path semantics and bounded resources.

See the root README for ABI registers, resource policy and checkpoint limits.
