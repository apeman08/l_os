//! The wipe plan computed at boot (spec §8.3, §8.4): it covers every `Usable`
//! byte, the kernel's writable segments and boot stack minus the island and
//! RELRO, and the island and kernel code are never `Usable`.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader_api::info::MemoryRegionKind;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};
use l_core::handles::{Object, Rights};
use l_os::{memory, secrets, serial_println, usermode, wipe};
use x86_64::VirtAddr;

bootloader_api::entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    l_os::init();
    // init_memory saves the BootFacts and arms the wipe plan.
    l_os::init_memory(boot_info);
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

const PAGE: u64 = 4096;

fn phys(virt: u64) -> u64 {
    memory::virt_to_phys(VirtAddr::new(virt)).expect("mapped").as_u64()
}

fn in_usable(p: u64) -> bool {
    let facts = l_os::boot_facts().expect("boot facts");
    facts
        .memory_regions
        .iter()
        .any(|r| r.kind == MemoryRegionKind::Usable && r.start <= p && p < r.end)
}

/// Physical address of every page of the virtual range `[start, start+len)`.
fn pages(start: u64, len: u64) -> impl Iterator<Item = u64> {
    let first = start & !(PAGE - 1);
    let end = (start + len).next_multiple_of(PAGE);
    (first..end).step_by(PAGE as usize).map(phys)
}

#[test_case]
fn plan_covers_every_usable_byte() {
    let plan = wipe::plan().expect("plan");
    let facts = l_os::boot_facts().expect("boot facts");
    let want = memory::usable_bytes(facts.memory_regions);
    serial_println!("  usable {} bytes in {} ranges", plan.usable.total_bytes(), plan.usable.len());
    assert!(want > 0);
    assert_eq!(plan.usable.total_bytes(), want);
    for r in facts.memory_regions.iter().filter(|r| r.kind == MemoryRegionKind::Usable) {
        assert!(plan.usable.contains(r.start) && plan.usable.contains(r.end - 1), "{:#x}..{:#x}", r.start, r.end);
    }
}

#[test_case]
fn island_and_code_not_in_usable() {
    let plan = wipe::plan().expect("plan");
    let (island, len) = wipe::island_virt();
    assert!(len >= 16384, "island holds the 16 KiB wipe stack");
    let mut n = 0;
    for p in pages(island, len as u64) {
        assert!(!in_usable(p), "island page {:#x} is Usable", p);
        assert!(plan.island_phys.contains(p), "island page {:#x} missing from island_phys", p);
        n += 1;
    }
    assert_eq!(plan.island_phys.total_bytes(), n * PAGE);
    // Kernel code (spec §8.3 a): this test kernel's entry and a library function.
    for code in [main as *const () as u64, l_os::hlt_loop as *const () as u64, wipe::arm as *const () as u64] {
        let p = phys(code);
        assert!(!in_usable(p), "code page {:#x} is Usable", p);
    }
    // The kernel's own L4 (the wipe switches to it) is not Usable either.
    assert!(!in_usable(plan.kernel_l4));
}

#[test_case]
fn kernel_rw_excludes_island_and_relro() {
    let plan = wipe::plan().expect("plan");
    let facts = l_os::boot_facts().expect("boot facts");
    let (island, len) = wipe::island_virt();
    for p in pages(island, len as u64) {
        assert!(!plan.kernel_rw.contains(p), "island page {:#x} in kernel_rw", p);
    }
    let info = wipe::kernel_load_info(&facts).expect("kernel ELF");
    let (rs, re) = info.relro.expect("the kernel has a PT_GNU_RELRO segment");
    let off = facts.kernel_image_offset;
    let mut n = 0;
    for p in pages(off + rs, re - rs) {
        assert!(!plan.kernel_rw.contains(p), "RELRO page {:#x} in kernel_rw", p);
        n += 1;
    }
    serial_println!("  RELRO {} pages, kernel_rw {} bytes in {} ranges", n, plan.kernel_rw.total_bytes(), plan.kernel_rw.len());
    assert!(n > 0);
}

static mut BSS_PROBE: [u8; 64] = [0; 64];
static DATA_PROBE: AtomicU64 = AtomicU64::new(0x5eed_5eed);

#[test_case]
fn kernel_rw_contains_a_bss_static() {
    let plan = wipe::plan().expect("plan");
    let bss = &raw const BSS_PROBE as u64;
    let data = &DATA_PROBE as *const AtomicU64 as u64;
    assert_eq!(DATA_PROBE.load(Ordering::Relaxed), 0x5eed_5eed);
    for (name, v) in [("bss", bss), ("data", data)] {
        let p = phys(v);
        assert!(plan.kernel_rw.contains(p), "{} static virt {:#x} phys {:#x} not in kernel_rw", name, v, p);
    }
    // We run on the bootloader's kernel stack: it is planned, too.
    let local = 0u64;
    let p = phys(&local as *const u64 as u64);
    assert!(plan.kernel_rw.contains(p), "boot stack page {:#x} not in kernel_rw", p);
    // The key slots are in .bss as well, and their virtual ranges are in the plan.
    assert_eq!(plan.keys, secrets::slot_ranges().map(|(v, n)| (v.as_u64(), n)));
    for (v, n) in secrets::slot_ranges() {
        assert_eq!(n, secrets::SLOT_SIZE);
        for p in pages(v.as_u64(), n as u64) {
            assert!(plan.kernel_rw.contains(p));
        }
    }
}

fn user_bits_set() -> u64 {
    (0..memory::USER_FRAMES.len()).map(|i| memory::USER_FRAMES[i].load(Ordering::Relaxed).count_ones() as u64).sum()
}

#[test_case]
fn user_frames_marked_and_cleared() {
    assert_eq!(user_bits_set(), 0, "no process yet");
    let before = memory::user_frames_marked();
    let handles: &[(Object, Rights)] = &[];
    let budget = l_os::time::ticks_from_ms(5500);
    let outcome = usermode::run(usermode::prog_stack_11(), handles, budget).expect("run");
    serial_println!("  outcome {:?}, user frames marked {}", outcome, memory::user_frames_marked() - before);
    // Exactly the code page plus the stack pages were mapped for the process ...
    assert_eq!(memory::user_frames_marked() - before, 1 + usermode::USER_STACK_PAGES);
    // ... and all of them were cleared when its address space was destroyed.
    assert_eq!(user_bits_set(), 0);
    assert!(!memory::is_user_frame(0));
}

#[test_case]
fn amnesia_plan_ready_after_arm() {
    let a = wipe::amnesia();
    assert!(a.plan_ready);
    assert!(!a.stick_sealed, "no USB stick driver yet");
    assert_eq!(a.serial_mirror, l_os::SERIAL_MIRROR);
    // Arming is one-shot; a second call is refused and keeps the plan.
    let facts = l_os::boot_facts().expect("boot facts");
    assert_eq!(wipe::arm(&facts), Err(wipe::ArmError::AlreadyArmed));
    assert!(wipe::amnesia().plan_ready);
    let plan = wipe::plan().expect("plan");
    assert_eq!(plan.power, l_os::acpi::power_off_plan());
    assert!(plan.power.is_some());
    let (fb, fb_len) = plan.fb;
    assert!(fb != 0 && fb_len > 0, "framebuffer base/len in the plan");
    assert_eq!(plan.fb, l_os::framebuffer::raw_region());
}

fn assert_tables_not_usable(what: &str, virt: u64) -> wipe::PageWalk {
    let w = wipe::page_walk(virt).unwrap_or_else(|| panic!("{} {:#x} unmapped", what, virt));
    assert!(w.levels >= 2, "{} walk too short", what);
    for &t in &w.tables[..w.levels] {
        assert!(!in_usable(t), "{} {:#x}: table frame {:#x} is Usable", what, virt, t);
    }
    assert_eq!(w.phys, phys(virt), "walk agrees with the mapper");
    w
}

#[test_case]
fn page_tables_of_wipe_path_not_in_usable() {
    let plan = wipe::plan().expect("plan");
    let facts = l_os::boot_facts().expect("boot facts");
    let (island, len) = wipe::island_virt();
    for v in (island..island + len as u64).step_by(PAGE as usize) {
        assert_tables_not_usable("island", v);
    }
    let info = wipe::kernel_load_info(&facts).expect("kernel ELF");
    let mut pages_checked = 0;
    for seg in info.segments() {
        let s = facts.kernel_image_offset + seg.vaddr;
        for v in ((s & !(PAGE - 1))..s + seg.mem_size).step_by(PAGE as usize) {
            assert_tables_not_usable("kernel segment", v);
            pages_checked += 1;
        }
    }
    for v in (facts.kernel_stack_bottom..facts.kernel_stack_bottom + facts.kernel_stack_len).step_by(PAGE as usize) {
        assert_tables_not_usable("boot stack", v);
    }
    // The offset window over every range the wipe zeroes.
    let mut windows = 0;
    for r in plan.usable.iter().chain(plan.kernel_rw.iter()).chain(plan.island_phys.iter()) {
        let mut a = r.start & !(PAGE - 1);
        while a < r.end {
            let w = assert_tables_not_usable("offset window", plan.phys_offset + a);
            a = (a | (w.leaf_size - 1)) + 1;
            windows += 1;
        }
    }
    serial_println!("  {} kernel pages, {} offset-window leaves checked", pages_checked, windows);
}

#[test_case]
fn check_walk_rejects_a_usable_table_frame() {
    // Positive control for arm's check: pretend one real table frame of the
    // island's translation (each level in turn) were Usable.
    let (island, _) = wipe::island_virt();
    let w = wipe::page_walk(island).expect("island mapped");
    let none = l_core::memrange::RangeList::<64>::new();
    assert!(wipe::check_walk(island, &none).is_ok());
    for &t in &w.tables[..w.levels] {
        let mut fake = l_core::memrange::RangeList::<64>::new();
        fake.push(l_core::memrange::PhysRange { start: t, end: t + PAGE }).unwrap();
        assert_eq!(wipe::check_walk(island, &fake), Err(wipe::ArmError::InUsable(t)));
    }
    // And the leaf itself.
    let mut fake = l_core::memrange::RangeList::<64>::new();
    let leaf = w.phys & !(PAGE - 1);
    fake.push(l_core::memrange::PhysRange { start: leaf, end: leaf + PAGE }).unwrap();
    assert!(matches!(wipe::check_walk(island, &fake), Err(wipe::ArmError::InUsable(_))));
}
