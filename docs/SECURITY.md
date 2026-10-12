# Security policy and posture

## Honest status
Larp OS (`l_os`) is an early research kernel. It has **no process isolation, no user mode, no authorization system, no network, and no storage**. It must not be used to protect anything real. Compilation or passing tests does not imply security.

## Rules for contributors
- No telemetry, no hidden network access, no background checks. Network and storage features must be denied by default and enforced by the kernel, not by application-level flags.
- Every `unsafe` block needs a `// SAFETY:` comment stating its invariants. Keep `unsafe` to hardware/kernel boundaries.
- No `unwrap()`/`expect()` on untrusted runtime input without a stated invariant. (Existing code still has some on initialization paths, e.g. UART init, VGA `write_fmt`; tracked in the roadmap.)
- Do not write custom cryptography or TLS. Evaluate every dependency for `no_std`, allocation needs, `unsafe`, maintenance, and license before adding.
- Never disable certificate validation or add silent HTTPS→HTTP fallback.
- Do not claim guarantees (e.g. "RAM-only means no traces", "zeroized means gone") that the platform cannot provide.

## Known weaknesses (current)
- Single privilege level; any kernel bug or malicious code owns the machine.
- Missing exception handlers (GPF, invalid opcode, etc.) → double fault.
- Page-fault handler prints and halts; no recovery or kill-process logic (no processes exist).
- Interrupt handlers lock the global PIC mutex; `print!` disables interrupts around the VGA lock to avoid deadlock, but other lock/ISR interactions are not audited.
- Keyboard scancode queue drops input on overflow (by design, counted).
- Unverified: behavior on real hardware, on UEFI, or with USB keyboards.

## Reporting
No disclosure process exists yet. Open a private issue/contact the maintainer.
