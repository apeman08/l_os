//! USB enumeration on the xHCI root ports (spec §7.1 steps 5–6).
//!
//! [`enumerate`] brings the controller up (if it is not yet), prepares every
//! root port ([`xhci::prepare_port`]: reset, acknowledgement without PED),
//! and enumerates each connected device: slot and address, the 8-byte
//! device-descriptor prefix (EP0 max packet corrected by Evaluate Context if
//! `bMaxPacketSize0` differs from the speed's default), the full device
//! descriptor, the configuration descriptor (header, then `wTotalLength`
//! bytes, at most [`MAX_CONFIG_LEN`]), classification, Configure Endpoint,
//! SET_CONFIGURATION and, for boot keyboards, SET_PROTOCOL(boot) and
//! SET_IDLE(0). One line per device goes to the console
//! (`usb: port 6 high-speed tastatur`).
//!
//! **Devices are untrusted.** Descriptor bytes are copied out of the DMA
//! page and parsed by the hardened `l_core::usb_desc` parsers; an invalid
//! `bMaxPacketSize0`, a malformed or oversized descriptor or a failing
//! request ends the enumeration of that device only (it is counted as
//! failed, the others continue). Nothing here panics on device data.
//!
//! Runs in thread context during boot (the xHCI driver lock is held while
//! waiting; spec §7.2's timer-ISR polling comes later and uses `try_lock`).

use crate::xhci::{self, PortReady, XhciError};
use l_core::usb_desc::{self, Config, DescError, DeviceDescriptor, DeviceKind, SetupPacket};
use l_core::xhci_ctx::Speed;
use spin::Mutex;

/// Devices kept in a [`UsbSummary`] (the driver's table size).
pub const MAX_DEVICES: usize = xhci::MAX_DEVICES;
/// Largest configuration descriptor read (spec §7.1 step 6).
pub const MAX_CONFIG_LEN: usize = 512;
const DT_DEVICE: u8 = 1;
const DT_CONFIG: u8 = 2;

/// One enumerated device. `kind` is [`DeviceKind::Unsupported`] for a device
/// that was addressed but is neither a boot keyboard nor bulk-only storage
/// (its slot stays addressed, nothing else is done).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbDevice {
    pub port: u8,
    pub slot: u8,
    pub speed: Speed,
    pub kind: DeviceKind,
    /// `idVendor`/`idProduct` of the device descriptor.
    pub vendor: u16,
    pub product: u16,
    /// EP0 max packet size in use (from `bMaxPacketSize0`).
    pub max_packet0: u16,
    /// `bConfigurationValue` selected by SET_CONFIGURATION (0: none).
    pub config: u8,
}

/// Result of [`enumerate`]: the devices (fixed list, at most
/// [`MAX_DEVICES`]) and how many connected devices failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbSummary {
    /// `list[..count]` are the devices; the rest is [`UsbDevice::NONE`].
    list: [UsbDevice; MAX_DEVICES],
    count: usize,
    failed: u8,
}

impl UsbDevice {
    /// Filler for unused summary entries (slot 0 is never a device).
    const NONE: UsbDevice = UsbDevice {
        port: 0,
        slot: 0,
        speed: Speed::Full,
        kind: DeviceKind::Unsupported,
        vendor: 0,
        product: 0,
        max_packet0: 0,
        config: 0,
    };
}

impl UsbSummary {
    pub const fn empty() -> UsbSummary {
        UsbSummary { list: [UsbDevice::NONE; MAX_DEVICES], count: 0, failed: 0 }
    }

    /// The enumerated devices, in port order.
    pub fn devices(&self) -> &[UsbDevice] {
        &self.list[..self.count.min(MAX_DEVICES)]
    }

    /// Connected devices whose enumeration failed (or that did not fit).
    pub fn failed(&self) -> u8 {
        self.failed
    }

    fn push(&mut self, d: UsbDevice) {
        if self.count < MAX_DEVICES {
            self.list[self.count] = d;
            self.count += 1;
        }
    }
}

/// Why one device could not be enumerated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbError {
    Xhci(XhciError),
    Descriptor(DescError),
    /// `bMaxPacketSize0` not valid for the speed (ledger Task 14).
    BadMaxPacket0(u8),
    /// `wTotalLength` above [`MAX_CONFIG_LEN`].
    ConfigTooLong(u16),
    /// Device descriptor reports no configuration.
    NoConfiguration,
}

impl From<XhciError> for UsbError {
    fn from(e: XhciError) -> Self {
        UsbError::Xhci(e)
    }
}

impl From<DescError> for UsbError {
    fn from(e: DescError) -> Self {
        UsbError::Descriptor(e)
    }
}

/// Result of the last enumeration and the controller generation it
/// belongs to (a re-init of the controller forgets every slot).
static SUMMARY: Mutex<Option<(u64, UsbSummary)>> = Mutex::new(None);

/// Console name of a speed.
pub fn speed_name(s: Speed) -> &'static str {
    match s {
        Speed::Low => "low-speed",
        Speed::Full => "full-speed",
        Speed::High => "high-speed",
        Speed::Super => "super-speed",
    }
}

/// Console name of a device kind.
pub fn kind_name(k: &DeviceKind) -> &'static str {
    match k {
        DeviceKind::BootKeyboard { .. } => "tastatur",
        DeviceKind::BulkOnlyStorage { .. } => "massenspeicher",
        DeviceKind::Unsupported => "nicht unterstützt",
    }
}

/// Bring the controller up and enumerate every connected root-port device
/// once per controller generation; later calls return the same summary.
/// Without an xHCI controller the summary is empty and nothing is printed.
pub fn enumerate() -> UsbSummary {
    match xhci::init() {
        Ok(()) | Err(XhciError::AlreadyInitialized) => {}
        Err(XhciError::NotFound) => return UsbSummary::empty(),
        Err(e) => {
            crate::println!("usb: controller-fehler {:?}", e);
            return UsbSummary::empty();
        }
    }
    // Held across the scan: concurrent callers wait and get the result.
    let mut g = SUMMARY.lock();
    let Some(generation) = xhci::generation() else { return UsbSummary::empty() };
    if let Some((gen_, s)) = *g {
        if gen_ == generation {
            return s;
        }
    }
    let s = scan();
    *g = Some((generation, s));
    s
}

fn scan() -> UsbSummary {
    let mut s = UsbSummary::empty();
    for port in 1..=xhci::port_count() {
        let speed = match xhci::prepare_port(port) {
            Ok(PortReady::Empty) => continue,
            Ok(PortReady::Ready(speed)) => speed,
            Ok(PortReady::Lost) => {
                crate::println!("usb: port {} verbindung während des resets verloren", port);
                s.failed = s.failed.saturating_add(1);
                continue;
            }
            Ok(PortReady::UnknownSpeed(raw)) => {
                // Ledger Task 13: the raw speed ID is reported, not guessed.
                crate::println!("usb: port {} unbekannte geschwindigkeit {}", port, raw);
                s.failed = s.failed.saturating_add(1);
                continue;
            }
            Err(e) => {
                crate::println!("usb: port {} controller-fehler {:?}", port, e);
                break;
            }
        };
        if s.count >= MAX_DEVICES {
            crate::println!("usb: port {} {} ignoriert (mehr als {} geräte)", port, speed_name(speed), MAX_DEVICES);
            s.failed = s.failed.saturating_add(1);
            continue;
        }
        match enumerate_device(port, speed) {
            Ok(d) => {
                crate::println!("usb: port {} {} {}", port, speed_name(speed), kind_name(&d.kind));
                s.push(d);
            }
            Err(e) => {
                crate::println!("usb: port {} {} fehler {:?}", port, speed_name(speed), e);
                s.failed = s.failed.saturating_add(1);
                if matches!(e, UsbError::Xhci(XhciError::ControllerGone | XhciError::HostError(_) | XhciError::Halted)) {
                    break;
                }
            }
        }
    }
    // Requests and resets may have set change bits again (e.g. PLC); clear
    // them so that the port-change handling starts from a clean state.
    // Writes only set change bits, PED as 0.
    for port in 1..=xhci::port_count() {
        if xhci::ack_port(port).is_err() {
            break;
        }
    }
    s
}

/// IN request of exactly `len` bytes into the slot's data page; returns the
/// bytes received, copied into `out[..n]`.
fn get(slot: u8, packet: SetupPacket, out: &mut [u8]) -> Result<usize, UsbError> {
    let len = usize::from(packet.length);
    let buf = xhci::slot_buffer(slot).ok_or(UsbError::Xhci(XhciError::BadSlot))?;
    if len > out.len() {
        return Err(UsbError::Xhci(XhciError::BadBuffer));
    }
    let n = xhci::control_in(slot, packet, buf, len)?.min(len);
    xhci::read_slot_buffer(slot, 0, &mut out[..n])?;
    Ok(n)
}

fn enumerate_device(port: u8, speed: Speed) -> Result<UsbDevice, UsbError> {
    let slot = xhci::address_device(port, speed)?;

    // 8-byte prefix: bMaxPacketSize0 (validated, ledger Task 14).
    let mut b = [0u8; 18];
    let n = get(slot, SetupPacket::get_descriptor(DT_DEVICE, 0, 8), &mut b)?;
    let mp0 = DeviceDescriptor::parse_prefix(&b[..n])?;
    let max_packet0 = speed.ep0_max_from_descriptor(mp0).ok_or(UsbError::BadMaxPacket0(mp0))?;
    if max_packet0 != speed.ep0_max_packet() {
        xhci::set_ep0_max_packet(slot, max_packet0)?;
    }
    let n = get(slot, SetupPacket::get_descriptor(DT_DEVICE, 0, 18), &mut b)?;
    let desc = DeviceDescriptor::parse(&b[..n])?;
    let mut dev = UsbDevice {
        port,
        slot,
        speed,
        kind: DeviceKind::Unsupported,
        vendor: desc.vendor,
        product: desc.product,
        max_packet0,
        config: 0,
    };
    if desc.num_configs == 0 {
        return Err(UsbError::NoConfiguration);
    }

    // Configuration 0: header, then wTotalLength (bounded).
    let mut cfg = [0u8; MAX_CONFIG_LEN];
    let n = get(slot, SetupPacket::get_descriptor(DT_CONFIG, 0, 9), &mut cfg)?;
    let total = config_total_length(&cfg[..n])?;
    let n = get(slot, SetupPacket::get_descriptor(DT_CONFIG, 0, total), &mut cfg)?;
    let config: Config = usb_desc::parse_config(&cfg[..n])?;
    let kind = usb_desc::classify(&config);
    dev.kind = kind;

    // xHCI 1.2 §4.3.5: Configure Endpoint first, then SET_CONFIGURATION.
    match kind {
        DeviceKind::Unsupported => return Ok(dev),
        DeviceKind::BootKeyboard { ep_in, .. } => xhci::configure_endpoints(slot, &[(ep_in, speed)])?,
        DeviceKind::BulkOnlyStorage { ep_in, ep_out, .. } => {
            xhci::configure_endpoints(slot, &[(ep_in, speed), (ep_out, speed)])?
        }
    }
    xhci::control_out_nodata(slot, SetupPacket::set_configuration(config.value))?;
    dev.config = config.value;
    if let DeviceKind::BootKeyboard { iface, .. } = kind {
        xhci::control_out_nodata(slot, SetupPacket::set_protocol_boot(iface))?;
        // SET_IDLE is optional for boot keyboards (HID 1.11 §7.2.4); a stall
        // only means the device reports at its own rate.
        if let Err(e) = xhci::control_out_nodata(slot, SetupPacket::set_idle(iface)) {
            crate::println!("usb: port {} set_idle abgelehnt ({:?})", port, e);
        }
    }
    Ok(dev)
}

/// `wTotalLength` from a configuration-descriptor header, checked: at least
/// the header itself, at most [`MAX_CONFIG_LEN`].
fn config_total_length(h: &[u8]) -> Result<u16, UsbError> {
    if h.len() < 9 {
        return Err(DescError::Truncated.into());
    }
    if h[1] != DT_CONFIG {
        return Err(DescError::BadType.into());
    }
    let total = u16::from_le_bytes([h[2], h[3]]);
    if total < 9 {
        return Err(DescError::BadLength.into());
    }
    if usize::from(total) > MAX_CONFIG_LEN {
        return Err(UsbError::ConfigTooLong(total));
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn config_header_bounds() {
        assert_eq!(config_total_length(&[9, 2, 32, 0, 1, 1, 0, 0x80, 50]), Ok(32));
        assert_eq!(config_total_length(&[9, 2, 8, 0, 1, 1, 0, 0x80, 50]), Err(UsbError::Descriptor(DescError::BadLength)));
        assert_eq!(config_total_length(&[9, 2, 0x01, 0x02, 1, 1, 0, 0x80, 50]), Err(UsbError::ConfigTooLong(513)));
        assert_eq!(config_total_length(&[9, 2, 0, 2, 1, 1, 0, 0x80, 50]), Ok(512));
        assert_eq!(config_total_length(&[9, 4, 32, 0, 1, 1, 0, 0x80, 50]), Err(UsbError::Descriptor(DescError::BadType)));
        assert_eq!(config_total_length(&[9, 2, 32]), Err(UsbError::Descriptor(DescError::Truncated)));
    }

    #[test_case]
    fn summary_without_controller_is_empty() {
        // The default test machine has no xHCI.
        let s = enumerate();
        assert_eq!(s.devices().len(), 0);
        assert_eq!(s.failed(), 0);
    }
}
