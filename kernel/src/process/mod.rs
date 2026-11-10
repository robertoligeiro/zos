pub mod arch;
mod elf;

use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec, vec::Vec};
use arch::{Context, Frame, FxState};
use spin::Mutex;
use x86_64::instructions::{interrupts as cpu_interrupts, port::Port};
use zos_libos::{Call, Error, MAX_MESSAGE};

use crate::memory::{self, AddressSpace, Permissions, PAGE_SIZE};

const MAX_THREADS: usize = 32;
const MAX_CHANNELS: usize = 16;
const STACK_PAGES: u64 = 16;
static SCHEDULER: Mutex<Option<Scheduler>> = Mutex::new(None);

struct Process {
    space: AddressSpace,
    stacks: u64,
    dynamic_pages: usize,
    driver: bool,
}

impl Process {
    fn stack(&mut self) -> Result<u64, Error> {
        if self.stacks == 8 {
            return Err(Error::Limit);
        }
        let top = 0x0000_7fff_ffff_f000 - self.stacks * 0x20000;
        let bottom = top - STACK_PAGES * PAGE_SIZE;
        for page in (bottom..top).step_by(PAGE_SIZE as usize) {
            if self.space.map(page, Permissions::DATA).is_err() {
                for allocated in (bottom..page).step_by(PAGE_SIZE as usize) {
                    self.space.unmap(allocated).unwrap();
                }
                return Err(Error::NoMemory);
            }
        }
        self.stacks += 1;
        Ok(top - 8)
    }
}

#[derive(Clone, Copy)]
enum State {
    Ready,
    Receiving {
        channel: usize,
        buffer: u64,
        capacity: usize,
    },
    Sending {
        channel: usize,
        message: [u8; MAX_MESSAGE],
        length: usize,
        reply: u64,
        capacity: usize,
        token: u64,
    },
    Reply {
        owner: usize,
        buffer: u64,
        capacity: usize,
        token: u64,
    },
    Exited(i64),
    Faulted(&'static str),
}

struct Thread {
    process: Arc<Mutex<Process>>,
    context: Context,
    stack: Box<[u128]>,
    base: u8,
    effective: u8,
    state: State,
    port: bool,
    expected_fault: Option<&'static str>,
}

impl Thread {
    fn new(
        process: Arc<Mutex<Process>>,
        entry: u64,
        argument: u64,
        priority: u8,
    ) -> Result<Self, Error> {
        let stack = process.lock().stack()?;
        Ok(Self {
            process,
            context: Context::new(entry, stack, argument),
            stack: vec![0_u128; 2048].into_boxed_slice(),
            base: priority,
            effective: priority,
            state: State::Ready,
            port: false,
            expected_fault: None,
        })
    }

    fn result(&mut self, result: Result<usize, Error>, length: usize) {
        self.context.frame.rax = match result {
            Ok(value) => value as u64,
            Err(error) => (-(error as i64)) as u64,
        };
        self.context.frame.rdx = length as u64;
        self.state = State::Ready;
    }
}

struct Channel {
    owner: usize,
    pending: VecDeque<usize>,
    closed: bool,
    irq_pending: bool,
}

struct Scheduler {
    threads: Vec<Thread>,
    channels: Vec<Channel>,
    current: Option<usize>,
    last: [usize; 32],
    sequence: u64,
    irq_channel: Option<usize>,
    replies: usize,
    cross_process: usize,
    donations: usize,
    preemptions: usize,
    irq_notifications: usize,
    baseline: memory::Stats,
}

impl Scheduler {
    fn channel(&self, handle: u64) -> Result<usize, Error> {
        let index = handle.checked_sub(1).ok_or(Error::Invalid)? as usize;
        let channel = self.channels.get(index).ok_or(Error::Invalid)?;
        if channel.closed {
            return Err(Error::Closed);
        }
        Ok(index)
    }

    fn depends_on(&self, thread: usize) -> Option<usize> {
        match self.threads[thread].state {
            State::Sending { channel, .. } => Some(self.channels[channel].owner),
            State::Reply { owner, .. } => Some(owner),
            _ => None,
        }
    }

    fn would_deadlock(&self, sender: usize, mut owner: usize) -> bool {
        for _ in 0..self.threads.len() {
            if owner == sender {
                return true;
            }
            match self.depends_on(owner) {
                Some(next) => owner = next,
                None => return false,
            }
        }
        true
    }

    fn priorities(&mut self) {
        for thread in &mut self.threads {
            thread.effective = thread.base;
        }
        for _ in 0..self.threads.len() {
            let mut changed = false;
            for sender in 0..self.threads.len() {
                if let Some(owner) = self.depends_on(sender) {
                    let donated = self.threads[sender].effective;
                    if donated < self.threads[owner].effective {
                        self.threads[owner].effective = donated;
                        self.donations += 1;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn choose(&mut self, timer: bool) -> Option<*const Context> {
        self.priorities();
        let priority = self
            .threads
            .iter()
            .filter(|t| matches!(t.state, State::Ready))
            .map(|t| t.effective)
            .min()?;
        let previous = self.current;
        let start = (self.last[priority as usize] + 1) % self.threads.len();
        let selected = (0..self.threads.len())
            .map(|offset| (start + offset) % self.threads.len())
            .find(|&index| {
                let thread = &self.threads[index];
                thread.effective == priority && matches!(thread.state, State::Ready)
            })
            .unwrap();
        if timer && previous.is_some_and(|id| id != selected) {
            self.preemptions += 1;
        }
        self.last[priority as usize] = selected;
        self.current = Some(selected);
        let thread = &self.threads[selected];
        let top = thread.stack.as_ptr() as u64 + core::mem::size_of_val(&*thread.stack) as u64;
        arch::kernel_stack(top);
        crate::interrupts::select_stack(top, thread.port);
        // SAFETY: all kernel code, stacks and heap are shared; no user reference
        // is live across this switch. Context validation precedes every return.
        unsafe { thread.process.lock().space.activate() };
        Some(&thread.context)
    }

    fn request(
        &self,
        thread: usize,
        pointer: u64,
        length: u64,
    ) -> Result<([u8; MAX_MESSAGE], usize), Error> {
        if length > MAX_MESSAGE as u64 {
            return Err(Error::TooLarge);
        }
        let mut bytes = [0; MAX_MESSAGE];
        self.threads[thread]
            .process
            .lock()
            .space
            .read_user(pointer, &mut bytes[..length as usize])
            .map_err(|_| Error::BadAddress)?;
        Ok((bytes, length as usize))
    }

    fn check_buffer(&self, thread: usize, pointer: u64, length: u64) -> Result<usize, Error> {
        if length > MAX_MESSAGE as u64 {
            return Err(Error::TooLarge);
        }
        self.threads[thread]
            .process
            .lock()
            .space
            .check(pointer, length as usize, true)
            .map_err(|_| Error::BadAddress)?;
        Ok(length as usize)
    }

    fn receive(&mut self, receiver: usize, channel: usize, buffer: u64, capacity: usize) {
        let queued = self.channels[channel]
            .pending
            .iter()
            .enumerate()
            .min_by_key(|(_, sender)| self.threads[**sender].effective)
            .map(|(position, sender)| (position, *sender));
        if let Some((position, sender)) = queued {
            let State::Sending {
                message,
                length,
                reply,
                capacity: reply_capacity,
                token,
                ..
            } = self.threads[sender].state
            else {
                panic!("Invalid IPC queue")
            };
            if length > capacity {
                self.threads[receiver].result(Err(Error::TooLarge), 0);
                return;
            }
            let copied = self.threads[receiver]
                .process
                .lock()
                .space
                .write_user(buffer, &message[..length]);
            if copied.is_err() {
                self.threads[receiver].result(Err(Error::BadAddress), 0);
                return;
            }
            self.channels[channel].pending.remove(position);
            self.threads[sender].state = State::Reply {
                owner: receiver,
                buffer: reply,
                capacity: reply_capacity,
                token,
            };
            self.threads[receiver].result(Ok(token as usize), length);
            if !Arc::ptr_eq(
                &self.threads[sender].process,
                &self.threads[receiver].process,
            ) {
                self.cross_process += 1;
            }
        } else if self.channels[channel].irq_pending {
            if capacity < 8 {
                self.threads[receiver].result(Err(Error::TooLarge), 0);
                return;
            }
            let copied = self.threads[receiver]
                .process
                .lock()
                .space
                .write_user(buffer, &0_u64.to_le_bytes());
            if copied.is_err() {
                self.threads[receiver].result(Err(Error::BadAddress), 0);
                return;
            }
            self.channels[channel].irq_pending = false;
            self.irq_notifications += 1;
            self.threads[receiver].result(Ok(0), 8);
        } else {
            self.threads[receiver].state = State::Receiving {
                channel,
                buffer,
                capacity,
            };
        }
    }

    fn notify_timer(&mut self) {
        if let Some(channel) = self.irq_channel {
            self.channels[channel].irq_pending = true;
            let owner = self.channels[channel].owner;
            if let State::Receiving {
                channel: waiting,
                buffer,
                capacity,
            } = self.threads[owner].state
            {
                if waiting == channel {
                    self.receive(owner, channel, buffer, capacity);
                }
            }
        }
    }

    fn close_owned(&mut self, owner: usize) {
        for (index, channel) in self.channels.iter_mut().enumerate() {
            if channel.owner == owner {
                channel.closed = true;
                channel.pending.clear();
                if self.irq_channel == Some(index) {
                    self.irq_channel = None;
                }
            }
        }
        for thread in &mut self.threads {
            let closed = match thread.state {
                State::Sending { channel, .. } => self.channels[channel].closed,
                State::Reply { owner: server, .. } => server == owner,
                _ => false,
            };
            if closed {
                thread.result(Err(Error::Closed), 0);
            }
        }
    }

    fn syscall(&mut self, id: usize) -> Result<Option<usize>, Error> {
        let frame = self.threads[id].context.frame;
        match frame.rax {
            x if x == Call::ChannelCreate as u64 => {
                if self.channels.len() == MAX_CHANNELS {
                    return Err(Error::Limit);
                }
                self.channels.push(Channel {
                    owner: id,
                    pending: VecDeque::new(),
                    closed: false,
                    irq_pending: false,
                });
                Ok(Some(self.channels.len()))
            }
            x if x == Call::Send as u64 => {
                let (message, length) = self.request(id, frame.rsi, frame.rdx)?;
                let capacity = self.check_buffer(id, frame.r10, frame.r8)?;
                let channel = self.channel(frame.rdi)?;
                let owner = self.channels[channel].owner;
                if self.would_deadlock(id, owner) {
                    return Err(Error::Deadlock);
                }
                self.sequence = self
                    .sequence
                    .checked_add(1)
                    .filter(|&value| value <= u64::MAX >> 16)
                    .ok_or(Error::Limit)?;
                let token = (self.sequence << 16) | (id + 1) as u64;
                self.threads[id].state = State::Sending {
                    channel,
                    message,
                    length,
                    reply: frame.r10,
                    capacity,
                    token,
                };
                self.channels[channel].pending.push_back(id);
                if let State::Receiving {
                    channel: waiting,
                    buffer,
                    capacity,
                } = self.threads[owner].state
                {
                    if channel == waiting {
                        self.receive(owner, channel, buffer, capacity);
                    }
                }
                Ok(None)
            }
            x if x == Call::Receive as u64 => {
                let channel = self.channel(frame.rdi)?;
                if self.channels[channel].owner != id {
                    return Err(Error::Denied);
                }
                let capacity = self.check_buffer(id, frame.rsi, frame.rdx)?;
                self.receive(id, channel, frame.rsi, capacity);
                Ok(None)
            }
            x if x == Call::Reply as u64 => {
                let (message, length) = self.request(id, frame.rsi, frame.rdx)?;
                let sender = (frame.rdi & 0xffff).checked_sub(1).ok_or(Error::Invalid)? as usize;
                let thread = self.threads.get(sender).ok_or(Error::Invalid)?;
                let State::Reply {
                    owner,
                    buffer,
                    capacity,
                    token,
                } = thread.state
                else {
                    return Err(Error::Invalid);
                };
                if owner != id || token != frame.rdi {
                    return Err(Error::Invalid);
                }
                if length > capacity {
                    return Err(Error::TooLarge);
                }
                let copied = thread
                    .process
                    .lock()
                    .space
                    .write_user(buffer, &message[..length]);
                if copied.is_err() {
                    self.threads[sender].result(Err(Error::BadAddress), 0);
                    return Err(Error::BadAddress);
                }
                self.threads[sender].result(Ok(length), 0);
                self.replies += 1;
                Ok(Some(0))
            }
            x if x == Call::ThreadCreate as u64 => {
                if self.threads.len() == MAX_THREADS {
                    return Err(Error::Limit);
                }
                if frame.rdx > 31 || frame.rdx < self.threads[id].base as u64 {
                    return Err(Error::Denied);
                }
                let process = self.threads[id].process.clone();
                if !process.lock().space.executable(frame.rdi) {
                    return Err(Error::BadAddress);
                }
                let thread = Thread::new(process, frame.rdi, frame.rsi, frame.rdx as u8)?;
                self.threads.push(thread);
                Ok(Some(self.threads.len()))
            }
            x if x == Call::MapMemory as u64 => {
                if !(0x1000_0000..0x7000_0000).contains(&frame.rdi)
                    || frame.rdi % PAGE_SIZE != 0
                    || frame.rsi & !3 != 0
                    || frame.rsi == 3
                {
                    return Err(Error::Invalid);
                }
                let mut process = self.threads[id].process.lock();
                if process.dynamic_pages == 128 {
                    return Err(Error::Limit);
                }
                process
                    .space
                    .map(
                        frame.rdi,
                        Permissions {
                            writable: frame.rsi & 1 != 0,
                            executable: frame.rsi & 2 != 0,
                        },
                    )
                    .map_err(|error| {
                        if error == memory::Error::OutOfFrames {
                            Error::NoMemory
                        } else {
                            Error::Invalid
                        }
                    })?;
                process.dynamic_pages += 1;
                Ok(Some(0))
            }
            x if x == Call::IrqAttach as u64 => {
                let channel = self.channel(frame.rdi)?;
                if frame.rsi > 15 {
                    return Err(Error::Invalid);
                }
                if frame.rsi != 0
                    || !self.threads[id].process.lock().driver
                    || self.channels[channel].owner != id
                    || self.irq_channel.is_some()
                {
                    return Err(Error::Denied);
                }
                self.irq_channel = Some(channel);
                Ok(Some(0))
            }
            x if x == Call::IoPortGrant as u64 => {
                if frame.rdi != 0x80 || frame.rsi != 1 || !self.threads[id].process.lock().driver {
                    return Err(Error::Denied);
                }
                self.threads[id].port = true;
                Ok(Some(0))
            }
            x if x == Call::Exit as u64 => {
                crate::logln!("USER EXIT: tid={} status={}", id + 1, frame.rdi as i64);
                self.threads[id].state = State::Exited(frame.rdi as i64);
                self.close_owned(id);
                Ok(None)
            }
            x if x == Call::DebugWrite as u64 => {
                let (bytes, length) = self.request(id, frame.rdi, frame.rsi)?;
                let text = core::str::from_utf8(&bytes[..length]).map_err(|_| Error::Invalid)?;
                crate::logln!("USER tid={}: {}", id + 1, text);
                Ok(Some(0))
            }
            _ => Err(Error::Invalid),
        }
    }

    fn validate_return(&self, id: usize) -> bool {
        let frame = self.threads[id].context.frame;
        let process = self.threads[id].process.lock();
        (PAGE_SIZE..0x0000_8000_0000_0000).contains(&frame.rsp)
            && frame.cs == 0x23
            && frame.ss == 0x1b
            && process.space.executable(frame.rip)
            && frame
                .rsp
                .checked_sub(8)
                .is_some_and(|stack| process.space.check(stack, 8, true).is_ok())
    }

    fn fault(&mut self, id: usize, name: &'static str, address: u64) {
        crate::logln!(
            "USER FAULT: tid={} kind={} address={:#x}",
            id + 1,
            name,
            address
        );
        self.threads[id].state = State::Faulted(name);
        self.close_owned(id);
    }
}

fn dispatch(frame: &Frame, fx: &FxState, timer: bool) -> *const Context {
    let next = {
        let mut guard = SCHEDULER.lock();
        let scheduler = guard.as_mut().expect("User trap before scheduler");
        let id = scheduler.current.expect("No current user thread");
        scheduler.threads[id].context.frame = *frame;
        scheduler.threads[id].context.fx = *fx;
        scheduler.threads[id].context.frame.flags = (frame.flags & 0x0000_0000_0020_0cd5) | 0x202;
        if !scheduler.validate_return(id) {
            scheduler.fault(id, "INVALID RETURN", frame.rsp);
        } else if timer {
            scheduler.notify_timer();
        } else {
            match scheduler.syscall(id) {
                Ok(Some(value)) => scheduler.threads[id].result(Ok(value), 0),
                Ok(None) => (),
                Err(error) => scheduler.threads[id].result(Err(error), 0),
            }
        }
        scheduler.choose(timer)
    };
    match next {
        Some(context) => context,
        None => arch::idle(),
    }
}

#[no_mangle]
extern "C" fn zos_trap(frame: &Frame, fx: &FxState, kind: u64) -> *const Context {
    if kind == 0 {
        crate::TICKS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        // SAFETY: this is the assembly IRQ0 path and must acknowledge the PIC.
        unsafe { Port::<u8>::new(0x20).write(0x20) };
    }
    dispatch(frame, fx, kind == 0)
}

pub fn idle_tick() {
    if let Some(scheduler) = SCHEDULER.lock().as_mut() {
        scheduler.notify_timer();
    }
}

pub fn user_fault(name: &'static str, address: u64) -> ! {
    let next = {
        let mut guard = SCHEDULER.lock();
        let scheduler = guard.as_mut().expect("User fault without scheduler");
        let id = scheduler.current.unwrap();
        scheduler.fault(id, name, address);
        scheduler.choose(false)
    };
    match next {
        // SAFETY: the faulting thread is terminated, selected root/TSS are active,
        // and no state from the dead thread is used after abandoning its stack.
        Some(context) => unsafe { arch::resume(context) },
        None => arch::idle(),
    }
}

#[no_mangle]
extern "C" fn zos_idle() -> ! {
    memory::activate_kernel();
    {
        let mut guard = SCHEDULER.lock();
        guard.as_mut().unwrap().current = None;
    }
    loop {
        let (finished, next) = {
            let mut guard = SCHEDULER.lock();
            let scheduler = guard.as_mut().unwrap();
            let finished = scheduler
                .threads
                .iter()
                .all(|thread| matches!(thread.state, State::Exited(_) | State::Faulted(_)));
            (
                finished,
                if finished {
                    None
                } else {
                    scheduler.choose(false)
                },
            )
        };
        if finished {
            complete();
        }
        if let Some(context) = next {
            // SAFETY: choose activated a validated live context on a shared stack.
            unsafe { arch::resume(context) };
        }
        cpu_interrupts::enable_and_hlt();
        cpu_interrupts::disable();
    }
}

fn complete() -> ! {
    let scheduler = SCHEDULER.lock().take().unwrap();
    assert!(scheduler.preemptions >= 1, "No user timer preemption");
    assert!(scheduler.donations > 0, "No priority inheritance");
    assert!(scheduler.replies >= 4 && scheduler.cross_process >= 3);
    assert!(
        scheduler.irq_notifications > 0,
        "No IRQ notification delivered"
    );
    let mut faults = 0;
    for thread in &scheduler.threads {
        match (thread.state, thread.expected_fault) {
            (State::Exited(0), None) => (),
            (State::Faulted(actual), Some(expected)) if actual == expected => faults += 1,
            (State::Exited(status), _) => panic!("User process failed with status {status}"),
            _ => panic!("Unexpected user process state"),
        }
    }
    assert_eq!(faults, 5);
    let baseline = scheduler.baseline;
    crate::framebuffer::clear();
    crate::logln!("ZOS - Zombie OS");
    crate::logln!("Step 4: threads, ring 3 and IPC verified");
    crate::logln!(
        "SCHEDULER PASS: preemptions={} donation_events={}, SSE state preserved",
        scheduler.preemptions,
        scheduler.donations
    );
    crate::logln!(
        "IPC PASS: cross_process={} replies={}, server-owned reply tokens",
        scheduler.cross_process,
        scheduler.replies
    );
    crate::logln!("IRQ/IO PASS: timer notification, granted port, denied unprivileged ports");
    crate::logln!("USER ISOLATION PASS: 5 faults contained; echo completed afterwards");
    drop(scheduler);
    let after = memory::stats();
    assert_eq!(after.free_bytes, baseline.free_bytes, "Process frame leak");
    assert_eq!(
        after.heap_used_bytes, baseline.heap_used_bytes,
        "Thread/IPC heap leak"
    );
    crate::logln!("PROCESS CLEANUP PASS: all address spaces, threads, channels released");
    if cfg!(feature = "test-ipc") {
        crate::logln!("IPC ACCEPTANCE PASS");
        crate::finish_test(true);
    }
    crate::logln!("ZOS ready: IPC checkpoint; no user-space shell yet.");
    loop {
        cpu_interrupts::enable_and_hlt();
    }
}

pub fn start() -> ! {
    cpu_interrupts::disable();
    arch::init();
    let modules = crate::boot::MODULES
        .response()
        .expect("Missing Limine modules")
        .modules();
    let module = |name: &str| {
        modules
            .iter()
            .find(|file| file.cmdline() == name)
            .unwrap_or_else(|| panic!("Missing user ELF module {name}"))
            .data()
    };
    elf::self_test(module("echo-server"));
    let mut scheduler = Scheduler {
        threads: Vec::new(),
        channels: Vec::new(),
        current: None,
        last: [usize::MAX; 32],
        sequence: 0,
        irq_channel: None,
        replies: 0,
        cross_process: 0,
        donations: 0,
        preemptions: 0,
        irq_notifications: 0,
        baseline: memory::stats(),
    };
    for (name, role, priority, driver, expected_fault) in [
        ("echo-server", 0, 0, true, None),
        ("echo-client", 0, 1, false, None),
        ("fault-probe", 0, 4, false, Some("PAGE FAULT")),
        ("fault-probe", 1, 4, false, Some("GENERAL PROTECTION")),
        ("fault-probe", 2, 4, false, Some("INVALID OPCODE")),
        ("fault-probe", 3, 4, false, Some("GENERAL PROTECTION")),
        ("fault-probe", 4, 4, false, Some("INVALID RETURN")),
    ] {
        let (space, entry) = elf::load(module(name)).expect("User ELF load failed");
        let process = Arc::new(Mutex::new(Process {
            space,
            stacks: 0,
            dynamic_pages: 0,
            driver,
        }));
        let mut thread = Thread::new(process, entry, role, priority).unwrap();
        thread.expected_fault = expected_fault;
        scheduler.threads.push(thread);
        crate::logln!("ELF LOAD: {} role={} entry={:#x}", name, role, entry);
    }
    // The first user is the server; normal round-robin applies from its first trap.
    scheduler.last = [scheduler.threads.len() - 1; 32];
    let next = scheduler.choose(false).unwrap();
    assert!(SCHEDULER.lock().replace(scheduler).is_none());
    // SAFETY: choose configured the first user root and kernel stack. Contexts
    // live in the Vec allocation, which is not moved until a later kernel trap.
    unsafe { arch::resume(next) }
}
