//! First Rust program in ring 3: prints a line through its console handle.
#![no_std]
#![no_main]

use l_user::{Console, entry, h, println};

fn main() -> u16 {
    let mut out = Console(h::CONSOLE);
    println!(out, "hello from Rust in ring 3");
    0
}

entry!(main);
