# Security policy and posture

## Honest status
Larp OS (`l_os`) is an early research kernel. It has **tested process isolation for hand-assembled ring-3 programs, capability handles with IPC, no network, and no storage**. The shell and the demo programs run in ring 3; the kernel and all drivers run in ring 0. It must not be used to protect anything real. Compilation or passing tests does not imply security.

## Rules for contributors
- No telemetry, no hidden network access, no background checks. Network and storage features must be denied by default and enforced by the kernel, not by application-level flags.
- Every `unsafe` block needs a `// SAFETY:` comment stating its invariants. Keep `unsafe` to hardware/kernel boundaries.
- No `unwrap()`/`expect()` on untrusted runtime input without a stated invariant. 
  Inventory (`grep -rn "unwrap()\|expect(\|panic!(" src`, checked 2026-10-12, non-test code):
  - `src/serial.rs` UART init `expect`: boot-time only, no external input.
  - `src/lib.rs` `init_memory` (`expect` x2): boot-time; failure means no usable memory/heap.
  - `src/memory.rs` double-`init` `panic!`: programming-error guard.
  - `src/usermode.rs` / `src/syscall.rs` / `src/sched.rs` `panic!`/`expect` on ring-3 event/syscall with no user process: invariant, can only be hit by kernel code issuing `int 0x80`.
  - `src/interrupts.rs`: deliberate fail-stop `panic!` for ring-0 exceptions.
  - Output paths (`print!`, serial) no longer `unwrap`/`expect`.
  Nothing on this list takes user input as its panic condition.
- Do not write custom cryptography or TLS. Evaluate every dependency for `no_std`, allocation needs, `unsafe`, maintenance, and license before adding.
- Never disable certificate validation or add silent HTTPS→HTTP fallback.
- Do not claim guarantees (e.g. "RAM-only means no traces", "zeroized means gone") that the platform cannot provide.

## Known weaknesses (current)
- All kernel code and (future) drivers run in ring 0; any kernel bug owns the machine. The shell is in ring 3.
- The user-program build (`build.rs` -> nested `cargo`) downloads crates and uses the host toolchain; there is no reproducible-build or signature check on the produced ELF images.
- Processes: single CPU, SMEP/SMAP only if the CPU offers them, assumed timer rate, abandoned kernel stacks on kill (see THREAT_MODEL.md).
- Unhandled exceptions: #NMI, #MC, x87/SIMD, virtualization (they would double fault).
- Interrupt handlers lock the global PIC mutex; `print!` disables interrupts around the VGA lock. Terminating a process abandons the interrupt frame it was in; no locks may be held at those points (not formally verified).
- Heap allocation from interrupt context would deadlock; there is none today, nothing enforces it.
- Keyboard scancode queue drops input on overflow (by design, counted).
- `NetPolicy` has no persistence (grants vanish at reboot, by design) and holds 16 entries. Grants are all-or-nothing per host:port; there is no per-program scoping yet.
- The network stack (smoltcp) and the virtio-net driver parse untrusted packets in ring 0. The emulated NIC can DMA anywhere (no IOMMU).
- TLS validity relies on the unauthenticated RTC; no certificate revocation checking.
- Dependencies of the user programs (rustls, RustCrypto crates, webpki-roots, linked_list_allocator) are fetched at build time and locked in `user/Cargo.lock`; the build is not reproducible and no advisory scan is part of CI yet. `rsa` 0.9 carries RUSTSEC-2023-0071 (private-key timing); only public-key verification is used.
- Unverified: behavior on real hardware, on UEFI, or with USB keyboards.

## Reporting
No disclosure process exists yet. Open a private issue/contact the maintainer.
