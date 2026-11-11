pub mod arch;
mod elf;
mod hardware;
mod management;

use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec, vec::Vec};
use arch::{Context, Frame, FxState};
use spin::Mutex;
use x86_64::instructions::interrupts as cpu_interrupts;
use zos_libos::{Call, Error, MAX_MESSAGE, MAX_STORAGE_MESSAGE};

use crate::memory::{self, AddressSpace, Permissions, PAGE_SIZE};

const MAX_THREADS: usize = 32;
const MAX_CHANNELS: usize = 16;
const STACK_PAGES: u64 = 16;
static SCHEDULER: Mutex<Option<Scheduler>> = Mutex::new(None);

struct Process {
    space: AddressSpace,
    stacks: u64,
    dynamic_pages: usize,
    ports: u8,
    irqs: u16,
    framebuffer: bool,
    framebuffer_mapped: bool,
    test_control: bool,
    storage_control: bool,
    bulk: bool,
    manager: bool,
    usb: hardware::Grant,
}

impl Drop for Process {
    fn drop(&mut self) {
        // Disable device DMA before AddressSpace returns any owned frames.
        self.usb.stop();
    }
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
    Reaped,
}

struct Thread {
    process: Arc<Mutex<Process>>,
    context: Context,
    stack: Box<[u128]>,
    base: u8,
    effective: u8,
    state: State,
    ports: u8,
    expected_fault: Option<&'static str>,
    pending_message: [u8; MAX_STORAGE_MESSAGE],
    name: [u8; zos_libos::PROCESS_NAME],
    generation: u64,
    system: bool,
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
            ports: 0,
            expected_fault: None,
            pending_message: [0; MAX_STORAGE_MESSAGE],
            name: [0; zos_libos::PROCESS_NAME],
            generation: 0,
            system: true,
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
    irq_pending: u16,
    generation: u64,
}

struct Scheduler {
    threads: Vec<Thread>,
    channels: Vec<Channel>,
    current: Option<usize>,
    last: [usize; 32],
    sequence: u64,
    irq_channels: [u16; 16],
    irq_seen: [usize; 16],
    console: bool,
    console_finished: bool,
    storage_finished: bool,
    shell_finished: bool,
    replies: usize,
    cross_process: usize,
    donations: usize,
    preemptions: usize,
    irq_notifications: usize,
    baseline: memory::Stats,
}

impl Scheduler {
    fn channel(&self, handle: u64) -> Result<usize, Error> {
        let index = (handle & 255).checked_sub(1).ok_or(Error::Invalid)? as usize;
        let channel = self.channels.get(index).ok_or(Error::Invalid)?;
        if channel.closed || channel.generation != handle >> 8 {
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
        crate::interrupts::select_stack(top, thread.ports);
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
    ) -> Result<([u8; MAX_STORAGE_MESSAGE], usize), Error> {
        let limit = if self.threads[thread].process.lock().bulk {
            MAX_STORAGE_MESSAGE
        } else {
            MAX_MESSAGE
        };
        if length > limit as u64 {
            return Err(Error::TooLarge);
        }
        let mut bytes = [0; MAX_STORAGE_MESSAGE];
        self.threads[thread]
            .process
            .lock()
            .space
            .read_user(pointer, &mut bytes[..length as usize])
            .map_err(|_| Error::BadAddress)?;
        Ok((bytes, length as usize))
    }

    fn check_buffer(&self, thread: usize, pointer: u64, length: u64) -> Result<usize, Error> {
        let limit = if self.threads[thread].process.lock().bulk {
            MAX_STORAGE_MESSAGE
        } else {
            MAX_MESSAGE
        };
        if length > limit as u64 {
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
                .write_user(buffer, &self.threads[sender].pending_message[..length]);
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
        } else if self.channels[channel].irq_pending != 0 {
            if capacity < 8 {
                self.threads[receiver].result(Err(Error::TooLarge), 0);
                return;
            }
            let irq = self.channels[channel].irq_pending.trailing_zeros() as usize;
            let copied = self.threads[receiver]
                .process
                .lock()
                .space
                .write_user(buffer, &(irq as u64).to_le_bytes());
            if copied.is_err() {
                self.threads[receiver].result(Err(Error::BadAddress), 0);
                return;
            }
            self.channels[channel].irq_pending &= !(1 << irq);
            self.irq_seen[irq] += 1;
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

    fn notify_irq(&mut self, irq: usize) {
        for channel in 0..self.channels.len() {
            if self.irq_channels[irq] & (1 << channel) == 0 {
                continue;
            }
            self.channels[channel].irq_pending |= 1 << irq;
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
            channel.pending.retain(|&sender| sender != owner);
            if channel.owner == owner {
                channel.closed = true;
                channel.pending.clear();
                for (irq, attachment) in self.irq_channels.iter_mut().enumerate() {
                    if *attachment & (1 << index) != 0 {
                        *attachment &= !(1 << index);
                        if *attachment == 0 {
                            crate::interrupts::irq_enable(irq, false);
                        }
                    }
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
                let slot = self
                    .channels
                    .iter()
                    .position(|channel| channel.closed)
                    .unwrap_or(self.channels.len());
                if slot == MAX_CHANNELS {
                    return Err(Error::Limit);
                }
                let generation = if slot == self.channels.len() {
                    0
                } else {
                    self.channels[slot]
                        .generation
                        .checked_add(1)
                        .filter(|&g| g <= u64::MAX >> 8)
                        .ok_or(Error::Limit)?
                };
                let channel = Channel {
                    owner: id,
                    pending: VecDeque::new(),
                    closed: false,
                    irq_pending: 0,
                    generation,
                };
                if slot == self.channels.len() {
                    self.channels.push(channel);
                } else {
                    self.channels[slot] = channel;
                }
                Ok(Some(((generation << 8) | (slot + 1) as u64) as usize))
            }
            x if x == Call::Send as u64 || x == Call::SendReady as u64 => {
                let ready = x == Call::SendReady as u64;
                if ready && !self.threads[id].process.lock().usb.allowed {
                    return Err(Error::Denied);
                }
                let (message, length) = self.request(id, frame.rsi, frame.rdx)?;
                let capacity = self.check_buffer(id, frame.r10, frame.r8)?;
                let channel = self.channel(frame.rdi)?;
                let owner = self.channels[channel].owner;
                if ready
                    && !matches!(self.threads[owner].state,
                    State::Receiving { channel: waiting, .. } if waiting == channel)
                {
                    return Err(Error::Busy);
                }
                if self.would_deadlock(id, owner) {
                    return Err(Error::Deadlock);
                }
                self.sequence = self
                    .sequence
                    .checked_add(1)
                    .filter(|&value| value <= u64::MAX >> 16)
                    .ok_or(Error::Limit)?;
                let token = (self.sequence << 16) | (id + 1) as u64;
                self.threads[id].pending_message = message;
                self.threads[id].state = State::Sending {
                    channel,
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
                if !self.threads[id].system {
                    return Err(Error::Denied);
                }
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
                let irq = frame.rsi as usize;
                if self.threads[id].process.lock().irqs & (1 << irq) == 0
                    || self.channels[channel].owner != id
                    || self.irq_channels[irq] & (1 << channel) != 0
                    || (irq != 0 && self.irq_channels[irq] != 0)
                {
                    return Err(Error::Denied);
                }
                self.irq_channels[irq] |= 1 << channel;
                crate::interrupts::irq_enable(irq, true);
                Ok(Some(0))
            }
            x if x == Call::IoPortGrant as u64 => {
                let (bit, count) = match frame.rdi {
                    0x80 => (1, 1),
                    0x60 => (2, 1),
                    0x64 => (4, 1),
                    0x170 => (8, 8),
                    0x376 => (16, 1),
                    _ => return Err(Error::Denied),
                };
                if frame.rsi != count || self.threads[id].process.lock().ports & bit == 0 {
                    return Err(Error::Denied);
                }
                self.threads[id].ports |= bit;
                Ok(Some(0))
            }
            x if x == Call::FramebufferMap as u64 => {
                let mut process = self.threads[id].process.lock();
                if !process.framebuffer || process.framebuffer_mapped {
                    return Err(Error::Denied);
                }
                if frame.rsi != 48 {
                    return Err(Error::Invalid);
                }
                process
                    .space
                    .check(frame.rdi, 48, true)
                    .map_err(|_| Error::BadAddress)?;
                let fb = process.space.map_framebuffer().map_err(|error| {
                    if error == memory::Error::OutOfFrames {
                        Error::NoMemory
                    } else {
                        Error::Invalid
                    }
                })?;
                process.framebuffer_mapped = true;
                let mut bytes = [0_u8; 48];
                for (index, word) in [
                    fb.address,
                    fb.width as u64,
                    fb.height as u64,
                    fb.pitch as u64,
                    u32::from_le_bytes([fb.shifts[0], fb.shifts[1], fb.shifts[2], 0]) as u64,
                    fb.length as u64,
                ]
                .into_iter()
                .enumerate()
                {
                    bytes[index * 8..index * 8 + 8].copy_from_slice(&word.to_le_bytes());
                }
                process
                    .space
                    .write_user(frame.rdi, &bytes)
                    .map_err(|_| Error::BadAddress)?;
                Ok(Some(0))
            }
            x if x == Call::ConsoleTestFinish as u64 => {
                if !cfg!(feature = "test-console") || !self.threads[id].process.lock().test_control
                {
                    return Err(Error::Denied);
                }
                assert!(self.irq_seen[1] > 0 && self.irq_seen[12] > 0);
                self.console_finished = true;
                for owner in 0..self.threads.len() {
                    assert!(!matches!(
                        self.threads[owner].state,
                        State::Faulted(_) | State::Exited(_)
                    ));
                    self.threads[owner].state = State::Exited(0);
                    self.close_owned(owner);
                }
                Ok(None)
            }
            x if x == Call::StorageTestFinish as u64 => {
                if !cfg!(any(feature = "test-fs-create", feature = "test-fs-read"))
                    || !self.threads[id].process.lock().storage_control
                {
                    return Err(Error::Denied);
                }
                assert!(self.console && self.threads.len() >= 7);
                self.storage_finished = true;
                for thread in &mut self.threads {
                    assert!(match thread.state {
                        State::Faulted(_) => false,
                        State::Exited(status) => status == 0,
                        _ => true,
                    });
                    thread.state = State::Exited(0);
                }
                for owner in 0..self.threads.len() {
                    self.close_owned(owner);
                }
                Ok(None)
            }
            x if x == Call::Exit as u64 => {
                crate::logln!("USER EXIT: tid={} status={}", id + 1, frame.rdi as i64);
                self.threads[id].state = State::Exited(frame.rdi as i64);
                self.close_owned(id);
                Ok(None)
            }
            x if x == Call::DebugWrite as u64 => {
                if frame.rsi > MAX_MESSAGE as u64 {
                    return Err(Error::TooLarge);
                }
                let (bytes, length) = self.request(id, frame.rdi, frame.rsi)?;
                let text = core::str::from_utf8(&bytes[..length]).map_err(|_| Error::Invalid)?;
                crate::logln!("USER tid={}: {}", id + 1, text);
                Ok(Some(0))
            }
            x if (Call::ProcessSpawn as u64..=Call::SystemControl as u64).contains(&x) => {
                self.management(id, frame)
            }
            x if (Call::UsbController as u64..=Call::UsbEnableDma as u64).contains(&x) => {
                self.hardware(id, frame)
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

fn dispatch(frame: &Frame, fx: &FxState, kind: u64) -> *const Context {
    let next = {
        let mut guard = SCHEDULER.lock();
        let scheduler = guard.as_mut().expect("User trap before scheduler");
        let id = scheduler.current.expect("No current user thread");
        scheduler.threads[id].context.frame = *frame;
        scheduler.threads[id].context.fx = *fx;
        scheduler.threads[id].context.frame.flags = (frame.flags & 0x0000_0000_0020_0cd5) | 0x202;
        if !scheduler.validate_return(id) {
            scheduler.fault(id, "INVALID RETURN", frame.rsp);
        } else if kind != 1 {
            scheduler.notify_irq(if kind == 0 { 0 } else { (kind - 1) as usize });
        } else {
            match scheduler.syscall(id) {
                Ok(Some(value)) => scheduler.threads[id].result(Ok(value), 0),
                Ok(None) => (),
                Err(error) => scheduler.threads[id].result(Err(error), 0),
            }
        }
        scheduler.choose(kind == 0)
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
    }
    if kind != 1 {
        crate::interrupts::acknowledge(if kind == 0 { 0 } else { (kind - 1) as usize });
    }
    dispatch(frame, fx, kind)
}

pub fn idle_irq(irq: usize) {
    if let Some(scheduler) = SCHEDULER.lock().as_mut() {
        scheduler.notify_irq(irq);
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
            let finished = scheduler.threads.iter().all(|thread| {
                matches!(
                    thread.state,
                    State::Exited(_) | State::Faulted(_) | State::Reaped
                )
            });
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
    if scheduler.shell_finished {
        let baseline = scheduler.baseline;
        drop(scheduler);
        let after = memory::stats();
        assert_eq!(after.free_bytes, baseline.free_bytes, "Shell frame leak");
        assert_eq!(
            after.heap_used_bytes, baseline.heap_used_bytes,
            "Shell heap leak"
        );
        crate::logln!("SHELL CLEANUP PASS: all processes, descriptors and services released");
        crate::logln!("SHELL ACCEPTANCE PASS");
        crate::finish_test(true);
    }
    if scheduler.storage_finished {
        let baseline = scheduler.baseline;
        drop(scheduler);
        let after = memory::stats();
        assert_eq!(after.free_bytes, baseline.free_bytes, "Storage frame leak");
        assert_eq!(
            after.heap_used_bytes, baseline.heap_used_bytes,
            "Storage heap leak"
        );
        crate::logln!("STORAGE CLEANUP PASS: all driver, filesystem and VFS resources released");
        crate::logln!("FILESYSTEM ACCEPTANCE PASS");
        crate::finish_test(true);
    }
    if scheduler.console {
        assert!(
            scheduler.console_finished,
            "Console services stopped unexpectedly"
        );
        let baseline = scheduler.baseline;
        drop(scheduler);
        let after = memory::stats();
        assert_eq!(after.free_bytes, baseline.free_bytes, "Console frame leak");
        assert_eq!(
            after.heap_used_bytes, baseline.heap_used_bytes,
            "Console heap leak"
        );
        crate::logln!("CONSOLE CLEANUP PASS: borrowed framebuffer and driver resources released");
        crate::logln!("CONSOLE ACCEPTANCE PASS");
        crate::finish_test(true);
    }
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
    start_console()
}

pub fn start() -> ! {
    cpu_interrupts::disable();
    arch::init();
    if cfg!(feature = "usb-diagnostic") {
        start_console();
    }
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
        irq_channels: [0; 16],
        irq_seen: [0; 16],
        console: false,
        console_finished: false,
        storage_finished: false,
        shell_finished: false,
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
            ports: if driver { 1 } else { 0 },
            irqs: if driver { 1 } else { 0 },
            framebuffer: false,
            framebuffer_mapped: false,
            test_control: false,
            storage_control: false,
            bulk: false,
            manager: false,
            usb: hardware::Grant::new(false),
        }));
        let mut thread = Thread::new(process, entry, role, priority).unwrap();
        thread.name[..name.len()].copy_from_slice(name.as_bytes());
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

fn start_console() -> ! {
    if cfg!(feature = "usb-diagnostic") {
        crate::logln!("Starting read-only USB diagnostic services");
    } else {
        crate::logln!("Starting ring-3 console, input and TTY servers");
    }
    crate::framebuffer::handoff();
    let mut scheduler = Scheduler {
        threads: Vec::new(),
        channels: Vec::new(),
        current: None,
        last: [0; 32],
        sequence: 0,
        irq_channels: [0; 16],
        irq_seen: [0; 16],
        console: true,
        console_finished: false,
        storage_finished: false,
        shell_finished: false,
        replies: 0,
        cross_process: 0,
        donations: 0,
        preemptions: 0,
        irq_notifications: 0,
        baseline: memory::stats(),
    };
    for (name, priority, ports, irqs, framebuffer) in [
        ("console-server", 0, 0, 0, true),
        (
            "input-server",
            1,
            if cfg!(feature = "usb") { 0 } else { 6 },
            if cfg!(feature = "usb") {
                1
            } else {
                (1 << 1) | (1 << 12)
            },
            false,
        ),
        (
            if shell_enabled() {
                "shell-server"
            } else {
                "tty-server"
            },
            2,
            0,
            0,
            false,
        ),
    ] {
        if cfg!(feature = "usb-diagnostic") && priority == 2 {
            continue;
        }
        let file = crate::boot::MODULES
            .response()
            .unwrap()
            .modules()
            .iter()
            .find(|file| file.cmdline() == name)
            .expect("Missing console ELF");
        let (space, entry) = elf::load(file.data()).expect("Console ELF load failed");
        let process = Arc::new(Mutex::new(Process {
            space,
            stacks: 0,
            dynamic_pages: 0,
            ports,
            irqs,
            framebuffer,
            framebuffer_mapped: false,
            test_control: name == "tty-server",
            storage_control: false,
            bulk: framebuffer
                || matches!(name, "shell-server" | "tty-server")
                || (name == "input-server" && cfg!(feature = "usb")),
            manager: false,
            usb: hardware::Grant::new(name == "input-server" && cfg!(feature = "usb")),
        }));
        let mut thread = Thread::new(
            process,
            entry,
            (cfg!(feature = "test-console") || cfg!(feature = "test-shell")) as u64
                | if cfg!(feature = "usb-diagnostic") {
                    16
                } else {
                    0
                }
                | if cfg!(feature = "usb") && matches!(name, "input-server" | "shell-server") {
                    8
                } else {
                    0
                },
            priority,
        )
        .unwrap();
        thread.name[..name.len()].copy_from_slice(name.as_bytes());
        scheduler.threads.push(thread);
    }
    if !cfg!(any(feature = "test-console", feature = "usb-diagnostic")) {
        for (name, priority, ports) in [
            ("blkdev-server", 3, 24),
            ("fs-simple-server", 4, 0),
            ("vfs-server", 5, 0),
            ("storage-probe", 6, 0),
        ] {
            let file = crate::boot::MODULES
                .response()
                .unwrap()
                .modules()
                .iter()
                .find(|file| file.cmdline() == name)
                .expect("Missing storage ELF");
            let (space, entry) = elf::load(file.data()).expect("Storage ELF load failed");
            let process = Arc::new(Mutex::new(Process {
                space,
                stacks: 0,
                dynamic_pages: 0,
                ports: if cfg!(feature = "usb") { 0 } else { ports },
                irqs: 0,
                framebuffer: false,
                framebuffer_mapped: false,
                test_control: false,
                storage_control: name == "storage-probe",
                bulk: true,
                manager: false,
                usb: hardware::Grant::new(false),
            }));
            let role = if name == "blkdev-server" && cfg!(feature = "usb") {
                8
            } else if cfg!(feature = "test-fs-create") {
                1
            } else if cfg!(feature = "test-fs-read") {
                2
            } else {
                0
            };
            let mut thread = Thread::new(process, entry, role, priority).unwrap();
            thread.name[..name.len()].copy_from_slice(name.as_bytes());
            scheduler.threads.push(thread);
        }
    }
    if shell_enabled() {
        let file = crate::boot::MODULES
            .response()
            .unwrap()
            .modules()
            .iter()
            .find(|file| file.cmdline() == "procmgr-server")
            .expect("Missing procmgr ELF");
        let (space, entry) = elf::load(file.data()).expect("Process manager ELF load failed");
        let process = Arc::new(Mutex::new(Process {
            space,
            stacks: 0,
            dynamic_pages: 0,
            ports: 0,
            irqs: 1,
            framebuffer: false,
            framebuffer_mapped: false,
            test_control: false,
            storage_control: false,
            bulk: true,
            manager: true,
            usb: hardware::Grant::new(false),
        }));
        let mut thread = Thread::new(process, entry, 0, 7).unwrap();
        thread.name[..14].copy_from_slice(b"procmgr-server");
        scheduler.threads.push(thread);
    }
    scheduler.last = [scheduler.threads.len() - 1; 32];
    let next = scheduler.choose(false).unwrap();
    assert!(SCHEDULER.lock().replace(scheduler).is_none());
    // SAFETY: all service contexts and capability sets are installed before entry.
    unsafe { arch::resume(next) }
}

fn shell_enabled() -> bool {
    !cfg!(any(
        feature = "usb-diagnostic",
        feature = "test-console",
        feature = "test-fs-create",
        feature = "test-fs-read"
    ))
}
