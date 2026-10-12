//! CPU hardening features: SMEP (no executing user pages from ring 0) and
//! SMAP (no accessing user pages from ring 0 outside explicit windows).
//!
//! Both are enabled only if CPUID reports them. Whether they are active is
//! queryable (`smep_enabled`, `smap_enabled`) so the shell and tests can say
//! so honestly instead of assuming.

use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::registers::control::{Cr4, Cr4Flags};

static SMEP: AtomicBool = AtomicBool::new(false);
static SMAP: AtomicBool = AtomicBool::new(false);

pub fn smep_enabled() -> bool {
    SMEP.load(Ordering::Relaxed)
}

pub fn smap_enabled() -> bool {
    SMAP.load(Ordering::Relaxed)
}

/// Enable SMEP/SMAP where the CPU supports them. Call once, early in boot.
pub fn init() {
    use core::arch::x86_64::{__cpuid, __cpuid_count};

    // SAFETY: CPUID is available on every x86_64 CPU.
    let max_leaf = unsafe { __cpuid(0) }.eax;
    if max_leaf < 7 {
        return;
    }
    // SAFETY: leaf 7 is within the reported maximum leaf.
    let ebx = unsafe { __cpuid_count(7, 0) }.ebx;
    let has_smep = ebx & (1 << 7) != 0;
    let has_smap = ebx & (1 << 20) != 0;

    // SAFETY: only sets protection bits that CPUID reported as supported;
    // they restrict ring-0 behavior and change no mappings.
    unsafe {
        Cr4::update(|f| {
            if has_smep {
                f.insert(Cr4Flags::SUPERVISOR_MODE_EXECUTION_PROTECTION);
            }
            if has_smap {
                f.insert(Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION);
            }
        });
    }
    SMEP.store(has_smep, Ordering::Relaxed);
    SMAP.store(has_smap, Ordering::Relaxed);
}

/// Run `f` with supervisor access to user pages allowed (SMAP window).
///
/// # Safety
/// `f` must only touch user memory that was validated by the caller, and must
/// not call anything that could fault or block while the window is open.
pub unsafe fn with_user_access<R>(f: impl FnOnce() -> R) -> R {
    if smap_enabled() {
        // SAFETY: `stac` is valid because SMAP is enabled (CPUID-checked).
        unsafe { core::arch::asm!("stac", options(nomem, nostack)) };
        let r = f();
        // SAFETY: closes the window opened above.
        unsafe { core::arch::asm!("clac", options(nomem, nostack)) };
        r
    } else {
        f()
    }
}
