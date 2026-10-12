//! Reads three scancodes through handle 0 and exits with their sum.
//! (With a wrong handle the three errors add up to a recognisable value.)
#![no_std]
#![no_main]

use l_user::{entry, h, read_key};

fn main() -> u16 {
    let a = read_key(h::CONSOLE); // slot 0: whatever the test put there
    let b = read_key(h::CONSOLE);
    let c = read_key(h::CONSOLE);
    (a + b + c) as u16
}

entry!(main);
