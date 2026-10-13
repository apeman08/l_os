//! xHCI capability decoding, PORTSC handling, port-event classification and
//! device-context encoding. Pure logic, no hardware access.
//!
//! Section numbers refer to the xHCI specification, revision 1.2.

/// Decoded capability registers (xHCI 1.2 section 5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub cap_length: u8,
    pub max_slots: u8,
    pub max_ports: u8,
    pub max_scratchpad: u16,
    pub csz64: bool,
    pub ac64: bool,
    pub doorbell_offset: u32,
    pub runtime_offset: u32,
}

impl Caps {
    /// `caplength_hciversion` is the dword at 0x00 (CAPLENGTH 7:0, HCIVERSION
    /// 31:16; section 5.3.1/5.3.2). All inputs are accepted, garbage included.
    pub fn decode(
        caplength_hciversion: u32,
        hcsparams1: u32,
        hcsparams2: u32,
        hccparams1: u32,
        dboff: u32,
        rtsoff: u32,
    ) -> Caps {
        // HCSPARAMS1: MaxSlots 7:0, MaxPorts 31:24 (section 5.3.3).
        // HCSPARAMS2: Max Scratchpad Bufs Hi 25:21, Lo 31:27 (section 5.3.4).
        let hi = (hcsparams2 >> 21) & 0x1F;
        let lo = (hcsparams2 >> 27) & 0x1F;
        Caps {
            cap_length: (caplength_hciversion & 0xFF) as u8,
            max_slots: (hcsparams1 & 0xFF) as u8,
            max_ports: ((hcsparams1 >> 24) & 0xFF) as u8,
            max_scratchpad: ((hi << 5) | lo) as u16,
            // HCCPARAMS1: AC64 bit 0, CSZ bit 2 (section 5.3.6).
            ac64: hccparams1 & 1 != 0,
            csz64: hccparams1 & (1 << 2) != 0,
            // DBOFF bits 1:0 reserved (5.3.7), RTSOFF bits 4:0 reserved (5.3.8).
            doorbell_offset: dboff & !0x3,
            runtime_offset: rtsoff & !0x1F,
        }
    }
}

/// Port speed as Protocol Speed ID with the default mapping
/// (section 7.2.2.1.1, Table 7-13; also PORTSC Port Speed 13:10 and Slot
/// Context Speed 23:20).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Full = 1,
    Low = 2,
    High = 3,
    Super = 4,
}

impl Speed {
    pub fn from_psi(psi: u8) -> Option<Speed> {
        match psi {
            1 => Some(Speed::Full),
            2 => Some(Speed::Low),
            3 => Some(Speed::High),
            4 => Some(Speed::Super),
            _ => None,
        }
    }

    /// Initial max packet size of the default control endpoint
    /// (section 4.3.5 / 6.2.3.1 and USB 2.0/3.x: 8 LS/FS initial, 64 HS, 512 SS).
    pub fn ep0_max_packet(self) -> u16 {
        match self {
            Speed::Full | Speed::Low => 8,
            Speed::High => 64,
            Speed::Super => 512,
        }
    }
}

// PORTSC layout (section 5.4.8).
const CCS: u32 = 1 << 0; // RO
const PED: u32 = 1 << 1; // RW1C: writing 1 DISABLES the port, never write it
const PR: u32 = 1 << 4; // RW1S
const PP: u32 = 1 << 9; // RWS
const PIC_MASK: u32 = 0b11 << 14; // RWS
const WAKE_MASK: u32 = 0b111 << 25; // WCE, WDE, WOE: RWS
const CSC: u32 = 1 << 17; // RW1C
const PEC: u32 = 1 << 18; // RW1C
const WRC: u32 = 1 << 19; // RW1C
const OCC: u32 = 1 << 20; // RW1C
const PRC: u32 = 1 << 21; // RW1C
const PLC: u32 = 1 << 22; // RW1C
const CEC: u32 = 1 << 23; // RW1C
const CHANGE_MASK: u32 = CSC | PEC | WRC | OCC | PRC | PLC | CEC;
/// Bits that must be written back unchanged (RWS) so a write does not alter
/// them. LWS (16), PLS (8:5, ignored without LWS), PED, PR, WPR, DR and all
/// read-only/reserved bits are written as 0.
const PRESERVE_MASK: u32 = PP | PIC_MASK | WAKE_MASK;

/// Raw PORTSC value (section 5.4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortSc(pub u32);

impl PortSc {
    pub fn ccs(self) -> bool {
        self.0 & CCS != 0
    }
    pub fn ped(self) -> bool {
        self.0 & PED != 0
    }
    pub fn pr(self) -> bool {
        self.0 & PR != 0
    }
    /// Port Link State, bits 8:5.
    pub fn pls(self) -> u8 {
        ((self.0 >> 5) & 0xF) as u8
    }
    pub fn pp(self) -> bool {
        self.0 & PP != 0
    }
    /// Port Speed, bits 13:10 (raw PSIV; 0 = undefined).
    pub fn speed(self) -> u8 {
        ((self.0 >> 10) & 0xF) as u8
    }
    pub fn csc(self) -> bool {
        self.0 & CSC != 0
    }
    pub fn pec(self) -> bool {
        self.0 & PEC != 0
    }
    pub fn wrc(self) -> bool {
        self.0 & WRC != 0
    }
    pub fn occ(self) -> bool {
        self.0 & OCC != 0
    }
    pub fn prc(self) -> bool {
        self.0 & PRC != 0
    }
    pub fn plc(self) -> bool {
        self.0 & PLC != 0
    }
    pub fn cec(self) -> bool {
        self.0 & CEC != 0
    }

    /// A removed or dead controller reads all ones.
    pub fn is_all_ones(self) -> bool {
        self.0 == 0xFFFF_FFFF
    }

    /// Word to write to acknowledge the change bits currently set (RW1C: write
    /// 1 to clear). Preserves PP, PIC and the wake-enable bits; PED is always
    /// 0 because writing 1 would disable the port.
    pub fn ack_changes(self) -> u32 {
        (self.0 & PRESERVE_MASK) | (self.0 & CHANGE_MASK)
    }

    /// Word to start a port reset: preserved bits plus PR. No change bit is
    /// written (so none is cleared) and PED stays 0.
    pub fn reset_request(self) -> u32 {
        (self.0 & PRESERVE_MASK) | PR
    }
}

/// What a PORTSC reading means for the enumeration logic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortChange {
    Connected(Speed),
    Disconnected,
    ResetDone,
    ControllerGone,
    Nothing,
}

/// Classify a PORTSC reading. `was_connected` is the driver's own view before
/// this reading, so a CCS drop is detected even when CSC was already acked.
pub fn classify(was_connected: bool, now: PortSc) -> PortChange {
    if now.is_all_ones() {
        return PortChange::ControllerGone;
    }
    if !now.ccs() {
        return if now.csc() || was_connected {
            PortChange::Disconnected
        } else {
            PortChange::Nothing
        };
    }
    if now.prc() {
        return PortChange::ResetDone;
    }
    if now.csc() {
        if let Some(s) = Speed::from_psi(now.speed()) {
            return PortChange::Connected(s);
        }
    }
    PortChange::Nothing
}

/// Device Context Index of an endpoint address (section 4.5.1): EP0 is 1,
/// otherwise `number * 2 + direction` (IN = 1).
pub fn dci(endpoint_address: u8) -> u8 {
    let num = endpoint_address & 0x0F;
    if num == 0 {
        1
    } else {
        num * 2 + (endpoint_address >> 7)
    }
}

/// Endpoint Context Interval (section 6.2.3.6, Table 6-12), interrupt
/// endpoints. FS/LS: `floor(log2(bInterval * 8))` clamped 3..=10 (bInterval is
/// in frames). HS/SS: `bInterval - 1` clamped 0..=15 (bInterval is already an
/// exponent of 125 us microframes).
pub fn ep_interval(speed: Speed, b_interval: u8) -> u8 {
    match speed {
        Speed::Full | Speed::Low => {
            let v = u32::from(b_interval) * 8;
            let log = if v == 0 { 0 } else { 31 - v.leading_zeros() };
            log.clamp(3, 10) as u8
        }
        Speed::High | Speed::Super => b_interval.saturating_sub(1).min(15),
    }
}

/// Slot Context fields we set (section 6.2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotCtx {
    pub route: u32,
    pub speed: Speed,
    pub context_entries: u8,
    pub root_port: u8,
}

impl SlotCtx {
    pub fn encode(&self) -> [u32; 8] {
        let mut d = [0u32; 8];
        // DW0: Route String 19:0, Speed 23:20, Context Entries 31:27.
        d[0] = (self.route & 0xF_FFFF)
            | ((self.speed as u32) << 20)
            | ((u32::from(self.context_entries) & 0x1F) << 27);
        // DW1: Root Hub Port Number 23:16.
        d[1] = u32::from(self.root_port) << 16;
        d
    }
}

/// Endpoint Context fields we set (section 6.2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointCtx {
    pub ep_type: u8,
    pub max_packet: u16,
    pub interval: u8,
    pub cerr: u8,
    pub dequeue: u64,
    pub dcs: bool,
    pub avg_trb_len: u16,
}

impl EndpointCtx {
    pub fn encode(&self) -> [u32; 8] {
        let mut d = [0u32; 8];
        // DW0: Interval 23:16.
        d[0] = u32::from(self.interval) << 16;
        // DW1: CErr 2:1, EP Type 5:3, Max Packet Size 31:16.
        d[1] = ((u32::from(self.cerr) & 3) << 1)
            | ((u32::from(self.ep_type) & 7) << 3)
            | (u32::from(self.max_packet) << 16);
        // DW2/DW3: TR Dequeue Pointer (16-byte aligned, bits 63:4), DCS bit 0.
        d[2] = ((self.dequeue as u32) & 0xFFFF_FFF0) | u32::from(self.dcs);
        d[3] = (self.dequeue >> 32) as u32;
        // DW4: Average TRB Length 15:0.
        d[4] = u32::from(self.avg_trb_len);
        d
    }
}

/// Input Control Context (section 6.2.5.1): DW0 Drop flags, DW1 Add flags.
/// D0/D1 are reserved and cleared.
pub fn input_control(add: u32, drop: u32) -> [u32; 8] {
    let mut d = [0u32; 8];
    d[0] = drop & !0b11;
    d[1] = add;
    d
}

/// Byte offset of context `index` within a context structure; each context is
/// 32 bytes, or 64 when HCCPARAMS1.CSZ is set (section 6.2.1).
pub fn ctx_offset(index: usize, csz64: bool) -> usize {
    index * if csz64 { 64 } else { 32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_decode_qemu_like_values() {
        // Real qemu-xhci values from the USB spike note.
        let c = Caps::decode(0x0100_0040, 0x0800_1040, 0x0000_000f, 0x0008_7001, 0x2000, 0x1000);
        assert_eq!(c.cap_length, 0x40);
        assert_eq!(c.max_slots, 0x40);
        assert_eq!(c.max_ports, 8);
        assert_eq!(c.max_scratchpad, 0);
        assert!(c.ac64);
        assert!(!c.csz64);
        assert_eq!(c.doorbell_offset, 0x2000);
        assert_eq!(c.runtime_offset, 0x1000);
    }

    #[test]
    fn scratchpad_hi_lo_combined() {
        // Hi = 3 (bits 25:21), Lo = 5 (bits 31:27) -> 3*32 + 5 = 101.
        let c = Caps::decode(0, 0, (3 << 21) | (5 << 27), 0x4, 0x2003, 0x101F);
        assert_eq!(c.max_scratchpad, 101);
        assert!(c.csz64);
        assert!(!c.ac64);
        assert_eq!(c.doorbell_offset, 0x2000);
        assert_eq!(c.runtime_offset, 0x1000);
    }

    #[test]
    fn caps_garbage_does_not_panic() {
        let c = Caps::decode(u32::MAX, u32::MAX, u32::MAX, u32::MAX, u32::MAX, u32::MAX);
        assert_eq!(c.max_scratchpad, 1023);
        let z = Caps::decode(0, 0, 0, 0, 0, 0);
        assert_eq!(z.max_ports, 0);
    }

    #[test]
    fn ack_never_writes_ped() {
        for v in [0x0002_1203u32, 0x0000_0003, 0xFFFF_FFFF, 0x0000_0002, 0x00FE_0002] {
            assert_eq!(PortSc(v).ack_changes() & PED, 0, "{v:#x}");
            assert_eq!(PortSc(v).reset_request() & PED, 0, "{v:#x}");
        }
    }

    #[test]
    fn ack_writes_only_set_change_bits() {
        // CSC + PRC set, PEC clear.
        let v = PortSc(CSC | PRC | PP | CCS | PED | (4 << 10));
        assert_eq!(v.ack_changes(), CSC | PRC | PP);
        // No change bits -> nothing but PP.
        assert_eq!(PortSc(0x0000_1203).ack_changes(), PP);
        // All seven change bits.
        assert_eq!(PortSc(CHANGE_MASK).ack_changes(), 0x00FE_0000);
    }

    #[test]
    fn ack_preserves_power() {
        // Qemu after device_del: CCS=0 CSC=1 PP=1 PLS=5.
        assert_eq!(PortSc(0x0002_02a0).ack_changes(), 0x0002_0200);
        // Wake bits and PIC are kept, PLS/speed/CCS dropped.
        let v = PortSc(WAKE_MASK | PIC_MASK | PP | CCS | (3 << 10) | (5 << 5) | (1 << 30) | (1 << 31));
        assert_eq!(v.ack_changes(), WAKE_MASK | PIC_MASK | PP);
    }

    #[test]
    fn reset_request_keeps_power_and_sets_pr() {
        assert_eq!(PortSc(0x0002_0ee1).reset_request(), 0x0000_0210);
        assert_eq!(PortSc(0x0000_0e03).reset_request(), 0x0000_0210);
        // Wake bits survive; change bits and WPR/DR do not leak.
        let v = PortSc(WAKE_MASK | PP | CHANGE_MASK | (1 << 30) | (1 << 31));
        assert_eq!(v.reset_request(), WAKE_MASK | PP | PR);
        // Powered-off port stays powered-off.
        assert_eq!(PortSc(0).reset_request(), PR);
    }

    #[test]
    fn csc_with_ccs0_is_disconnect() {
        assert_eq!(classify(true, PortSc(0x0002_02a0)), PortChange::Disconnected);
        assert_eq!(classify(false, PortSc(0x0002_02a0)), PortChange::Disconnected);
    }

    #[test]
    fn ccs_drop_without_csc_is_disconnect() {
        assert_eq!(classify(true, PortSc(0x0000_02a0)), PortChange::Disconnected);
        assert_eq!(classify(false, PortSc(0x0000_02a0)), PortChange::Nothing);
    }

    #[test]
    fn csc_with_ccs1_is_connect_with_speed() {
        // SuperSpeed (PSIV 4), CSC set.
        assert_eq!(classify(false, PortSc(CSC | CCS | PP | (4 << 10))), PortChange::Connected(Speed::Super));
        assert_eq!(classify(false, PortSc(CSC | CCS | PP | (3 << 10))), PortChange::Connected(Speed::High));
        // Connected but unknown speed: do not guess.
        assert_eq!(classify(false, PortSc(CSC | CCS | PP)), PortChange::Nothing);
        // Steady state, no change.
        assert_eq!(classify(true, PortSc(0x0000_1203)), PortChange::Nothing);
    }

    #[test]
    fn prc_is_reset_done() {
        assert_eq!(classify(true, PortSc(PRC | CCS | PED | PP | (3 << 10))), PortChange::ResetDone);
        // Reset complete wins over a concurrent CSC while still connected.
        assert_eq!(classify(false, PortSc(PRC | CSC | CCS | PED | PP | (3 << 10))), PortChange::ResetDone);
        // But a device that vanished during reset is a disconnect.
        assert_eq!(classify(true, PortSc(PRC | CSC | PP)), PortChange::Disconnected);
    }

    #[test]
    fn all_ones_is_controller_gone() {
        assert_eq!(classify(true, PortSc(0xFFFF_FFFF)), PortChange::ControllerGone);
        assert_eq!(classify(false, PortSc(0xFFFF_FFFF)), PortChange::ControllerGone);
        assert!(PortSc(0xFFFF_FFFF).is_all_ones());
        assert!(!PortSc(0xFFFF_FFFE).is_all_ones());
    }

    #[test]
    fn portsc_accessors_decode_spike_values() {
        let s = PortSc(0x0000_1203); // stick, USB3
        assert!(s.ccs() && s.ped() && s.pp() && !s.pr());
        assert_eq!(s.speed(), 4);
        assert_eq!(s.pls(), 0);
        let k = PortSc(0x0002_02a0);
        assert!(!k.ccs() && k.csc() && k.pp());
        assert_eq!(k.pls(), 5);
        let all = PortSc(u32::MAX);
        assert!(all.pec() && all.wrc() && all.occ() && all.prc() && all.plc() && all.cec());
        let none = PortSc(0);
        assert!(!none.csc() && !none.is_all_ones());
        assert_eq!(classify(false, none), PortChange::Nothing);
    }

    #[test]
    fn dci_for_ep0_and_in_out() {
        assert_eq!(dci(0x00), 1);
        assert_eq!(dci(0x80), 1);
        assert_eq!(dci(0x01), 2);
        assert_eq!(dci(0x81), 3);
        assert_eq!(dci(0x02), 4);
        assert_eq!(dci(0x83), 7);
        assert_eq!(dci(0x8F), 31);
    }

    #[test]
    fn interval_fs_10ms_is_6() {
        assert_eq!(ep_interval(Speed::Full, 10), 6);
        assert_eq!(ep_interval(Speed::Low, 10), 6);
        assert_eq!(ep_interval(Speed::Full, 0), 3);
        assert_eq!(ep_interval(Speed::Full, 1), 3);
        assert_eq!(ep_interval(Speed::Full, 255), 10);
    }

    #[test]
    fn interval_hs_binterval_4_is_3() {
        assert_eq!(ep_interval(Speed::High, 4), 3);
        assert_eq!(ep_interval(Speed::Super, 1), 0);
        assert_eq!(ep_interval(Speed::High, 0), 0);
        assert_eq!(ep_interval(Speed::High, 255), 15);
    }

    #[test]
    fn speed_helpers() {
        assert_eq!(Speed::from_psi(0), None);
        assert_eq!(Speed::from_psi(5), None);
        assert_eq!(Speed::from_psi(255), None);
        assert_eq!(Speed::from_psi(4), Some(Speed::Super));
        assert_eq!(Speed::Low.ep0_max_packet(), 8);
        assert_eq!(Speed::Full.ep0_max_packet(), 8);
        assert_eq!(Speed::High.ep0_max_packet(), 64);
        assert_eq!(Speed::Super.ep0_max_packet(), 512);
    }

    #[test]
    fn slot_ctx_fields_in_right_bits() {
        let s = SlotCtx { route: 0x12345, speed: Speed::High, context_entries: 3, root_port: 6 };
        let d = s.encode();
        assert_eq!(d[0], (3 << 27) | (3 << 20) | 0x12345);
        assert_eq!(d[1], 6 << 16);
        assert_eq!(&d[2..], &[0; 6]);
        // Oversized fields are masked, not spilled into neighbours.
        let h = SlotCtx { route: u32::MAX, speed: Speed::Super, context_entries: 255, root_port: 255 }.encode();
        assert_eq!(h[0], (31 << 27) | (4 << 20) | 0xF_FFFF);
        assert_eq!(h[1], 255 << 16);
    }

    #[test]
    fn ep_ctx_dequeue_carries_dcs() {
        let e = EndpointCtx {
            ep_type: 4,
            max_packet: 64,
            interval: 0,
            cerr: 3,
            dequeue: 0x0000_00C0_1234_5670,
            dcs: true,
            avg_trb_len: 8,
        };
        let d = e.encode();
        assert_eq!(d[0], 0);
        assert_eq!(d[1], (64 << 16) | (4 << 3) | (3 << 1));
        assert_eq!(d[2], 0x1234_5671);
        assert_eq!(d[3], 0xC0);
        assert_eq!(d[4], 8);
        let n = EndpointCtx { dcs: false, interval: 6, ..e }.encode();
        assert_eq!(n[2], 0x1234_5670);
        assert_eq!(n[0], 6 << 16);
        // Misaligned dequeue bits 3:0 never leak into DCS/reserved.
        let m = EndpointCtx { dequeue: 0xF, dcs: false, ..e }.encode();
        assert_eq!(m[2], 0);
        // Garbage in does not panic nor corrupt neighbours.
        let g = EndpointCtx { ep_type: 255, max_packet: u16::MAX, interval: 255, cerr: 255, dequeue: u64::MAX, dcs: true, avg_trb_len: u16::MAX }.encode();
        assert_eq!(g[1], (0xFFFF << 16) | (7 << 3) | (3 << 1));
        assert_eq!(g[2], 0xFFFF_FFF1);
        assert_eq!(g[3], 0xFFFF_FFFF);
    }

    #[test]
    fn input_control_add_drop_words() {
        let d = input_control(0b101, 0b1110);
        assert_eq!(d[0], 0b1100);
        assert_eq!(d[1], 0b101);
        assert_eq!(&d[2..], &[0; 6]);
    }

    #[test]
    fn csz64_doubles_offsets() {
        assert_eq!(ctx_offset(0, false), 0);
        assert_eq!(ctx_offset(3, false), 96);
        assert_eq!(ctx_offset(3, true), 192);
        assert_eq!(ctx_offset(31, true), 31 * 64);
    }
}
