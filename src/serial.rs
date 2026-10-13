use uart_16550::{Config, Uart16550Tty, backend::PioBackend};
use spin::Mutex;
use lazy_static::lazy_static;

lazy_static! {
    pub static ref SERIAL1: Mutex<Uart16550Tty<PioBackend>> = Mutex::new(unsafe {
        Uart16550Tty::new_port(0x3F8, Config::default()).expect("failed to initialize UART")
    });
}

#[doc(hidden)]
pub fn _print(args: ::core::fmt::Arguments) {
    use core::fmt::Write;
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        // A write error must not turn into a panic (this runs on the panic path too).
        let _ = SERIAL1.lock().write_fmt(args);
    });
}

/// Prints to the host through the serial interface.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!($($arg)*))
    };
}

/// Prints to the host through the serial interface, appending a newline.
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::serial_print!(
        concat!($fmt, "\n"), $($arg)*));
}
/// Serial output from a panic path; never blocks on the port lock.
pub fn emergency_print(args: ::core::fmt::Arguments) {
    use core::fmt::Write;

    x86_64::instructions::interrupts::disable();
    // SAFETY: panicking is terminal and interrupts are off: whoever holds the
    // guard was interrupted by the panic and never resumes, so nothing races.
    unsafe { SERIAL1.force_unlock() };
    let _ = SERIAL1.lock().write_fmt(args);
}

/// COM1 base port (the UART `SERIAL1` drives).
const COM1: u16 = 0x3F8;
/// Line status register bit: transmit holding register empty.
const LSR_THRE: u8 = 1 << 5;
/// Bounded wait per byte (port reads). A missing or stuck UART costs at most
/// this many reads per byte, then the byte is written anyway (and lost).
const RAW_POLL_LIMIT: u32 = 100_000;

/// Write `bytes` to COM1 by polling the UART directly: no lock, no `static`,
/// no heap, no formatting. For the wipe path (spec §8.2 stage `Reported`),
/// which runs after the kernel's `.data`/`.bss` (including `SERIAL1` and its
/// lock) were zeroed. Relies on the UART having been configured at boot
/// (firmware or `SERIAL1`); it does not reprogram it. Interleaves with a
/// concurrent `SERIAL1` user only at byte granularity.
pub fn raw_write(bytes: &[u8]) {
    for &b in bytes {
        let mut n = 0;
        while n < RAW_POLL_LIMIT {
            // SAFETY: reading the 16550 line status register (COM1 + 5) has
            // no side effect besides reporting state; the port belongs to the
            // kernel's serial console.
            let lsr: u8 = unsafe { x86_64::instructions::port::PortReadOnly::new(COM1 + 5).read() };
            if lsr & LSR_THRE != 0 {
                break;
            }
            core::hint::spin_loop();
            n += 1;
        }
        // SAFETY: writing the transmit holding register (COM1 + 0, DLAB is 0
        // after initialisation) sends one byte; nothing else is affected.
        unsafe { x86_64::instructions::port::PortWriteOnly::<u8>::new(COM1).write(b) };
    }
}
