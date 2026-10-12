//! Tries to execute code from its stack: the stack is mapped no-execute.
#![no_std]
#![no_main]

use l_user::entry;

fn main() -> u16 {
    let mut buf = [0xc3u8; 16]; // `ret`
    let p = core::hint::black_box(buf.as_mut_ptr());
    // SAFETY: deliberately jumping to non-executable memory; must fault.
    let f: extern "C" fn() = unsafe { core::mem::transmute(p) };
    f();
    0
}

entry!(main);
