//! Nothing here is ever authorized: every connection attempt must be refused
//! BEFORE the NIC is even initialized. scripts/net_tests.sh additionally
//! checks QEMU's own packet capture of this run: it must contain no frame.
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

fn probe(handles: &[(Object, Rights)], args: &str) -> Outcome {
    let spec = Spec {
        program: Program::Elf(user_elf("tcp_probe").unwrap()),
        handles,
        budget_ticks: 400,
        args: args.as_bytes(),
    };
    usermode::run_all(&[spec]).unwrap().remove(0).outcome
}

fn err(code: i64) -> Outcome {
    Outcome::Exited(0x8000 | (-code) as u16)
}

fn assert_silent() {
    let st = l_os::net::status();
    assert!(!st.nic_up, "the NIC must not have been initialized");
    assert_eq!(l_os::virtio_net::counters(), (0, 0), "no frame may have been sent or received");
}

#[test_case]
fn a_nothing_is_authorized_by_default() {
    assert_eq!(POLICY.lock().grant_count(), 0);
    assert!(l_os::net::status().nic_present, "the emulated NIC exists but is down");
    assert_silent();
}

#[test_case]
fn connect_by_ip_is_denied_without_a_grant() {
    POLICY.lock().revoke_all();
    assert_eq!(probe(NET, "10.0.2.2 18080 hello"), err(-10));
    assert_silent();
}

#[test_case]
fn connect_by_name_is_denied_without_a_grant() {
    POLICY.lock().revoke_all();
    assert_eq!(probe(NET, "test.example 18080 hello"), err(-10));
    assert_silent();
}

#[test_case]
fn a_granted_name_still_cannot_be_resolved_without_authorizing_the_resolver() {
    POLICY.lock().revoke_all();
    POLICY.lock().grant("test.example", 18080, None).unwrap();
    assert_eq!(probe(NET, "test.example 18080 hello"), err(-13)); // DENIED_DNS_RESOLVER
    assert_silent();
    POLICY.lock().revoke_all();
}

#[test_case]
fn an_authorized_resolver_does_not_authorize_names() {
    POLICY.lock().revoke_all();
    POLICY.lock().grant("10.0.2.3", 53, None).unwrap();
    assert_eq!(probe(NET, "other.example 18080 hello"), err(-10));
    assert_silent();
    POLICY.lock().revoke_all();
}

#[test_case]
fn other_ports_and_hosts_stay_denied_next_to_a_grant() {
    POLICY.lock().revoke_all();
    POLICY.lock().grant("10.0.2.2", 18080, None).unwrap();
    assert_eq!(probe(NET, "10.0.2.2 18081 x"), err(-10)); // other port
    assert_eq!(probe(NET, "10.0.2.3 18080 x"), err(-10)); // other host
    assert_eq!(probe(NET, "test.example 18080 x"), err(-10)); // a name is not its address
    assert_silent();
    POLICY.lock().revoke_all();
}

#[test_case]
fn expired_grants_are_denied() {
    POLICY.lock().revoke_all();
    POLICY.lock().grant("10.0.2.2", 18080, Some(0)).unwrap(); // already expired
    assert_eq!(probe(NET, "10.0.2.2 18080 x"), err(-11)); // DENIED_EXPIRED
    assert_silent();
    POLICY.lock().revoke_all();
}

#[test_case]
fn malformed_destinations_are_refused() {
    POLICY.lock().revoke_all();
    for bad in ["bad_host", "*.example", "a..b", "-x.example"] {
        let r = probe(NET, &alloc_fmt(bad));
        assert!(matches!(r, Outcome::Exited(0x800C) | Outcome::Exited(0x7002)), "{bad}: {r:?}");
    }
    assert_eq!(probe(NET, "10.0.2.2 0 x"), err(-3)); // port 0
    assert_silent();
}

fn alloc_fmt(host: &str) -> heapless_string::S {
    heapless_string::S::new(host)
}

/// Tiny fixed buffer so this test file needs no allocator import.
mod heapless_string {
    pub struct S([u8; 64], usize);
    impl S {
        pub fn new(host: &str) -> S {
            let mut b = [0u8; 64];
            let text = ["", host, " 18080 x"];
            let mut n = 0;
            for t in text {
                b[n..n + t.len()].copy_from_slice(t.as_bytes());
                n += t.len();
            }
            S(b, n)
        }
    }
    impl core::ops::Deref for S {
        type Target = str;
        fn deref(&self) -> &str {
            core::str::from_utf8(&self.0[..self.1]).unwrap_or("")
        }
    }
}

#[test_case]
fn a_process_without_a_netclient_handle_cannot_even_ask() {
    assert_eq!(probe(&[], "10.0.2.2 18080 x"), err(-1)); // ERR_BAD_HANDLE
    assert_silent();
}

#[test_case]
fn z_the_nic_is_still_down_at_the_end() {
    assert_silent();
}
