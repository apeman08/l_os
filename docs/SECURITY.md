# Security policy and posture

## Honest status
Larp OS (`l_os`) is an early research kernel. It has **limited process isolation (one ring-3 process at a time), a coarse capability check, no network, and no storage**. It must not be used to protect anything real. Compilation or passing tests does not imply security.

## Rules for contributors
- No telemetry, no hidden network access, no background checks. Network and storage features must be denied by default and enforced by the kernel, not by application-level flags.
- Every `unsafe` block needs a `// SAFETY:` comment stating its invariants. Keep `unsafe` to hardware/kernel boundaries.
- No `unwrap()`/`expect()` on untrusted runtime input without a stated invariant. (Existing code still has some on initialization paths, e.g. UART init, VGA `write_fmt`, `init_memory`; not reachable from untrusted input.)
- Do not write custom cryptography or TLS. Evaluate every dependency for `no_std`, allocation needs, `unsafe`, maintenance, and license before adding.
- Never disable certificate validation or add silent HTTPS→HTTP fallback.
- Do not claim guarantees (e.g. "RAM-only means no traces", "zeroized means gone") that the platform cannot provide.

## Known weaknesses (current)
- The shell, input handling and all kernel code run in ring 0; any kernel bug owns the machine.
- User mode: one process at a time, no SMEP/SMAP, leaked frames, assumed timer rate (see THREAT_MODEL.md).
- Unhandled exceptions: #NMI, #MC, x87/SIMD, virtualization (they would double fault).
- Interrupt handlers lock the global PIC mutex; `print!` disables interrupts around the VGA lock. Terminating a process abandons the interrupt frame it was in; no locks may be held at those points (not formally verified).
- Heap allocation from interrupt context would deadlock; there is none today, nothing enforces it.
- Keyboard scancode queue drops input on overflow (by design, counted).
- `NetPolicy` has no persistence and no runtime interface for a human to grant; `SYS_NET_CHECK` only answers a query.
- Unverified: behavior on real hardware, on UEFI, or with USB keyboards.

## Reporting
No disclosure process exists yet. Open a private issue/contact the maintainer.
