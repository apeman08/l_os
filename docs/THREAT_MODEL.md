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
| Network denied by default | No NIC driver exists; `NetPolicy` is default-deny and consulted by `SYS_NET_CHECK` | Holds by absence of code. When a driver lands it must be reachable only through a syscall that calls the same check. |
| RAM-only | No storage driver exists | Holds by absence. No claim about forensic traces: RAM contents, firmware, and emulator disk images are outside our control. |
| Telemetry | None in code | Source-auditable; the project is small. |
| Memory safety | Rust; `unsafe` limited to hardware access, page-table edits, asm entry/exit, user-pointer copies, heap init | Kernel bugs in `unsafe` or logic remain possible. `l_core` forbids `unsafe`. |
| User/kernel isolation | **Demonstrated for one ring-3 process**: cannot read/write kernel pages, run privileged instructions, pass kernel pointers to syscalls, or run past its budget (all in `tests/usermode.rs`) | Single process, single address-space layout shared with the kernel's supervisor mappings. SMEP/SMAP off (the kernel could execute/read user memory if it had a bug). No multi-process isolation tests, no user stack-overflow test, no fuzzing of syscall arguments. |
| Capabilities | Bits checked before any syscall work; default none | Coarse (`Console`, `Net`). Fixed at process creation; no delegation, no object handles. |
| Input hardening | Shell accepts printable ASCII only, bounded 128 bytes; console writes from user are filtered to printable ASCII | Prevents screen-control injection. |
| The shell itself | Runs in ring 0 | Anything the shell can do, the kernel can do; a shell-level check is **not** a boundary. |
| Exception handling | ring 3 faults kill the process; ring 0 faults panic | Fail-stop, no recovery for kernel faults; #NMI/#MC not handled. |

Known weaknesses specific to the user-mode design: frames of killed processes are leaked (resource exhaustion by repeated `run`), the time budget depends on the assumed PIT rate, one shared kernel stack for all ring 3 -> ring 0 transitions (fine with one process and interrupts masked in gates, must be revisited with multiple processes), and processes abandoned handler frames when killed (relies on no locks being held at those points; verified only by tests, not proved).

## Principles for upcoming work
- Enforcement lives in the kernel at a boundary untrusted code cannot bypass (syscalls/IPC capability checks), never in the shell.
- DNS is a network operation and needs the same authorization as TCP.
- TLS and crypto only via vetted `no_std` libraries; certificate validation must work before any HTTPS feature is exposed; no HTTP fallback.
- Secrets: dedicated zeroizing type, minimal copies; documented as best-effort (registers, caches, swap-like copies cannot be guaranteed cleared).
- Persistent storage: separate, opt-in, authenticated encryption, explicit consent; design before code.
