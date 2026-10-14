//! The boot stick (spec §7.1 steps 7–8, E2, E7): recognized by the boot
//! marker in its LBA 0 (bulk-only, read-only SCSI subset), then sealed. The
//! runner attaches `qemu-xhci` + `usb-storage` (this very image, read-only)
//! + `usb-kbd` to every `usb_*` test.
//!
//! Tests run in name order; the `a_` test reads before anything is sealed,
//! every other test is independent of the order (identify is idempotent).
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

#[test_case]
fn amnesia_label_reflects_stick_and_mirror() {
    usb::identify_boot_stick().expect("boot stick");
    usb::storage_seal();
    let a = wipe::amnesia();
    assert!(a.stick_sealed);
    assert!(a.plan_ready, "wipe plan armed by init_memory");
    assert_eq!(a.serial_mirror, l_os::SERIAL_MIRROR);
    let want = if l_os::SERIAL_MIRROR { "amnesisch: nein" } else { "amnesisch: ja" };
    assert_eq!(a.label(), want);
    assert_eq!(l_os::aurora::status_right(), want);
    if !l_os::SERIAL_MIRROR {
        assert_eq!(a.reason(), "");
    }
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

#[test_case]
fn bulk_rejects_bad_arguments() {
    let s = usb::enumerate();
    let tds = xhci::bulk_transfers();
    for d in s.devices() {
        let buf = xhci::slot_buffer(d.slot).expect("slot buffer");
        // EP0 / unconfigured endpoint addresses are no bulk endpoints.
        assert_eq!(xhci::bulk(d.slot, 0x00, buf, 8), Err(xhci::XhciError::BadEndpoint));
        assert_eq!(xhci::bulk(d.slot, 0x8f, buf, 8), Err(xhci::XhciError::BadEndpoint));
        if let DeviceKind::BulkOnlyStorage { ep_in, .. } = d.kind {
            // Outside the slot's data page, empty, too long.
            assert_eq!(xhci::bulk(d.slot, ep_in.address, buf + 4096, 8), Err(xhci::XhciError::BadBuffer));
            assert_eq!(xhci::bulk(d.slot, ep_in.address, buf, 0), Err(xhci::XhciError::BadBuffer));
            assert_eq!(xhci::bulk(d.slot, ep_in.address, buf + 8, 4096), Err(xhci::XhciError::BadBuffer));
        }
        if let DeviceKind::BootKeyboard { ep_in, .. } = d.kind {
            // An interrupt endpoint is not a bulk endpoint.
            assert_eq!(xhci::bulk(d.slot, ep_in.address, buf, 8), Err(xhci::XhciError::BadEndpoint));
        }
    }
    assert_eq!(xhci::bulk(0, 0x81, 0, 8), Err(xhci::XhciError::BadSlot));
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
