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
            Some(Ok(())) => 0,
            Some(Err(e)) => handle_err(e),
            None => abi::ERR_BAD_HANDLE,
        },
        abi::SYS_WRITE => sys_write(a0, a1, a2),
        abi::SYS_NET_CHECK => sys_net_check(a0, a1, a2, a3),
        abi::SYS_SEND => sys_send(a0, a1, a2, a3, a4),
        abi::SYS_RECV => sys_recv(a0, a1, a2),
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
        // Only printable ASCII and newline reach the screen.
        let c = if (0x20..=0x7e).contains(&b) || b == b'\n' { b } else { b'?' };
        crate::print!("{}", c as char);
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
