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

## Next (Stage B follow-ups, in priority order)
1. **Multiple processes**: separate address spaces (own L4 table per process), scheduler, per-process kernel stacks, frame *free*. *Check:* two processes cannot see each other's memory; killed process memory is reclaimed (no frame leak).
2. **Enable SMEP/SMAP** (check CPUID, set CR4) and re-run the user-mode suite. Add user stack-overflow and syscall-argument fuzz-style tests.
3. **Capability handles** instead of bits: kernel objects referenced through per-process tables, with revoke and (explicit) delegation.
4. Move the shell itself out of ring 0 (it should hold only Console + Keyboard capabilities).
5. **Remaining exception handlers** (#NMI, #MC, SIMD/x87) and a panic path that does not rely on the VGA lock.
6. Frame-allocator reclaim of boot-time memory, guard pages for kernel stacks.

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
