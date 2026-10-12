# Roadmap

Ordered by prerequisite dependency. Each item lists its acceptance check.

## Done
- Boot, VGA, serial, GDT/IDT, PIC/PIT, PS/2 keyboard (pre-existing).
- **Stage A terminal**: ISR-light input path, line editor, parser, shell commands. Verified by tests and `scripts/qemu_shell_smoke.py`.
- **Stage B (first pass)**:
  1. Exception handlers (#DE #UD #TS #NP #SS #GP #PF) with integration tests through the real IDT.
  2. Frame allocator, paging access, kernel heap (`tests/memory.rs`).
  3. Host-testable `l_core` crate (`scripts/host_tests.sh`).
  4. User mode: ring 3, `int 0x80`, per-process capability bits, pointer validation, time budget (`tests/usermode.rs`).
  5. Default-deny `NetPolicy` consulted by `SYS_NET_CHECK` (allow, deny, revoke, expiry tests).

- **Phase 0** (see PLAN_NEXT.md): CI, remaining exception handlers, panic path without lock, unwrap inventory.
- **Phase 1**: frame free list, per-process address spaces, process table, preemptive scheduler, SMEP/SMAP, pointer/stack-overflow tests.
- **Phase 2.1-2.3**: capability handles, delegation with attenuation, blocking IPC with deadlock cleanup.

## Next
1. **User-space build pipeline**: compile Rust programs for ring 3 (second crate, linker script, flat binary embedded in the kernel) and a dynamic `spawn`/`wait` syscall.
2. Keyboard/console as capability objects; **shell as a ring-3 process** (reusing `l_core::shell`).
3. Then Stage C below (PCI, NIC as service, policy at the syscall edge).

## Stage C — networking (blocked on multi-process isolation and capability handles)
- PCI enumeration; one emulated NIC (e.g. virtio-net or e1000/RTL8139 under QEMU), interface down until authorized.
- Candidate stack: `smoltcp` (no_std; evaluate license/unsafe/features). DNS and TCP only through the authorization gate.
- TLS via an audited `no_std` implementation (e.g. `rustls` with a suitable crypto provider; evaluate feasibility). Certificate validation and trust store design before any `fetch`.
- Commands (only when real): `net status`, `net devices`, `net request host:port`, `net permissions`, `net revoke host:port`.
- Acceptance for `fetch https://example.com`: works end-to-end on the OS under QEMU user networking; fails closed on denied auth and bad certificates (tests for each).

## Later
- Secret-handling type with zeroization; RAM-only enforcement design; optional encrypted storage (AEAD + KDF, explicit consent).
- Text browser as an unprivileged process: HTTPS only, no JS/cookies/history by default, numbered links, `open N`/`back`, redirect checks.
- UEFI boot and verified boot chain.
