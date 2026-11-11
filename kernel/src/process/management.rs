use super::*;

impl Scheduler {
    fn target(&self, pid: u64) -> Result<usize, Error> {
        let slot = (pid & 255).checked_sub(1).ok_or(Error::Invalid)? as usize;
        let thread = self.threads.get(slot).ok_or(Error::Invalid)?;
        if thread.generation != pid >> 8 || matches!(thread.state, State::Reaped) {
            return Err(Error::Invalid);
        }
        Ok(slot)
    }

    fn copy_result(&self, id: usize, address: u64, bytes: &[u8]) -> Result<(), Error> {
        self.threads[id]
            .process
            .lock()
            .space
            .write_user(address, bytes)
            .map_err(|_| Error::BadAddress)
    }

    pub(super) fn management(&mut self, id: usize, frame: Frame) -> Result<Option<usize>, Error> {
        if !self.threads[id].process.lock().manager {
            return Err(Error::Denied);
        }
        match frame.rax {
            x if x == Call::ProcessSpawn as u64 => {
                if frame.rsi == 0 || frame.rsi > 65536 || frame.r10 == 0 || frame.r10 > 24 {
                    return Err(Error::TooLarge);
                }
                let slot = self
                    .threads
                    .iter()
                    .position(|t| matches!(t.state, State::Reaped))
                    .unwrap_or(self.threads.len());
                if slot == MAX_THREADS {
                    return Err(Error::Limit);
                }
                let generation = if slot == self.threads.len() {
                    1
                } else {
                    self.threads[slot]
                        .generation
                        .checked_add(1)
                        .filter(|&g| g <= u64::MAX >> 8)
                        .ok_or(Error::Limit)?
                };
                let mut image = vec![0; frame.rsi as usize];
                let mut name = [0; 24];
                {
                    let process = self.threads[id].process.lock();
                    process
                        .space
                        .read_user(frame.rdi, &mut image)
                        .map_err(|_| Error::BadAddress)?;
                    process
                        .space
                        .read_user(frame.rdx, &mut name[..frame.r10 as usize])
                        .map_err(|_| Error::BadAddress)?;
                }
                if name[..frame.r10 as usize]
                    .iter()
                    .any(|b| !(32..=126).contains(b))
                {
                    return Err(Error::Invalid);
                }
                let (space, entry) = elf::load(&image).map_err(|e| match e {
                    elf::Error::Memory => Error::NoMemory,
                    _ => Error::Invalid,
                })?;
                let process = Arc::new(Mutex::new(Process {
                    space,
                    stacks: 0,
                    dynamic_pages: 0,
                    ports: 0,
                    irqs: 0,
                    framebuffer: false,
                    framebuffer_mapped: false,
                    test_control: false,
                    storage_control: false,
                    bulk: true,
                    manager: false,
                    usb: hardware::Grant::new(false),
                }));
                let pid = (generation << 8) | (slot + 1) as u64;
                let mut thread = Thread::new(process, entry, pid, 9)?;
                thread.system = false;
                thread.name = name;
                thread.generation = generation;
                if slot == self.threads.len() {
                    self.threads.push(thread);
                } else {
                    self.threads[slot] = thread;
                }
                crate::logln!(
                    "PROCESS SPAWN: pid={} name={}",
                    pid,
                    core::str::from_utf8(&name[..frame.r10 as usize]).unwrap()
                );
                Ok(Some(pid as usize))
            }
            x if x == Call::ProcessInfo as u64 => {
                if frame.rdx != 50 {
                    return Err(Error::Invalid);
                }
                let slot = frame.rdi as usize;
                let thread = self.threads.get(slot).ok_or(Error::Invalid)?;
                let (state, status) = match thread.state {
                    State::Ready => (0, 0),
                    State::Receiving { .. } | State::Sending { .. } | State::Reply { .. } => (1, 0),
                    State::Exited(status) => (2, status),
                    State::Faulted(_) => (3, -128),
                    State::Reaped => (4, 0),
                };
                let memory = if state == 4 {
                    0
                } else {
                    thread.process.lock().space.resident_bytes()
                        + core::mem::size_of_val(&*thread.stack) as u64
                };
                let mut bytes = [0; 50];
                bytes[..8]
                    .copy_from_slice(&((thread.generation << 8) | (slot + 1) as u64).to_le_bytes());
                bytes[8] = state;
                bytes[9] = thread.system as u8;
                bytes[10..18].copy_from_slice(&memory.to_le_bytes());
                bytes[18..26].copy_from_slice(&status.to_le_bytes());
                bytes[26..50].copy_from_slice(&thread.name);
                self.copy_result(id, frame.rsi, &bytes)?;
                Ok(Some(0))
            }
            x if x == Call::ProcessKill as u64 || x == Call::ProcessReap as u64 => {
                let target = self.target(frame.rdi)?;
                if self.threads[target].system || target == id {
                    return Err(Error::Denied);
                }
                if x == Call::ProcessKill as u64 {
                    if matches!(
                        self.threads[target].state,
                        State::Exited(_) | State::Faulted(_)
                    ) {
                        return Err(Error::Invalid);
                    }
                    self.threads[target].state = State::Exited(-9);
                    self.close_owned(target);
                } else {
                    if !matches!(
                        self.threads[target].state,
                        State::Exited(_) | State::Faulted(_)
                    ) {
                        return Err(Error::Invalid);
                    }
                    // Target is inactive. Share the manager's root only in this
                    // unschedulable tombstone, releasing the old root and stack.
                    self.threads[target].process = self.threads[id].process.clone();
                    self.threads[target].stack = Vec::new().into_boxed_slice();
                    self.threads[target].state = State::Reaped;
                }
                Ok(Some(0))
            }
            x if x == Call::MemoryStats as u64 => {
                if frame.rsi != 32 {
                    return Err(Error::Invalid);
                }
                let stats = memory::stats();
                let mut bytes = [0; 32];
                for (i, n) in [
                    stats.usable_bytes,
                    stats.free_bytes,
                    stats.used_bytes,
                    stats.heap_used_bytes as u64,
                ]
                .into_iter()
                .enumerate()
                {
                    bytes[i * 8..i * 8 + 8].copy_from_slice(&n.to_le_bytes());
                }
                self.copy_result(id, frame.rdi, &bytes)?;
                Ok(Some(0))
            }
            x if x == Call::BootProgram as u64 => {
                let file = crate::boot::MODULES
                    .response()
                    .unwrap()
                    .modules()
                    .iter()
                    .find(|file| file.cmdline() == "zos-command")
                    .ok_or(Error::Invalid)?;
                if file.data().len() > frame.rsi as usize {
                    return Err(Error::TooLarge);
                }
                self.copy_result(id, frame.rdi, file.data())?;
                Ok(Some(file.data().len()))
            }
            x if x == Call::SystemControl as u64 => {
                match frame.rdi {
                    0 => {
                        crate::logln!("ZOS HALTED: filesystem synced");
                        crate::halt()
                    }
                    1 => {
                        crate::logln!("ZOS REBOOT: filesystem synced");
                        // SAFETY: only the process manager can request chipset
                        // reset after synchronizing storage; port is not user-granted.
                        unsafe { x86_64::instructions::port::Port::<u8>::new(0x64).write(0xfe) };
                        crate::halt()
                    }
                    2 if cfg!(feature = "test-shell") => {
                        self.shell_finished = true;
                        for thread in &mut self.threads {
                            assert!(!matches!(thread.state, State::Faulted(_)) || !thread.system);
                            assert!(
                                !matches!(thread.state, State::Exited(s) if s != 0 && thread.system)
                            );
                            thread.state = State::Exited(0);
                        }
                        for owner in 0..self.threads.len() {
                            self.close_owned(owner);
                        }
                        Ok(None)
                    }
                    _ => Err(Error::Denied),
                }
            }
            _ => Err(Error::Invalid),
        }
    }
}
