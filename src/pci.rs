//! Minimal PCI configuration-space access (legacy ports 0xCF8/0xCFC) and bus
//! enumeration. Reading configuration space sends nothing on any network; it
//! only asks the (emulated) chipset what devices exist.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

pub const MAX_DEVICES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Device {
    pub bus: u8,
    pub slot: u8,
    pub func: u8,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub irq_line: u8,
    pub bars: [u32; 6],
}

static DEVICES: Mutex<([Option<Device>; MAX_DEVICES], usize)> = Mutex::new(([None; MAX_DEVICES], 0));
static CONFIG_LOCK: Mutex<()> = Mutex::new(());
/// Lock-free copy of the enumerated functions' configuration addresses
/// (`bus << 16 | slot << 11 | func << 8`), for the wipe, which must not take
/// `DEVICES` (spec E14). Written once by [`enumerate`] before interrupts are
/// enabled; `BDF_COUNT` is published last.
static BDF: [AtomicU32; MAX_DEVICES] = [const { AtomicU32::new(0) }; MAX_DEVICES];
static BDF_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Command register bit 2: the function may master the bus (DMA).
const CMD_BUS_MASTER: u32 = 1 << 2;

fn address(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000 | (bus as u32) << 16 | (slot as u32) << 11 | (func as u32) << 8 | (offset as u32 & 0xfc)
}

pub fn read32(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    let _g = CONFIG_LOCK.lock();
    let mut addr_port = Port::<u32>::new(0xCF8);
    let mut data_port = Port::<u32>::new(0xCFC);
    // SAFETY: 0xCF8/0xCFC are the standard PCI configuration mechanism #1
    // ports; the lock keeps the address/data pair atomic.
    unsafe {
        addr_port.write(address(bus, slot, func, offset));
        data_port.read()
    }
}

pub fn write32(bus: u8, slot: u8, func: u8, offset: u8, value: u32) {
    let _g = CONFIG_LOCK.lock();
    let mut addr_port = Port::<u32>::new(0xCF8);
    let mut data_port = Port::<u32>::new(0xCFC);
    // SAFETY: as in `read32`.
    unsafe {
        addr_port.write(address(bus, slot, func, offset));
        data_port.write(value);
    }
}

/// Scan all buses and remember what was found. Safe to call once at boot.
pub fn enumerate() {
    let mut found = [None; MAX_DEVICES];
    let mut n = 0;
    'scan: for bus in 0..=255u8 {
        for slot in 0..32u8 {
            if read32(bus, slot, 0, 0) & 0xffff == 0xffff {
                continue;
            }
            let multifunction = read32(bus, slot, 0, 0x0c) >> 16 & 0x80 != 0;
            for func in 0..if multifunction { 8 } else { 1 } {
                let id = read32(bus, slot, func, 0);
                if id & 0xffff == 0xffff {
                    continue;
                }
                let class = read32(bus, slot, func, 0x08);
                let mut bars = [0u32; 6];
                for (i, bar) in bars.iter_mut().enumerate() {
                    *bar = read32(bus, slot, func, 0x10 + 4 * i as u8);
                }
                if n == MAX_DEVICES {
                    break 'scan;
                }
                found[n] = Some(Device {
                    bus,
                    slot,
                    func,
                    vendor: (id & 0xffff) as u16,
                    device: (id >> 16) as u16,
                    class: (class >> 24) as u8,
                    subclass: (class >> 16) as u8,
                    irq_line: read32(bus, slot, func, 0x3c) as u8,
                    bars,
                });
                n += 1;
            }
        }
    }
    for (slot, d) in BDF.iter().zip(found[..n].iter().flatten()) {
        slot.store(address(d.bus, d.slot, d.func, 0) & 0x00ff_ff00, Ordering::Relaxed);
    }
    BDF_COUNT.store(n, Ordering::Release);
    *DEVICES.lock() = (found, n);
}

pub fn devices() -> impl Iterator<Item = Device> {
    let (list, n) = *DEVICES.lock();
    (0..n).filter_map(move |i| list[i])
}

pub fn find(vendor: u16, device: u16) -> Option<Device> {
    devices().find(|d| d.vendor == vendor && d.device == device)
}

/// Allow the device to master the bus (DMA) and respond to I/O and memory
/// accesses. Only called for a device the kernel decided to use.
pub fn enable_bus_mastering(d: &Device) {
    let cmd = read32(d.bus, d.slot, d.func, 0x04);
    write32(d.bus, d.slot, d.func, 0x04, cmd | 0x7);
}

/// Clear the bus-master bit of the function at configuration address `bdf`
/// (`bus << 16 | slot << 11 | func << 8`) without taking `CONFIG_LOCK`.
fn bus_master_off_raw(bdf: u32) {
    let addr = 0x8000_0000 | (bdf & 0x00ff_ff00) | 0x04;
    let mut addr_port = Port::<u32>::new(0xCF8);
    let mut data_port = Port::<u32>::new(0xCFC);
    // SAFETY: configuration mechanism #1 (0xCF8 address, 0xCFC data) on the
    // command register (offset 4) of a function found by `enumerate`; only
    // bit 2 (bus master) is cleared, I/O and memory decoding stay as they
    // were, and the status half is written as 0 (its bits are RW1C, so
    // nothing is cleared). No lock: the address is written before each data
    // access, so the only effect of racing a `read32`/`write32` is a wrong
    // configuration value for that caller, never a memory-safety issue. On
    // the wipe path the interrupted access never resumes.
    unsafe {
        addr_port.write(addr);
        let cmd = data_port.read();
        addr_port.write(addr);
        data_port.write(cmd & !CMD_BUS_MASTER & 0xffff);
    }
}

/// Stop `d` from mastering the bus (DMA), without any lock (wipe stage
/// `DmaStopped`). The status half of the register is written as 0, so no
/// RW1C status bit is cleared by accident.
pub fn disable_bus_master_unlocked(d: &Device) {
    bus_master_off_raw(address(d.bus, d.slot, d.func, 0) & 0x00ff_ff00);
}

/// [`disable_bus_master_unlocked`] for every function [`enumerate`] found,
/// from the lock-free list (at most [`MAX_DEVICES`] iterations). Bridges are
/// included on purpose: a bridge without bus master forwards no DMA from
/// behind it.
pub fn disable_all_bus_masters_unlocked() {
    let n = BDF_COUNT.load(Ordering::Acquire).min(MAX_DEVICES);
    for slot in &BDF[..n] {
        bus_master_off_raw(slot.load(Ordering::Relaxed));
    }
}

pub fn class_name(d: &Device) -> &'static str {
    match (d.class, d.subclass) {
        (0x00, _) => "unclassified",
        (0x01, 0x01) => "IDE controller",
        (0x01, _) => "storage",
        (0x02, 0x00) => "ethernet",
        (0x02, _) => "network",
        (0x03, _) => "display",
        (0x06, 0x00) => "host bridge",
        (0x06, 0x01) => "ISA bridge",
        (0x06, _) => "bridge",
        _ => "other",
    }
}
