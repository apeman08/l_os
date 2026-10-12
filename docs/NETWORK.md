# Network design (Phase 3 and 4)

Status legend as in ARCHITECTURE.md. This document records decisions, what is
verified, and what is not.

## What exists

| Piece | State | Notes |
|---|---|---|
| PCI enumeration (`src/pci.rs`) | [impl] | Port 0xCF8/0xCFC; reading config space sends nothing. |
| virtio-net driver (`src/virtio_net.rs`) | [impl] | Legacy (transitional) device over I/O ports, polled; the device interrupt only wakes waiting processes. Starts only when the first connection is authorized. |
| TCP/IP stack | [impl] | `smoltcp` 0.14 (0BSD, `#![deny(unsafe_code)]`) inside the kernel. Static IP config, no DHCP. |
| DNS client | [impl] | `l_core::dns` (strict parser, host-tested) over a smoltcp UDP socket. Resolver is configurable at runtime. |
| Policy gate (`src/net.rs`) | [impl] | Checked before DNS, before NIC bring-up, before connect, and again on every send/receive. |
| Socket handles | [impl] | `Object::Socket`, created without `GRANT` (cannot be delegated); closed with the process. |
| `netctl` syscall + `NetManager` handle | [impl] | Management interface for the trusted shell (grant, revoke, list, status). |
| `net` shell commands | [planned] | Not written yet; until then grants are made by kernel tests only. |
| TLS 1.3 client (`crates/l_tls`) | [partial] | Works on the host against `openssl s_server` (13 tests); not yet wired to socket handles in ring 3. |
| `fetch` | [planned] | Not written yet. |

## Decision: where the stack lives (plan item 3.2/3.3)

The plan asked for the NIC driver as a *service process* holding a device
capability. Not done, deliberately:

* Without an IOMMU, a process that can program a NIC's DMA can read and write
  all RAM, so delegating the device does not isolate anything.
* The IPC messages are 64 bytes; a data path through a service process would
  need shared memory first.

What we did instead: driver and TCP/IP stack in the kernel, **socket handles as
the only network authority** a process can hold, and TLS (the largest and most
exposed parser) in ring 3. Cost: `smoltcp` and the driver are in the trusted
computing base and parse untrusted packets in ring 0.

NIC choice: `virtio-net` (legacy I/O transport) because QEMU provides it, its
interface is small, and it needs no MMIO mapping. Real hardware is not
supported.

## Nothing happens without an authorization

* The NIC is not initialized at boot. Before the first *authorized* connection
  it never sends or receives a frame. There is no DHCP, no ARP at boot, no
  connectivity check, no time sync, no update check. The IP configuration is
  static (QEMU user-mode defaults 10.0.2.15/24, gateway 10.0.2.2).
* A connection needs a `NetClient` handle (the right to ask) **and** a live
  grant for exactly `host:port`. Names additionally need a grant for the DNS
  resolver `ip:port`, because a DNS query reveals intent.
* Intended design (not yet implemented): grants are made only by the shell's
  `net request`, after a typed `y`. Child programs have no keyboard handle, so
  they cannot answer for the user. Today only kernel tests grant.
* Revoking or expiring a grant resets open connections to it before more data
  moves.

Verified (kernel tests in QEMU, `scripts/net_tests.sh`):

* `network_denied` (10 tests): with nothing authorized, connects by IP and by
  name, expired grants, resolver-only or name-only grants, malformed names and
  a missing handle are all refused, the NIC is still down at the end, and
  QEMU's own packet capture (`filter-dump`) of that run contains **no frame**.
  The capture mechanism was shown to work: the functional run produces a
  non-empty capture, and with the policy check removed (mutation) the denied
  run's capture is no longer empty and its tests fail.
* `network` (10 tests): echo through the real stack against a host server,
  repeated connections, denied attempts do not change the transmit counter
  even with the NIC up, revoke-then-deny, revoking cuts an open connection
  (`ERR_REVOKED`), an expiring grant ends the connection, refused ports,
  name resolution with both grants, NXDOMAIN, and revoking the resolver stops
  further lookups.

## Not verified / known limits

* Only QEMU's emulated virtio-net with user-mode networking. No real NIC, no
  IPv6, no DHCP, no fragmentation tuning, no congestion tuning.
* DMA is not isolated (no IOMMU); the emulated device is trusted.
* The packet-capture check proves "no frame in this run", not that no code path
  could ever send one.
* Timers use the 18 Hz PIT tick; TCP retransmissions are therefore coarse.
* A process can open at most 6 sockets system-wide; there is no per-process
  quota and no rate limiting.
* The grant table holds 16 entries.
