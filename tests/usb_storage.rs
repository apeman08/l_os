//! The boot stick (spec §7.1 steps 7–8, E2, E7): recognized by the boot
//! marker in its LBA 0 (bulk-only, read-only SCSI subset), then sealed. The
//! runner attaches `qemu-xhci` + `usb-storage` (this very image, read-only)
//! + `usb-kbd` to every `usb_*` test.
//!
//! Order coupling: the custom test harness runs the `#[test_case]`s sorted
//! by name, all in one boot. The `a_` tests rely on running before anything
//! is sealed, the `z_` test (controller restart) on running last; renaming
//! a test can break that. The others are independent of the order
//! (identify is idempotent).
//!
//! The raw-driver checks (`xhci::bulk_for_tests`: argument checks, the seal
//! inside `xhci::bulk`) exist only with `--features test-hooks`
//! (`xhci::bulk` is crate-private): CI runs this file a second time as
//! `cargo test --features test-hooks --test usb_storage`.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use l_core::usb_desc::DeviceKind;
use l_os::usb::{self, StickError, StorageError};
use l_os::{serial_println, wipe, xhci};

bootloader_api::entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    l_os::init();
    l_os::init_memory(boot_info);
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

/// Root port of the attached `usb-storage` device, from the enumeration.
fn storage_port() -> u8 {
    let s = usb::enumerate();
    let d = s
        .devices()
        .iter()
        .find(|d| matches!(d.kind, DeviceKind::BulkOnlyStorage { .. }))
        .expect("usb-storage enumerated (runner attaches it to usb_* tests)");
    d.port
}

#[test_case]
fn a_storage_read_before_seal_reads_lba0() {
    let port = usb::identify_boot_stick().expect("boot stick");
    let reads = usb::stick_reads();
    let tds = xhci::bulk_transfers();
    let len = usb::stick_block_len().expect("block length known");
    assert_eq!(len, 512, "QEMU usb-storage: 512-byte blocks");
    // A buffer of the wrong size is refused without traffic.
    let mut small = [0u8; 100];
    assert_eq!(usb::storage_read(0, &mut small), Err(StorageError::BadBuffer));
    assert_eq!((usb::stick_reads(), xhci::bulk_transfers()), (reads, tds));
    let mut sector = [0u8; 512];
    usb::storage_read(0, &mut sector).expect("read LBA 0 before the seal");
    assert_eq!(l_core::bootmark::parse_lba0(&sector), l_os::boot_id());
    assert_eq!(usb::stick_reads(), reads + 1, "one READ(10) command");
    assert_eq!(xhci::bulk_transfers(), tds + 3, "CBW, data, CSW");
    serial_println!("\n  stick port {}, reads {}, bulk TDs {}", port, usb::stick_reads(), xhci::bulk_transfers());
}

/// "amnesisch: ja" needs the stick identified, sealed **and** watched. This
/// kernel never arms the stick watch: without `test-hooks` its
/// `wipe::trigger` is the real wipe, and `z_restart_after_seal_skips_storage`
/// halts the controller, which an armed watch would answer with a real
/// wipe of the test kernel (ControllerLost). So the label here is "nein"
/// ("kein Boot-Stick"); the remaining inputs (plan, mirror) are checked by
/// flipping only the stick input, which yields "ja" in a build without
/// mirror and without `test-hooks`. The armed case itself:
/// `tests/usb_wipe_hooks.rs` (`stick_sealed` turns true on arming) and the
/// `wipe-test` build, which arms and shows "amnesisch: ja".
#[test_case]
fn amnesia_label_reflects_stick_and_mirror() {
    usb::identify_boot_stick().expect("boot stick");
    usb::storage_seal();
    assert!(usb::stick_sealed());
    assert_eq!(xhci::watched_port(), None, "this kernel never arms the watch");
    let a = wipe::amnesia();
    assert!(!a.stick_sealed, "sealed but unwatched stick is not an amnesia input");
    assert_eq!(a.plan_ready, !cfg!(feature = "test-hooks"), "plan armed by init_memory; test-hooks reports not ready");
    assert_eq!(a.serial_mirror, l_os::SERIAL_MIRROR);
    assert_eq!(a.label(), "amnesisch: nein");
    assert_eq!(l_os::aurora::status_right(), "amnesisch: nein");
    if !l_os::SERIAL_MIRROR {
        assert_eq!(a.reason(), "kein Boot-Stick");
    }
    let watched = l_core::wipe::Amnesia { stick_sealed: true, ..a };
    let want = if l_os::SERIAL_MIRROR || cfg!(feature = "test-hooks") { "amnesisch: nein" } else { "amnesisch: ja" };
    assert_eq!(watched.label(), want);
}

#[test_case]
fn boot_id_is_patched() {
    assert!(l_os::boot_id().is_some(), "kernel was not patched by l_os_image");
}

#[test_case]
fn identify_rejects_foreign_markers() {
    let id = l_os::boot_id().expect("patched");
    let mut sector = [0u8; 512];
    sector[..32].copy_from_slice(&l_core::bootmark::lba0_marker(&id));
    assert_eq!(usb::match_boot_marker(&sector, Some(id)), Ok(()));
    // Another image's stick: same magic, other id.
    let mut other = id;
    other[0] ^= 1;
    let mut foreign = [0u8; 512];
    foreign[..32].copy_from_slice(&l_core::bootmark::lba0_marker(&other));
    assert_eq!(usb::match_boot_marker(&foreign, Some(id)), Err(StickError::NotBootStick));
    // Any other storage (no marker), short or empty sectors.
    assert_eq!(usb::match_boot_marker(&[0u8; 512], Some(id)), Err(StickError::NotBootStick));
    assert_eq!(usb::match_boot_marker(&sector[..31], Some(id)), Err(StickError::NotBootStick));
    assert_eq!(usb::match_boot_marker(&[], Some(id)), Err(StickError::NotBootStick));
    // An unpatched kernel recognizes no stick, not even one with a marker.
    assert_eq!(usb::match_boot_marker(&sector, None), Err(StickError::NoBootId));
}

#[cfg(feature = "test-hooks")]
#[test_case]
fn a_bulk_rejects_bad_arguments() {
    let s = usb::enumerate();
    let tds = xhci::bulk_transfers();
    for d in s.devices() {
        let buf = xhci::slot_buffer(d.slot).expect("slot buffer");
        // EP0 / unconfigured endpoint addresses are no bulk endpoints.
        assert_eq!(xhci::bulk_for_tests(d.slot, 0x00, buf, 8), Err(xhci::XhciError::BadEndpoint));
        assert_eq!(xhci::bulk_for_tests(d.slot, 0x8f, buf, 8), Err(xhci::XhciError::BadEndpoint));
        if let DeviceKind::BulkOnlyStorage { ep_in, .. } = d.kind {
            // Outside the slot's data page, empty, too long.
            assert_eq!(xhci::bulk_for_tests(d.slot, ep_in.address, buf + 4096, 8), Err(xhci::XhciError::BadBuffer));
            assert_eq!(xhci::bulk_for_tests(d.slot, ep_in.address, buf, 0), Err(xhci::XhciError::BadBuffer));
            assert_eq!(xhci::bulk_for_tests(d.slot, ep_in.address, buf + 8, 4096), Err(xhci::XhciError::BadBuffer));
        }
        if let DeviceKind::BootKeyboard { ep_in, .. } = d.kind {
            // An interrupt endpoint is not a bulk endpoint.
            assert_eq!(xhci::bulk_for_tests(d.slot, ep_in.address, buf, 8), Err(xhci::XhciError::BadEndpoint));
        }
    }
    assert_eq!(xhci::bulk_for_tests(0, 0x81, 0, 8), Err(xhci::XhciError::BadSlot));
    assert_eq!(xhci::bulk_transfers(), tds, "nothing was enqueued");
}

#[test_case]
fn reads_refused_after_seal() {
    usb::identify_boot_stick().expect("boot stick");
    usb::storage_seal();
    let (reads, tds) = (usb::stick_reads(), xhci::bulk_transfers());
    let mut sector = [0u8; 512];
    for lba in [0u64, 1, 100] {
        assert_eq!(usb::storage_read(lba, &mut sector), Err(StorageError::Sealed));
    }
    // Even a malformed request is refused as sealed (nothing is looked at).
    assert_eq!(usb::storage_read(0, &mut [0u8; 3]), Err(StorageError::Sealed));
    usb::storage_seal(); // idempotent
    assert_eq!(usb::identify_boot_stick(), Ok(usb::stick_port().unwrap()), "cached, no traffic");
    // The seal is enforced in the xHCI driver itself: a direct bulk call on
    // the stick's endpoints (bypassing usb::storage_read) is refused. Only
    // with test-hooks: `xhci::bulk` is crate-private.
    #[cfg(feature = "test-hooks")]
    {
        let s = usb::enumerate();
        let stick = s.devices().iter().find(|d| Some(d.port) == usb::stick_port()).expect("stick in summary");
        let DeviceKind::BulkOnlyStorage { ep_in, ep_out, .. } = stick.kind else { panic!("stick kind {:?}", stick.kind) };
        let buf = xhci::slot_buffer(stick.slot).expect("slot buffer");
        assert_eq!(xhci::bulk_for_tests(stick.slot, ep_out.address, buf, 31), Err(xhci::XhciError::Sealed));
        assert_eq!(xhci::bulk_for_tests(stick.slot, ep_in.address, buf, 512), Err(xhci::XhciError::Sealed));
        assert_eq!(xhci::bulk_for_tests(stick.slot, ep_in.address, buf, 13), Err(xhci::XhciError::Sealed));
    }
    assert!(xhci::bulk_sealed(), "driver-level seal set by storage_seal");
    assert_eq!(usb::stick_reads(), reads, "no read after the seal");
    assert_eq!(xhci::bulk_transfers(), tds, "no bulk TD after the seal");
    assert!(sector.iter().all(|&b| b == 0), "nothing copied");
}

#[test_case]
fn stick_is_identified_by_boot_marker() {
    let port = usb::identify_boot_stick().expect("boot stick");
    assert_eq!(port, storage_port());
    assert_eq!(usb::stick_port(), Some(port));
    assert!(usb::keyboard_present(), "usb-kbd enumerated");
}

/// Spec §7.1 step 7 ("nie wieder angesprochen") and §2 (no hotplug
/// enumeration): a controller restart after the seal enumerates the
/// keyboard again but skips the stick's port entirely (no port reset, no
/// slot, no control transfer).
#[test_case]
fn z_restart_after_seal_skips_storage() {
    usb::identify_boot_stick().expect("boot stick");
    usb::storage_seal();
    let port = usb::stick_port().expect("stick port");
    let before = usb::enumerate();
    let gen0 = xhci::generation().expect("running");
    let (reads, tds) = (usb::stick_reads(), xhci::bulk_transfers());
    xhci::halt_for_wipe();
    let after = usb::enumerate();
    serial_println!("\n  before {:?}\n  after  {:?}", before.devices(), after.devices());
    assert_eq!(xhci::generation(), Some(gen0 + 1), "controller restarted, fresh scan");
    assert!(after.devices().iter().all(|d| d.port != port), "stick port skipped");
    assert!(after.devices().iter().all(|d| !matches!(d.kind, DeviceKind::BulkOnlyStorage { .. })));
    let kbd = |s: &usb::UsbSummary| s.devices().iter().filter(|d| matches!(d.kind, DeviceKind::BootKeyboard { .. })).count();
    assert_eq!(kbd(&after), kbd(&before), "keyboard enumerated again");
    assert_eq!(kbd(&after), 1);
    assert_eq!(after.devices().len(), before.devices().len() - 1);
    assert_eq!((usb::stick_reads(), xhci::bulk_transfers()), (reads, tds));
    assert!(usb::stick_sealed(), "still identified and sealed after the restart");
    // Never watched in this kernel (see amnesia_label_reflects_stick_and_mirror).
    assert!(!wipe::amnesia().stick_sealed);
}
