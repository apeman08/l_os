//! Capability handles and IPC: forged/closed handles, delegation with
//! attenuation, blocking receive and deadlock cleanup.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use l_core::{
    handles::{Object, Rights},
    syscall::{ERR_BAD_HANDLE, ERR_NO_RIGHTS, Outcome},
};
use l_os::{
    ipc,
    usermode::{self, Program, RunError, Spec},
};

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    l_os::init();
    l_os::init_memory(boot_info);
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

const BUDGET: u64 = 100;
const MSG_LEN: u16 = 4; // "hi!\n"

fn ep() -> u16 {
    ipc::create_endpoint().expect("endpoint")
}

fn console(extra: Rights) -> (Object, Rights) {
    (Object::Console, Rights::WRITE.union(extra))
}

#[test_case]
fn closed_handle_cannot_be_used() {
    let r = usermode::run(usermode::prog_use_after_close(), &[console(Rights::NONE)], BUDGET).unwrap();
    assert_eq!(r, Outcome::Exited((-ERR_BAD_HANDLE) as u16));
}

#[test_case]
fn forged_handle_value_is_rejected() {
    let r = usermode::run(usermode::prog_forged_handle(), &[console(Rights::NONE)], BUDGET).unwrap();
    assert_eq!(r, Outcome::Exited((-ERR_BAD_HANDLE) as u16));
}

#[test_case]
fn a_handle_of_the_wrong_kind_is_not_a_console() {
    // Slot 0 is an endpoint; write() needs a console with WRITE -> insufficient rights.
    let e = ep();
    let r = usermode::run(usermode::prog_hello(), &[(Object::Endpoint(e), Rights::SEND.union(Rights::RECV))], BUDGET);
    // hello exits with the raw (negative) result of write().
    assert_eq!(r.unwrap(), Outcome::Exited(ERR_NO_RIGHTS as u16));
}

fn delegation(parent_console: (Object, Rights), parent: &[u8], child_first: bool) -> (Outcome, Outcome) {
    let e = ep();
    let p_handles = [parent_console, (Object::Endpoint(e), Rights::SEND)];
    let c_handles = [(Object::Endpoint(e), Rights::RECV)];
    let p = Spec { program: Program::Raw(parent), handles: &p_handles, budget_ticks: BUDGET };
    let c = Spec { program: Program::Raw(usermode::prog_recv_and_write()), handles: &c_handles, budget_ticks: BUDGET };
    let r = if child_first { usermode::run_all(&[c, p]).unwrap() } else { usermode::run_all(&[p, c]).unwrap() };
    if child_first { (r[1].outcome, r[0].outcome) } else { (r[0].outcome, r[1].outcome) }
}

#[test_case]
fn delegated_console_handle_works_in_the_child() {
    for child_first in [true, false] {
        // Child blocks in recv() first (or the message is already queued).
        let (parent, child) = delegation(console(Rights::GRANT), usermode::prog_send_console_write(), child_first);
        assert_eq!(parent, Outcome::Exited(0));
        assert_eq!(child, Outcome::Exited(MSG_LEN), "child must be able to write with the received handle");
    }
}

#[test_case]
fn delegation_requires_the_grant_right() {
    // Parent's console handle has no GRANT: send with a transfer must fail,
    // and the child, which never gets a message, is ended as deadlocked.
    let (parent, child) = delegation(console(Rights::NONE), usermode::prog_send_console_write(), false);
    assert_eq!(parent, Outcome::Exited((-ERR_NO_RIGHTS) as u16));
    assert_eq!(child, Outcome::Deadlock);
}

#[test_case]
fn mask_strips_rights_on_delegation() {
    // Mask = SEND only: the child receives a console handle with no WRITE.
    let (parent, child) = delegation(console(Rights::GRANT), usermode::prog_send_console_nothing(), false);
    assert_eq!(parent, Outcome::Exited(0));
    assert_eq!(child, Outcome::Exited(ERR_NO_RIGHTS as u16));
}

#[test_case]
fn full_mask_gives_exactly_what_the_parent_had() {
    let (parent, child) = delegation(console(Rights::GRANT), usermode::prog_send_console_amplify(), false);
    assert_eq!(parent, Outcome::Exited(0));
    assert_eq!(child, Outcome::Exited(MSG_LEN)); // WRITE survived; nothing else could be added
}

#[test_case]
fn plain_message_passing_wakes_a_blocked_receiver() {
    let e = ep();
    let s_handles = [(Object::Endpoint(e), Rights::SEND)];
    let r_handles = [(Object::Endpoint(e), Rights::RECV)];
    let r = usermode::run_all(&[
        Spec { program: Program::Raw(usermode::prog_recv_only()), handles: &r_handles, budget_ticks: BUDGET },
        Spec { program: Program::Raw(usermode::prog_send_plain()), handles: &s_handles, budget_ticks: BUDGET },
    ])
    .unwrap();
    assert_eq!(r[0].outcome, Outcome::Exited(MSG_LEN));
    assert_eq!(r[1].outcome, Outcome::Exited(0));
}

#[test_case]
fn receiving_needs_the_recv_right() {
    let e = ep();
    let h = [(Object::Endpoint(e), Rights::SEND)];
    let r = usermode::run(usermode::prog_recv_wrong_right(), &h, BUDGET).unwrap();
    assert_eq!(r, Outcome::Exited((-ERR_NO_RIGHTS) as u16));
}

#[test_case]
fn process_blocked_forever_is_ended_not_hung() {
    let e = ep();
    let h = [(Object::Endpoint(e), Rights::RECV)];
    let r = usermode::run(usermode::prog_recv_only(), &h, BUDGET).unwrap();
    assert_eq!(r, Outcome::Deadlock);
}

#[test_case]
fn two_processes_waiting_on_each_other_are_both_ended() {
    let a = ep();
    let b = ep();
    let ha = [(Object::Endpoint(a), Rights::RECV)];
    let hb = [(Object::Endpoint(b), Rights::RECV)];
    let r = usermode::run_all(&[
        Spec { program: Program::Raw(usermode::prog_recv_only()), handles: &ha, budget_ticks: BUDGET },
        Spec { program: Program::Raw(usermode::prog_recv_only()), handles: &hb, budget_ticks: BUDGET },
    ])
    .unwrap();
    assert_eq!(r[0].outcome, Outcome::Deadlock);
    assert_eq!(r[1].outcome, Outcome::Deadlock);
}

#[test_case]
fn spec_naming_a_missing_endpoint_is_rejected() {
    let h = [(Object::Endpoint(7), Rights::SEND)];
    assert_eq!(usermode::run(usermode::prog_send_plain(), &h, BUDGET).err(), Some(RunError::BadHandles));
}

#[test_case]
fn a_blocked_process_does_not_stop_others_and_nothing_leaks() {
    let before = l_os::memory::frames_in_use();
    for _ in 0..20 {
        let e = ep();
        let h = [(Object::Endpoint(e), Rights::RECV)];
        let r = usermode::run_all(&[
            Spec { program: Program::Raw(usermode::prog_recv_only()), handles: &h, budget_ticks: BUDGET },
            Spec { program: Program::Raw(usermode::prog_stack_11()), handles: &[], budget_ticks: BUDGET },
        ])
        .unwrap();
        assert_eq!(r[0].outcome, Outcome::Deadlock);
        assert_eq!(r[1].outcome, Outcome::Exited(11));
    }
    assert_eq!(l_os::memory::frames_in_use(), before);
}
