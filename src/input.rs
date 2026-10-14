//! Lock-free scancode queue plus the kernel-side emergency hotkey.
//!
//! Producers: every raw set-1 byte enters through [`feed`], called only from
//! interrupt context with IF=0: the PS/2 keyboard ISR today, the timer ISR
//! (USB HID keyboard, M2 Task 22) later. Both run on the single CPU with
//! interrupts off, so they never run at the same time and the queue keeps its
//! single-producer contract. Interleaving caveat: each producer feeds its
//! bytes of one event in one ISR run, but a PS/2 key with an `E0` prefix
//! arrives as two IRQs, so USB bytes fed by a timer tick in between can
//! split it (and the shared hotkey detector sees the mixed stream). Typing
//! on both keyboards at the same moment can thus misdecode one key; with
//! one keyboard in use (the normal case) the streams never mix. The USB
//! path can feed up to 640 bytes per tick (see `crate::xhci`); what does
//! not fit into [`QUEUE_CAPACITY`] is dropped and counted.
//! The only consumer is the shell's `read_key`
//! path. Producers do no formatting, locking or decoding beyond the hotkey
//! detector (spec E6). Uses atomics only (no `unsafe`, no allocation).

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use l_core::hotkey::HotkeyDetector;
use l_core::wipe::Trigger;

pub const QUEUE_CAPACITY: usize = 128;

pub struct ScancodeQueue {
    slots: [AtomicU8; QUEUE_CAPACITY],
    /// Total number of items ever pushed (producer-owned).
    head: AtomicUsize,
    /// Total number of items ever popped (consumer-owned).
    tail: AtomicUsize,
    dropped: AtomicUsize,
}

impl ScancodeQueue {
    pub const fn new() -> Self {
        Self {
            slots: [const { AtomicU8::new(0) }; QUEUE_CAPACITY],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        }
    }

    /// Producer side. Returns `false` (and counts a drop) if the queue is full.
    pub fn push(&self, byte: u8) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= QUEUE_CAPACITY {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        self.slots[head % QUEUE_CAPACITY].store(byte, Ordering::Relaxed);
        self.head.store(head.wrapping_add(1), Ordering::Release);
        true
    }

    /// Consumer side.
    pub fn pop(&self) -> Option<u8> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let byte = self.slots[tail % QUEUE_CAPACITY].load(Ordering::Relaxed);
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(byte)
    }

    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire) == self.tail.load(Ordering::Acquire)
    }

    pub fn dropped(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Raw keyboard scancodes, filled by [`feed`].
pub static SCANCODES: ScancodeQueue = ScancodeQueue::new();

/// State of the emergency hotkey detector between bytes
/// (`HotkeyDetector::to_bits`). Only [`feed`] touches it, with IF=0.
static HOTKEY: AtomicU8 = AtomicU8::new(HotkeyDetector::new().to_bits());

/// Take one raw set-1 scancode byte from a keyboard (interrupt context, IF=0
/// only): queue it for the shell, then run the emergency hotkey detector
/// (Ctrl+Alt+Backspace, spec E6). On a hit the wipe starts here and never
/// returns ([`crate::wipe::trigger`]; in `cfg(test)` and with the feature
/// `test-hooks` it is only counted). The byte is queued first: the queue is
/// zeroed with the rest of memory anyway.
pub fn feed(byte: u8) {
    SCANCODES.push(byte);
    let mut detector = HotkeyDetector::from_bits(HOTKEY.load(Ordering::Relaxed));
    let hit = detector.feed(byte);
    HOTKEY.store(detector.to_bits(), Ordering::Relaxed);
    if hit {
        crate::wipe::trigger(Trigger::Hotkey);
    }
}

#[test_case]
fn queue_is_fifo() {
    let q = ScancodeQueue::new();
    assert!(q.pop().is_none());
    for b in 1..=5u8 {
        assert!(q.push(b));
    }
    for b in 1..=5u8 {
        assert_eq!(q.pop(), Some(b));
    }
    assert!(q.is_empty());
}

#[test_case]
fn queue_drops_when_full_and_recovers() {
    let q = ScancodeQueue::new();
    for i in 0..QUEUE_CAPACITY {
        assert!(q.push(i as u8));
    }
    assert!(!q.push(0xff));
    assert_eq!(q.dropped(), 1);
    assert_eq!(q.pop(), Some(0));
    assert!(q.push(0xaa));
    // Oldest data is preserved; the overflowing byte was the one lost.
    for i in 1..QUEUE_CAPACITY {
        assert_eq!(q.pop(), Some(i as u8));
    }
    assert_eq!(q.pop(), Some(0xaa));
    assert!(q.pop().is_none());
}
