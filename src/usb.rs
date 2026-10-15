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
//!
//! **Boot stick** (spec §7.1 steps 7–8, E2): [`identify_boot_stick`] talks
//! bulk-only transport to each enumerated mass-storage device with the
//! read-only commands of `l_core::usb_msc` only (TEST UNIT READY up to
//! [`TUR_ATTEMPTS`] times, READ CAPACITY(10), READ(10) of LBA 0) and compares
//! the boot marker in LBA 0 with [`crate::boot_id`]. [`storage_seal`] then
//! closes the path for good: [`storage_read`] returns `Err(Sealed)` without
//! a transfer, [`xhci::bulk`] itself refuses every bulk TD (whoever calls
//! it), and a later [`enumerate`] (after a controller restart) touches only
//! the ports of the non-storage devices known at the seal, so neither the
//! stick nor foreign storage is addressed again (spec §7.1 step 7, "nie
//! wieder angesprochen"; §2: no hotplug enumeration). A failed command, a phase error, a stall or a malformed CSW
//! ends the attempt for that device (no reset recovery: the endpoint is
//! not used again); the system keeps running with "amnesisch: nein" (E8).
//! The status inputs ([`stick_sealed`], [`stick_reads`],
//! [`keyboard_present`], [`keyboard_reports`]) are atomics, readable
//! without a lock.
//!
//! **Keyboard** (Task 22): [`start_keyboard`] starts the boot keyboard's
//! interrupt-IN reports and the timer-ISR polling ([`xhci::poll_tick`]).
//!
//! **Arming** (Task 23): [`arm_stick_watch`] ends the boot phase: once the
//! stick is identified and sealed, its removal or the loss of the
//! controller wipes (spec §7.1 step 8, §7.2).

use crate::xhci::{self, PortReady, XhciError};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use l_core::bootmark::BootId;
use l_core::usb_desc::{self, Config, DescError, DeviceDescriptor, DeviceKind, SetupPacket};
use l_core::usb_msc::{self, Cbw, CswStatus, MscError};
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
    /// `bConfigurationValue` 0: SET_CONFIGURATION(0) would unconfigure
    /// the device (ledger Task 20).
    ZeroConfigValue,
    /// [`start_keyboard`]: no configured boot keyboard was enumerated.
    NoKeyboard,
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
///
/// After [`storage_seal`], a new generation (a re-`init` after a halt or a
/// timed-out command) scans only the root ports that held a non-storage
/// device (keyboard, unsupported) at the seal: the ports of mass storage,
/// the boot stick's included, get no port reset, no slot and no control
/// transfer, and ports that were empty or failed at the seal are left
/// alone too (a device there could be the stick, re-plugged; spec §2 has no
/// hotplug enumeration). Should a sealed-time keyboard port now hold mass
/// storage, that device is addressed (it has to be, to be classified) but
/// gets no endpoints and no configuration.
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
    // Read under the summary lock, which `storage_seal` also takes: the
    // seal and its port list are seen together.
    let sealed = SEALED.load(Ordering::Acquire);
    for port in 1..=xhci::port_count() {
        if sealed && !rescan_allowed(port) {
            continue;
        }
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
                if matches!(d.kind, DeviceKind::BootKeyboard { .. }) && d.config != 0 {
                    KEYBOARD.store(true, Ordering::Release);
                }
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
    check_config_value(config.value)?;
    let kind = usb_desc::classify(&config);
    dev.kind = kind;

    // xHCI 1.2 §4.3.5: Configure Endpoint first, then SET_CONFIGURATION.
    match kind {
        DeviceKind::Unsupported => return Ok(dev),
        // Mass storage found after the seal (see `enumerate`): addressed to
        // be classified, then left unconfigured (`config` 0, no bulk
        // endpoints); `identify_boot_stick` skips it.
        DeviceKind::BulkOnlyStorage { .. } if SEALED.load(Ordering::Acquire) => return Ok(dev),
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

/// A configuration value that SET_CONFIGURATION may select (not 0).
fn check_config_value(v: u8) -> Result<(), UsbError> {
    if v == 0 { Err(UsbError::ZeroConfigValue) } else { Ok(()) }
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

// ------------------------------------------------------------ boot stick

/// TEST UNIT READY attempts before a device counts as not ready (spec §7.1
/// step 7); the first one may report a unit attention after power-on.
pub const TUR_ATTEMPTS: usize = 5;
/// Pause between TEST UNIT READY attempts.
const TUR_PAUSE_MS: u64 = 20;
/// Bytes of a CBW / CSW on the wire.
const CBW_LEN: usize = 31;
const CSW_LEN: usize = 13;
/// Data page of a slot ([`xhci::slot_buffer`]): one block must fit.
const PAGE: usize = 4096;
// Worst-case boot delay of `identify_boot_stick`: every bulk TD waits at
// most `xhci::BULK_TIMEOUT_MS` (3 s), and a timeout or any other error ends
// the probe of that device at once. A hostile device that answers each TD
// just before the deadline still bounds it: at most 16 TDs per device
// (TEST UNIT READY 5 x 2, READ CAPACITY 3, READ 3) = 48 s plus the TUR
// pauses (4 x 20 ms); with `MAX_DEVICES` (8) storage devices at most about
// 8 x 48 s = 6.4 min. A device that simply does not answer costs one
// timeout, 3 s (8 such devices: 24 s). Typical: milliseconds (QEMU: 4
// commands, 10 TDs).

/// Why [`identify_boot_stick`] found no boot stick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickError {
    /// No bulk-only mass-storage device was enumerated.
    NotFound,
    /// Mass storage was read, but its LBA 0 carries no marker with this
    /// kernel's boot id (another image's stick, other storage).
    NotBootStick,
    /// The kernel carries no boot id (not patched by the image builder): it
    /// cannot recognize any stick.
    NoBootId,
    /// The storage path was sealed before a stick was recognized.
    Sealed,
    /// Reading LBA 0 failed (last error of the devices tried).
    Read(StorageError),
}

/// Errors of [`storage_read`] and the bulk-only layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageError {
    /// [`storage_seal`] was called; nothing is transferred any more.
    Sealed,
    /// No boot stick was identified.
    NoStick,
    /// The buffer is not exactly one block.
    BadBuffer,
    /// LBA beyond the device's last block (or beyond READ(10)).
    OutOfRange,
    Xhci(XhciError),
    /// Malformed CSW or capacity data.
    Msc(MscError),
    /// The device reported the command as failed (CSW status 1).
    Failed,
    /// CSW status 2: the device needs a reset recovery, which is not done;
    /// the device is given up.
    PhaseError,
    /// Fewer bytes than the command needs (CBW not fully sent, capacity
    /// data shorter than 8 bytes, READ(10) data not exactly one block), or
    /// a CSW residue larger than the requested length. A residue that
    /// disagrees with the bytes actually received is not checked.
    Short,
    /// TEST UNIT READY never passed.
    NotReady,
}

impl From<XhciError> for StorageError {
    fn from(e: XhciError) -> Self {
        StorageError::Xhci(e)
    }
}

impl From<MscError> for StorageError {
    fn from(e: MscError) -> Self {
        StorageError::Msc(e)
    }
}

/// A bulk-only device whose capacity is known.
#[derive(Debug, Clone, Copy)]
struct Msc {
    port: u8,
    slot: u8,
    ep_in: u8,
    ep_out: u8,
    block_len: u32,
    last_lba: u32,
}

struct Storage {
    /// The identified boot stick.
    stick: Option<Msc>,
    /// Next CBW tag.
    tag: u32,
}

/// Held across every bulk-only command and by [`storage_seal`], so a read
/// in progress finishes before the seal and none starts after it.
static STORAGE: Mutex<Storage> = Mutex::new(Storage { stick: None, tag: 1 });
static SEALED: AtomicBool = AtomicBool::new(false);
/// Root port of the identified boot stick, 0 = none.
static STICK_PORT: AtomicU8 = AtomicU8::new(0);
/// Bulk-only commands attempted on mass storage since boot (counted before
/// the CBW is submitted, whether or not it then succeeds).
static STICK_READS: AtomicU64 = AtomicU64::new(0);
/// Root ports (bit n of word n / 64 = port n) a scan may touch after the
/// seal: those of the non-storage devices of the summary at the seal.
/// Written once, by the first [`storage_seal`], before `SEALED` is set.
static RESCAN_PORTS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

fn rescan_allowed(port: u8) -> bool {
    RESCAN_PORTS[usize::from(port / 64)].load(Ordering::Acquire) & (1 << (port % 64)) != 0
}
/// A boot keyboard was enumerated and configured.
static KEYBOARD: AtomicBool = AtomicBool::new(false);

/// Bulk-only commands (TEST UNIT READY, READ CAPACITY, READ) *attempted* on
/// mass storage since boot: each is counted before its CBW is submitted,
/// also when it then fails, so the count never understates the traffic.
/// Constant after [`storage_seal`] (which also seals [`xhci::bulk`]).
pub fn stick_reads() -> u64 {
    STICK_READS.load(Ordering::Acquire)
}

/// Root port of the identified boot stick.
pub fn stick_port() -> Option<u8> {
    match STICK_PORT.load(Ordering::Acquire) {
        0 => None,
        p => Some(p),
    }
}

/// Block length of the identified boot stick (READ CAPACITY).
pub fn stick_block_len() -> Option<u32> {
    STORAGE.lock().stick.map(|m| m.block_len)
}

/// The boot stick was identified and the storage path is sealed. Lock-free.
/// Spec E7's first condition additionally needs the removal watch armed
/// ([`xhci::watched_port`]; combined in `wipe::amnesia`).
pub fn stick_sealed() -> bool {
    SEALED.load(Ordering::Acquire) && STICK_PORT.load(Ordering::Acquire) != 0
}

/// A USB boot keyboard was enumerated and has not been stopped since (by a
/// disconnect or a transfer error, [`xhci::keyboard_lost`]). Lock-free.
pub fn keyboard_present() -> bool {
    KEYBOARD.load(Ordering::Acquire) && !xhci::keyboard_lost()
}

/// USB keyboard reports received since boot. Lock-free.
pub fn keyboard_reports() -> u64 {
    xhci::keyboard_reports()
}

/// End of the USB boot phase (spec §7.1 step 6, §7.2; after [`enumerate`],
/// [`identify_boot_stick`] and [`storage_seal`]): start the enumerated boot
/// keyboard's interrupt-IN transfers ([`xhci::start_keyboard`]: 8 report
/// TDs) and let the timer ISR poll ([`xhci::enable_polling`]; also without
/// a keyboard, for the port handling of Task 23). From then on the keyboard
/// feeds [`crate::input::feed`] like the PS/2 ISR. Idempotent.
pub fn start_keyboard() -> Result<(), UsbError> {
    let summary = SUMMARY.lock().filter(|(g, _)| Some(*g) == xhci::generation()).map(|(_, s)| s);
    let kbd = summary.and_then(|s| {
        s.devices().iter().find_map(|d| match d.kind {
            DeviceKind::BootKeyboard { ep_in, .. } if d.config != 0 => Some((d.slot, d.port, ep_in.address)),
            _ => None,
        })
    });
    let r = match kbd {
        Some((slot, port, ep)) => xhci::start_keyboard(slot, port, ep).map_err(UsbError::from),
        None => Err(UsbError::NoKeyboard),
    };
    xhci::enable_polling();
    r
}

/// Why [`arm_stick_watch`] did not arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchError {
    /// No boot stick was identified (E8: nothing to watch, no wipe on the
    /// USB path; the hotkey stays armed).
    NoStick,
    /// The stick is identified but the storage path is not sealed yet.
    NotSealed,
    Xhci(XhciError),
}

/// Spec §7.1 step 8, "scharf schalten" (M2 Task 23): only once the boot
/// stick is identified **and** sealed ([`stick_sealed`], [`xhci::bulk_sealed`])
/// the xHCI watch is armed on its root port ([`xhci::arm_stick_watch`]):
/// the timer ISR polls, and the port is read once right away. From then on
/// the stick leaving its port (CCS=0, or CSC=1) wipes with
/// `Trigger::StickRemoved`, a lost, failed or halted controller with
/// `Trigger::ControllerLost`, both at the first observation. A stick
/// already gone at this check wipes too: it was identified, so this is
/// after "Erkennung" (E8); with no user state yet, the wipe amounts to
/// blanking and powering off. Without an identified stick nothing is armed
/// (E8). Idempotent; returns the watched port. Production calls it in
/// `src/main.rs` after [`start_keyboard`].
pub fn arm_stick_watch() -> Result<u8, WatchError> {
    let port = stick_port().ok_or(WatchError::NoStick)?;
    if !stick_sealed() || !xhci::bulk_sealed() {
        return Err(WatchError::NotSealed);
    }
    xhci::arm_stick_watch(port).map_err(WatchError::Xhci)?;
    // Feature `wipe-test` only (COM1): scripts/qemu_wipe_test.py waits for
    // this line before it pulls the stick (`--trigger unplug`).
    #[cfg(feature = "wipe-test")]
    crate::serial_println!("wipe-test: watch armed port={}", port);
    Ok(port)
}

/// Does `sector` (LBA 0 of a device) carry the marker of boot id `id`?
/// `None` (an unpatched kernel) recognizes nothing.
pub fn match_boot_marker(sector: &[u8], id: Option<BootId>) -> Result<(), StickError> {
    let id = id.ok_or(StickError::NoBootId)?;
    match l_core::bootmark::parse_lba0(sector) {
        Some(found) if found == id => Ok(()),
        _ => Err(StickError::NotBootStick),
    }
}

/// Close the mass-storage path for good (spec §7.1 step 8): afterwards
/// [`storage_read`] returns `Err(Sealed)`, [`xhci::bulk`] refuses every
/// transfer with `XhciError::Sealed` (enforced in the driver, whoever calls
/// it), and a later [`enumerate`] scans only the ports of the non-storage
/// devices of the current summary (see there). Idempotent.
pub fn storage_seal() {
    // Waits for a command in progress. Lock order STORAGE, SUMMARY, DRIVER
    // (as in `identify_boot_stick` -> `enumerate` -> `scan`).
    let _g = STORAGE.lock();
    {
        // A scan holds SUMMARY: it sees either no seal or the seal together
        // with its port list.
        let summary = SUMMARY.lock();
        if !SEALED.load(Ordering::Acquire) {
            if let Some((_, s)) = *summary {
                for d in s.devices() {
                    if !matches!(d.kind, DeviceKind::BulkOnlyStorage { .. }) {
                        RESCAN_PORTS[usize::from(d.port / 64)].fetch_or(1 << (d.port % 64), Ordering::AcqRel);
                    }
                }
            }
            SEALED.store(true, Ordering::Release);
        }
    }
    xhci::seal_bulk();
}

/// Read block `lba` of the identified boot stick into `buf` (exactly one
/// block). Refused with `Err(Sealed)` after [`storage_seal`], before
/// anything else is looked at.
pub fn storage_read(lba: u64, buf: &mut [u8]) -> Result<(), StorageError> {
    let mut st = STORAGE.lock();
    if SEALED.load(Ordering::Acquire) {
        return Err(StorageError::Sealed);
    }
    let m = st.stick.ok_or(StorageError::NoStick)?;
    if buf.len() != m.block_len as usize {
        return Err(StorageError::BadBuffer);
    }
    let lba = u32::try_from(lba).map_err(|_| StorageError::OutOfRange)?;
    if lba > m.last_lba {
        return Err(StorageError::OutOfRange);
    }
    read_block(&m, &mut st.tag, lba, buf)
}

/// Find the boot stick among the enumerated mass-storage devices (spec
/// §7.1 step 7): the device whose LBA 0 carries this kernel's boot marker.
/// Returns its root port. Once found, later calls return the same port
/// without any transfer. Every other device is left alone afterwards.
pub fn identify_boot_stick() -> Result<u8, StickError> {
    let mut st = STORAGE.lock();
    if let Some(m) = st.stick {
        return Ok(m.port);
    }
    if SEALED.load(Ordering::Acquire) {
        return Err(StickError::Sealed);
    }
    let id = crate::boot_id().ok_or(StickError::NoBootId)?;
    let summary = enumerate();
    let mut result = Err(StickError::NotFound);
    for d in summary.devices() {
        let DeviceKind::BulkOnlyStorage { ep_in, ep_out, .. } = d.kind else { continue };
        if d.config == 0 {
            continue;
        }
        let mut sector = [0u8; 32];
        let m = Msc { port: d.port, slot: d.slot, ep_in: ep_in.address, ep_out: ep_out.address, block_len: 0, last_lba: 0 };
        match probe(m, &mut st.tag, &mut sector) {
            Ok(m) => match match_boot_marker(&sector, Some(id)) {
                Ok(()) => {
                    st.stick = Some(m);
                    STICK_PORT.store(m.port, Ordering::Release);
                    return Ok(m.port);
                }
                Err(e) => {
                    if !matches!(result, Err(StickError::Read(_))) {
                        result = Err(e);
                    }
                }
            },
            Err(e) => {
                crate::println!("usb: port {} massenspeicher nicht lesbar ({:?})", d.port, e);
                result = Err(StickError::Read(e));
            }
        }
    }
    result
}

/// TEST UNIT READY (bounded retries), READ CAPACITY(10), READ(10) of LBA 0;
/// the first `sector.len()` bytes of LBA 0 are copied to `sector`.
fn probe(mut m: Msc, tag: &mut u32, sector: &mut [u8; 32]) -> Result<Msc, StorageError> {
    let mut ready = false;
    for attempt in 0..TUR_ATTEMPTS {
        let t = next_tag(tag);
        match command(&m, t, Cbw::test_unit_ready(t), &mut [])? {
            (CswStatus::Passed, _) => {
                ready = true;
                break;
            }
            // E.g. a unit attention after power-on; REQUEST SENSE is not on
            // the whitelist, so just ask again.
            (CswStatus::Failed, _) => {
                if attempt + 1 < TUR_ATTEMPTS {
                    xhci::pause_ms(TUR_PAUSE_MS);
                }
            }
            (CswStatus::PhaseError, _) => return Err(StorageError::PhaseError),
        }
    }
    if !ready {
        return Err(StorageError::NotReady);
    }
    let t = next_tag(tag);
    let mut cap = [0u8; 8];
    let n = passed(command(&m, t, Cbw::read_capacity10(t), &mut cap)?)?;
    if n < cap.len() {
        return Err(StorageError::Short);
    }
    let (last_lba, block_len) = usb_msc::parse_read_capacity10(&cap)?;
    if block_len as usize > PAGE {
        return Err(StorageError::Msc(MscError::BadBlockLen));
    }
    m.block_len = block_len;
    m.last_lba = last_lba;
    read_block(&m, tag, 0, sector)?;
    Ok(m)
}

/// READ(10) of one block at `lba`; the first `out.len()` bytes of it are
/// copied to `out` (at most one block).
fn read_block(m: &Msc, tag: &mut u32, lba: u32, out: &mut [u8]) -> Result<(), StorageError> {
    let t = next_tag(tag);
    let n = passed(command(m, t, Cbw::read10(t, lba, 1, m.block_len), out)?)?;
    if n != m.block_len as usize {
        return Err(StorageError::Short);
    }
    Ok(())
}

fn passed(r: (CswStatus, usize)) -> Result<usize, StorageError> {
    match r.0 {
        CswStatus::Passed => Ok(r.1),
        CswStatus::Failed => Err(StorageError::Failed),
        CswStatus::PhaseError => Err(StorageError::PhaseError),
    }
}

fn next_tag(tag: &mut u32) -> u32 {
    let t = *tag;
    *tag = tag.wrapping_add(1);
    t
}

/// One bulk-only command with tag `tag` (the tag inside `cbw`): CBW on
/// bulk-OUT, the data stage (if any) on bulk-IN, CSW on bulk-IN, all
/// through the slot's data page at offset 0. Of the data received, the
/// first `out.len()` bytes are copied to `out` before the CSW stage reuses
/// the page. Returns the CSW status and the number of data bytes received.
/// Every device answer is checked; nothing here panics on device data.
fn command(m: &Msc, tag: u32, cbw: Cbw, out: &mut [u8]) -> Result<(CswStatus, usize), StorageError> {
    let buf = xhci::slot_buffer(m.slot).ok_or(StorageError::Xhci(XhciError::BadSlot))?;
    let len = cbw.data_len() as usize;
    if len > PAGE || out.len() > len {
        return Err(StorageError::BadBuffer);
    }
    STICK_READS.fetch_add(1, Ordering::AcqRel);
    xhci::write_slot_buffer(m.slot, 0, &cbw.encode())?;
    if xhci::bulk(m.slot, m.ep_out, buf, CBW_LEN)? != CBW_LEN {
        return Err(StorageError::Short);
    }
    let mut got = 0;
    let mut csw = [0u8; CSW_LEN];
    let mut csw_in_data = false;
    if len > 0 {
        got = xhci::bulk(m.slot, m.ep_in, buf, len)?.min(len);
        // A device that fails a command may skip the data stage and send
        // the CSW right away (QEMU's usb-storage does): accept exactly that,
        // a 13-byte answer that is a valid CSW for this tag.
        if got == CSW_LEN && len != CSW_LEN {
            xhci::read_slot_buffer(m.slot, 0, &mut csw)?;
            if usb_msc::parse_csw(&csw, tag).is_ok() {
                csw_in_data = true;
                got = 0;
            }
        }
        let k = got.min(out.len());
        xhci::read_slot_buffer(m.slot, 0, &mut out[..k])?;
    }
    let n = if csw_in_data {
        CSW_LEN
    } else {
        let n = xhci::bulk(m.slot, m.ep_in, buf, CSW_LEN)?.min(CSW_LEN);
        xhci::read_slot_buffer(m.slot, 0, &mut csw[..n])?;
        n
    };
    let (status, residue) = usb_msc::parse_csw(&csw[..n], tag)?;
    // BOT 6.7.3: the residue never exceeds the requested length (ledger
    // Task 16: the caller checks it). The callers check the bytes received.
    if residue as usize > len {
        return Err(StorageError::Short);
    }
    Ok((status, got))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn zero_config_value_refused() {
        assert_eq!(check_config_value(0), Err(UsbError::ZeroConfigValue));
        assert_eq!(check_config_value(1), Ok(()));
        assert_eq!(check_config_value(255), Ok(()));
    }

    #[test_case]
    fn storage_without_stick() {
        // The default test machine has no xHCI: nothing found, no traffic.
        let reads = stick_reads();
        let want = if crate::boot_id().is_some() { StickError::NotFound } else { StickError::NoBootId };
        assert_eq!(identify_boot_stick(), Err(want));
        assert_eq!(storage_read(0, &mut [0u8; 512]), Err(StorageError::NoStick));
        assert!(!stick_sealed());
        assert_eq!(stick_port(), None);
        assert_eq!(stick_reads(), reads);
    }

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
