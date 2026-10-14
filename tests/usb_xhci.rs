//! The xHCI controller on PCI and its registers through the kernel's own
//! uncached MMIO window (spec §7.1 step 1, E5). The runner attaches
//! `qemu-xhci` + `usb-storage` + `usb-kbd` to every `usb_*` test.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use l_core::pci_bar::Bar;
use l_os::{memory, mmio, pci, serial_println, wipe};
use x86_64::structures::paging::{PageTable, PageTableFlags as F, PhysFrame};

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

fn xhci() -> pci::Device {
    pci::devices()
        .find(|d| d.class == 0x0c && d.subclass == 0x03 && d.prog_if == 0x30)
        .expect("xHCI controller (class 0x0C/0x03/0x30); runner adds qemu-xhci to usb_* tests")
}

fn map_bar0() -> mmio::Mmio {
    let d = xhci();
    let base = pci::bar64(&d, 0).mem_base().expect("BAR0 is a memory BAR");
    let size = pci::bar_size(&d, 0).expect("BAR0 size");
    mmio::map(base, size).expect("map BAR0")
}

#[test_case]
fn finds_xhci_by_class() {
    let d = xhci();
    serial_println!("\n  xhci {:02x}:{:02x}.{} {:04x}:{:04x} prog_if {:#x}", d.bus, d.slot, d.func, d.vendor, d.device, d.prog_if);
    assert_eq!((d.vendor, d.device), (0x1b36, 0x000d), "qemu-xhci ids (spike note §3)");
    let bar0 = pci::bar64(&d, 0);
    let base = match bar0 {
        Bar::Mem64 { base, .. } => base,
        other => panic!("BAR0 is not a 64-bit memory BAR: {:?}", other),
    };
    assert_ne!(base, 0, "BAR0 assigned by the firmware");
    assert_eq!(base % 0x1000, 0);

    let cfg = |off: u8| pci::read32(d.bus, d.slot, d.func, off);
    let (cmd, lo, hi) = (cfg(0x04) & 0xffff, cfg(0x10), cfg(0x14));
    let size = pci::bar_size(&d, 0).expect("BAR0 size");
    serial_println!("  BAR0 base {:#x} size {:#x} cmd {:#x}", base, size, cmd);
    assert!(size.is_power_of_two() && size >= 0x1000, "BAR0 size {:#x}", size);
    assert_eq!(base % size, 0, "BAR naturally aligned");
    // The measurement restored the BAR and the command register.
    assert_eq!((cfg(0x04) & 0xffff, cfg(0x10), cfg(0x14)), (cmd, lo, hi));

    // Unimplemented BAR (qemu-xhci has only BAR0/1): no size, still restored.
    let (cmd, b5) = (cfg(0x04) & 0xffff, cfg(0x24));
    assert_eq!(pci::bar_size(&d, 5), None);
    assert_eq!((cfg(0x04) & 0xffff, cfg(0x24)), (cmd, b5));

    // Hostile/invalid indices never panic.
    assert_eq!(pci::bar64(&d, 6), Bar::Invalid);
    assert_eq!(pci::bar_size(&d, 6), None);
    assert_eq!(pci::bar_size(&d, usize::MAX), None);
    let mut fake = d;
    fake.bars[5] = 0x4; // 64-bit type in the last slot: no upper half
    assert_eq!(pci::bar64(&fake, 5), Bar::Invalid);
}

#[test_case]
fn mmio_window_reads_capability_length() {
    let m = map_bar0();
    // Capability registers only with 32-bit reads (spike note §3 quirk).
    let dw0 = m.read32(0).expect("in bounds");
    let (caplength, hciversion) = (dw0 & 0xff, dw0 >> 16);
    let hcs1 = m.read32(4).expect("in bounds");
    serial_println!(
        "\n  window virt {:#x} phys {:#x} len {:#x}: CAPLENGTH {:#x} HCIVERSION {:#x} HCSPARAMS1 {:#x}",
        m.virt(), m.phys(), m.len(), caplength, hciversion, hcs1
    );
    assert!(caplength >= 0x20, "CAPLENGTH {:#x}", caplength);
    assert!(hciversion >= 0x0100, "HCIVERSION {:#x}", hciversion);

    // Bounds and alignment checks: errors, no access, no panic.
    let len = m.len();
    assert!(m.read32(len - 4).is_ok());
    assert!(m.read32(len).is_err());
    assert!(m.read32(len - 2).is_err());
    assert!(m.read32(2).is_err());
    assert!(m.read32(u64::MAX).is_err());
    assert!(m.write32(len, 0).is_err());
    assert!(m.write64(len - 4, 0).is_err());
    assert!(m.write64(4, 0).is_err());
    assert!(m.write64(u64::MAX - 3, 0).is_err());

    // Hostile inputs to map: empty, overflowing, RAM, non-canonical phys.
    assert_eq!(mmio::map(m.phys(), 0).err(), Some(mmio::MapError::Empty));
    assert_eq!(mmio::map(u64::MAX - 0xfff, 0x2000).err(), Some(mmio::MapError::BadAddress));
    assert_eq!(mmio::map(1 << 52, 0x1000).err(), Some(mmio::MapError::BadAddress));
    assert_eq!(mmio::map(m.phys(), 1 << 40).err(), Some(mmio::MapError::TooLarge));
    let facts = l_os::boot_facts().expect("boot facts");
    let ram = facts
        .memory_regions
        .iter()
        .find(|r| r.kind == bootloader_api::info::MemoryRegionKind::Usable)
        .expect("usable RAM");
    assert_eq!(mmio::map(ram.start, 0x1000).err(), Some(mmio::MapError::OverlapsRam));
}

fn table(frame_phys: u64) -> &'static PageTable {
    // SAFETY: page-table frames are RAM mapped at the physical-memory offset;
    // only read, by this single-CPU test.
    unsafe { &*(memory::phys_offset() + frame_phys).as_ptr::<PageTable>() }
}

/// Flags of every level (L4 first) for `virt` in the tables rooted at `l4`,
/// and the leaf's physical address; `None` if a level is not present.
fn walk(l4: PhysFrame, virt: u64) -> Option<([F; 4], u64)> {
    let mut flags = [F::empty(); 4];
    let mut t = l4.start_address().as_u64();
    for (n, level) in (1..=4u64).rev().enumerate() {
        let e = &table(t)[((virt >> (12 + 9 * (level - 1))) & 0x1ff) as usize];
        if !e.flags().contains(F::PRESENT) {
            return None;
        }
        flags[n] = e.flags();
        if level > 1 {
            assert!(!e.flags().contains(F::HUGE_PAGE), "window uses 4 KiB pages");
        }
        t = e.addr().as_u64();
    }
    Some((flags, t))
}

#[test_case]
fn mmio_mapping_is_uncached_and_nx() {
    let m = map_bar0();
    assert_eq!(m.virt() >> 39 & 0x1ff, mmio::MMIO_L4_INDEX as u64, "window in L4 slot 144");
    assert_eq!(mmio::MMIO_L4_INDEX, 144);
    let pages = m.len().div_ceil(4096);
    for p in 0..pages {
        let virt = (m.virt() & !0xfff) + p * 4096;
        let (flags, phys) = walk(memory::kernel_l4(), virt).expect("window page mapped");
        let leaf = flags[3];
        let want = F::PRESENT | F::WRITABLE | F::NO_CACHE | F::WRITE_THROUGH | F::NO_EXECUTE;
        assert!(leaf.contains(want), "page {:#x}: flags {:?}", virt, leaf);
        assert!(!leaf.contains(F::USER_ACCESSIBLE) && !leaf.contains(F::GLOBAL), "page {:#x}: {:?}", virt, leaf);
        assert!(flags.iter().all(|f| !f.contains(F::USER_ACCESSIBLE)), "supervisor only: {:?}", flags);
        assert_eq!(phys, (m.phys() & !0xfff) + p * 4096, "maps the BAR itself");
    }
    // Unmapped guard page right after the mapping.
    let after = (m.virt() & !0xfff) + pages * 4096;
    assert!(walk(memory::kernel_l4(), after).is_none(), "guard page after the window mapping");

    // The window is shared with process address spaces (timer-ISR polling
    // runs on a process's CR3): the copied L4 entry reaches the same leaf.
    {
        let mut guard = memory::MEMORY.lock();
        let mem = guard.as_mut().unwrap();
        let l4 = memory::create_address_space(mem).expect("frames");
        let k = walk(memory::kernel_l4(), m.virt()).unwrap();
        assert_eq!(walk(l4, m.virt()), Some(k));
        memory::destroy_address_space(mem, l4);
    }

    // The window's table frames come from the allocator (zeroed by wipe
    // stage 7); the BAR itself is not RAM, so no wipe range covers it.
    let usable = |p: u64| {
        l_os::boot_facts().unwrap().memory_regions.iter().any(|r| {
            r.kind == bootloader_api::info::MemoryRegionKind::Usable && r.start <= p && p < r.end
        })
    };
    assert!(!usable(m.phys()), "BAR inside Usable RAM");
    // Arming the wipe (in init_memory, after the window's L3 was created)
    // was not broken by the window.
    assert!(wipe::plan().is_some(), "wipe plan armed with the MMIO window present");
}

// ---------------------------------------------------------------------------
// Task 19: controller reset, rings and command submission (spec §7.1 1–4).

use l_core::xhci_ring::{Event, SUCCESS, Trb};
use l_os::xhci::{self, XhciError};

const USBCMD_RS: u32 = 1;
const USBCMD_HCRST: u32 = 1 << 1;
const USBCMD_INTE: u32 = 1 << 2;
const USBSTS_HCH: u32 = 1;
const USBSTS_CNR: u32 = 1 << 11;

/// Bring the driver up for a test that does not care whether an earlier
/// test already did.
fn up() {
    match xhci::init() {
        Ok(()) | Err(XhciError::AlreadyInitialized) => {}
        Err(e) => panic!("xhci::init: {:?}", e),
    }
}

fn noop() -> u64 {
    let ring = xhci::command_ring().expect("command ring after init");
    let ev = xhci::command(Trb::noop_cmd(), 500).expect("no-op command");
    match ev {
        Event::CommandCompletion { trb, code, slot } => {
            assert_eq!(code, SUCCESS, "no-op completion code");
            assert_eq!(slot, 0, "no-op has no slot");
            assert_eq!(trb, ring.enqueue, "completion points at the no-op TRB");
            trb
        }
        other => panic!("command() returned {:?}", other),
    }
}

/// Before `init` nothing is mapped: the wipe hook must be a harmless no-op.
/// (The test framework runs tests sorted by name; this one sorts first, and
/// every other test calls `up()`, so none depends on the order.)
#[test_case]
fn before_init_halt_for_wipe_is_noop() {
    assert_eq!(xhci::caps(), None);
    assert_eq!(xhci::usbsts(), None);
    xhci::halt_for_wipe();
    assert_eq!(xhci::command(Trb::noop_cmd(), 10), Err(XhciError::NotInitialized));
}

#[test_case]
fn controller_resets_and_runs() {
    up();
    let caps = xhci::caps().expect("caps after init");
    let (cmd, sts) = (xhci::usbcmd().unwrap(), xhci::usbsts().unwrap());
    let ring = xhci::command_ring().unwrap();
    serial_println!("\n  caps {:?}\n  USBCMD {:#x} USBSTS {:#x} cmd ring {:#x}+{}", caps, cmd, sts, ring.base, ring.slots);
    assert_eq!(sts & USBSTS_HCH, 0, "running (HCH=0): USBSTS {:#x}", sts);
    assert_eq!(sts & USBSTS_CNR, 0, "controller ready (CNR=0): USBSTS {:#x}", sts);
    assert_ne!(cmd & USBCMD_RS, 0, "RS=1: USBCMD {:#x}", cmd);
    assert_eq!(cmd & (USBCMD_HCRST | USBCMD_INTE), 0, "no reset pending, no interrupts: USBCMD {:#x}", cmd);
    // qemu-xhci (spike note §3).
    assert_eq!((caps.max_slots, caps.max_ports, caps.max_scratchpad), (64, 8, 0));
    // DMA structures below 4 GiB, page aligned (no AC64 assumption).
    assert!(ring.base % 4096 == 0 && ring.base + 64 * 16 <= 1 << 32, "ring {:#x}", ring.base);
    assert_eq!(ring.slots, 63, "64 TRBs incl. link");
}

#[test_case]
fn init_twice_is_rejected_and_keeps_running() {
    up();
    let before = xhci::command_ring().unwrap();
    assert_eq!(xhci::init(), Err(XhciError::AlreadyInitialized));
    assert_eq!(xhci::usbsts().unwrap() & USBSTS_HCH, 0);
    assert_eq!(xhci::command_ring().unwrap(), before, "second init touched nothing");
    noop();
}

#[test_case]
fn noop_command_completes() {
    up();
    let a = noop();
    let b = noop();
    assert_eq!(b, a + 16, "consecutive slots");
}

#[test_case]
fn command_ring_wraps() {
    up();
    let ring = xhci::command_ring().unwrap();
    let mut wrapped = 0;
    let mut prev = None;
    for i in 0..70 {
        let at = noop();
        if let Some(p) = prev {
            if at == ring.base {
                assert_eq!(p, ring.base + (ring.slots as u64 - 1) * 16, "wrap after the last usable slot");
                wrapped += 1;
            } else {
                assert_eq!(at, p + 16, "no-op {} at the next slot", i);
            }
        }
        assert!(at >= ring.base && at < ring.base + ring.slots as u64 * 16, "never the link slot");
        prev = Some(at);
    }
    assert!(wrapped >= 1, "70 no-ops over 63 usable slots wrap at least once");
}

#[test_case]
fn command_rejects_link_trb() {
    up();
    let before = xhci::command_ring().unwrap();
    assert_eq!(xhci::command(Trb::link(0, true), 100), Err(XhciError::InvalidTrb));
    assert_eq!(xhci::command_ring().unwrap(), before, "nothing enqueued");
    noop();
}

/// Drains whatever non-command events the real controller queued. QEMU
/// sends none after HCRST/RS=1 (CSC is set without an event, spike note
/// §5), so on QEMU this only checks that draining is clean and bounded;
/// the queueing and routing logic itself is covered with a scripted event
/// source by the in-crate tests `xhci::tests::drain_*`.
#[test_case]
fn queued_events_drain_cleanly() {
    up();
    noop();
    let mut n = 0;
    while let Some(e) = xhci::pop_event() {
        serial_println!("  queued event {:?}", e);
        assert!(!matches!(e, Event::CommandCompletion { .. }), "command completions are not queued");
        n += 1;
        assert!(n <= xhci::EVENT_QUEUE_LEN, "queue is bounded");
    }
    serial_println!("  {} queued, {} dropped", n, xhci::dropped_events());
}

#[test_case]
fn halt_for_wipe_stops_controller() {
    up();
    noop();
    let t0 = l_os::time::rdtsc();
    // As on the wipe path: interrupts off (ticks frozen, TSC bound only).
    x86_64::instructions::interrupts::without_interrupts(xhci::halt_for_wipe);
    let cycles = l_os::time::rdtsc().wrapping_sub(t0);
    let (cmd, sts) = (xhci::usbcmd().unwrap(), xhci::usbsts().unwrap());
    serial_println!("\n  after halt_for_wipe: USBCMD {:#x} USBSTS {:#x} ({} cycles)", cmd, sts, cycles);
    assert_eq!(cmd & USBCMD_RS, 0, "RS cleared");
    assert_ne!(sts & USBSTS_HCH, 0, "controller halted (HCH=1)");
    // A halted controller fails commands cleanly instead of timing out.
    assert_eq!(xhci::command(Trb::noop_cmd(), 100), Err(XhciError::Halted));
    // Idempotent and bounded.
    xhci::halt_for_wipe();
    assert_ne!(xhci::usbsts().unwrap() & USBSTS_HCH, 0);
    // The driver can be brought up again (fresh reset and rings).
    xhci::init().expect("re-init after halt");
    let ring = xhci::command_ring().unwrap();
    assert_eq!(ring.enqueue, ring.base + 16, "fresh ring after re-init (slot 0: init's self-test no-op)");
    noop();
}

// ---------------------------------------------------------------------------
// Task 20: port reset and enumeration on the root ports (spec §7.1 5–6).

use l_core::usb_desc::{DeviceKind, SetupPacket};
use l_core::xhci_ctx::{PortSc, Speed};
use l_os::usb::{self, UsbDevice, UsbSummary};

/// Root ports of the runner's devices (spike note §4): `usb-storage` at the
/// USB3 port 1 (SuperSpeed), `usb-kbd` at the USB2 port 6 (High-Speed).
const STICK_PORT: u8 = 1;
const KBD_PORT: u8 = 6;
/// xHCI slot states (section 6.2.2, Slot Context DW3 31:27) and endpoint
/// states (section 6.2.3, Endpoint Context DW0 2:0).
const SLOT_CONFIGURED: u8 = 3;
const EP_RUNNING: u8 = 1;

fn enumerated() -> UsbSummary {
    let s = usb::enumerate();
    for d in s.devices() {
        serial_println!("  {:?}", d);
    }
    s
}

fn by_kind(s: &UsbSummary, keyboard: bool) -> UsbDevice {
    *s.devices()
        .iter()
        .find(|d| matches!(
            (keyboard, d.kind),
            (true, DeviceKind::BootKeyboard { .. }) | (false, DeviceKind::BulkOnlyStorage { .. })
        ))
        .unwrap_or_else(|| panic!("no {} in {:?}", if keyboard { "keyboard" } else { "storage" }, s))
}

#[test_case]
fn enumerates_keyboard_and_storage() {
    serial_println!();
    let s = enumerated();
    assert_eq!(s.devices().len(), 2, "two devices: {:?}", s);
    assert_eq!(s.failed(), 0, "no device failed: {:?}", s);
    let kbd = by_kind(&s, true);
    let stick = by_kind(&s, false);
    assert_eq!((kbd.port, kbd.speed), (KBD_PORT, Speed::High), "keyboard {:?}", kbd);
    assert_eq!((stick.port, stick.speed), (STICK_PORT, Speed::Super), "stick {:?}", stick);
    assert_ne!(kbd.slot, stick.slot);
    assert!(kbd.slot >= 1 && stick.slot >= 1);
    match (kbd.kind, stick.kind) {
        (DeviceKind::BootKeyboard { ep_in, .. }, DeviceKind::BulkOnlyStorage { ep_in: bi, ep_out: bo, .. }) => {
            assert_eq!(ep_in.address & 0x80, 0x80, "interrupt IN");
            assert_eq!((bi.address & 0x80, bo.address & 0x80), (0x80, 0), "bulk IN + OUT");
        }
        k => panic!("kinds {:?}", k),
    }
}

#[test_case]
fn device_descriptor_vendor_matches_qemu() {
    let s = enumerated();
    let (kbd, stick) = (by_kind(&s, true), by_kind(&s, false));
    // QEMU hw/usb/dev-hid.c (usb-kbd) and dev-storage.c (CRC16 of "QEMU").
    assert_eq!((kbd.vendor, kbd.product), (0x0627, 0x0001), "usb-kbd ids");
    assert_eq!((stick.vendor, stick.product), (0x46f4, 0x0001), "usb-storage ids");
    // EP0 max packet after the 8-byte prefix: HS 64, SS 2^9 = 512.
    assert_eq!(kbd.max_packet0, 64);
    assert_eq!(stick.max_packet0, 512);
}

#[test_case]
fn port_ack_keeps_port_enabled() {
    let _ = enumerated();
    for port in [STICK_PORT, KBD_PORT] {
        let p = xhci::portsc(port).expect("PORTSC");
        serial_println!("  PORTSC[{}] = {:#010x}", port, p.0);
        assert!(p.ccs(), "port {} connected", port);
        assert!(p.ped(), "port {} still enabled after every acknowledgement", port);
        assert!(!p.pr(), "port {} reset finished", port);
        assert!(!(p.csc() || p.prc() || p.pec() || p.plc() || p.wrc() || p.occ() || p.cec()), "port {}: change bits acked ({:#x})", port, p.0);
    }
    // Empty ports (CSC after HCRST) are acked too and stay disabled.
    let p = xhci::portsc(2).expect("PORTSC[2]");
    assert!(!p.ccs() && !p.ped() && !p.csc(), "empty port 2: {:#x}", p.0);
    // Out-of-range ports are errors, not panics.
    assert!(xhci::portsc(0).is_err());
    assert!(xhci::portsc(200).is_err());
    let _ = PortSc(0);
}

#[test_case]
fn configuration_selected_and_endpoints_running() {
    let s = enumerated();
    for d in s.devices() {
        assert_eq!(d.config, 1, "SET_CONFIGURATION(bConfigurationValue) for {:?}", d);
        assert_eq!(xhci::slot_state(d.slot), Some(SLOT_CONFIGURED), "slot {} configured", d.slot);
        let eps: &[u8] = match d.kind {
            DeviceKind::BootKeyboard { ep_in, .. } => &[ep_in.address],
            DeviceKind::BulkOnlyStorage { ep_in, ep_out, .. } => &[ep_in.address, ep_out.address],
            DeviceKind::Unsupported => &[],
        };
        assert!(!eps.is_empty());
        for &a in eps {
            let dci = l_core::xhci_ctx::dci(a);
            assert_eq!(xhci::endpoint_state(d.slot, dci), Some(EP_RUNNING), "slot {} ep {:#x} (dci {})", d.slot, a, dci);
        }
        assert_eq!(xhci::endpoint_state(d.slot, 1), Some(EP_RUNNING), "EP0 slot {}", d.slot);
    }
}

/// A second call reuses the result (no new slots), and EP0 still works:
/// re-reading the device descriptor through `control_in` gives the same ids.
#[test_case]
fn enumerate_is_idempotent_and_ep0_still_works() {
    let a = enumerated();
    let b = usb::enumerate();
    assert_eq!(a, b);
    let stick = by_kind(&a, false);
    let buf = xhci::slot_buffer(stick.slot).expect("buffer of an enumerated slot");
    let got = xhci::control_in(stick.slot, SetupPacket::get_descriptor(1, 0, 18), buf, 18).expect("GET_DESCRIPTOR");
    assert_eq!(got, 18);
    let mut d = [0u8; 18];
    xhci::read_slot_buffer(stick.slot, 0, &mut d).expect("read buffer");
    assert_eq!(u16::from_le_bytes([d[8], d[9]]), stick.vendor);
}

/// Misuse of the transfer interface fails cleanly, without touching a ring.
#[test_case]
fn control_transfer_rejects_bad_arguments() {
    let s = enumerated();
    let kbd = by_kind(&s, true);
    let buf = xhci::slot_buffer(kbd.slot).unwrap();
    let dev = SetupPacket::get_descriptor(1, 0, 18);
    // Unknown slots.
    assert!(xhci::control_in(0, dev, buf, 18).is_err());
    assert!(xhci::control_in(63, dev, buf, 18).is_err());
    assert_eq!(xhci::slot_buffer(63), None);
    // Buffer outside the slot's own DMA page, or past its end.
    assert_eq!(xhci::control_in(kbd.slot, dev, 0x1000, 18), Err(XhciError::BadBuffer));
    assert_eq!(xhci::control_in(kbd.slot, SetupPacket::get_descriptor(2, 0, 4096), buf + 8, 4096), Err(XhciError::BadBuffer));
    // Length must match wLength; direction must match.
    assert_eq!(xhci::control_in(kbd.slot, dev, buf, 8), Err(XhciError::BadBuffer));
    assert_eq!(xhci::control_in(kbd.slot, SetupPacket::set_configuration(1), buf, 0), Err(XhciError::BadBuffer));
    assert_eq!(xhci::control_out_nodata(kbd.slot, dev), Err(XhciError::BadBuffer));
    assert!(xhci::read_slot_buffer(kbd.slot, 4090, &mut [0u8; 18]).is_err());
    // Endpoint configuration refuses more than the table holds.
    let (e, sp) = match kbd.kind {
        DeviceKind::BootKeyboard { ep_in, .. } => (ep_in, kbd.speed),
        _ => unreachable!(),
    };
    assert!(xhci::configure_endpoints(kbd.slot, &[(e, sp), (e, sp), (e, sp)]).is_err());
    assert!(xhci::configure_endpoints(0, &[(e, sp)]).is_err());
    // The device still answers afterwards.
    assert_eq!(xhci::control_in(kbd.slot, dev, buf, 18), Ok(18));
}

/// Evaluate Context (the path for a `bMaxPacketSize0` that differs from the
/// speed default; QEMU's devices never need it): change the keyboard's EP0
/// max packet to 8 and back to 64, EP0 keeps working; 0 is refused.
#[test_case]
fn evaluate_context_changes_ep0_max_packet() {
    let s = enumerated();
    let kbd = by_kind(&s, true);
    let buf = xhci::slot_buffer(kbd.slot).unwrap();
    let dev = SetupPacket::get_descriptor(1, 0, 18);
    assert_eq!(xhci::set_ep0_max_packet(kbd.slot, 0), Err(XhciError::BadEndpoint));
    xhci::set_ep0_max_packet(kbd.slot, 8).expect("Evaluate Context 8");
    assert_eq!(xhci::control_in(kbd.slot, dev, buf, 18), Ok(18));
    xhci::set_ep0_max_packet(kbd.slot, 64).expect("Evaluate Context 64");
    assert_eq!(xhci::control_in(kbd.slot, dev, buf, 18), Ok(18));
    assert_eq!(xhci::set_ep0_max_packet(0, 64), Err(XhciError::BadSlot));
}
