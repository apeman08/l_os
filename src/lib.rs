#![no_std]

#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use core::panic::PanicInfo;

pub mod serial;
pub mod fb_console;
pub mod aurora;
pub mod framebuffer;
pub mod interrupts;
pub mod gdt;
pub mod frames;
pub mod memory;
pub mod mmio;
pub mod net;
pub mod rng;
pub mod rtc;
pub mod pci;
pub mod acpi;
pub mod secrets;
pub mod wipe;
pub mod virtio_net;
pub mod allocator;
pub mod input;
pub mod ipc;
pub mod sched;
pub mod syscall;
pub mod cpu;
pub mod time;
pub mod usermode;

pub fn init() {
    cpu::init();
    // Learn which devices exist (reads PCI configuration space; sends nothing).
    pci::enumerate();
    interrupts::idt_init();
    gdt::init();
    // SAFETY: remaps the PICs to the vectors the IDT (loaded above) handles,
    // then unmasks only IRQ0 (timer), IRQ1 (keyboard) and IRQ2 (cascade).
    // UEFI firmware leaves every PIC line masked (the BIOS used to leave timer
    // and keyboard on), so these are enabled explicitly. IRQ 9/10/11 do have
    // handlers (`nic_interrupt`) but stay masked, so those handlers are
    // dormant: networking still works because every timer tick calls
    // `sched::wake_net`, which lets waiting processes poll the NIC.
    unsafe {
        let mut pics = interrupts::PICS.lock();
        pics.initialize();
        pics.write_masks(0b1111_1000, 0b1111_1111);
    }
    time::init_pit();
    x86_64::instructions::interrupts::enable();
    // Wipe timing (spec §8.6): ~50 ms against the PIT. 0 = uncalibrated.
    time::calibrate_tsc();
}

/// Virtual address at which the bootloader maps all physical memory
/// (level-4 slot 32). Fixed so that it can never land on a slot the kernel
/// uses itself (heap: slot 136, user window: slot 224).
pub const PHYSICAL_MEMORY_OFFSET: u64 = 0x_1000_0000_0000;

/// Highest address the bootloader may pick for its dynamic mappings (kernel
/// stack, boot info, framebuffer): everything stays below level-4 slot 128,
/// i.e. below [`allocator::HEAP_START`] and the user slot
/// ([`memory::USER_L4_INDEX`]).
pub const DYNAMIC_RANGE_END: u64 = 0x_3fff_ffff_f000;

/// Boot configuration shared by the kernel and every test kernel; pass it to
/// `bootloader_api::entry_point!(main, config = &l_os::BOOTLOADER_CONFIG)`.
pub const BOOTLOADER_CONFIG: bootloader_api::BootloaderConfig = {
    use bootloader_api::config::Mapping;
    let mut config = bootloader_api::BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::FixedAddress(PHYSICAL_MEMORY_OFFSET));
    config.mappings.dynamic_range_end = Some(DYNAMIC_RANGE_END);
    // Same as bootloader 0.9's default (512 pages); 0.11 would give 80 KiB.
    config.kernel_stack_size = 512 * 4096;
    config
};

/// What the wipe plan needs from the bootloader's `BootInfo`, saved at boot
/// by [`init_memory`] (spec §8.4). All addresses as the bootloader reports
/// them: `kernel_addr` physical, `kernel_stack_bottom` virtual.
#[derive(Debug, Clone, Copy)]
pub struct BootFacts {
    pub rsdp_addr: Option<u64>,
    /// Physical address and length of the kernel ELF file in RAM.
    pub kernel_addr: u64,
    pub kernel_len: u64,
    /// Added to the ELF's virtual addresses (the kernel is a static PIE).
    pub kernel_image_offset: u64,
    /// Lowest virtual address and length of the boot kernel stack.
    pub kernel_stack_bottom: u64,
    pub kernel_stack_len: u64,
    pub memory_regions: &'static [bootloader_api::info::MemoryRegion],
}

static BOOT_FACTS: spin::Mutex<Option<BootFacts>> = spin::Mutex::new(None);

/// The facts saved by [`init_memory`], `None` before it ran.
pub fn boot_facts() -> Option<BootFacts> {
    *BOOT_FACTS.lock()
}

/// The boot id slot. The image builder (`tools/image`) finds these 32 bytes in
/// the kernel ELF and replaces them with the embedded marker; the placeholder
/// must appear exactly once in the binary, so nothing else may use
/// `l_core::bootmark::PLACEHOLDER` in non-test code. `#[used]` keeps the slot
/// even though nothing but `boot_id` reads it.
#[used]
#[unsafe(link_section = ".rodata.l_os_bootid")]
pub static BOOT_ID: [u8; 32] = l_core::bootmark::PLACEHOLDER;

/// The id the image builder embedded in this kernel; `None` when the kernel
/// was not patched (still the placeholder, e.g. started outside `l_os_run`).
pub fn boot_id() -> Option<l_core::bootmark::BootId> {
    // SAFETY: `BOOT_ID` is a valid, initialized, aligned static of 32 bytes
    // that lives for the whole run. The volatile read keeps the compiler from
    // constant-folding the placeholder, which the image builder changes after
    // compilation.
    let raw = unsafe { core::ptr::read_volatile(&BOOT_ID) };
    l_core::bootmark::parse_embedded(&raw)
}

/// Whether console writes are mirrored to COM1 (cargo feature `serial-console`,
/// off by default; never on in an amnesic image).
pub const SERIAL_MIRROR: bool = cfg!(feature = "serial-console");

/// Take over what the bootloader handed over: the framebuffer, paging access,
/// the frame allocator and the kernel heap; then start the framebuffer console
/// (it needs the heap). Until then there is no screen console: output
/// reaches only the serial mirror (feature `serial-console`, off by default),
/// i.e. nowhere in a default build.
pub fn init_memory(boot_info: &'static mut bootloader_api::BootInfo) {
    use x86_64::VirtAddr;
    let fb_memory = framebuffer::init(boot_info.framebuffer.take());
    let boot_info: &'static bootloader_api::BootInfo = boot_info;
    let facts = BootFacts {
        rsdp_addr: boot_info.rsdp_addr.into_option(),
        kernel_addr: boot_info.kernel_addr,
        kernel_len: boot_info.kernel_len,
        kernel_image_offset: boot_info.kernel_image_offset,
        kernel_stack_bottom: boot_info.kernel_stack_bottom,
        kernel_stack_len: boot_info.kernel_stack_len,
        memory_regions: &boot_info.memory_regions,
    };
    *BOOT_FACTS.lock() = Some(facts);
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("bootloader did not map physical memory (BOOTLOADER_CONFIG not used?)");
    assert_eq!(offset, PHYSICAL_MEMORY_OFFSET, "physical memory mapped at an unexpected offset");
    // SAFETY: values come straight from the bootloader's BootInfo; the config
    // above makes it map all physical memory at `offset`, and the regions it
    // reports as usable are unused. This is called once during boot.
    unsafe {
        memory::init(VirtAddr::new(offset), &boot_info.memory_regions);
    }
    let mut guard = memory::MEMORY.lock();
    let mem = guard.as_mut().expect("memory initialized above");
    // SAFETY: `init` (called first by every entry point) loaded the kernel's
    // own GDT and IDT, and the bootloader's context-switch code is never run
    // again; the function asserts the GDT/IDT part. No address space exists
    // yet, so no copy of the removed slots survives.
    unsafe { memory::remove_boot_identity_map(mem, &boot_info.memory_regions) };
    allocator::init_heap(mem).expect("kernel heap initialization failed");
    // Before any address space exists: processes copy the window's L4 entry.
    mmio::init(mem);
    drop(guard);
    // Everything up to the end of the highest region is mapped at the offset.
    let phys_end = boot_info.memory_regions.iter().map(|r| r.end).max().unwrap_or(0);
    acpi::set_phys_limit(phys_end);
    acpi::init(facts.rsdp_addr);
    let raw_info = framebuffer::info();
    let fb_info = raw_info.as_ref().and_then(framebuffer::render_info);
    match (fb_info, fb_memory) {
        (Some(info), Some(memory)) => fb_console::init(info, memory),
        // Not silent: without a console the screen stays dark. Serial always
        // works (independent of the `serial-console` feature).
        _ => match raw_info {
            Some(raw) => serial_println!(
                "no usable framebuffer (format {:?}); no screen console",
                raw.pixel_format
            ),
            None => serial_println!("no usable framebuffer (format: none, no GOP framebuffer); no screen console"),
        },
    }
    // After acpi::init (the plan holds the S5 values) and after the console,
    // so that a failure is visible. Errors are reported by `arm` itself; the
    // system keeps running with "amnesisch: nein".
    let _ = wipe::arm(&facts);
}

/// Run the interactive shell as a ring-3 process, forever. It holds exactly:
/// the console (write + grant), the keyboard, read-only system info, and a
/// spawner for the built-in programs. If it ends, it is started again.
pub fn run_shell_forever() -> ! {
    use l_core::handles::{Object, Rights};
    use usermode::{Spec, run_all, shell_program, PROGRAM_COUNT};

    let handles = [
        (Object::Console, Rights::WRITE.union(Rights::GRANT)),
        (Object::Keyboard, Rights::READ),
        (Object::System, Rights::READ.union(Rights::GRANT)),
        (Object::Spawner((1u16 << PROGRAM_COUNT) - 1), Rights::USE),
        // Trusted-path authority: only the shell may change network grants.
        (Object::NetManager, Rights::USE),
        // The right to ask; programs get it only if the shell passes it on.
        (Object::NetClient, Rights::USE.union(Rights::GRANT)),
        (Object::Random, Rights::READ.union(Rights::GRANT)),
    ];
    // A shell that dies right after starting would be restarted in a tight loop;
    // give up after a few very short runs in a row.
    let min_healthy_ticks = time::ticks_from_seconds(1);
    const MAX_QUICK_FAILURES: u32 = 3;
    let mut quick_failures = 0;
    loop {
        let started = time::ticks();
        match run_all(&[Spec { program: shell_program(), handles: &handles, budget_ticks: u64::MAX, args: &[] }]) {
            Ok(results) => println!("\n[shell ended: {:?}; restarting]", results[0].outcome),
            Err(e) => {
                println!("\ncannot start the shell: {:?}", e);
                hlt_loop();
            }
        }
        if time::ticks() - started < min_healthy_ticks {
            quick_failures += 1;
            if quick_failures >= MAX_QUICK_FAILURES {
                println!("the shell keeps failing right after start; not restarting again");
                hlt_loop();
            }
        } else {
            quick_failures = 0;
        }
    }
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

pub trait Testable {
    fn run(&self) -> ();
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial::emergency_print(format_args!("[failed]\n\nError: {}\n\n", info));
    exit_qemu(QemuExitCode::Failed);
    hlt_loop();
}

// Entry point for `cargo test`
#[cfg(test)]
bootloader_api::entry_point!(test_kernel_main, config = &BOOTLOADER_CONFIG);

#[cfg(test)]
fn test_kernel_main(boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    init();
    // The in-crate wipe tests need the framebuffer, the heap and the plan.
    init_memory(boot_info);
    test_main();
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    use x86_64::instructions::port::Port;

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }
}