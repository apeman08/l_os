//! Exercises the limits of spawn/wait from the inside. Exit code = number of
//! failed checks (0 = all good). Expected handles (set by the test):
//!   0: console (write+grant)   1: spawner (programs 0..=2 only)
//!   2: console (write only, no grant)
#![no_std]
#![no_main]

use l_core::{
    handles::Rights,
    syscall::{ERR_BAD_HANDLE, ERR_INVALID, ERR_NO_RIGHTS, ERR_NO_SLOT, ERR_NOT_CHILD, Outcome},
};
use l_user::{Console, entry, h, println, spawn, wait};

const SPAWNER: u64 = 1;
const HELLO: u64 = 0;
const SPIN: u64 = 2;
const HELLO_RS: u64 = 3; // exists, but is not in this spawner's mask

fn main() -> u16 {
    let mut out = Console(h::CONSOLE);
    let mut failed = 0u16;
    macro_rules! check {
        ($name:expr, $cond:expr) => {
            if !$cond {
                failed += 1;
                println!(out, "check failed: {}", $name);
            }
        };
    }
    let write = Rights::WRITE.bits() as u64;

    let pid = spawn(SPAWNER, HELLO, &[(h::CONSOLE, write)], &[], 18);
    check!("spawn hello", pid > 0);
    check!("child outcome is its exit code", wait(pid as u64) == Outcome::Exited(18).to_raw() as i64);
    check!("a reaped child cannot be waited on again", wait(pid as u64) == ERR_NOT_CHILD);
    check!("waiting on a stranger fails", wait(9999) == ERR_NOT_CHILD);

    check!("program outside the spawner's mask", spawn(SPAWNER, HELLO_RS, &[], &[], 18) == ERR_NO_RIGHTS);
    check!("unknown program id", spawn(SPAWNER, 99, &[], &[], 18) == ERR_NO_RIGHTS);
    check!("a console handle is not a spawner", spawn(h::CONSOLE, HELLO, &[], &[], 18) == ERR_NO_RIGHTS);
    check!("granting needs the GRANT right", spawn(SPAWNER, HELLO, &[(2, write)], &[], 18) == ERR_NO_RIGHTS);
    check!("forged handle in the grant list", spawn(SPAWNER, HELLO, &[(0x777, write)], &[], 18) == ERR_BAD_HANDLE);
    check!("zero budget is invalid", spawn(SPAWNER, SPIN, &[], &[], 0) == ERR_INVALID);

    // Rights narrow: grant the console with an empty mask. The child's write is
    // refused and hello exits with that (negative) result.
    let pid = spawn(SPAWNER, HELLO, &[(h::CONSOLE, 0)], &[], 18);
    check!("spawn with empty rights", pid > 0);
    check!(
        "child holds no right to write",
        wait(pid as u64) == Outcome::Exited(ERR_NO_RIGHTS as u16).to_raw() as i64
    );

    // A child cannot get more CPU time than its parent has left.
    let pid = spawn(SPAWNER, SPIN, &[], &[], u64::MAX);
    check!("spawn spinner", pid > 0);
    check!("spinner is cut off at the parent's budget", wait(pid as u64) == Outcome::Timeout.to_raw() as i64);

    // Fill the process table: this process occupies one slot, so exactly
    // MAX_PROCS - 1 children fit.
    let mut pids = [0i64; 16];
    let mut n = 0;
    let last_err = loop {
        let r = spawn(SPAWNER, SPIN, &[], &[], 3);
        if r < 0 || n == pids.len() {
            break r;
        }
        pids[n] = r;
        n += 1;
    };
    check!("table fills up after 7 children", n == 7);
    check!("then spawn reports no free slot", last_err == ERR_NO_SLOT);
    for &p in &pids[..n] {
        check!("each spinner ends by timeout", wait(p as u64) == Outcome::Timeout.to_raw() as i64);
    }
    // Slots are free again after reaping.
    let pid = spawn(SPAWNER, HELLO, &[(h::CONSOLE, write)], &[], 18);
    check!("slots are reusable after reaping", pid > 0 && wait(pid as u64) >= 0);

    failed
}

entry!(main);
