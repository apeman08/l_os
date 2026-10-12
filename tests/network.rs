//! End-to-end networking against host-side test servers (TCP echo on
//! 10.0.2.2:18080, DNS responder on 10.0.2.2:5353). Run with
//! scripts/net_tests.sh, which starts the servers.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use l_core::{
    handles::{Object, Rights},
    syscall::Outcome,
};
use l_os::usermode::{self, POLICY, Program, Spec, user_elf};

entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    l_os::init();
    l_os::init_memory(boot_info);
    l_os::pci::enumerate();
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

const NET: &[(Object, Rights)] = &[(Object::NetClient, Rights::USE)];

fn elf(name: &str, handles: &'static [(Object, Rights)], args: &'static str) -> Spec<'static> {
    Spec { program: Program::Elf(user_elf(name).unwrap()), handles, budget_ticks: 2000, args: args.as_bytes() }
}

fn run(spec: Spec<'static>) -> Outcome {
    usermode::run_all(&[spec]).unwrap().remove(0).outcome
}

fn err(code: i64) -> Outcome {
    Outcome::Exited(0x8000 | (-code) as u16)
}

fn reset() {
    POLICY.lock().revoke_all();
    l_os::net::policy_changed();
    l_os::net::set_resolver([10, 0, 2, 3], 53);
}

const MSG: &str = "10.0.2.2 18080 hello from larp os";

#[test_case]
fn authorized_connection_echoes_through_the_real_stack() {
    reset();
    POLICY.lock().grant("10.0.2.2", 18080, None).unwrap();
    assert_eq!(run(elf("tcp_probe", NET, MSG)), Outcome::Exited("hello from larp os".len() as u16));
    let st = l_os::net::status();
    assert!(st.nic_up, "the NIC comes up only after the first authorized connection");
    let (tx, rx) = l_os::virtio_net::counters();
    assert!(tx > 0 && rx > 0, "frames really went over the wire (tx {tx}, rx {rx})");
    assert_eq!(st.open_sockets, 0, "the socket was closed with the program");
}

#[test_case]
fn several_connections_in_a_row_and_a_longer_message() {
    reset();
    POLICY.lock().grant("10.0.2.2", 18080, None).unwrap();
    for _ in 0..3 {
        assert_eq!(run(elf("tcp_probe", NET, MSG)), Outcome::Exited(18));
    }
    let long = "10.0.2.2 18080 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    assert_eq!(run(elf("tcp_probe", NET, long)), Outcome::Exited(112));
}

#[test_case]
fn denied_attempts_send_no_frame_even_with_the_nic_up() {
    reset();
    POLICY.lock().grant("10.0.2.2", 18080, None).unwrap();
    assert_eq!(run(elf("tcp_probe", NET, MSG)), Outcome::Exited(18)); // make sure the NIC is up
    let before = l_os::virtio_net::counters();
    assert_eq!(run(elf("tcp_probe", NET, "10.0.2.2 18081 x")), err(-10)); // other port
    assert_eq!(run(elf("tcp_probe", NET, "10.0.2.9 18080 x")), err(-10)); // other host
    assert_eq!(l_os::virtio_net::counters().0, before.0, "a denied request must not transmit anything");
}

#[test_case]
fn revoked_permission_is_denied_again() {
    reset();
    POLICY.lock().grant("10.0.2.2", 18080, None).unwrap();
    assert_eq!(run(elf("tcp_probe", NET, MSG)), Outcome::Exited(18));
    assert!(POLICY.lock().revoke("10.0.2.2", 18080));
    let tx = l_os::virtio_net::counters().0;
    assert_eq!(run(elf("tcp_probe", NET, MSG)), err(-10));
    assert_eq!(l_os::virtio_net::counters().0, tx);
}

#[test_case]
fn revoking_cuts_an_open_connection() {
    reset();
    POLICY.lock().grant("10.0.2.2", 18080, None).unwrap();
    static MANAGER: &[(Object, Rights)] = &[(Object::NetManager, Rights::USE), (Object::System, Rights::READ)];
    let r = usermode::run_all(&[
        elf("tcp_hold", NET, "10.0.2.2 18080"),
        elf("revoker", MANAGER, "10 10.0.2.2 18080"),
    ])
    .unwrap();
    assert_eq!(r[1].outcome, Outcome::Exited(1), "the grant existed and was revoked");
    assert_eq!(r[0].outcome, err(-18), "the open connection must fail with ERR_REVOKED");
    assert_eq!(l_os::net::status().open_sockets, 0);
}

#[test_case]
fn an_expiring_grant_ends_the_connection() {
    reset();
    POLICY.lock().grant("10.0.2.2", 18080, Some(l_os::time::ticks() + 25)).unwrap();
    assert_eq!(run(elf("tcp_hold", NET, "10.0.2.2 18080")), err(-18));
}

#[test_case]
fn connection_refused_is_a_clean_error() {
    reset();
    POLICY.lock().grant("10.0.2.2", 18999, None).unwrap(); // authorized, but nobody listens
    assert_eq!(run(elf("tcp_probe", NET, "10.0.2.2 18999 x")), err(-16)); // ERR_CONNECT_FAILED
    assert_eq!(l_os::net::status().open_sockets, 0);
}

#[test_case]
fn names_resolve_only_with_both_authorizations() {
    reset();
    l_os::net::set_resolver([10, 0, 2, 2], 5353); // the host-side test resolver
    POLICY.lock().grant("test.example", 18080, None).unwrap();
    POLICY.lock().grant("10.0.2.2", 5353, None).unwrap();
    assert_eq!(run(elf("tcp_probe", NET, "test.example 18080 resolved ok")), Outcome::Exited("resolved ok".len() as u16));
}

#[test_case]
fn unknown_names_fail_cleanly() {
    reset();
    l_os::net::set_resolver([10, 0, 2, 2], 5353);
    POLICY.lock().grant("missing.example", 18080, None).unwrap();
    POLICY.lock().grant("10.0.2.2", 5353, None).unwrap();
    assert_eq!(run(elf("tcp_probe", NET, "missing.example 18080 x")), err(-15)); // ERR_DNS_FAILED
}

#[test_case]
fn revoking_the_resolver_stops_name_lookups() {
    reset();
    l_os::net::set_resolver([10, 0, 2, 2], 5353);
    POLICY.lock().grant("test.example", 18080, None).unwrap();
    POLICY.lock().grant("10.0.2.2", 5353, None).unwrap();
    assert_eq!(run(elf("tcp_probe", NET, "test.example 18080 first")), Outcome::Exited(5));
    assert!(POLICY.lock().revoke("10.0.2.2", 5353));
    let tx = l_os::virtio_net::counters().0;
    assert_eq!(run(elf("tcp_probe", NET, "test.example 18080 second")), err(-13));
    assert_eq!(l_os::virtio_net::counters().0, tx, "no DNS packet after the resolver grant was revoked");
}
