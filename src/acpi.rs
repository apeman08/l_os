//! ACPI in the kernel: find the PM1 control block and the `_S5` sleep type for
//! poweroff, and make sure ACPI mode (SCI_EN) is on. Table parsing lives in
//! `l_core::acpi`; this module only reads physical memory through the
//! bootloader's offset mapping, bounds-checked, and talks to the I/O ports.
//! Firmware tables are hostile input: every read is range-checked, sizes are
//! capped and every wait is bounded.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use l_core::acpi::{self as parse, AcpiError, SdtHeader};
use spin::Mutex;
use x86_64::instructions::port::Port;

/// What is needed to power off through ACPI S5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerOff {
    pub pm1a_cnt: u16,
    pub pm1b_cnt: u16,
    pub value_a: u16,
    pub value_b: u16,
}

const SCI_EN: u16 = 1;
/// A FADT is at most ~276 bytes; anything bigger is not believed.
const MAX_FADT_LEN: usize = 1024;
/// DSDTs are tens of KiB; refuse absurd lengths from broken firmware.
const MAX_DSDT_LEN: usize = 1 << 20;
const RSDP_READ_LEN: usize = 36;
/// Bounded wait for SCI_EN after the enable command (port reads).
const SCI_POLL_LIMIT: u32 = 100_000;

static PLAN: Mutex<Option<PowerOff>> = Mutex::new(None);
/// Exclusive end of the physical range mapped at the offset (0 = nothing).
static PHYS_LIMIT: AtomicU64 = AtomicU64::new(0);

/// Tell this module how much physical memory the bootloader mapped; physical
/// reads beyond it are refused instead of faulting.
pub fn set_phys_limit(end: u64) {
    PHYS_LIMIT.store(end, Ordering::Relaxed);
}

/// Copy `buf.len()` bytes from physical `addr` through the offset mapping.
fn read_phys(addr: u64, buf: &mut [u8]) -> bool {
    let Some(end) = addr.checked_add(buf.len() as u64) else {
        return false;
    };
    if end > PHYS_LIMIT.load(Ordering::Relaxed) {
        return false;
    }
    let src = (crate::memory::phys_offset() + addr).as_ptr::<u8>();
    // SAFETY: [addr, end) lies below PHYS_LIMIT, which `init_memory` sets to
    // the end of the highest region of the memory map; the bootloader mapped
    // all of that at the offset (BOOTLOADER_CONFIG), and `buf` is a distinct
    // writable slice of exactly this length.
    unsafe { core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), buf.len()) };
    true
}

/// Read a whole table: header first, then exactly `length` bytes (capped).
fn read_table(addr: u64, max_len: usize) -> Result<Vec<u8>, AcpiError> {
    let mut head = [0u8; 36];
    if !read_phys(addr, &mut head) {
        return Err(AcpiError::Truncated);
    }
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    if len < head.len() || len > max_len {
        return Err(AcpiError::Unsupported);
    }
    let mut t = alloc::vec![0u8; len];
    if !read_phys(addr, &mut t) {
        return Err(AcpiError::Truncated);
    }
    Ok(t)
}

fn pm1a_read(port: u16) -> u16 {
    // SAFETY: `port` is the PM1a control block address from the FADT; reading
    // an ACPI register has no side effects.
    unsafe { Port::<u16>::new(port).read() }
}

/// Switch to ACPI mode if SCI_EN is clear: write ACPI_ENABLE to SMI_CMD, then
/// wait a bounded number of reads for SCI_EN.
fn enable_acpi(f: &parse::Fadt) {
    if pm1a_read(f.pm1a_cnt) & SCI_EN != 0 {
        return;
    }
    let Ok(smi) = u16::try_from(f.smi_cmd) else { return };
    if smi == 0 || f.acpi_enable == 0 {
        return; // hardware-reduced / already ACPI-only: nothing to do
    }
    // SAFETY: SMI_CMD is the port the FADT names for the ACPI enable handshake;
    // ACPI_ENABLE is the value it defines for it.
    unsafe { Port::<u8>::new(smi).write(f.acpi_enable) };
    for _ in 0..SCI_POLL_LIMIT {
        if pm1a_read(f.pm1a_cnt) & SCI_EN != 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

fn discover(rsdp_addr: u64) -> Result<(parse::Fadt, (u8, u8)), AcpiError> {
    let mut raw = [0u8; RSDP_READ_LEN];
    if !read_phys(rsdp_addr, &mut raw) {
        return Err(AcpiError::Truncated);
    }
    let rsdp = parse::parse_rsdp(&raw)?;
    let fadt_addr = parse::find_table(&rsdp, *b"FACP", read_phys)?;
    let fadt = parse::parse_fadt(&read_table(fadt_addr, MAX_FADT_LEN)?)?;
    let dsdt = read_table(fadt.dsdt, MAX_DSDT_LEN)?;
    let SdtHeader { signature, .. } = parse::parse_sdt(&dsdt)?;
    if &signature != b"DSDT" {
        return Err(AcpiError::BadSignature);
    }
    let s5 = parse::find_s5(&dsdt).ok_or(AcpiError::NotFound)?;
    Ok((fadt, s5))
}

/// Read the tables once at boot, enable ACPI mode if needed and remember the
/// poweroff plan. Without an RSDP or with unusable tables there is no plan
/// (and `power_off_plan` stays `None`); nothing here panics on bad firmware.
pub fn init(rsdp_addr: Option<u64>) {
    let Some(rsdp_addr) = rsdp_addr else { return };
    let Ok((fadt, (typ_a, typ_b))) = discover(rsdp_addr) else { return };
    enable_acpi(&fadt);
    let plan = PowerOff {
        pm1a_cnt: fadt.pm1a_cnt,
        pm1b_cnt: fadt.pm1b_cnt,
        value_a: parse::s5_value(typ_a),
        value_b: if fadt.pm1b_cnt != 0 { parse::s5_value(typ_b) } else { 0 },
    };
    *PLAN.lock() = Some(plan);
}

pub fn power_off_plan() -> Option<PowerOff> {
    *PLAN.lock()
}
