//! Tries to write to read-only data.
#![no_std]
#![no_main]

use l_user::entry;

static CONSTANT: [u8; 8] = *b"readonly";

fn main() -> u16 {
    let p = core::hint::black_box(CONSTANT.as_ptr()) as *mut u8;
    // SAFETY: deliberately writing read-only memory; the kernel must kill us.
    unsafe { p.write_volatile(b'X') };
    0
}

entry!(main);
