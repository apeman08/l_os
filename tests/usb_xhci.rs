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
