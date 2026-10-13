//! Kernel networking: a TCP/IP stack (smoltcp) on the virtio-net NIC, always
//! behind the network policy.
//!
//! Design rules (see docs/ARCHITECTURE.md, docs/THREAT_MODEL.md):
//! * Nothing touches the NIC until a connection request has passed the policy.
//!   Before that the device is never initialized: no ARP, no DHCP, no DNS, no
//!   connectivity checks. The IP configuration is static (QEMU user-mode
//!   defaults) precisely so that no discovery traffic exists.
//! * A name is resolved only if the name:port is authorized AND the resolver
//!   itself is authorized. DNS is a network operation like any other.
//! * Every send/receive re-checks the grant; revoking or expiry aborts the
//!   connection (TCP reset) and later calls fail.
//! * User programs only ever hold socket handles; they cannot reach the
//!   device, the stack, or the policy tables.

use crate::{rng, sched, time, usermode::POLICY, virtio_net::{self, VirtioNet}};
use alloc::{string::String, vec, vec::Vec};
use l_core::{
    dns,
    policy::{Decision, DenyReason},
    syscall as abi,
};
use smoltcp::{
    iface::{Config, Interface, PollResult, SocketHandle, SocketSet},
    socket::{tcp, udp},
    time::{Duration, Instant},
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address},
};
use spin::Mutex;
use x86_64::instructions::interrupts;

pub const MAX_SOCKETS: usize = 6;
const TCP_BUFFER: usize = 16 * 1024;

/// QEMU user-mode networking defaults.
const OUR_IP: [u8; 4] = [10, 0, 2, 15];
const GATEWAY: [u8; 4] = [10, 0, 2, 2];
pub const DEFAULT_RESOLVER: ([u8; 4], u16) = ([10, 0, 2, 3], 53);

const CONNECT_TIMEOUT_TICKS: u64 = 10 * time::TICK_HZ as u64; // 10 s
const DNS_TIMEOUT_TICKS: u64 = 5 * time::TICK_HZ as u64; // 5 s
const IO_TIMEOUT_TICKS: u64 = 30 * time::TICK_HZ as u64; // 30 s
const CLOSE_GRACE_TICKS: u64 = 5 * time::TICK_HZ as u64; // 5 s

struct Slot {
    tcp: SocketHandle,
    /// The exact authorization this connection runs under.
    host: String,
    port: u16,
    owner: u32,
    /// When the program closed it; freed once the stack is done with it.
    closing_since: Option<u64>,
    revoked: bool,
}

struct Stack {
    nic: VirtioNet,
    iface: Interface,
    sockets: SocketSet<'static>,
    udp: SocketHandle,
    slots: [Option<Slot>; MAX_SOCKETS],
    rng_state: u64,
}

static STACK: Mutex<Option<Stack>> = Mutex::new(None);
static RESOLVER: Mutex<([u8; 4], u16)> = Mutex::new(DEFAULT_RESOLVER);

fn now() -> Instant {
    Instant::from_millis(time::ticks_to_ms(time::ticks()).min(i64::MAX as u64) as i64)
}

/// Run `f` with the stack locked and interrupts off (the lock is also taken
/// from interrupt-adjacent paths, so it must never be held with interrupts on).
fn with_stack<R>(f: impl FnOnce(&mut Option<Stack>) -> R) -> R {
    interrupts::without_interrupts(|| f(&mut STACK.lock()))
}

fn deny_code(d: Decision) -> Result<(), i64> {
    match d {
        Decision::Allow => Ok(()),
        Decision::Deny(DenyReason::NoGrant) => Err(abi::DENIED_NO_GRANT),
        Decision::Deny(DenyReason::Expired) => Err(abi::DENIED_EXPIRED),
        Decision::Deny(DenyReason::InvalidDestination) => Err(abi::DENIED_INVALID_DESTINATION),
    }
}

fn check(host: &str, port: u16) -> Result<(), i64> {
    deny_code(POLICY.lock().check(host, port, time::ticks()))
}

pub fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = s.split('.');
    for o in out.iter_mut() {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        *o = p.parse::<u8>().ok()?;
    }
    if parts.next().is_some() { None } else { Some(out) }
}

fn ipv4_string(ip: [u8; 4]) -> String {
    alloc::format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
}

impl Stack {
    fn poll(&mut self) {
        // Policy first: connections whose grant ended are reset before more
        // data can move in either direction.
        self.enforce_policy();
        let _: PollResult = self.iface.poll(now(), &mut self.nic, &mut self.sockets);
        self.free_closed();
    }

    fn enforce_policy(&mut self) {
        let t = time::ticks();
        for slot in self.slots.iter_mut().flatten() {
            if slot.closing_since.is_some() || slot.revoked {
                continue;
            }
            if POLICY.lock().check(&slot.host, slot.port, t) != Decision::Allow {
                slot.revoked = true;
                self.sockets.get_mut::<tcp::Socket>(slot.tcp).abort();
            }
        }
    }

    fn free_closed(&mut self) {
        let t = time::ticks();
        for entry in self.slots.iter_mut() {
            let done = match entry {
                Some(slot) => match slot.closing_since {
                    Some(since) => {
                        let s = self.sockets.get_mut::<tcp::Socket>(slot.tcp);
                        if !s.is_open() {
                            true
                        } else if t - since > CLOSE_GRACE_TICKS {
                            s.abort();
                            true
                        } else {
                            false
                        }
                    }
                    None => false,
                },
                None => false,
            };
            if done {
                if let Some(slot) = entry.take() {
                    self.sockets.remove(slot.tcp);
                }
            }
        }
    }

    fn next_random(&mut self) -> u64 {
        // xorshift64*, seeded from the hardware generator; only used for
        // ephemeral ports and DNS transaction IDs, which need unpredictability
        // but not cryptographic strength beyond the seed.
        let mut x = self.rng_state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng_state = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn ephemeral_port(&mut self) -> u16 {
        49152 + (self.next_random() % 16000) as u16
    }
}

/// Bring the NIC and the stack up (first authorized use only).
fn ensure_up(slot: &mut Option<Stack>) -> Result<(), i64> {
    if slot.is_some() {
        return Ok(());
    }
    let probe = virtio_net::probe().ok_or(abi::ERR_NO_DEVICE)?;
    let seed = rng::next_u64().ok_or(abi::ERR_NO_ENTROPY)?;
    let mut nic = VirtioNet::bring_up(&probe).map_err(|_| abi::ERR_NO_DEVICE)?;

    let mut cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(probe.mac)));
    cfg.random_seed = seed;
    let mut iface = Interface::new(cfg, &mut nic, now());
    iface.update_ip_addrs(|a| {
        let _ = a.push(IpCidr::new(IpAddress::v4(OUR_IP[0], OUR_IP[1], OUR_IP[2], OUR_IP[3]), 24));
    });
    let _ = iface.routes_mut().add_default_ipv4_route(Ipv4Address::new(GATEWAY[0], GATEWAY[1], GATEWAY[2], GATEWAY[3]));

    let mut sockets = SocketSet::new(Vec::new());
    let udp_sock = udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 2048]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 2048]),
    );
    let udp = sockets.add(udp_sock);
    *slot = Some(Stack {
        nic,
        iface,
        sockets,
        udp,
        slots: [const { None }; MAX_SOCKETS],
        rng_state: seed | 1,
    });
    Ok(())
}

/// Poll until `check` yields a result or the timeout passes. The stack lock is
/// released while waiting so other processes can use the network.
fn wait_until<T>(
    timeout_ticks: u64,
    mut check: impl FnMut(&mut Stack) -> Option<Result<T, i64>>,
) -> Result<T, i64> {
    let start = time::ticks();
    loop {
        let outcome = with_stack(|g| {
            let st = g.as_mut().ok_or(abi::ERR_NO_DEVICE)?;
            st.poll();
            match check(st) {
                Some(r) => Ok(Some(r)),
                None => Ok::<_, i64>(None),
            }
        })?;
        if let Some(r) = outcome {
            // Push out whatever the check queued (acks, data, resets).
            with_stack(|g| {
                if let Some(st) = g.as_mut() {
                    st.poll();
                }
            });
            return r;
        }
        if time::ticks() - start > timeout_ticks {
            return Err(abi::ERR_TIMEOUT);
        }
        sched::block_on_net();
    }
}

fn resolve(name: &str) -> Result<[u8; 4], i64> {
    let (rip, rport) = *RESOLVER.lock();
    // The resolver is a destination like any other.
    check(&ipv4_string(rip), rport).map_err(|_| abi::DENIED_DNS_RESOLVER)?;
    with_stack(|g| ensure_up(g))?;

    let mut query = [0u8; dns::QUERY_MAX_LEN];
    let (id, local_port) = with_stack(|g| {
        let st = g.as_mut().ok_or(abi::ERR_NO_DEVICE)?;
        let id = (st.next_random() & 0xffff) as u16;
        let port = st.ephemeral_port();
        Ok::<_, i64>((id, port))
    })?;
    let qlen = dns::encode_query(id, name, &mut query).map_err(|_| abi::DENIED_INVALID_DESTINATION)?;
    let server = IpEndpoint::new(IpAddress::v4(rip[0], rip[1], rip[2], rip[3]), rport);

    let mut sent = false;
    wait_until(DNS_TIMEOUT_TICKS, |st| {
        let sock = st.sockets.get_mut::<udp::Socket>(st.udp);
        if !sent {
            sock.close();
            if sock.bind(local_port).is_err() || sock.send_slice(&query[..qlen], server).is_err() {
                return Some(Err(abi::ERR_DNS_FAILED));
            }
            sent = true;
            return None;
        }
        while let Ok((data, meta)) = sock.recv() {
            if meta.endpoint != server {
                continue; // not from the resolver we asked
            }
            match dns::parse_a_response(data, id, name) {
                Ok(addr) => return Some(Ok(addr)),
                Err(dns::DnsError::Unexpected) | Err(dns::DnsError::Malformed) => continue, // junk: keep waiting
                Err(_) => return Some(Err(abi::ERR_DNS_FAILED)),
            }
        }
        None
    })
    .map_err(|e| if e == abi::ERR_TIMEOUT { abi::ERR_DNS_FAILED } else { e })
}

/// Open an authorized TCP connection; returns the socket slot.
pub fn connect(host: &str, port: u16, owner: u32) -> Result<u16, i64> {
    // 1. Policy decides before anything else happens. A denial touches nothing.
    check(host, port)?;

    // 2. Resolve names (needs its own authorization); literals are used as is.
    let addr = match parse_ipv4(host) {
        Some(a) => a,
        None => resolve(host)?,
    };

    // 3. Only now does the NIC come up.
    let slot_index = with_stack(|g| -> Result<usize, i64> {
        ensure_up(g)?;
        let st = g.as_mut().ok_or(abi::ERR_NO_DEVICE)?;
        let free = st.slots.iter().position(Option::is_none).ok_or(abi::ERR_NO_SLOT)?;
        let mut sock = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0u8; TCP_BUFFER]),
            tcp::SocketBuffer::new(vec![0u8; TCP_BUFFER]),
        );
        sock.set_nagle_enabled(false);
        sock.set_timeout(Some(Duration::from_secs(60)));
        let local = st.ephemeral_port();
        let remote = IpEndpoint::new(IpAddress::v4(addr[0], addr[1], addr[2], addr[3]), port);
        sock.connect(st.iface.context(), remote, local).map_err(|_| abi::ERR_CONNECT_FAILED)?;
        let handle = st.sockets.add(sock);
        st.slots[free] = Some(Slot { tcp: handle, host: String::from(host), port, owner, closing_since: None, revoked: false });
        Ok(free)
    })?;

    let result = wait_until(CONNECT_TIMEOUT_TICKS, |st| {
        let h = st.slots[slot_index].as_ref()?.tcp;
        let s = st.sockets.get_mut::<tcp::Socket>(h);
        match s.state() {
            tcp::State::Established => Some(Ok(())),
            tcp::State::Closed | tcp::State::TimeWait | tcp::State::Closing => Some(Err(abi::ERR_CONNECT_FAILED)),
            _ => None,
        }
    });
    match result {
        Ok(()) => Ok(slot_index as u16),
        Err(e) => {
            close(slot_index as u16, owner, true);
            Err(e)
        }
    }
}

fn slot_check(st: &mut Stack, slot: u16, owner: u32) -> Result<SocketHandle, i64> {
    let s = st.slots.get(slot as usize).and_then(Option::as_ref).ok_or(abi::ERR_BAD_HANDLE)?;
    if s.owner != owner || s.closing_since.is_some() {
        return Err(abi::ERR_BAD_HANDLE);
    }
    if s.revoked {
        return Err(abi::ERR_REVOKED);
    }
    Ok(s.tcp)
}

/// Send some of `data`; returns the number of bytes accepted (> 0).
pub fn send(slot: u16, owner: u32, data: &[u8]) -> Result<usize, i64> {
    if data.is_empty() {
        return Ok(0);
    }
    wait_until(IO_TIMEOUT_TICKS, |st| {
        let h = match slot_check(st, slot, owner) {
            Ok(h) => h,
            Err(e) => return Some(Err(e)),
        };
        let s = st.sockets.get_mut::<tcp::Socket>(h);
        if !s.may_send() {
            return Some(Err(if st.slots[slot as usize].as_ref().is_some_and(|x| x.revoked) { abi::ERR_REVOKED } else { abi::ERR_CONNECT_FAILED }));
        }
        if s.can_send() {
            return Some(s.send_slice(data).map_err(|_| abi::ERR_CONNECT_FAILED));
        }
        None
    })
}

/// Receive into `buf`; `Ok(0)` means the peer closed the connection.
pub fn recv(slot: u16, owner: u32, buf: &mut [u8]) -> Result<usize, i64> {
    wait_until(IO_TIMEOUT_TICKS, |st| {
        let h = match slot_check(st, slot, owner) {
            Ok(h) => h,
            Err(e) => return Some(Err(e)),
        };
        let revoked = st.slots[slot as usize].as_ref().is_some_and(|x| x.revoked);
        let s = st.sockets.get_mut::<tcp::Socket>(h);
        if s.can_recv() {
            return Some(s.recv_slice(buf).map_err(|_| abi::ERR_CONNECT_FAILED));
        }
        if revoked {
            return Some(Err(abi::ERR_REVOKED));
        }
        if !s.may_recv() {
            return Some(Ok(0));
        }
        None
    })
}

/// Close a socket. `abort` resets immediately instead of a graceful close.
pub fn close(slot: u16, owner: u32, abort: bool) {
    with_stack(|g| {
        let Some(st) = g.as_mut() else { return };
        let Some(s) = st.slots.get_mut(slot as usize).and_then(Option::as_mut) else { return };
        if s.owner != owner {
            return;
        }
        let sock = st.sockets.get_mut::<tcp::Socket>(s.tcp);
        if abort {
            sock.abort();
        } else {
            sock.close();
        }
        s.closing_since.get_or_insert(time::ticks());
        st.poll();
    });
}

/// A process ended: reset everything it still had open.
pub fn release_owner(owner: u32) {
    with_stack(|g| {
        let Some(st) = g.as_mut() else { return };
        for slot in st.slots.iter_mut().flatten() {
            if slot.owner == owner && slot.closing_since.is_none() {
                st.sockets.get_mut::<tcp::Socket>(slot.tcp).abort();
                slot.closing_since = Some(time::ticks());
            }
        }
        st.poll();
    });
}

/// Called after the policy changed: reset connections that lost their grant
/// immediately (the RST goes out before this returns).
pub fn policy_changed() {
    with_stack(|g| {
        if let Some(st) = g.as_mut() {
            st.poll();
        }
    });
}

// ------------------------------------------------------------ management

#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub nic_present: bool,
    pub nic_up: bool,
    pub tx_frames: u64,
    pub rx_frames: u64,
    pub open_sockets: usize,
}

pub fn status() -> Status {
    let nic_present = virtio_net::probe().is_some();
    let (nic_up, open) = with_stack(|g| match g {
        Some(st) => (true, st.slots.iter().flatten().filter(|s| s.closing_since.is_none()).count()),
        None => (false, 0),
    });
    let (tx, rx) = virtio_net::counters();
    Status { nic_present, nic_up, tx_frames: tx, rx_frames: rx, open_sockets: open }
}

pub fn resolver() -> ([u8; 4], u16) {
    *RESOLVER.lock()
}

pub fn set_resolver(ip: [u8; 4], port: u16) {
    *RESOLVER.lock() = (ip, port);
}

/// Describe PCI device `index` as one text line.
pub fn describe_device(index: usize) -> Option<String> {
    let d = crate::pci::devices().nth(index)?;
    let mut line = alloc::format!(
        "{:02x}:{:02x}.{} {:04x}:{:04x} {}",
        d.bus, d.slot, d.func, d.vendor, d.device, crate::pci::class_name(&d)
    );
    if let Some(p) = virtio_net::probe() {
        if p.pci.bus == d.bus && p.pci.slot == d.slot && p.pci.func == d.func {
            line += &alloc::format!(
                " mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} irq {}",
                p.mac[0], p.mac[1], p.mac[2], p.mac[3], p.mac[4], p.mac[5], d.irq_line
            );
        }
    }
    Some(line)
}
