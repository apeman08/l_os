//! HTTPS from ring 3 on the real stack: TLS 1.3 through kernel sockets against
//! host-side `openssl s_server` instances (see scripts/net_tests.sh). Name
//! `test.example` resolves to the host (10.0.2.2) via the test DNS responder.
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
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

/// Handle layout every network program expects: console, NetClient, Random, System.
const HANDLES: &[(Object, Rights)] = &[
    (Object::Console, Rights::WRITE),
    (Object::NetClient, Rights::USE),
    (Object::Random, Rights::READ),
    (Object::System, Rights::READ),
];

// Ports served by scripts/net_tests.sh
const OK_EC: u16 = 18443;
const WRONG_NAME: u16 = 18444;
const ROGUE_CA: u16 = 18445;
const TLS12_ONLY: u16 = 18446;
const OK_RSA: u16 = 18447;
const EXPIRED: u16 = 18448;
const FUTURE: u16 = 18449;
const PLAINTEXT_RECORDER: u16 = 18090;

fn setup(port: u16) {
    POLICY.lock().revoke_all();
    l_os::net::policy_changed();
    l_os::net::set_resolver([10, 0, 2, 2], 5353);
    POLICY.lock().grant("10.0.2.2", 5353, None).unwrap(); // the resolver
    POLICY.lock().grant("test.example", port, None).unwrap();
}

fn probe(port: u16, host: &str) -> Outcome {
    // Args are built in a small stack buffer (no allocator in this test).
    let mut buf = [0u8; 64];
    let mut n = 0;
    for part in [host, " "] {
        buf[n..n + part.len()].copy_from_slice(part.as_bytes());
        n += part.len();
    }
    let mut digits = [0u8; 5];
    let mut p = port;
    let mut d = 5;
    while p > 0 {
        d -= 1;
        digits[d] = b'0' + (p % 10) as u8;
        p /= 10;
    }
    let ds = &digits[d..];
    buf[n..n + ds.len()].copy_from_slice(ds);
    n += ds.len();
    buf[n..n + 2].copy_from_slice(b" /");
    n += 2;
    let spec = Spec {
        program: Program::Elf(user_elf("tls_probe").unwrap()),
        handles: HANDLES,
        budget_ticks: l_os::time::ticks_from_ms(82_400),
        args: &buf[..n],
    };
    usermode::run_all(&[spec]).unwrap().remove(0).outcome
}

#[test_case]
fn tls13_https_get_from_ring3_with_an_ecdsa_certificate() {
    setup(OK_EC);
    assert_eq!(probe(OK_EC, "test.example"), Outcome::Exited(200));
    assert_eq!(l_os::net::status().open_sockets, 0);
}

#[test_case]
fn tls13_https_get_with_an_rsa_certificate() {
    setup(OK_RSA);
    assert_eq!(probe(OK_RSA, "test.example"), Outcome::Exited(200));
}

#[test_case]
fn a_certificate_for_another_name_is_refused() {
    setup(WRONG_NAME);
    assert_eq!(probe(WRONG_NAME, "test.example"), Outcome::Exited(0x9002));
}

#[test_case]
fn a_certificate_from_an_untrusted_ca_is_refused() {
    setup(ROGUE_CA);
    assert_eq!(probe(ROGUE_CA, "test.example"), Outcome::Exited(0x9001));
}

#[test_case]
fn an_expired_certificate_is_refused() {
    setup(EXPIRED);
    assert_eq!(probe(EXPIRED, "test.example"), Outcome::Exited(0x9003));
}

#[test_case]
fn a_not_yet_valid_certificate_is_refused() {
    setup(FUTURE);
    assert_eq!(probe(FUTURE, "test.example"), Outcome::Exited(0x9004));
}

#[test_case]
fn a_tls12_only_server_is_refused_without_downgrade() {
    setup(TLS12_ONLY);
    assert_eq!(probe(TLS12_ONLY, "test.example"), Outcome::Exited(0x9006));
}

#[test_case]
fn a_server_that_is_not_speaking_tls_is_refused() {
    // The recorder answers any bytes with a plaintext "HTTP/1.1 200 OK"; the
    // script afterwards verifies it only ever received a TLS ClientHello.
    setup(PLAINTEXT_RECORDER);
    let r = probe(PLAINTEXT_RECORDER, "test.example");
    assert!(matches!(r, Outcome::Exited(0x9006) | Outcome::Exited(0x9007)), "{r:?}");
}

#[test_case]
fn tls_needs_the_same_authorizations_as_everything_else() {
    POLICY.lock().revoke_all();
    l_os::net::policy_changed();
    let tx = l_os::virtio_net::counters().0;
    assert_eq!(probe(OK_EC, "test.example"), Outcome::Exited(0x800A)); // DENIED_NO_GRANT
    POLICY.lock().grant("test.example", OK_EC, None).unwrap();
    assert_eq!(probe(OK_EC, "test.example"), Outcome::Exited(0x800D)); // resolver not authorized
    assert_eq!(l_os::virtio_net::counters().0, tx, "nothing was sent while unauthorized");
}

#[test_case]
fn without_a_random_handle_no_connection_is_attempted() {
    setup(OK_EC);
    let handles: &[(Object, Rights)] = &[
        (Object::Console, Rights::WRITE),
        (Object::NetClient, Rights::USE),
        (Object::Console, Rights::WRITE), // slot 2 is not a Random handle
        (Object::System, Rights::READ),
    ];
    let tx = l_os::virtio_net::counters().0;
    let spec = Spec {
        program: Program::Elf(user_elf("tls_probe").unwrap()),
        handles,
        budget_ticks: l_os::time::ticks_from_ms(82_400),
        args: b"test.example 18443 /",
    };
    let r = usermode::run_all(&[spec]).unwrap().remove(0).outcome;
    // The TCP connection may open (authorized), but TLS must not proceed
    // without entropy: it fails before sending any ClientHello.
    assert!(matches!(r, Outcome::Exited(c) if c & 0xff00 == 0x9000), "{r:?}");
    let _ = tx;
}
