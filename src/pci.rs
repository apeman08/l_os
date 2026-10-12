//! Minimal PCI configuration-space access (legacy ports 0xCF8/0xCFC) and bus
//! enumeration. Reading configuration space sends nothing on any network; it
//! only asks the (emulated) chipset what devices exist.

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
