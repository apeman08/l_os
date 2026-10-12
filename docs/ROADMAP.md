# Roadmap

Ordered by prerequisite dependency. Each item lists its acceptance check.

## Done
- Boot, VGA, serial, GDT/IDT, PIC/PIT, PS/2 keyboard (pre-existing).
- **Stage A terminal** (this cycle): ISR-light input path, line editor, parser, `help/version/uptime/security/echo/clear`. Verified by in-kernel tests and `scripts/qemu_shell_smoke.py`.

## Next (Stage B — foundations)
1. **More exception handlers** (GPF, invalid opcode, stack segment, etc.) with fail-stop messages. *Check:* test that triggers each where practical.
2. **Physical frame allocator** from the bootloader memory map; **paging** (offset page table mapper); **kernel heap** + `alloc`. *Check:* in-kernel tests for map/unmap, allocation, exhaustion.
3. **Scrollback/cursor/serial console mirror** for usability (optional).
4. **Host-testable core crates** (`no_std`): parser, policy engine. *Check:* `cargo test` on host.
5. **User mode**: ring 3 GDT entries, per-process address spaces, `syscall`, scheduler. *Check:* a user program cannot read kernel memory (test faults and is killed).
6. **Capability table + policy engine** enforced at the syscall boundary; default: no capabilities. *Check:* unauthorized, revoked, and expired access tests.

## Stage C — networking (blocked on 2, 5, 6)
- PCI enumeration; one emulated NIC (e.g. virtio-net or e1000/RTL8139 under QEMU), interface down until authorized.
- Candidate stack: `smoltcp` (no_std; evaluate license/unsafe/features). DNS and TCP only through the authorization gate.
- TLS via an audited `no_std` implementation (e.g. `rustls` with a suitable crypto provider; evaluate feasibility). Certificate validation and trust store design before any `fetch`.
- Commands (only when real): `net status`, `net devices`, `net request host:port`, `net permissions`, `net revoke host:port`.
- Acceptance for `fetch https://example.com`: works end-to-end on the OS under QEMU user networking; fails closed on denied auth and bad certificates (tests for each).

## Later
- Secret-handling type with zeroization; RAM-only enforcement design; optional encrypted storage (AEAD + KDF, explicit consent).
- Text browser as an unprivileged process: HTTPS only, no JS/cookies/history by default, numbered links, `open N`/`back`, redirect checks.
- UEFI boot and verified boot chain.
