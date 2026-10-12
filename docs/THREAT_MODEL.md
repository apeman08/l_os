# Threat model (initial, for the current development stage)

This describes goals and what is **actually** defended today. Today the answer is: almost nothing, because there is no isolation boundary. Do not treat any statement here as a guarantee about future behavior.

## Assets
Typed input; (future) network destinations and page content; (future) cryptographic keys and passwords; (future) persistent volumes; kernel integrity.

## Adversaries (in scope as design targets)
1. **Remote network attacker / malicious site** (future, once networking exists).
2. **Malicious or buggy application/service** (future, needs user mode).
3. **Passive observer of traffic** (DNS and connection metadata count).
4. **Offline attacker with a stolen powered-off device** (relevant once optional storage exists).
5. **Local physical attacker with a running device** (cold boot, DMA, firmware): *out of scope to fully defend*; documented as residual risk.

## Out of scope / non-goals for now
Hardware bugs (speculative execution, Rowhammer), malicious firmware/BIOS/SMM, and a compromised build toolchain. Secure Boot is not implemented, so a modified boot image is undetectable.

## Current protections vs. gaps

| Property | Today | Truth |
|---|---|---|
| Network denied by default | No NIC driver exists | Holds by absence of code, not by a policy mechanism. Must be replaced by an enforced capability check before any driver lands. |
| RAM-only | No storage driver exists | Holds by absence. No claim about forensic traces: RAM contents, firmware, and emulator disk images are outside our control. |
| Telemetry | None in code | Source-auditable; the project is small. |
| Memory safety | Rust, `unsafe` limited to hardware access (port I/O, VGA at 0xb8000, GDT/IDT loading, PIC) | Kernel bugs in `unsafe` or logic remain possible. |
| Input hardening | Shell accepts printable ASCII only, bounded 128 bytes, no heap | Prevents screen-control injection via typed input. |
| Process isolation | **None.** Single address space, ring 0 | Any code in the image is fully trusted. |
| Exception handling | Only 3 handlers | Unhandled exceptions → double fault → panic/halt (fail-stop, no recovery). |

## Principles for upcoming work
- Enforcement lives in the kernel at a boundary untrusted code cannot bypass (syscalls/IPC capability checks), never in the shell.
- DNS is a network operation and needs the same authorization as TCP.
- TLS and crypto only via vetted `no_std` libraries; certificate validation must work before any HTTPS feature is exposed; no HTTP fallback.
- Secrets: dedicated zeroizing type, minimal copies; documented as best-effort (registers, caches, swap-like copies cannot be guaranteed cleared).
- Persistent storage: separate, opt-in, authenticated encryption, explicit consent; design before code.
