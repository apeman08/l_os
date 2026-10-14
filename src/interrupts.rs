use x86_64::{PrivilegeLevel, VirtAddr};
use l_core::syscall::Outcome;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use crate::println;
use lazy_static::lazy_static;
use crate::gdt;
use pic8259::ChainedPics;
use spin;

lazy_static! {
    static ref IDT:  InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        unsafe {
            idt.double_fault.set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt.page_fault.set_handler_fn(page_fault_handler);
        // SAFETY: syscall_entry is an assembly stub that follows the interrupt
        // gate contract (saves/restores registers and ends with iretq).
        // DPL 3 lets ring 3 invoke exactly this vector with `int 0x80`.
        unsafe {
            idt[0x80]
                .set_handler_addr(VirtAddr::new(crate::usermode::syscall_entry as *const () as u64))
                .set_privilege_level(PrivilegeLevel::Ring3);
        }
        idt.divide_error.set_handler_fn(divide_error_handler);
        idt.non_maskable_interrupt.set_handler_fn(nmi_handler);
        idt.overflow.set_handler_fn(overflow_handler);
        idt.bound_range_exceeded.set_handler_fn(bound_range_handler);
        idt.device_not_available.set_handler_fn(device_not_available_handler);
        idt.x87_floating_point.set_handler_fn(x87_handler);
        idt.simd_floating_point.set_handler_fn(simd_handler);
        idt.alignment_check.set_handler_fn(alignment_check_handler);
        idt.machine_check.set_handler_fn(machine_check_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.invalid_tss.set_handler_fn(invalid_tss_handler);
        idt.segment_not_present.set_handler_fn(segment_not_present_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_fault_handler);
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);
        idt[InterruptIndex::Timer.as_u8()].set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);
        // PCI interrupt lines the virtio NIC may be routed to (QEMU: 10 or 11).
        idt[PIC_1_OFFSET + 9].set_handler_fn(nic_irq9);
        idt[PIC_1_OFFSET + 10].set_handler_fn(nic_irq10);
        idt[PIC_1_OFFSET + 11].set_handler_fn(nic_irq11);
        idt
    };
}

pub fn idt_init() {
    IDT.load();
}

// Fault handlers are fail-stop: with no processes yet there is nothing to
// kill or recover, so they panic (which prints and halts; tests see a failure).
extern "x86-interrupt" fn page_fault_handler(stack_frame: InterruptStackFrame, error_code: PageFaultErrorCode) {
    use x86_64::registers::control::Cr2;

    if from_user(&stack_frame) {
        crate::usermode::terminate_current(Outcome::PageFault);
    }
    panic!(
        "EXCEPTION: PAGE FAULT\nAccessed Address: {:?}\nError Code: {:?}\n{:#?}",
        Cr2::read(), error_code, stack_frame
    );
}

/// Did the exception interrupt ring-3 code?
fn from_user(frame: &InterruptStackFrame) -> bool {
    frame.code_segment.rpl() == PrivilegeLevel::Ring3
}

extern "x86-interrupt" fn divide_error_handler(stack_frame: InterruptStackFrame) {
    if from_user(&stack_frame) {
        crate::usermode::terminate_current(Outcome::DivideError);
    }
    panic!("EXCEPTION: DIVIDE ERROR\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn nmi_handler(stack_frame: InterruptStackFrame) {
    // An NMI is a hardware/firmware event, never caused by a user process.
    panic!("EXCEPTION: NON-MASKABLE INTERRUPT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn machine_check_handler(stack_frame: InterruptStackFrame) -> ! {
    panic!("EXCEPTION: MACHINE CHECK\n{:#?}", stack_frame);
}

macro_rules! simple_fault_handlers {
    ($($name:ident => $label:literal),* $(,)?) => {$(
        extern "x86-interrupt" fn $name(stack_frame: InterruptStackFrame) {
            if from_user(&stack_frame) {
                crate::usermode::terminate_current(Outcome::OtherFault);
            }
            panic!(concat!("EXCEPTION: ", $label, "\n{:#?}"), stack_frame);
        }
    )*};
}

simple_fault_handlers! {
    overflow_handler => "OVERFLOW",
    bound_range_handler => "BOUND RANGE EXCEEDED",
    device_not_available_handler => "DEVICE NOT AVAILABLE",
    x87_handler => "x87 FLOATING POINT",
    simd_handler => "SIMD FLOATING POINT",
}

extern "x86-interrupt" fn alignment_check_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    if from_user(&stack_frame) {
        crate::usermode::terminate_current(Outcome::OtherFault);
    }
    panic!("EXCEPTION: ALIGNMENT CHECK (code {:#x})\n{:#?}", error_code, stack_frame);
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    if from_user(&stack_frame) {
        crate::usermode::terminate_current(Outcome::InvalidOpcode);
    }
    panic!("EXCEPTION: INVALID OPCODE\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn invalid_tss_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    if from_user(&stack_frame) {
        crate::usermode::terminate_current(Outcome::GeneralProtection);
    }
    panic!("EXCEPTION: INVALID TSS (selector {:#x})\n{:#?}", error_code, stack_frame);
}

extern "x86-interrupt" fn segment_not_present_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    if from_user(&stack_frame) {
        crate::usermode::terminate_current(Outcome::GeneralProtection);
    }
    panic!("EXCEPTION: SEGMENT NOT PRESENT (selector {:#x})\n{:#?}", error_code, stack_frame);
}

extern "x86-interrupt" fn stack_segment_fault_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    if from_user(&stack_frame) {
        crate::usermode::terminate_current(Outcome::GeneralProtection);
    }
    panic!("EXCEPTION: STACK SEGMENT FAULT (selector {:#x})\n{:#?}", error_code, stack_frame);
}

extern "x86-interrupt" fn general_protection_fault_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    if from_user(&stack_frame) {
        crate::usermode::terminate_current(Outcome::GeneralProtection);
    }
    panic!("EXCEPTION: GENERAL PROTECTION FAULT (code {:#x})\n{:#?}", error_code, stack_frame);
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    let mut port = Port::new(0x60);
    // SAFETY: 0x60 is the PS/2 controller data port; reading it inside the
    // keyboard IRQ handler is the required way to acknowledge the byte.
    let scancode: u8 = unsafe { port.read() };
    // Once a wipe runs, input is discarded (stage `InputLocked`). Decoding
    // and echo happen in the shell loop, not in interrupt context; `feed`
    // only queues the byte and checks the emergency hotkey (which starts the
    // wipe right here and never returns).
    if !crate::wipe::wiping() {
        crate::input::feed(scancode);
    }
    // A process may be sleeping in read_key(); the lock is only ever held with
    // interrupts disabled, so taking it here cannot deadlock.
    crate::sched::wake_keyboard();

    // SAFETY: EOI for the vector that is currently being serviced.
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

extern "x86-interrupt" fn timer_interrupt_handler(stack_frame: InterruptStackFrame) {
    crate::time::tick();

    // SAFETY: EOI for the vector that is currently being serviced.
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
    // Spec §7.2: USB polling after EOI, before `on_tick` (which may switch
    // away). IF=0 here; it only try-locks the driver and skips if busy.
    crate::xhci::poll_tick();
    // After EOI and with no locks held: a runaway user process may be killed.
    crate::sched::wake_net();
    crate::sched::on_tick(from_user(&stack_frame));
}

fn nic_interrupt(irq: u8) {
    // Reading the device's ISR register acknowledges it; then let waiting
    // processes poll the stack (the stack itself is never touched here).
    if crate::virtio_net::acknowledge_irq() {
        crate::sched::wake_net();
    }
    // SAFETY: EOI for the vector currently being serviced.
    unsafe { PICS.lock().notify_end_of_interrupt(PIC_1_OFFSET + irq) };
}

extern "x86-interrupt" fn nic_irq9(_: InterruptStackFrame) {
    nic_interrupt(9);
}
extern "x86-interrupt" fn nic_irq10(_: InterruptStackFrame) {
    nic_interrupt(10);
}
extern "x86-interrupt" fn nic_irq11(_: InterruptStackFrame) {
    nic_interrupt(11);
}

extern "x86-interrupt" fn  breakpoint_handler(stack_frame: InterruptStackFrame) {
    println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame)
    
}

extern "x86-interrupt" fn double_fault_handler(
        stack_frame: InterruptStackFrame, _error_code: u64) -> !
    {
        panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
    }

// Setup Hardware Interrupts
pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: spin::Mutex<ChainedPics> =
    spin::Mutex::new(unsafe {ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET)});

// Timer handler setup
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }
}

// TESTS

#[test_case]
fn test_breakpoint_exception() {
    // invoke breakpoint exception
    x86_64::instructions::interrupts::int3();
}