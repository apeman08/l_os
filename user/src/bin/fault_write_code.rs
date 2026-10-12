//! Tries to overwrite its own code page: the loader maps it read+execute only.
#![no_std]
#![no_main]

use l_user::entry;

fn main() -> u16 {
    let code = core::hint::black_box(main as *const () as *mut u8);
    // SAFETY: deliberately violating W^X; the kernel must kill this process.
    unsafe { code.write_volatile(0xc3) };
    0
}

entry!(main);
