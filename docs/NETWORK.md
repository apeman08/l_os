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
| `netctl` syscall + `NetManager` handle | [impl] | Management interface for the trusted shell (grant, revoke, list, status, resolver). |
| `net` shell commands | [impl] | `net status/devices/permissions/request/revoke/resolver`. Trusted path: only the shell holds the keyboard and the manager. |
| Hardware RNG (`src/rng.rs`) | [impl] | RDSEED, then RDRAND; no software fallback. |
| Wall clock (`src/rtc.rs`) | [impl] | CMOS RTC, plausibility window 2026-2099, else "no clock". |
| TLS 1.3 client (`crates/l_tls`) | [impl] | rustls 0.23.45 + RustCrypto provider; runs in ring 3 on socket handles. |
| `fetch` | [impl, limited] | `run fetch https://HOST[:PORT]/PATH`; HTTPS only. |

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
* Grants are made only by the shell's `net request`, after a typed `y`. The
  question states what is requested and that asking sends nothing; the answer
  shown afterwards is the kernel's own list (request != enforced). Child
  programs have no keyboard handle and no manager handle, so they cannot
  answer for the user.
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
  This holds only because the runner gives the boot disk `bootindex=0`, so
  OVMF connects only the disk; otherwise OVMF itself sends IPv6 frames (MLD
  report, neighbour solicitation with the MAC) before the kernel starts. Real
  UEFI firmware can likewise send frames before the kernel runs; the kernel's
  guarantee starts when it takes over.
* `network` (10 tests): echo through the real stack against a host server,
  repeated connections, denied attempts do not change the transmit counter
  even with the NIC up, revoke-then-deny, revoking cuts an open connection
  (`ERR_REVOKED`), an expiring grant ends the connection, refused ports,
  name resolution with both grants, NXDOMAIN, and revoking the resolver stops
  further lookups.

## TLS and `fetch` (phase 4)

* **Library**: `rustls` 0.23.45 for the protocol and `rustls-webpki` 0.103 for
  certificate validation. The existing no_std provider (`rustls-rustcrypto`)
  is an alpha pinned to rustls 0.23.4 / webpki 0.102 (known-fixed security
  issues), so `crates/l_tls` contains new glue that connects the RustCrypto
  primitives (AES-GCM, ChaCha20-Poly1305, SHA-2, HMAC, X25519, P-256,
  ECDSA/Ed25519/RSA *verification*) to rustls 0.23.45, structured after that
  crate (MIT/Apache). No primitive is implemented in this repository.
* **Policy**: client only, **TLS 1.3 only**, no client certificates, no
  signing keys, no session resumption or 0-RTT. A server without TLS 1.3 is
  refused; there is no downgrade and no plaintext fallback.
* **Entropy**: only the kernel's hardware RNG through a handle. If it fails,
  TLS does not start.
* **Time**: certificate validity uses the CMOS RTC, which is not
  authenticated (QEMU: the host clock). An implausible RTC (before 2026) makes
  every handshake fail; a manipulated RTC could make an expired certificate
  look valid.
* **Trust store**: `fetch` trusts the `webpki-roots` set (Mozilla roots,
  CDLA-Permissive-2.0), embedded in the program. The test CA exists only in the
  test program `tls_probe`. No revocation checking (CRL/OCSP), no certificate
  transparency.
* **Software crypto**: the target has no SSE, so the portable constant-time
  software backends are forced (no AES-NI). `rsa` 0.9 has RUSTSEC-2023-0071
  (timing side channel in *private-key* operations); this system only verifies
  signatures with public keys.
* **fetch**: GET with HTTP/1.0 (no chunked encoding, no keep-alive), redirects
  are shown but never followed (a new destination needs its own
  authorization), body shown is limited to 12 KiB and sanitized so server data
  cannot send terminal control codes.

Verified:

* Host (`crates/l_tls/tests`, against real `openssl s_server`): handshake and
  HTTP exchange with ECDSA and RSA(-PSS) certificates; wrong name, unknown
  CA, self-signed, expired, not yet valid, missing clock, TLS 1.2-only server,
  plaintext server (it receives only a ClientHello), cut stream, missing
  entropy are all rejected.
* Kernel (`tests/tls.rs`, ring-3 `tls_probe` through kernel sockets): the same
  outcomes on the real OS, plus: the same authorizations as every other
  connection (denied attempts leave the transmit counter unchanged), and TLS
  refuses to start without a Random handle. `scripts/net_tests.sh` also checks
  that the non-TLS server received only a TLS record and no `GET`.
* Real shell (`scripts/qemu_fetch_smoke.py`): `net resolver`, `net request`
  (3x), `run fetch https://test.example:18443/` goes through DNS, TCP and a TLS
  1.3 handshake with the production root set and is refused with "authority
  this system does not trust" (the server uses the test CA). Also refused:
  `http://`, and a host without authorization.

**Not shown**: a *successful* `fetch` with the production root set against a
public site. This sandbox forces all TLS through an egress gateway with its own
CA (checked with `openssl s_client`: issuer "Anthropic ... Egress Gateway CA"),
so a real public certificate chain is never presented to the guest. The
success path is exercised by `tls_probe`, which runs the same `https::get`
code with the test CA instead of the production roots. Do not read
"`fetch https://example.com` works" into any of this.

## Not verified / known limits

* Only QEMU's emulated virtio-net with user-mode networking. No real NIC, no
  IPv6, no DHCP, no fragmentation tuning, no congestion tuning.
* DMA is not isolated (no IOMMU); the emulated device is trusted.
* The packet-capture check proves "no frame in this run", not that no code path
  could ever send one.
* Timers use the 100 Hz PIT tick (10 ms resolution); connect/DNS/IO timeouts
  are 10/5/30 s.
* A process can open at most 6 sockets system-wide; there is no per-process
  quota and no rate limiting.
* The grant table holds 16 entries.
* Handshake latency with software-only crypto was not measured; expect it to be noticeably slower than on hardware-accelerated systems.
