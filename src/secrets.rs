//! Key slots: the one place where long-lived secrets (e.g. Tor keys in M3)
//! must live so that wipe stage `KeysZeroed` (spec §8.2) erases them first.
//!
//! The slots are a page-aligned `static` in `.bss` (zero at boot, never in a
//! `Usable` frame). In M2 production code leaves them empty; the feature
//! `wipe-test` fills slot 0 (Task 10). The wipe zeroes them through their
//! virtual addresses ([`slot_ranges`]) without taking [`LOCK`]: it never
//! returns to the code it interrupted, so a holder of the lock never sees the
//! slot again.

use core::cell::UnsafeCell;
use spin::Mutex;
use x86_64::VirtAddr;

pub const SLOT_SIZE: usize = 4096;
pub const SLOTS: usize = 4;

#[repr(C, align(4096))]
struct Slots(UnsafeCell<[[u8; SLOT_SIZE]; SLOTS]>);

// SAFETY: all access from normal kernel code goes through `with_slot`, which
// holds `LOCK` for the whole borrow; the only other writer is the wipe, which
// runs with interrupts off on the single CPU and never returns to code that
// could still hold a borrow.
unsafe impl Sync for Slots {}

static SLOT_MEM: Slots = Slots(UnsafeCell::new([[0; SLOT_SIZE]; SLOTS]));
static LOCK: Mutex<()> = Mutex::new(());

/// Virtual address and length of every slot, in slot order.
pub fn slot_ranges() -> [(VirtAddr, usize); SLOTS] {
    let base = SLOT_MEM.0.get() as u64;
    core::array::from_fn(|i| (VirtAddr::new(base + (i * SLOT_SIZE) as u64), SLOT_SIZE))
}

/// Run `f` with exclusive access to slot `i`; `None` if `i >= SLOTS`.
pub fn with_slot<R>(i: usize, f: impl FnOnce(&mut [u8; SLOT_SIZE]) -> R) -> Option<R> {
    if i >= SLOTS {
        return None;
    }
    let _guard = LOCK.lock();
    // SAFETY: `LOCK` is held until `f` returns, so this is the only live
    // reference into `SLOT_MEM` from normal code (see `Sync` above); `i` is in
    // bounds, and the pointer comes from the static's `UnsafeCell`.
    let slot = unsafe { &mut (*SLOT_MEM.0.get())[i] };
    Some(f(slot))
}
