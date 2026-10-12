use x86_64::VirtAddr;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::structures::gdt::SegmentSelector;
use lazy_static::lazy_static;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// Selectors (RPL 3) for entering user mode.
pub fn user_selectors() -> (SegmentSelector, SegmentSelector) {
    (GDT.1.user_code_selector, GDT.1.user_data_selector)
}

pub fn init() {
    use x86_64::instructions::tables::load_tss;
    use x86_64::instructions::segmentation::{CS, Segment};

    use x86_64::instructions::segmentation::{DS, ES, SS};

    init_tss();
    GDT.0.load();
    // SAFETY: the selectors come from the GDT that was just loaded, so they
    // reference valid kernel code/data descriptors and the TSS descriptor.
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        SS::set_reg(GDT.1.data_selector);
        DS::set_reg(GDT.1.data_selector);
        ES::set_reg(GDT.1.data_selector);
        load_tss(GDT.1.tss_selector);
    }
}

/// The TSS is mutated at runtime (ring-0 stack pointer per process), so it
/// lives in an `UnsafeCell` instead of behind a shared reference.
struct TssCell(core::cell::UnsafeCell<TaskStateSegment>);
// SAFETY: single CPU; the TSS is written only with interrupts disabled (during
// init and context switches) and read by the CPU on privilege changes.
unsafe impl Sync for TssCell {}

static TSS: TssCell = TssCell(core::cell::UnsafeCell::new(TaskStateSegment::new()));

const IST_STACK_SIZE: usize = 4096 * 5;
static mut DOUBLE_FAULT_STACK: [u8; IST_STACK_SIZE] = [0; IST_STACK_SIZE];
static mut BOOT_PRIVILEGE_STACK: [u8; IST_STACK_SIZE] = [0; IST_STACK_SIZE];

/// Point the CPU's ring-3 -> ring-0 stack at `top` (16-byte aligned stack top).
/// Called on every switch to a process, with interrupts disabled.
pub fn set_kernel_stack(top: u64) {
    // SAFETY: see `TssCell`; the field is packed, so write unaligned.
    unsafe {
        let tss = TSS.0.get();
        (&raw mut (*tss).privilege_stack_table[0]).write_unaligned(VirtAddr::new(top));
    }
}

fn init_tss() {
    // SAFETY: called once from `init` before the TSS descriptor is loaded.
    unsafe {
        let tss = TSS.0.get();
        let df_top = VirtAddr::from_ptr(&raw const DOUBLE_FAULT_STACK) + IST_STACK_SIZE as u64;
        (&raw mut (*tss).interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize]).write_unaligned(df_top);
    }
    // Until a process runs, ring-3 entries (none expected) use this stack.
    set_kernel_stack(
        VirtAddr::from_ptr(&raw const BOOT_PRIVILEGE_STACK).as_u64() + IST_STACK_SIZE as u64,
    );
}

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        // User data before user code: matches the layout `sysret` would need.
        let user_data_selector = gdt.append(Descriptor::user_data_segment());
        let user_code_selector = gdt.append(Descriptor::user_code_segment());
        // SAFETY: TSS is a static with a stable address that lives forever; the
        // descriptor only records its address and size.
        let tss_selector = gdt.append(unsafe { Descriptor::tss_segment_unchecked(TSS.0.get()) });
        (gdt, Selectors { code_selector, data_selector, user_code_selector, user_data_selector, tss_selector })
    };
}

struct Selectors {
    code_selector: SegmentSelector,
    data_selector: SegmentSelector,
    user_code_selector: SegmentSelector,
    user_data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

// https://os.phil-opp.com/double-fault-exceptions/
// The Final Steps