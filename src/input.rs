//! Lock-free single-producer/single-consumer scancode queue.
//!
//! The keyboard interrupt handler is the only producer and the shell loop is
//! the only consumer. The ISR does no formatting, locking or decoding: it only
//! stores the raw byte. Uses atomics only (no `unsafe`, no allocation).

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

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

/// Raw keyboard scancodes, filled by the keyboard ISR.
pub static SCANCODES: ScancodeQueue = ScancodeQueue::new();

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
