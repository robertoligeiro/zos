# ZOS user-space library

`zos-libos` is a `no_std` workspace crate containing the shared syscall IDs,
errors, bounded-message limit, raw SYSCALL ABI and safe wrappers for
ChannelCreate, Send, Receive, Reply, ThreadCreate, MapMemory, IrqAttach,
IoPortGrant and Exit. DebugWrite provides bounded serial diagnostics.
FramebufferMap returns metadata for a capability-checked boot framebuffer
mapping, only to the initial console process. ConsoleTestFinish is a private
acceptance gate restricted to the TTY in dedicated test images; production
images deny it.

Send keeps its message/reply buffers borrowed until the blocking call returns.
Receive returns a server-owned reply token and message length; token zero is an
IRQ notification carrying an eight-byte little-endian IRQ number and must not
be replied to. Multiple IRQs may share a channel, coalesced separately by IRQ.
The device owner must drain its device after a notification.
Messages/replies are capped at
64 bytes. Errors are explicit, including Closed, TooLarge, Denied and Deadlock.

See the root README for ABI registers, resource policy and checkpoint limits.
