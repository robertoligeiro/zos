# ZOS user-space library

`zos-libos` is a `no_std` workspace crate containing the shared syscall IDs,
errors, bounded-message limit, raw SYSCALL ABI and safe wrappers for
ChannelCreate, Send, Receive, Reply, ThreadCreate, MapMemory, IrqAttach,
IoPortGrant and Exit. DebugWrite is temporary diagnostic output, not a console.

Send keeps its message/reply buffers borrowed until the blocking call returns.
Receive returns a server-owned reply token and message length; token zero is an
IRQ notification and must not be replied to. Messages/replies are capped at
64 bytes. Errors are explicit, including Closed, TooLarge, Denied and Deadlock.

See the root README for ABI registers, resource policy and checkpoint limits.
