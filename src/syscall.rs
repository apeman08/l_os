//! `int 0x80` system-call dispatcher. Everything a user program passes in is
//! untrusted: capabilities are checked first, then every pointer is validated
//! against the page tables before the kernel dereferences it.

use crate::memory::MEMORY;
use crate::usermode;
use l_core::{
    caps::Cap,
    syscall::{self as abi, Outcome},
};
use x86_64::{
    VirtAddr,
    structures::paging::{PageTableFlags, Translate, mapper::TranslateResult},
};

/// True if every byte of `[ptr, ptr+len)` is in user space and every page it
/// touches is mapped present *and* user-accessible in the current tables.
fn user_buffer_ok(ptr: u64, len: u64) -> bool {
    if !abi::range_in_user_space(ptr, len) {
        return false;
    }
    let guard = MEMORY.lock();
    let Some(mem) = guard.as_ref() else { return false };
    let first = ptr & !0xfff;
    let last = (ptr + len - 1) & !0xfff;
    let mut page = first;
    loop {
        match mem.mapper.translate(VirtAddr::new(page)) {
            TranslateResult::Mapped { flags, .. }
                if flags.contains(PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE) => {}
            _ => return false,
        }
        if page == last {
            return true;
        }
        page += 4096;
    }
}

/// Copy `len` validated user bytes into `out[..len]`.
fn copy_from_user(ptr: u64, len: u64, out: &mut [u8]) -> bool {
    if len as usize > out.len() || !user_buffer_ok(ptr, len) {
        return false;
    }
    // SAFETY: user_buffer_ok verified the whole range is mapped and readable
    // in the active address space; single CPU with interrupts disabled in the
    // int gate, so the mapping cannot change underneath us.
    unsafe { core::ptr::copy_nonoverlapping(ptr as *const u8, out.as_mut_ptr(), len as usize) };
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn syscall_dispatch(num: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    if !usermode::user_process_active() {
        // The int 0x80 gate is DPL 3, but with no user process this is a
        // kernel bug (kernel code never issues syscalls).
        panic!("syscall {} issued with no user process", num);
    }
    let caps = usermode::current_caps();
    match num {
        abi::SYS_EXIT => usermode::terminate_current(Outcome::Exited(a0 as u16)),
        abi::SYS_WRITE => {
            if !caps.has(Cap::Console) {
                return abi::ERR_NO_CAP;
            }
            if a1 > abi::MAX_WRITE_LEN {
                return abi::ERR_INVALID;
            }
            let mut buf = [0u8; abi::MAX_WRITE_LEN as usize];
            if !copy_from_user(a0, a1, &mut buf) {
                return abi::ERR_BAD_POINTER;
            }
            for &b in &buf[..a1 as usize] {
                // Only printable ASCII and newline reach the screen.
                let c = if (0x20..=0x7e).contains(&b) || b == b'\n' { b } else { b'?' };
                crate::print!("{}", c as char);
            }
            a1 as i64
        }
        abi::SYS_NET_CHECK => {
            if !caps.has(Cap::Net) {
                return abi::ERR_NO_CAP;
            }
            if a1 == 0 || a1 > abi::MAX_HOST_ARG_LEN || a2 > u16::MAX as u64 {
                return abi::ERR_INVALID;
            }
            let mut buf = [0u8; abi::MAX_HOST_ARG_LEN as usize];
            if !copy_from_user(a0, a1, &mut buf) {
                return abi::ERR_BAD_POINTER;
            }
            let Ok(host) = core::str::from_utf8(&buf[..a1 as usize]) else {
                return abi::ERR_INVALID;
            };
            let decision = usermode::POLICY.lock().check(host, a2 as u16, crate::time::ticks());
            abi::decision_to_ret(decision)
        }
        _ => abi::ERR_NO_SYSCALL,
    }
}
