//! `int 0x80` system-call dispatcher. Everything a user program passes in is
//! untrusted: capabilities are checked first, then every pointer is validated
//! against the page tables before the kernel dereferences it.

use crate::{ipc, memory, sched, usermode};
use l_core::{
    handles::{HandleError, NO_HANDLE, Object, Rights},
    ipc::{MAX_MSG, Message},
    syscall::{self as abi, Outcome},
};
use x86_64::{
    VirtAddr,
    structures::paging::{PageTableFlags, Translate, mapper::TranslateResult},
};

/// True if every byte of `[ptr, ptr+len)` is in user space and every page it
/// touches is mapped present *and* user-accessible in the running process's
/// page tables.
fn user_buffer_ok(ptr: u64, len: u64) -> bool {
    if !abi::range_in_user_space(ptr, len) {
        return false;
    }
    let first = ptr & !0xfff;
    let last = (ptr + len - 1) & !0xfff;
    memory::with_active_tables(|tables| {
        let mut page = first;
        loop {
            match tables.translate(VirtAddr::new(page)) {
                TranslateResult::Mapped { flags, .. }
                    if flags.contains(PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE) => {}
                _ => return false,
            }
            if page == last {
                return true;
            }
            page += 4096;
        }
    })
    .unwrap_or(false)
}

/// Like [`user_buffer_ok`] but every page must also be writable.
fn user_buffer_writable(ptr: u64, len: u64) -> bool {
    if !user_buffer_ok(ptr, len) {
        return false;
    }
    let first = ptr & !0xfff;
    let last = (ptr + len - 1) & !0xfff;
    memory::with_active_tables(|tables| {
        let mut page = first;
        loop {
            match tables.translate(VirtAddr::new(page)) {
                TranslateResult::Mapped { flags, .. } if flags.contains(PageTableFlags::WRITABLE) => {}
                _ => return false,
            }
            if page == last {
                return true;
            }
            page += 4096;
        }
    })
    .unwrap_or(false)
}

/// Copy `len` validated user bytes into `out[..len]`.
fn copy_from_user(ptr: u64, len: u64, out: &mut [u8]) -> bool {
    if len as usize > out.len() || !user_buffer_ok(ptr, len) {
        return false;
    }
    // SAFETY: user_buffer_ok verified the whole range is mapped and readable
    // in the active address space (the calling process's); single CPU with
    // interrupts disabled in the int gate, so the mapping cannot change.
    // The SMAP window covers exactly this copy (a plain memcpy of validated memory).
    unsafe {
        crate::cpu::with_user_access(|| {
            core::ptr::copy_nonoverlapping(ptr as *const u8, out.as_mut_ptr(), len as usize)
        })
    };
    true
}

/// Copy `data` to validated, writable user memory.
fn copy_to_user(ptr: u64, data: &[u8]) -> bool {
    if data.is_empty() || !user_buffer_writable(ptr, data.len() as u64) {
        return false;
    }
    // SAFETY: user_buffer_writable verified the range is mapped, user-accessible
    // and writable in the running process's tables; the SMAP window covers
    // exactly this memcpy.
    unsafe {
        crate::cpu::with_user_access(|| {
            core::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len())
        })
    };
    true
}

fn handle_err(e: HandleError) -> i64 {
    match e {
        HandleError::Invalid => abi::ERR_BAD_HANDLE,
        HandleError::InsufficientRights => abi::ERR_NO_RIGHTS,
        HandleError::TableFull => abi::ERR_TABLE_FULL,
    }
}

/// Resolve `handle` in the caller's table, requiring `needed` rights.
fn lookup(handle: u64, needed: Rights) -> Result<(Object, Rights), i64> {
    sched::with_handles(|t| t.lookup(handle, needed))
        .ok_or(abi::ERR_BAD_HANDLE)?
        .map_err(handle_err)
}

#[unsafe(no_mangle)]
pub extern "C" fn syscall_dispatch(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    if !usermode::user_process_active() {
        // The int 0x80 gate is DPL 3, but with no user process this is a
        // kernel bug (kernel code never issues syscalls).
        panic!("syscall {} issued with no user process", num);
    }
    match num {
        abi::SYS_YIELD => {
            sched::yield_current();
            0
        }
        abi::SYS_EXIT => usermode::terminate_current(Outcome::Exited(a0 as u16)),
        abi::SYS_CLOSE => match sched::with_handles(|t| t.close(a0)) {
            Some(Ok(object)) => {
                if let (Object::Socket(slot), Some(pid)) = (object, sched::current_pid()) {
                    crate::net::close(slot, pid, false);
                }
                0
            }
            Some(Err(e)) => handle_err(e),
            None => abi::ERR_BAD_HANDLE,
        },
        abi::SYS_WRITE => sys_write(a0, a1, a2),
        abi::SYS_NET_CHECK => sys_net_check(a0, a1, a2, a3),
        abi::SYS_SEND => sys_send(a0, a1, a2, a3, a4),
        abi::SYS_RECV => sys_recv(a0, a1, a2),
        abi::SYS_SPAWN => sys_spawn(a0, a1, a2, a3, a4),
        abi::SYS_WAIT => sys_wait(a0),
        abi::SYS_READ_KEY => sys_read_key(a0),
        abi::SYS_SYSINFO => sys_sysinfo(a0, a1),
        abi::SYS_RANDOM => sys_random(a0, a1, a2),
        abi::SYS_NET_CONNECT => sys_net_connect(a0, a1, a2, a3),
        abi::SYS_SOCK_SEND => sys_sock_send(a0, a1, a2),
        abi::SYS_SOCK_RECV => sys_sock_recv(a0, a1, a2),
        abi::SYS_NETCTL => sys_netctl(a0, a1, a2, a3, a4),
        _ => abi::ERR_NO_SYSCALL,
    }
}

fn sys_write(handle: u64, ptr: u64, len: u64) -> i64 {
    match lookup(handle, Rights::WRITE) {
        Ok((Object::Console, _)) => {}
        Ok(_) => return abi::ERR_NO_RIGHTS, // a handle, but not a console
        Err(e) => return e,
    }
    if len > abi::MAX_WRITE_LEN {
        return abi::ERR_INVALID;
    }
    let mut buf = [0u8; abi::MAX_WRITE_LEN as usize];
    if !copy_from_user(ptr, len, &mut buf) {
        return abi::ERR_BAD_POINTER;
    }
    for &b in &buf[..len as usize] {
        match b {
            // Terminal controls a console holder is allowed to use.
            0x08 => crate::vga_buffer::backspace(),
            0x0c => crate::vga_buffer::clear_screen(),
            // Anything else non-printable is shown as '?', never interpreted.
            0x20..=0x7e | b'\n' => crate::print!("{}", b as char),
            _ => crate::print!("?"),
        }
    }
    len as i64
}

fn sys_net_check(handle: u64, ptr: u64, len: u64, port: u64) -> i64 {
    match lookup(handle, Rights::USE) {
        Ok((Object::NetClient, _)) => {}
        Ok(_) => return abi::ERR_NO_RIGHTS,
        Err(e) => return e,
    }
    if len == 0 || len > abi::MAX_HOST_ARG_LEN || port > u16::MAX as u64 {
        return abi::ERR_INVALID;
    }
    let mut buf = [0u8; abi::MAX_HOST_ARG_LEN as usize];
    if !copy_from_user(ptr, len, &mut buf) {
        return abi::ERR_BAD_POINTER;
    }
    let Ok(host) = core::str::from_utf8(&buf[..len as usize]) else {
        return abi::ERR_INVALID;
    };
    let decision = usermode::POLICY.lock().check(host, port as u16, crate::time::ticks());
    abi::decision_to_ret(decision)
}

fn sys_send(ep: u64, ptr: u64, len: u64, xfer: u64, mask: u64) -> i64 {
    let id = match lookup(ep, Rights::SEND) {
        Ok((Object::Endpoint(id), _)) => id,
        Ok(_) => return abi::ERR_NO_RIGHTS,
        Err(e) => return e,
    };
    if len > MAX_MSG as u64 {
        return abi::ERR_INVALID;
    }
    let mut buf = [0u8; MAX_MSG];
    if len > 0 && !copy_from_user(ptr, len, &mut buf) {
        return abi::ERR_BAD_POINTER;
    }
    // Delegation: the handle must carry GRANT; what arrives is rights ∩ mask.
    let carried = if xfer == NO_HANDLE {
        None
    } else {
        match sched::with_handles(|t| t.derive(xfer, Rights::from_bits(mask))) {
            Some(Ok(pair)) => Some(pair),
            Some(Err(e)) => return handle_err(e),
            None => return abi::ERR_BAD_HANDLE,
        }
    };
    let Some(msg) = Message::new(&buf[..len as usize], carried) else {
        return abi::ERR_INVALID;
    };
    match ipc::send(id, msg) {
        Ok(()) => {
            sched::wake_endpoint(id);
            0
        }
        Err(ipc::SendError::Full) => abi::ERR_WOULD_BLOCK,
        Err(ipc::SendError::NoSuchEndpoint) => abi::ERR_BAD_HANDLE,
    }
}

fn sys_recv(ep: u64, ptr: u64, max_len: u64) -> i64 {
    let id = match lookup(ep, Rights::RECV) {
        Ok((Object::Endpoint(id), _)) => id,
        Ok(_) => return abi::ERR_NO_RIGHTS,
        Err(e) => return e,
    };
    if max_len > MAX_MSG as u64 {
        return abi::ERR_INVALID;
    }
    loop {
        if let Some(msg) = ipc::peek(id) {
            // Check everything that could fail BEFORE removing the message.
            if (msg.len as u64) > max_len {
                return abi::ERR_INVALID;
            }
            if msg.len > 0 && !user_buffer_writable(ptr, msg.len as u64) {
                return abi::ERR_BAD_POINTER;
            }
            let table_full = msg.xfer.is_some() && sched::with_handles(|t| t.count() >= l_core::handles::MAX_HANDLES) == Some(true);
            if table_full {
                return abi::ERR_TABLE_FULL;
            }
            let Some(msg) = ipc::pop(id) else { continue };
            if msg.len > 0 {
                copy_to_user(ptr, msg.bytes());
            }
            let mut ret = msg.len as i64;
            if let Some((object, rights)) = msg.xfer {
                match sched::with_handles(|t| t.insert(object, rights)) {
                    Some(Ok(h)) => ret |= (1 << 16) | ((h as i64) << 32),
                    _ => return abi::ERR_TABLE_FULL,
                }
            }
            return ret;
        }
        // Nothing queued: sleep until a sender wakes us (or we are found to be
        // deadlocked and ended, in which case this call never returns).
        sched::block_on_recv(id);
    }
}

fn sys_read_key(handle: u64) -> i64 {
    match lookup(handle, Rights::READ) {
        Ok((Object::Keyboard, _)) => {}
        Ok(_) => return abi::ERR_NO_RIGHTS,
        Err(e) => return e,
    }
    loop {
        if let Some(sc) = crate::input::SCANCODES.pop() {
            return sc as i64;
        }
        // Sleep until the keyboard interrupt queues a scancode.
        sched::block_on_key();
    }
}

fn sys_sysinfo(handle: u64, what: u64) -> i64 {
    match lookup(handle, Rights::READ) {
        Ok((Object::System, _)) => {}
        Ok(_) => return abi::ERR_NO_RIGHTS,
        Err(e) => return e,
    }
    match what {
        abi::SYSINFO_TICKS => crate::time::ticks().min(i64::MAX as u64) as i64,
        abi::SYSINFO_CPU => {
            (if crate::cpu::smep_enabled() { abi::CPU_SMEP } else { 0 }
                | if crate::cpu::smap_enabled() { abi::CPU_SMAP } else { 0 }) as i64
        }
        abi::SYSINFO_WALL_CLOCK => match crate::rtc::unix_time() {
            Some(t) => t as i64,
            None => abi::ERR_NO_CLOCK,
        },
        _ => abi::ERR_INVALID,
    }
}

fn sys_spawn(spawner: u64, program: u64, grants_ptr: u64, packed: u64, budget: u64) -> i64 {
    // `packed`: low 8 bits = number of grants, the rest = length of the
    // argument bytes that follow the grant list in the same user buffer.
    let grants_len = packed & 0xff;
    let args_len = packed >> 8;
    let mask = match lookup(spawner, Rights::USE) {
        Ok((Object::Spawner(mask), _)) => mask,
        Ok(_) => return abi::ERR_NO_RIGHTS,
        Err(e) => return e,
    };
    // The spawner names exactly which programs its holder may start.
    if program >= usermode::PROGRAM_COUNT as u64 || mask & (1 << program) == 0 {
        return abi::ERR_NO_RIGHTS;
    }
    let Some(image) = usermode::builtin(program as u16) else { return abi::ERR_INVALID };
    if grants_len > abi::MAX_SPAWN_GRANTS || budget == 0 || args_len as usize > usermode::ARGS_MAX {
        return abi::ERR_INVALID;
    }
    // Read the grant list ((parent_handle, rights_mask) pairs of u64) and the
    // arguments that follow it.
    let mut raw = [0u8; abi::MAX_SPAWN_GRANTS as usize * 16 + usermode::ARGS_MAX];
    let bytes = grants_len as usize * 16 + args_len as usize;
    if bytes > 0 && !copy_from_user(grants_ptr, bytes as u64, &mut raw) {
        return abi::ERR_BAD_POINTER;
    }
    let args_start = grants_len as usize * 16;
    // Derive every grant from the parent's own table: needs GRANT, only narrows.
    let mut granted = [(Object::Console, Rights::NONE); abi::MAX_SPAWN_GRANTS as usize];
    for i in 0..grants_len as usize {
        let word = |o: usize| u64::from_le_bytes(raw[i * 16 + o..i * 16 + o + 8].try_into().unwrap_or([0; 8]));
        let (handle, rmask) = (word(0), word(8));
        match sched::with_handles(|t| t.derive(handle, Rights::from_bits(rmask))) {
            Some(Ok(pair)) => granted[i] = pair,
            Some(Err(e)) => return handle_err(e),
            None => return abi::ERR_BAD_HANDLE,
        }
    }
    let Some(parent) = sched::current_pid() else { return abi::ERR_BAD_HANDLE };
    // A child can never be given more CPU time than its parent has left.
    let budget = budget.min(sched::current_remaining_budget());
    match usermode::spawn_child(parent, image, &granted[..grants_len as usize], budget, &raw[args_start..args_start + args_len as usize]) {
        Ok(pid) => pid as i64,
        Err(e) => e,
    }
}

fn sys_wait(pid: u64) -> i64 {
    let Some(me) = sched::current_pid() else { return abi::ERR_BAD_HANDLE };
    let Ok(pid) = u32::try_from(pid) else { return abi::ERR_NOT_CHILD };
    loop {
        match sched::reap_child(me, pid) {
            sched::Reap::NotChild => return abi::ERR_NOT_CHILD,
            sched::Reap::Done(child) => {
                let raw = child.outcome().unwrap_or(Outcome::GeneralProtection).to_raw();
                usermode::release(child);
                return raw as i64;
            }
            sched::Reap::StillRunning => sched::block_on_child(pid),
        }
    }
}

fn sys_random(handle: u64, ptr: u64, len: u64) -> i64 {
    match lookup(handle, Rights::READ) {
        Ok((Object::Random, _)) => {}
        Ok(_) => return abi::ERR_NO_RIGHTS,
        Err(e) => return e,
    }
    if len == 0 || len > abi::MAX_RANDOM_LEN {
        return abi::ERR_INVALID;
    }
    let mut buf = [0u8; abi::MAX_RANDOM_LEN as usize];
    if !crate::rng::fill(&mut buf[..len as usize]) {
        return abi::ERR_NO_ENTROPY; // never substitute weaker bytes
    }
    if !copy_to_user(ptr, &buf[..len as usize]) {
        return abi::ERR_BAD_POINTER;
    }
    len as i64
}

fn sys_net_connect(handle: u64, ptr: u64, len: u64, port: u64) -> i64 {
    match lookup(handle, Rights::USE) {
        Ok((Object::NetClient, _)) => {}
        Ok(_) => return abi::ERR_NO_RIGHTS,
        Err(e) => return e,
    }
    if len == 0 || len > abi::MAX_HOST_ARG_LEN || port == 0 || port > u16::MAX as u64 {
        return abi::ERR_INVALID;
    }
    let mut buf = [0u8; abi::MAX_HOST_ARG_LEN as usize];
    if !copy_from_user(ptr, len, &mut buf) {
        return abi::ERR_BAD_POINTER;
    }
    let Ok(host) = core::str::from_utf8(&buf[..len as usize]) else { return abi::ERR_INVALID };
    let Some(pid) = sched::current_pid() else { return abi::ERR_BAD_HANDLE };

    // The policy check, DNS, bring-up and the TCP handshake all happen inside.
    let slot = match crate::net::connect(host, port as u16, pid) {
        Ok(slot) => slot,
        Err(e) => return e,
    };
    match sched::with_handles(|t| t.insert(Object::Socket(slot), Rights::READ.union(Rights::WRITE))) {
        Some(Ok(h)) => h as i64,
        _ => {
            crate::net::close(slot, pid, true);
            abi::ERR_TABLE_FULL
        }
    }
}

const SOCK_IO_MAX: usize = 2048;

fn socket_slot(handle: u64, needed: Rights) -> Result<u16, i64> {
    match lookup(handle, needed) {
        Ok((Object::Socket(slot), _)) => Ok(slot),
        Ok(_) => Err(abi::ERR_NO_RIGHTS),
        Err(e) => Err(e),
    }
}

fn sys_sock_send(handle: u64, ptr: u64, len: u64) -> i64 {
    let slot = match socket_slot(handle, Rights::WRITE) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if len == 0 || len > SOCK_IO_MAX as u64 {
        return abi::ERR_INVALID;
    }
    let mut buf = [0u8; SOCK_IO_MAX];
    if !copy_from_user(ptr, len, &mut buf) {
        return abi::ERR_BAD_POINTER;
    }
    let Some(pid) = sched::current_pid() else { return abi::ERR_BAD_HANDLE };
    match crate::net::send(slot, pid, &buf[..len as usize]) {
        Ok(n) => n as i64,
        Err(e) => e,
    }
}

fn sys_sock_recv(handle: u64, ptr: u64, max_len: u64) -> i64 {
    let slot = match socket_slot(handle, Rights::READ) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if max_len == 0 || max_len > SOCK_IO_MAX as u64 {
        return abi::ERR_INVALID;
    }
    if !user_buffer_writable(ptr, max_len) {
        return abi::ERR_BAD_POINTER;
    }
    let mut buf = [0u8; SOCK_IO_MAX];
    let Some(pid) = sched::current_pid() else { return abi::ERR_BAD_HANDLE };
    match crate::net::recv(slot, pid, &mut buf[..max_len as usize]) {
        Ok(0) => 0,
        Ok(n) => {
            if !copy_to_user(ptr, &buf[..n]) {
                return abi::ERR_BAD_POINTER;
            }
            n as i64
        }
        Err(e) => e,
    }
}

/// Copy `text` into the user buffer (truncated to `len`); returns bytes written.
fn put_text(ptr: u64, len: u64, text: &str) -> i64 {
    let n = text.len().min(len as usize);
    if n == 0 {
        return 0;
    }
    if copy_to_user(ptr, &text.as_bytes()[..n]) { n as i64 } else { abi::ERR_BAD_POINTER }
}

fn sys_netctl(handle: u64, op: u64, ptr: u64, len: u64, arg: u64) -> i64 {
    match lookup(handle, Rights::USE) {
        Ok((Object::NetManager, _)) => {}
        Ok(_) => return abi::ERR_NO_RIGHTS,
        Err(e) => return e,
    }
    // Operations that take a host string in (ptr, len).
    let read_host = |buf: &mut [u8; 253]| -> Result<usize, i64> {
        if len == 0 || len > 253 {
            return Err(abi::ERR_INVALID);
        }
        if !copy_from_user(ptr, len, buf) {
            return Err(abi::ERR_BAD_POINTER);
        }
        Ok(len as usize)
    };
    match op {
        abi::NETCTL_STATUS => {
            let st = crate::net::status();
            ((st.nic_present as u64 * abi::NET_STATUS_PRESENT) | (st.nic_up as u64 * abi::NET_STATUS_UP)) as i64
        }
        abi::NETCTL_DEVICE => match crate::net::describe_device(arg as usize) {
            Some(line) => put_text(ptr, len, &line),
            None => abi::ERR_INVALID,
        },
        abi::NETCTL_GRANT => {
            let mut buf = [0u8; 253];
            let n = match read_host(&mut buf) {
                Ok(n) => n,
                Err(e) => return e,
            };
            let Ok(host) = core::str::from_utf8(&buf[..n]) else { return abi::ERR_INVALID };
            let (port, duration) = ((arg & 0xffff) as u16, arg >> 16);
            let expires = if duration == 0 { None } else { Some(crate::time::ticks().saturating_add(duration)) };
            match usermode::POLICY.lock().grant(host, port, expires) {
                Ok(()) => 0,
                Err(l_core::policy::PolicyError::TableFull) => abi::ERR_TABLE_FULL,
                Err(_) => abi::ERR_INVALID,
            }
        }
        abi::NETCTL_REVOKE => {
            let mut buf = [0u8; 253];
            let n = match read_host(&mut buf) {
                Ok(n) => n,
                Err(e) => return e,
            };
            let Ok(host) = core::str::from_utf8(&buf[..n]) else { return abi::ERR_INVALID };
            let existed = usermode::POLICY.lock().revoke(host, (arg & 0xffff) as u16);
            crate::net::policy_changed(); // resets connections that lost their grant
            existed as i64
        }
        abi::NETCTL_REVOKE_ALL => {
            usermode::POLICY.lock().revoke_all();
            crate::net::policy_changed();
            0
        }
        abi::NETCTL_LIST => {
            let now = crate::time::ticks();
            // Format while holding the lock, write to user memory after releasing it.
            let text = {
                let policy = usermode::POLICY.lock();
                policy.grants().nth(arg as usize).map(|(host, port, expires)| match expires {
                    None => alloc::format!("{}:{} until revoked", host, port),
                    Some(t) => alloc::format!(
                        "{}:{} for {} more s",
                        host,
                        port,
                        l_core::time::approx_seconds(t.saturating_sub(now))
                    ),
                })
            };
            match text {
                Some(text) => put_text(ptr, len, &text),
                None => abi::ERR_INVALID,
            }
        }
        abi::NETCTL_STATS => {
            let (tx, rx) = crate::virtio_net::counters();
            ((tx & 0xffff_ffff) | (rx & 0x7fff_ffff) << 32) as i64
        }
        abi::NETCTL_SET_RESOLVER => {
            let mut buf = [0u8; 253];
            let n = match read_host(&mut buf) {
                Ok(n) => n,
                Err(e) => return e,
            };
            let Some(ip) = core::str::from_utf8(&buf[..n]).ok().and_then(crate::net::parse_ipv4) else {
                return abi::ERR_INVALID;
            };
            if arg == 0 || arg > u16::MAX as u64 {
                return abi::ERR_INVALID;
            }
            crate::net::set_resolver(ip, arg as u16);
            0
        }
        abi::NETCTL_GET_RESOLVER => {
            let (ip, port) = crate::net::resolver();
            put_text(ptr, len, &alloc::format!("{}.{}.{}.{}:{}", ip[0], ip[1], ip[2], ip[3], port))
        }
        _ => abi::ERR_INVALID,
    }
}
