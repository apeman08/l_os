//! USB descriptors, setup packets and device classification (pure logic).
//! Descriptors come from untrusted devices: every walk is bounds-checked and
//! malformed input yields a `DescError`, never a panic or an endless loop.

const DT_DEVICE: u8 = 1;
const DT_CONFIG: u8 = 2;
const DT_INTERFACE: u8 = 4;
const DT_ENDPOINT: u8 = 5;

/// Control-transfer setup packet (USB 2.0 §9.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupPacket {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl SetupPacket {
    /// Little-endian packing as carried in the xHCI Setup Stage TRB parameter.
    pub fn to_u64(&self) -> u64 {
        self.request_type as u64
            | (self.request as u64) << 8
            | (self.value as u64) << 16
            | (self.index as u64) << 32
            | (self.length as u64) << 48
    }

    pub fn dir_in(&self) -> bool {
        self.request_type & 0x80 != 0
    }

    pub fn get_descriptor(dtype: u8, index: u8, len: u16) -> SetupPacket {
        SetupPacket { request_type: 0x80, request: 6, value: (dtype as u16) << 8 | index as u16, index: 0, length: len }
    }

    pub fn set_configuration(value: u8) -> SetupPacket {
        SetupPacket { request_type: 0x00, request: 9, value: value as u16, index: 0, length: 0 }
    }

    /// HID SET_PROTOCOL(boot protocol = 0).
    pub fn set_protocol_boot(iface: u8) -> SetupPacket {
        SetupPacket { request_type: 0x21, request: 0x0b, value: 0, index: iface as u16, length: 0 }
    }

    /// HID SET_IDLE(0): report only on change.
    pub fn set_idle(iface: u8) -> SetupPacket {
        SetupPacket { request_type: 0x21, request: 0x0a, value: 0, index: iface as u16, length: 0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescError {
    Truncated,
    BadLength,
    BadType,
    TooLong,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub usb: u16,
    pub class: u8,
    pub max_packet0: u8,
    pub vendor: u16,
    pub product: u16,
    pub num_configs: u8,
}

impl DeviceDescriptor {
    /// The first 8 bytes of a device descriptor suffice to learn `bMaxPacketSize0`.
    pub fn parse_prefix(b: &[u8]) -> Result<u8, DescError> {
        if b.len() < 8 {
            return Err(DescError::Truncated);
        }
        if b[0] < 8 {
            return Err(DescError::BadLength);
        }
        if b[1] != DT_DEVICE {
            return Err(DescError::BadType);
        }
        Ok(b[7])
    }

    pub fn parse(b: &[u8]) -> Result<DeviceDescriptor, DescError> {
        let mp0 = Self::parse_prefix(b)?;
        if b.len() < 18 {
            return Err(DescError::Truncated);
        }
        if b[0] < 18 {
            return Err(DescError::BadLength);
        }
        Ok(DeviceDescriptor {
            usb: u16::from_le_bytes([b[2], b[3]]),
            class: b[4],
            max_packet0: mp0,
            vendor: u16::from_le_bytes([b[8], b[9]]),
            product: u16::from_le_bytes([b[10], b[11]]),
            num_configs: b[17],
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub address: u8,
    pub attributes: u8,
    pub max_packet: u16,
    pub interval: u8,
}

impl Endpoint {
    fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }
    fn kind(&self) -> u8 {
        self.attributes & 3
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interface {
    pub number: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: [Option<Endpoint>; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub value: u8,
    pub total_length: u16,
    pub interfaces: [Option<Interface>; 4],
}

/// Parse a configuration descriptor with its trailing interface/endpoint
/// descriptors. Unknown descriptor types are skipped; alternate settings other
/// than 0 are ignored.
pub fn parse_config(b: &[u8]) -> Result<Config, DescError> {
    if b.len() < 9 {
        return Err(DescError::Truncated);
    }
    if b[0] < 9 {
        return Err(DescError::BadLength);
    }
    if b[1] != DT_CONFIG {
        return Err(DescError::BadType);
    }
    let total = u16::from_le_bytes([b[2], b[3]]);
    if (total as usize) < b[0] as usize {
        return Err(DescError::BadLength);
    }
    if total as usize > b.len() {
        return Err(DescError::Truncated);
    }
    let b = &b[..total as usize];
    let mut cfg = Config { value: b[5], total_length: total, interfaces: [None; 4] };
    let mut n_if = 0usize;
    // Index of the interface endpoints currently attach to (None: ignored alt setting).
    let mut cur: Option<usize> = None;
    let mut seen_iface = false;
    let mut off = b[0] as usize;
    while off < b.len() {
        if b.len() - off < 2 {
            return Err(DescError::Truncated);
        }
        let len = b[off] as usize;
        let ty = b[off + 1];
        if len < 2 {
            return Err(DescError::BadLength);
        }
        if off + len > b.len() {
            return Err(DescError::Truncated);
        }
        let d = &b[off..off + len];
        match ty {
            DT_INTERFACE => {
                if len < 9 {
                    return Err(DescError::BadLength);
                }
                seen_iface = true;
                if d[3] == 0 {
                    if n_if == 4 {
                        return Err(DescError::TooLong);
                    }
                    cfg.interfaces[n_if] = Some(Interface {
                        number: d[2],
                        class: d[5],
                        subclass: d[6],
                        protocol: d[7],
                        endpoints: [None; 4],
                    });
                    cur = Some(n_if);
                    n_if += 1;
                } else {
                    cur = None;
                }
            }
            DT_ENDPOINT => {
                if len < 7 {
                    return Err(DescError::BadLength);
                }
                if !seen_iface {
                    return Err(DescError::BadType);
                }
                if let Some(i) = cur {
                    let e = Endpoint {
                        address: d[2],
                        attributes: d[3],
                        max_packet: u16::from_le_bytes([d[4], d[5]]),
                        interval: d[6],
                    };
                    let iface = cfg.interfaces[i].as_mut().ok_or(DescError::BadType)?;
                    let slot = iface.endpoints.iter_mut().find(|s| s.is_none()).ok_or(DescError::TooLong)?;
                    *slot = Some(e);
                }
            }
            _ => {}
        }
        off += len;
    }
    Ok(cfg)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    BootKeyboard { iface: u8, ep_in: Endpoint },
    BulkOnlyStorage { iface: u8, ep_in: Endpoint, ep_out: Endpoint },
    Unsupported,
}

pub fn classify(c: &Config) -> DeviceKind {
    for i in c.interfaces.iter().flatten() {
        let eps = || i.endpoints.iter().flatten();
        if (i.class, i.subclass, i.protocol) == (3, 1, 1) {
            if let Some(e) = eps().find(|e| e.is_in() && e.kind() == 3) {
                return DeviceKind::BootKeyboard { iface: i.number, ep_in: *e };
            }
        } else if (i.class, i.subclass, i.protocol) == (8, 6, 0x50) {
            let ein = eps().find(|e| e.is_in() && e.kind() == 2);
            let eout = eps().find(|e| !e.is_in() && e.kind() == 2);
            if let (Some(a), Some(b)) = (ein, eout) {
                return DeviceKind::BulkOnlyStorage { iface: i.number, ep_in: *a, ep_out: *b };
            }
        }
    }
    DeviceKind::Unsupported
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev18() -> [u8; 18] {
        [18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x6d, 0x04, 0x16, 0xc3, 0x00, 0x01, 1, 2, 3, 1]
    }

    fn iface(num: u8, class: u8, sub: u8, proto: u8, n_ep: u8) -> [u8; 9] {
        [9, 4, num, 0, n_ep, class, sub, proto, 0]
    }

    fn ep(addr: u8, attr: u8, mps: u16, iv: u8) -> [u8; 7] {
        [7, 5, addr, attr, mps as u8, (mps >> 8) as u8, iv]
    }

    fn build(parts: &[&[u8]], n_if: u8) -> alloc::vec::Vec<u8> {
        let mut v = alloc::vec![9u8, 2, 0, 0, n_if, 1, 0, 0x80, 50];
        for p in parts {
            v.extend_from_slice(p);
        }
        let t = v.len() as u16;
        v[2] = t as u8;
        v[3] = (t >> 8) as u8;
        v
    }

    #[test]
    fn setup_get_descriptor_device_bytes() {
        let s = SetupPacket::get_descriptor(1, 0, 18);
        assert_eq!(s.to_u64(), 0x0012_0000_0100_0680);
        assert!(s.dir_in());
    }

    #[test]
    fn set_protocol_boot_is_class_interface_request() {
        let s = SetupPacket::set_protocol_boot(2);
        assert_eq!((s.request_type, s.request, s.value, s.index, s.length), (0x21, 0x0b, 0, 2, 0));
        assert!(!s.dir_in());
        let i = SetupPacket::set_idle(1);
        assert_eq!((i.request_type, i.request, i.index), (0x21, 0x0a, 1));
        let c = SetupPacket::set_configuration(1);
        assert_eq!((c.request_type, c.request, c.value), (0x00, 9, 1));
        assert_eq!(c.to_u64(), 0x0000_0000_0001_0900);
    }

    #[test]
    fn device_descriptor_parse() {
        let d = DeviceDescriptor::parse(&dev18()).unwrap();
        assert_eq!(d, DeviceDescriptor { usb: 0x0200, class: 0, max_packet0: 64, vendor: 0x046d, product: 0xc316, num_configs: 1 });
        assert_eq!(DeviceDescriptor::parse(&dev18()[..17]), Err(DescError::Truncated));
        let mut b = dev18();
        b[1] = 2;
        assert_eq!(DeviceDescriptor::parse(&b), Err(DescError::BadType));
        let mut b = dev18();
        b[0] = 0;
        assert_eq!(DeviceDescriptor::parse(&b), Err(DescError::BadLength));
    }

    #[test]
    fn device_prefix_8_bytes_gives_max_packet0() {
        assert_eq!(DeviceDescriptor::parse_prefix(&dev18()[..8]), Ok(64));
        assert_eq!(DeviceDescriptor::parse_prefix(&dev18()[..7]), Err(DescError::Truncated));
    }

    #[test]
    fn config_keyboard_classified() {
        let b = build(&[&iface(0, 3, 1, 1, 1), &[9, 0x21, 0x11, 1, 0, 1, 0x22, 63, 0], &ep(0x81, 3, 8, 10)], 1);
        let c = parse_config(&b).unwrap();
        assert_eq!(c.value, 1);
        match classify(&c) {
            DeviceKind::BootKeyboard { iface, ep_in } => {
                assert_eq!(iface, 0);
                assert_eq!(ep_in, Endpoint { address: 0x81, attributes: 3, max_packet: 8, interval: 10 });
            }
            k => panic!("{:?}", k),
        }
    }

    #[test]
    fn config_mass_storage_bbb_classified() {
        let b = build(&[&iface(0, 8, 6, 0x50, 2), &ep(0x81, 2, 512, 0), &ep(0x02, 2, 512, 0)], 1);
        match classify(&parse_config(&b).unwrap()) {
            DeviceKind::BulkOnlyStorage { iface, ep_in, ep_out } => {
                assert_eq!((iface, ep_in.address, ep_out.address), (0, 0x81, 0x02));
                assert_eq!(ep_in.max_packet, 512);
            }
            k => panic!("{:?}", k),
        }
    }

    #[test]
    fn config_storage_without_bulk_out_unsupported() {
        let b = build(&[&iface(0, 8, 6, 0x50, 1), &ep(0x81, 2, 512, 0)], 1);
        assert_eq!(classify(&parse_config(&b).unwrap()), DeviceKind::Unsupported);
        // interrupt endpoints do not count as bulk
        let b = build(&[&iface(0, 8, 6, 0x50, 2), &ep(0x81, 3, 64, 1), &ep(0x02, 3, 64, 1)], 1);
        assert_eq!(classify(&parse_config(&b).unwrap()), DeviceKind::Unsupported);
    }

    #[test]
    fn blength_zero_is_error_not_loop() {
        let b = build(&[&iface(0, 3, 1, 1, 1), &[0, 5, 0x81, 3, 8, 0, 10]], 1);
        assert_eq!(parse_config(&b), Err(DescError::BadLength));
        let b = build(&[&iface(0, 3, 1, 1, 1), &[1, 0x21]], 1);
        assert_eq!(parse_config(&b), Err(DescError::BadLength));
    }

    #[test]
    fn total_length_beyond_buffer_is_truncated() {
        let mut b = build(&[&iface(0, 3, 1, 1, 1), &ep(0x81, 3, 8, 10)], 1);
        b[2] = 200;
        assert_eq!(parse_config(&b), Err(DescError::Truncated));
        // descriptor running past total_length
        let b = build(&[&iface(0, 3, 1, 1, 1), &[20, 0x21, 0]], 1);
        assert_eq!(parse_config(&b), Err(DescError::Truncated));
        assert_eq!(parse_config(&b[..5]), Err(DescError::Truncated));
    }

    #[test]
    fn unknown_descriptors_are_skipped() {
        let b = build(&[&iface(0, 3, 1, 1, 1), &[9, 0x21, 0x11, 1, 0, 1, 0x22, 63, 0], &[4, 0x30, 1, 2], &ep(0x81, 3, 8, 10)], 1);
        let c = parse_config(&b).unwrap();
        assert!(c.interfaces[0].unwrap().endpoints[0].is_some());
    }

    #[test]
    fn more_than_four_interfaces_is_too_long() {
        let i: [[u8; 9]; 5] = [iface(0, 3, 1, 1, 0), iface(1, 3, 1, 1, 0), iface(2, 3, 1, 1, 0), iface(3, 3, 1, 1, 0), iface(4, 3, 1, 1, 0)];
        let parts: alloc::vec::Vec<&[u8]> = i.iter().map(|x| &x[..]).collect();
        assert_eq!(parse_config(&build(&parts, 5)), Err(DescError::TooLong));
    }

    #[test]
    fn more_than_four_endpoints_is_too_long() {
        let e = ep(0x81, 3, 8, 1);
        let b = build(&[&iface(0, 3, 1, 1, 5), &e, &e, &e, &e, &e], 1);
        assert_eq!(parse_config(&b), Err(DescError::TooLong));
    }

    #[test]
    fn bad_header_and_stray_endpoint() {
        let mut b = build(&[&iface(0, 3, 1, 1, 0)], 1);
        b[1] = 1;
        assert_eq!(parse_config(&b), Err(DescError::BadType));
        let b = build(&[&ep(0x81, 3, 8, 1)], 0);
        assert_eq!(parse_config(&b), Err(DescError::BadType));
    }

    #[test]
    fn fuzz_never_panics() {
        let mut s: u32 = 0x1234_5678;
        let mut next = || {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            (s >> 16) as u8
        };
        for _ in 0..20000 {
            let n = (next() % 80) as usize;
            let mut v: alloc::vec::Vec<u8> = (0..n).map(|_| next() % 8).collect();
            if n >= 9 && next() % 2 == 0 {
                v[0] = 9;
                v[1] = 2;
                v[2] = n as u8;
                v[3] = 0;
            }
            if let Ok(c) = parse_config(&v) {
                let _ = classify(&c);
            }
            let _ = DeviceDescriptor::parse(&v);
            let _ = DeviceDescriptor::parse_prefix(&v);
        }
    }
}
