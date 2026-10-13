//! xHCI TRBs, command/transfer producer rings and the event ring (pure logic).

// TRB types (xHCI 1.x, table 6-91).
pub const NORMAL: u8 = 1;
pub const SETUP: u8 = 2;
pub const DATA: u8 = 3;
pub const STATUS: u8 = 4;
pub const LINK: u8 = 6;
pub const ENABLE_SLOT: u8 = 9;
pub const ADDRESS_DEVICE: u8 = 11;
pub const CONFIGURE_ENDPOINT: u8 = 12;
pub const EVALUATE_CONTEXT: u8 = 13;
pub const NOOP_CMD: u8 = 23;
pub const TRANSFER_EVENT: u8 = 32;
pub const COMMAND_COMPLETION: u8 = 33;
pub const PORT_STATUS_CHANGE: u8 = 34;
pub const HOST_CONTROLLER: u8 = 37;

// Completion codes (table 6-90).
pub const SUCCESS: u8 = 1;
pub const DATA_BUFFER_ERROR: u8 = 2;
pub const BABBLE: u8 = 3;
pub const USB_TRANSACTION_ERROR: u8 = 4;
pub const TRB_ERROR: u8 = 5;
pub const STALL: u8 = 6;
pub const RESOURCE_ERROR: u8 = 7;
pub const BANDWIDTH_ERROR: u8 = 8;
pub const NO_SLOTS_AVAILABLE: u8 = 9;
pub const SHORT_PACKET: u8 = 13;

const CYCLE: u32 = 1;
const TOGGLE_CYCLE: u32 = 1 << 1;
const ISP: u32 = 1 << 2;
const IOC: u32 = 1 << 5;
const IDT: u32 = 1 << 6;
const BSR: u32 = 1 << 9;
const DIR_IN: u32 = 1 << 16;

/// One 16-byte transfer request block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Trb {
    pub param: u64,
    pub status: u32,
    pub control: u32,
}

impl Trb {
    pub fn to_words(&self) -> [u32; 4] {
        [self.param as u32, (self.param >> 32) as u32, self.status, self.control]
    }

    pub fn from_words(w: [u32; 4]) -> Trb {
        Trb { param: w[0] as u64 | (w[1] as u64) << 32, status: w[2], control: w[3] }
    }

    pub fn trb_type(&self) -> u8 {
        (self.control >> 10 & 0x3f) as u8
    }

    pub fn cycle(&self) -> bool {
        self.control & CYCLE != 0
    }

    pub fn with_cycle(mut self, c: bool) -> Trb {
        self.control = self.control & !CYCLE | c as u32;
        self
    }

    fn typed(ty: u8, param: u64, status: u32, flags: u32) -> Trb {
        Trb { param, status, control: (ty as u32) << 10 | flags }
    }

    pub fn noop_cmd() -> Trb {
        Trb::typed(NOOP_CMD, 0, 0, 0)
    }

    pub fn enable_slot() -> Trb {
        Trb::typed(ENABLE_SLOT, 0, 0, 0)
    }

    pub fn address_device(input_ctx: u64, slot: u8, bsr: bool) -> Trb {
        let bsr = if bsr { BSR } else { 0 };
        Trb::typed(ADDRESS_DEVICE, input_ctx, 0, bsr | (slot as u32) << 24)
    }

    pub fn configure_endpoint(input_ctx: u64, slot: u8) -> Trb {
        Trb::typed(CONFIGURE_ENDPOINT, input_ctx, 0, (slot as u32) << 24)
    }

    pub fn evaluate_context(input_ctx: u64, slot: u8) -> Trb {
        Trb::typed(EVALUATE_CONTEXT, input_ctx, 0, (slot as u32) << 24)
    }

    /// Setup stage: the 8-byte packet is carried immediate in `param`.
    pub fn setup(packet: u64, trt: u8) -> Trb {
        Trb::typed(SETUP, packet, 8, IDT | (trt as u32 & 3) << 16)
    }

    pub fn data(buf: u64, len: u32, dir_in: bool) -> Trb {
        let dir = if dir_in { DIR_IN | ISP } else { 0 };
        Trb::typed(DATA, buf, len & 0x1_FFFF, dir)
    }

    pub fn status(dir_in: bool) -> Trb {
        Trb::typed(STATUS, 0, 0, IOC | if dir_in { DIR_IN } else { 0 })
    }

    pub fn normal(buf: u64, len: u32, ioc: bool) -> Trb {
        Trb::typed(NORMAL, buf, len & 0x1_FFFF, ISP | if ioc { IOC } else { 0 })
    }

    pub fn link(target: u64, toggle_cycle: bool) -> Trb {
        Trb::typed(LINK, target, 0, if toggle_cycle { TOGGLE_CYCLE } else { 0 })
    }
}

/// Decoded event TRB. Decoding is total: hardware-supplied words never panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    CommandCompletion { trb: u64, code: u8, slot: u8 },
    Transfer { trb: u64, code: u8, residual: u32, slot: u8, endpoint: u8 },
    PortStatusChange { port: u8 },
    HostController { code: u8 },
    Other(u8),
}

impl Event {
    pub fn decode(t: Trb) -> Event {
        let code = (t.status >> 24) as u8;
        let slot = (t.control >> 24) as u8;
        match t.trb_type() {
            COMMAND_COMPLETION => Event::CommandCompletion { trb: t.param, code, slot },
            TRANSFER_EVENT => Event::Transfer {
                trb: t.param,
                code,
                residual: t.status & 0x00FF_FFFF,
                slot,
                endpoint: (t.control >> 16 & 0x1f) as u8,
            },
            PORT_STATUS_CHANGE => Event::PortStatusChange { port: (t.param >> 24) as u8 },
            HOST_CONTROLLER => Event::HostController { code },
            other => Event::Other(other),
        }
    }
}

/// Result of [`ProducerRing::enqueue`]; the caller writes `trb` at `index`
/// and, if present, the link TRB at its index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Enqueued {
    pub index: usize,
    pub trb: Trb,
    pub link: Option<(usize, Trb)>,
}

/// Command / transfer ring: `len - 1` usable slots, the last is a Link TRB.
#[derive(Clone, Copy, Debug)]
pub struct ProducerRing {
    base: u64,
    len: usize,
    enqueue: usize,
    cycle: bool,
}

impl ProducerRing {
    pub fn new(base: u64, len: usize) -> ProducerRing {
        assert!(len >= 2, "ring needs at least 2 slots");
        ProducerRing { base, len, enqueue: 0, cycle: true }
    }

    pub fn enqueue(&mut self, t: Trb) -> Enqueued {
        let index = self.enqueue;
        let trb = t.with_cycle(self.cycle);
        self.enqueue += 1;
        let mut link = None;
        if self.enqueue == self.len - 1 {
            let l = Trb::link(self.base, true).with_cycle(self.cycle);
            link = Some((self.len - 1, l));
            self.enqueue = 0;
            self.cycle = !self.cycle;
        }
        Enqueued { index, trb, link }
    }

    pub fn enqueue_ptr(&self) -> u64 {
        self.base + (self.enqueue as u64) * 16
    }
}

/// Consumer side of an event ring.
#[derive(Clone, Copy, Debug)]
pub struct EventRing {
    len: usize,
    dequeue: usize,
    cycle: bool,
}

impl EventRing {
    pub fn new(len: usize) -> EventRing {
        assert!(len >= 1);
        EventRing { len, dequeue: 0, cycle: true }
    }

    pub fn poll(&mut self, read: impl Fn(usize) -> [u32; 4]) -> Option<Trb> {
        let t = Trb::from_words(read(self.dequeue));
        if t.cycle() != self.cycle {
            return None;
        }
        self.dequeue += 1;
        if self.dequeue == self.len {
            self.dequeue = 0;
            self.cycle = !self.cycle;
        }
        Some(t)
    }

    /// Value for ERDP: dequeue address with the Event Handler Busy bit set.
    pub fn erdp(&self, base: u64) -> u64 {
        (base + (self.dequeue as u64) * 16) | 8
    }
}

/// Event Ring Segment Table entry.
pub fn erst_entry(base: u64, size: u16) -> [u32; 4] {
    [base as u32, (base >> 32) as u32, size as u32, 0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trb_words_roundtrip() {
        let t = Trb { param: 0x1122_3344_5566_7788, status: 0xAABB_CCDD, control: 0x0102_0304 };
        let w = t.to_words();
        assert_eq!(w, [0x5566_7788, 0x1122_3344, 0xAABB_CCDD, 0x0102_0304]);
        assert_eq!(Trb::from_words(w), t);
    }

    #[test]
    fn noop_cmd_type_23() {
        let t = Trb::noop_cmd();
        assert_eq!(t.trb_type(), 23);
        assert_eq!(t.control >> 10 & 0x3f, 23);
        assert_eq!(Trb::enable_slot().trb_type(), 9);
    }

    #[test]
    fn setup_trb_has_idt_and_len_8() {
        let t = Trb::setup(0x0123_4567_89AB_CDEF, 3);
        assert_eq!(t.trb_type(), SETUP);
        assert_eq!(t.param, 0x0123_4567_89AB_CDEF);
        assert_eq!(t.status, 8);
        assert_ne!(t.control & (1 << 6), 0);
        assert_eq!(t.control >> 16 & 3, 3);
        assert_eq!(Trb::setup(0, 0).control >> 16 & 3, 0);
    }

    #[test]
    fn cycle_bit_set_on_first_pass() {
        let mut r = ProducerRing::new(0x1000, 4);
        let e = r.enqueue(Trb::noop_cmd());
        assert_eq!(e.index, 0);
        assert!(e.trb.cycle());
        assert!(e.link.is_none());
        assert_eq!(r.enqueue_ptr(), 0x1010);
    }

    #[test]
    fn link_trb_points_to_base_with_toggle() {
        let mut r = ProducerRing::new(0x2000, 3);
        assert!(r.enqueue(Trb::noop_cmd()).link.is_none());
        let e = r.enqueue(Trb::noop_cmd());
        assert_eq!(e.index, 1);
        let (i, l) = e.link.expect("link on last usable slot");
        assert_eq!(i, 2);
        assert_eq!(l.trb_type(), LINK);
        assert_eq!(l.param, 0x2000);
        assert_ne!(l.control & 2, 0);
        assert!(l.cycle());
        assert_eq!(r.enqueue_ptr(), 0x2000);
    }

    #[test]
    fn cycle_flips_after_wrap() {
        let mut r = ProducerRing::new(0x2000, 3);
        r.enqueue(Trb::noop_cmd());
        r.enqueue(Trb::noop_cmd());
        let e = r.enqueue(Trb::noop_cmd());
        assert_eq!(e.index, 0);
        assert!(!e.trb.cycle());
        r.enqueue(Trb::noop_cmd());
        let e = r.enqueue(Trb::noop_cmd());
        assert!(e.trb.cycle());
    }

    #[test]
    fn ring_len_2_works() {
        let mut r = ProducerRing::new(0x4000, 2);
        let e = r.enqueue(Trb::noop_cmd());
        assert_eq!(e.index, 0);
        assert!(e.trb.cycle());
        let (i, l) = e.link.unwrap();
        assert_eq!(i, 1);
        assert_eq!(l.param, 0x4000);
        let e = r.enqueue(Trb::noop_cmd());
        assert_eq!(e.index, 0);
        assert!(!e.trb.cycle());
        assert!(!e.link.unwrap().1.cycle());
    }

    fn ring_with(cycles: &[(usize, bool)]) -> impl Fn(usize) -> [u32; 4] + '_ {
        move |i| {
            let c = cycles.iter().find(|(k, _)| *k == i).map(|(_, c)| *c).unwrap_or(false);
            Trb { param: i as u64, status: 0, control: (34 << 10) | c as u32 }.to_words()
        }
    }

    #[test]
    fn event_ring_empty_when_cycle_mismatch() {
        let mut r = EventRing::new(4);
        assert!(r.poll(|_| [0; 4]).is_none());
        let mut r = EventRing::new(4);
        // Slot 0 carries cycle 0 while the consumer expects 1.
        assert!(r.poll(ring_with(&[])).is_none());
    }

    #[test]
    fn event_ring_consumes_in_order_and_wraps() {
        let mut r = EventRing::new(2);
        let first = [(0, true), (1, true)];
        let rd = ring_with(&first);
        assert_eq!(r.poll(&rd).unwrap().param, 0);
        assert_eq!(r.poll(&rd).unwrap().param, 1);
        // Wrapped: consumer cycle is now 0; old cycle-1 entries are stale.
        assert!(r.poll(&rd).is_none());
        let second = [(0, false), (1, true)];
        let rd = ring_with(&second);
        assert_eq!(r.poll(&rd).unwrap().param, 0);
        assert!(r.poll(&rd).is_none());
    }

    #[test]
    fn erdp_sets_ehb_and_points_at_dequeue() {
        let mut r = EventRing::new(4);
        assert_eq!(r.erdp(0x8000), 0x8000 | 8);
        let c = [(0, true), (1, true)];
        let rd = ring_with(&c);
        r.poll(&rd);
        r.poll(&rd);
        assert_eq!(r.erdp(0x8000), (0x8000 + 32) | 8);
    }

    #[test]
    fn decode_port_status_change_port_id() {
        let t = Trb { param: 5u64 << 24 | 0x00AB_CDEF, status: 1 << 24, control: 34 << 10 | 1 };
        assert_eq!(Event::decode(t), Event::PortStatusChange { port: 5 });
    }

    #[test]
    fn decode_transfer_event_fields() {
        let t = Trb {
            param: 0xDEAD_BEE0,
            status: (13 << 24) | 0x0012_3456,
            control: (7 << 24) | (3 << 16) | (32 << 10) | 1,
        };
        assert_eq!(
            Event::decode(t),
            Event::Transfer { trb: 0xDEAD_BEE0, code: 13, residual: 0x12_3456, slot: 7, endpoint: 3 }
        );
        let c = Trb { param: 0x1230, status: 1 << 24, control: (4 << 24) | (33 << 10) };
        assert_eq!(Event::decode(c), Event::CommandCompletion { trb: 0x1230, code: 1, slot: 4 });
        let h = Trb { param: 0, status: 5 << 24, control: 37 << 10 };
        assert_eq!(Event::decode(h), Event::HostController { code: 5 });
    }

    #[test]
    fn decode_unknown_type_is_other() {
        let t = Trb { param: 0, status: 0, control: 48 << 10 };
        assert_eq!(Event::decode(t), Event::Other(48));
    }

    #[test]
    fn decode_never_panics_on_arbitrary_words() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..100_000 {
            let (a, b) = (next(), next());
            let _ = Event::decode(Trb::from_words([a as u32, (a >> 32) as u32, b as u32, (b >> 32) as u32]));
        }
        for ty in 0..64u32 {
            let _ = Event::decode(Trb { param: u64::MAX, status: u32::MAX, control: ty << 10 });
        }
    }

    #[test]
    fn command_constructors_encode_fields() {
        let a = Trb::address_device(0x5000, 9, true);
        assert_eq!((a.trb_type(), a.param), (ADDRESS_DEVICE, 0x5000));
        assert_eq!(a.control >> 24, 9);
        assert_ne!(a.control & (1 << 9), 0);
        assert_eq!(Trb::address_device(0, 1, false).control & (1 << 9), 0);
        let c = Trb::configure_endpoint(0x6000, 2);
        assert_eq!((c.trb_type(), c.control >> 24), (CONFIGURE_ENDPOINT, 2));
        let e = Trb::evaluate_context(0x7000, 3);
        assert_eq!((e.trb_type(), e.control >> 24), (EVALUATE_CONTEXT, 3));
    }

    #[test]
    fn transfer_constructors_encode_fields() {
        let d = Trb::data(0x9000, 18, true);
        assert_eq!((d.trb_type(), d.param, d.status & 0x1FFFF), (DATA, 0x9000, 18));
        assert_ne!(d.control & (1 << 16), 0);
        assert_eq!(Trb::data(0, 1, false).control & (1 << 16), 0);
        let s = Trb::status(true);
        assert_eq!(s.trb_type(), STATUS);
        assert_ne!(s.control & (1 << 16), 0);
        assert_ne!(s.control & (1 << 5), 0);
        let n = Trb::normal(0xA000, 8, true);
        assert_eq!(n.trb_type(), NORMAL);
        assert_ne!(n.control & (1 << 5), 0);
        assert_eq!(Trb::normal(0, 8, false).control & (1 << 5), 0);
    }

    #[test]
    fn erst_entry_layout() {
        assert_eq!(erst_entry(0x1_2345_6000, 16), [0x2345_6000, 1, 16, 0]);
    }
}
