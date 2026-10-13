//! What the kernel's page tables may execute.
//!
//! bootloader 0.11 identity-maps two pages of its own `context_switch` code and
//! its GDT page with flags PRESENT only (supervisor, executable). Those frames
//! sit in UEFI LOADER_CODE / bootloader memory, and LOADER_CODE is reported as
//! `Usable`, so without cleanup the frame allocator could hand a frame to a
//! process while the same bytes stay executable in ring 0 at virt == phys in
//! every address space. These tests walk the real page tables and require that
//! the only executable mappings are the kernel image's own executable segments,
//! and that none of them points into memory the allocator may hand out.
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(l_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader_api::info::MemoryRegionKind;
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use l_os::{memory, serial_println};
use spin::Mutex;
use x86_64::{
    registers::control::Cr3,
    structures::paging::{PageTable, PageTableFlags as F, PhysFrame},
};

entry_point!(main, config = &l_os::BOOTLOADER_CONFIG);

const MAX_RANGES: usize = 256;

/// Half-open address ranges, copied out of the boot info before
/// `init_memory` takes it over (no heap yet, hence fixed arrays).
struct Ranges {
    r: [(u64, u64); MAX_RANGES],
    n: usize,
}

impl Ranges {
    const fn new() -> Self {
        Self { r: [(0, 0); MAX_RANGES], n: 0 }
    }
    fn push(&mut self, start: u64, end: u64) {
        assert!(self.n < MAX_RANGES, "too many ranges for the test's table");
        self.r[self.n] = (start, end);
        self.n += 1;
    }
    fn overlaps(&self, start: u64, end: u64) -> bool {
        self.r[..self.n].iter().any(|&(s, e)| start < e && s < end)
    }
    fn contains(&self, start: u64, end: u64) -> bool {
        self.r[..self.n].iter().any(|&(s, e)| s <= start && end <= e)
    }
}

/// Physical ranges the bootloader reports as `Usable`.
static USABLE: Mutex<Ranges> = Mutex::new(Ranges::new());
/// Virtual ranges of the kernel image's executable PT_LOAD segments.
static KERNEL_TEXT: Mutex<Ranges> = Mutex::new(Ranges::new());

fn main(boot_info: &'static mut BootInfo) -> ! {
    {
        let mut usable = USABLE.lock();
        for r in boot_info.memory_regions.iter().filter(|r| r.kind == MemoryRegionKind::Usable) {
            usable.push(r.start, r.end);
        }
    }
    record_kernel_text(boot_info);
    l_os::init();
    l_os::init_memory(boot_info);
    test_main();
    l_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    l_os::test_panic_handler(info)
}

/// Read the kernel ELF's program headers (the file stays in memory, at
/// `kernel_addr`, reachable through the physical-memory mapping) and record
/// every executable PT_LOAD segment, relocated by `kernel_image_offset`.
fn record_kernel_text(boot_info: &BootInfo) {
    let base = l_os::PHYSICAL_MEMORY_OFFSET + boot_info.kernel_addr;
    // SAFETY: the bootloader mapped all physical memory at
    // PHYSICAL_MEMORY_OFFSET and the kernel file occupies
    // [kernel_addr, kernel_addr + kernel_len); every read is bounds-checked
    // against kernel_len below.
    let file = unsafe { core::slice::from_raw_parts(base as *const u8, boot_info.kernel_len as usize) };
    let u16_at = |o: usize| u16::from_le_bytes(file[o..o + 2].try_into().unwrap()) as usize;
    let u32_at = |o: usize| u32::from_le_bytes(file[o..o + 4].try_into().unwrap());
    let u64_at = |o: usize| u64::from_le_bytes(file[o..o + 8].try_into().unwrap());
    assert_eq!(&file[..4], b"\x7fELF", "kernel_addr does not point at the kernel ELF");
    let (phoff, phentsize, phnum) = (u64_at(0x20) as usize, u16_at(0x36), u16_at(0x38));
    let mut text = KERNEL_TEXT.lock();
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        let (p_type, p_flags) = (u32_at(ph), u32_at(ph + 4));
        const PT_LOAD: u32 = 1;
        const PF_X: u32 = 1;
        if p_type == PT_LOAD && p_flags & PF_X != 0 {
            let start = boot_info.kernel_image_offset + u64_at(ph + 16);
            let end = start + u64_at(ph + 40);
            text.push(start & !0xfff, (end + 0xfff) & !0xfff);
        }
    }
}

/// One executable leaf mapping found by [`walk`].
#[derive(Clone, Copy)]
struct Leaf {
    virt: u64,
    size: u64,
    phys: u64,
    flags: F,
}

fn table(frame: PhysFrame) -> &'static PageTable {
    // SAFETY: page-table frames are valid RAM mapped at the physical-memory
    // offset; the tables are only read, on a single CPU, by this test.
    unsafe { &*(memory::phys_offset() + frame.start_address().as_u64()).as_ptr::<PageTable>() }
}

fn sign_extend(v: u64) -> u64 {
    if v & (1 << 47) != 0 { v | 0xffff_0000_0000_0000 } else { v }
}

/// Visit every present leaf of the tables rooted at `l4` (all 512 slots, so
/// both halves) and call `f` for each one that is executable in ring 0, i.e.
/// NO_EXECUTE is clear on every level. Returns the number of present leaves.
fn walk(l4: PhysFrame, mut f: impl FnMut(Leaf)) -> usize {
    let mut leaves = 0;
    for (i4, e4) in table(l4).iter().enumerate() {
        if !e4.flags().contains(F::PRESENT) {
            continue;
        }
        let nx4 = e4.flags().contains(F::NO_EXECUTE);
        let l3 = table(PhysFrame::containing_address(e4.addr()));
        for (i3, e3) in l3.iter().enumerate() {
            if !e3.flags().contains(F::PRESENT) {
                continue;
            }
            let nx3 = nx4 || e3.flags().contains(F::NO_EXECUTE);
            let v3 = sign_extend(((i4 as u64) << 39) | ((i3 as u64) << 30));
            if e3.flags().contains(F::HUGE_PAGE) {
                leaves += 1;
                if !nx3 {
                    f(Leaf { virt: v3, size: 1 << 30, phys: e3.addr().as_u64(), flags: e3.flags() });
                }
                continue;
            }
            let l2 = table(PhysFrame::containing_address(e3.addr()));
            for (i2, e2) in l2.iter().enumerate() {
                if !e2.flags().contains(F::PRESENT) {
                    continue;
                }
                let nx2 = nx3 || e2.flags().contains(F::NO_EXECUTE);
                let v2 = v3 | ((i2 as u64) << 21);
                if e2.flags().contains(F::HUGE_PAGE) {
                    leaves += 1;
                    if !nx2 {
                        f(Leaf { virt: v2, size: 1 << 21, phys: e2.addr().as_u64(), flags: e2.flags() });
                    }
                    continue;
                }
                let l1 = table(PhysFrame::containing_address(e2.addr()));
                for (i1, e1) in l1.iter().enumerate() {
                    if !e1.flags().contains(F::PRESENT) {
                        continue;
                    }
                    leaves += 1;
                    if !(nx2 || e1.flags().contains(F::NO_EXECUTE)) {
                        let v1 = v2 | ((i1 as u64) << 12);
                        f(Leaf { virt: v1, size: 4096, phys: e1.addr().as_u64(), flags: e1.flags() });
                    }
                }
            }
        }
    }
    leaves
}

/// Check the tables rooted at `l4`; prints every offending mapping before
/// failing so a regression names the address, frame and flags.
fn check_address_space(l4: PhysFrame, what: &str) {
    let text = KERNEL_TEXT.lock();
    let usable = USABLE.lock();
    assert!(text.n > 0, "no executable segment found in the kernel ELF");
    let own_code = check_address_space as *const () as u64;
    let (mut exec, mut own_code_seen, mut outside_image, mut in_usable) = (0, false, 0, 0);
    let leaves = walk(l4, |l| {
        exec += 1;
        if (l.virt..l.virt + l.size).contains(&own_code) {
            own_code_seen = true;
        }
        let outside = !text.contains(l.virt, l.virt + l.size);
        let usable_frame = usable.overlaps(l.phys, l.phys + l.size);
        if outside || usable_frame {
            serial_println!(
                "\n  {}: executable mapping virt {:#x} (L4 {}) -> phys {:#x}, {} KiB, flags {:?}{}{}",
                what, l.virt, (l.virt >> 39) & 0x1ff, l.phys, l.size / 1024, l.flags,
                if outside { ", outside the kernel image's text" } else { "" },
                if usable_frame { ", frame in a Usable region" } else { "" },
            );
        }
        outside_image += outside as usize;
        in_usable += usable_frame as usize;
    });
    serial_println!("  {}: {} present leaves, {} executable", what, leaves, exec);
    // Non-vacuity: the walk must see the code it is running.
    assert!(own_code_seen, "{}: the walk did not find the running kernel code as executable", what);
    assert_eq!(outside_image, 0, "{}: executable mappings outside the kernel image (listed above)", what);
    assert_eq!(in_usable, 0, "{}: executable mappings onto allocatable (Usable) frames (listed above)", what);
}

#[test_case]
fn kernel_tables_execute_only_the_kernel_image() {
    let (active, _) = Cr3::read();
    assert_eq!(active, memory::kernel_l4(), "test must run on the kernel's own tables");
    check_address_space(active, "kernel L4");
}

#[test_case]
fn no_identity_mapping_in_the_lower_half() {
    // The bootloader's leftovers were exactly virt == phys pages; nothing the
    // kernel maps itself is identity-mapped.
    let mut found = 0;
    let leaves = walk_all_present(memory::kernel_l4(), |virt, phys, size| {
        if virt == phys && virt >> 47 == 0 {
            serial_println!("\n  identity mapping {:#x}, {} KiB", virt, size / 1024);
            found += 1;
        }
    });
    assert!(leaves > 0);
    assert_eq!(found, 0, "identity mappings in the kernel's lower half (listed above)");
}

#[test_case]
fn process_address_spaces_inherit_no_stray_executable_mapping() {
    let mut guard = memory::MEMORY.lock();
    let mem = guard.as_mut().unwrap();
    let l4 = memory::create_address_space(mem).expect("out of frames");
    check_address_space(l4, "process L4");
    memory::destroy_address_space(mem, l4);
}

/// Like [`walk`] but for every present leaf, executable or not.
fn walk_all_present(l4: PhysFrame, mut f: impl FnMut(u64, u64, u64)) -> usize {
    let mut leaves = 0;
    for (i4, e4) in table(l4).iter().enumerate() {
        if !e4.flags().contains(F::PRESENT) {
            continue;
        }
        for (i3, e3) in table(PhysFrame::containing_address(e4.addr())).iter().enumerate() {
            if !e3.flags().contains(F::PRESENT) {
                continue;
            }
            let v3 = sign_extend(((i4 as u64) << 39) | ((i3 as u64) << 30));
            if e3.flags().contains(F::HUGE_PAGE) {
                leaves += 1;
                f(v3, e3.addr().as_u64(), 1 << 30);
                continue;
            }
            for (i2, e2) in table(PhysFrame::containing_address(e3.addr())).iter().enumerate() {
                if !e2.flags().contains(F::PRESENT) {
                    continue;
                }
                let v2 = v3 | ((i2 as u64) << 21);
                if e2.flags().contains(F::HUGE_PAGE) {
                    leaves += 1;
                    f(v2, e2.addr().as_u64(), 1 << 21);
                    continue;
                }
                for (i1, e1) in table(PhysFrame::containing_address(e2.addr())).iter().enumerate() {
                    if e1.flags().contains(F::PRESENT) {
                        leaves += 1;
                        f(v2 | ((i1 as u64) << 12), e1.addr().as_u64(), 4096);
                    }
                }
            }
        }
    }
    leaves
}
