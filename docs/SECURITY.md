# Security policy and posture

## Honest status
Larp OS (`l_os`) is an early research kernel. It has **tested process isolation for ring-3 programs, capability handles with IPC, a default-deny network path (QEMU virtio-net only), and an amnesic wipe tested only under QEMU**. The shell and the demo programs run in ring 3; the kernel and all drivers run in ring 0. It must not be used to protect anything real. Compilation or passing tests does not imply security.

M2 (amnesic live system, `docs/ROADMAP.md`) added, honestly stated:
- A **USB (xHCI) driver in ring 0** that parses data from devices (descriptors, HID boot reports, SCSI replies). A malicious USB device plugged in at boot talks directly to kernel code; the parsers are host-tested with hostile inputs (`usb_desc::fuzz_never_panics`, `usb_hid::fuzz_any_report_never_panics_and_stream_is_well_formed`, `usb_msc::csw_*_rejected`, `xhci_ring::decode_never_panics_on_arbitrary_words`), the driver itself is not fuzzed.
- **Mass storage read-only and sealed**: the code has no write command; at boot it sends only TEST UNIT READY, READ CAPACITY(10) and READ(10) of LBA 0 to find the boot stick, then seals the path in `usb.rs` and in the xHCI driver's bulk path (`tests/usb_storage.rs::reads_refused_after_seal`, which also calls `xhci::bulk` directly on the stick's endpoints, and `z_restart_after_seal_skips_storage`). The stick is identified by a boot marker, not authenticated (no verified boot, M6).
- A **wipe** on stick removal, emergency hotkey or controller loss, accepted by QEMU memory dump (`scripts/qemu_wipe_test.py`); limits and what is not zeroed in `docs/THREAT_MODEL.md` ("Amnesic wipe").

## Rules for contributors
- No telemetry, no hidden network access, no background checks. Network and storage features must be denied by default and enforced by the kernel, not by application-level flags.
- Every `unsafe` block needs a `// SAFETY:` comment stating its invariants. Keep `unsafe` to hardware/kernel boundaries.
- No `unwrap()`/`expect()` on untrusted runtime input without a stated invariant. 
  Inventory (`grep -rn "unwrap()\|expect(\|panic!(" src`, checked 2026-10-15: 35 hits, 33 in non-test code; `assert!` is not counted):
  - `src/serial.rs` UART init `expect`: boot-time only, no external input.
  - `src/lib.rs` `init_memory` (`expect` x3): boot-time; failure means no physical-memory mapping, no memory or no heap.
  - `src/mmio.rs` `init` `expect` (frame for the MMIO window's L3 table): boot-time, called once.
  - `src/memory.rs`: double-free and double-`init` `panic!`: programming-error guards. `destroy_address_space` `expect` x4 (no huge pages, plain table entries in user space): invariant of the kernel's own user mappings.
  - `src/sched.rs` `expect` x4 (live slot, new process Ready, slot checked) and `panic!` on a ring-3 event with no user process; `src/syscall.rs` `panic!` on a syscall with no user process; `src/usermode.rs` `expect` on the build-embedded shell image: invariants, not reachable from user input.
  - `src/aurora.rs` `expect` on `HintBar::new` of a constant 3-hint array.
  - `src/wipe.rs` `try_into().unwrap()` x2 on fixed 8- and 2-byte slices of the kernel ELF header copy (infallible conversions; the length is checked before).
  - `src/interrupts.rs` (12 hits): deliberate fail-stop `panic!` for ring-0 exceptions and double fault.
  - Test-only (`#[cfg(test)]` in `src/wipe.rs`): 2 hits, `expect` on the virtio-net device of the test VM and `unwrap` on a report line.
  - Output paths (`print!`, serial) do not `unwrap`/`expect`.
  The xHCI/USB code (`src/xhci.rs`, `src/usb.rs`) has no hit: device data errors are returned as `Result`. Nothing on this list takes user or device input as its panic condition.
- Do not write custom cryptography or TLS. Evaluate every dependency for `no_std`, allocation needs, `unsafe`, maintenance, and license before adding.
- Never disable certificate validation or add silent HTTPS→HTTP fallback.
- Do not claim guarantees (e.g. "RAM-only means no traces", "zeroized means gone") that the platform cannot provide.

## Known weaknesses (current)
- All kernel code and drivers run in ring 0; any kernel bug owns the machine. The shell is in ring 3.
- The user-program build (`build.rs` -> nested `cargo`) downloads crates and uses the host toolchain; there is no reproducible-build or signature check on the produced ELF images.
- Processes: single CPU, SMEP/SMAP only if the CPU offers them, assumed timer rate, abandoned kernel stacks on kill (see THREAT_MODEL.md).
- Unhandled exceptions: #NMI, #MC, x87/SIMD, virtualization (they would double fault).
- Interrupt handlers lock the global PIC mutex; `print!` disables interrupts around the console lock. Terminating a process abandons the interrupt frame it was in; no locks may be held at those points (not formally verified).
- Heap allocation from interrupt context would deadlock; there is none today, nothing enforces it.
- Keyboard scancode queue drops input on overflow (by design, counted).
- `NetPolicy` has no persistence (grants vanish at reboot, by design) and holds 16 entries. Grants are all-or-nothing per host:port; there is no per-program scoping yet.
- The network stack (smoltcp) and the virtio-net driver parse untrusted packets in ring 0. The emulated NIC can DMA anywhere (no IOMMU).
- TLS validity relies on the unauthenticated RTC; no certificate revocation checking.
- Dependencies of the user programs (rustls, RustCrypto crates, webpki-roots, linked_list_allocator) are fetched at build time and locked in `user/Cargo.lock`; the build is not reproducible and no advisory scan is part of CI yet. `rsa` 0.9 carries RUSTSEC-2023-0071 (private-key timing); only public-key verification is used.
- The USB stack (xHCI driver, enumeration, HID and mass-storage parsing) runs in ring 0 and trusts the controller's DMA; there is no IOMMU.
- No hotplug enumeration and no hubs: devices plugged in after boot are counted and ignored, the keyboard must be present at boot, devices behind a hub are not seen.
- Polling latency: the stick watch runs once per timer tick (100 Hz, no xHCI interrupts); the detection delay is by design at most one tick plus processing, not measured on its own.
- The wipe zeroes RAM, but CPU registers, caches (beyond `wbinvd`), device buffers and firmware memory are best effort or not touched; a panic during the wipe powers off with RAM partly unzeroed; without a wipe plan RAM is not zeroed at all (`docs/THREAT_MODEL.md`, "Amnesic wipe").
- A USB stall on the keyboard endpoint leaves the USB keyboard dead until reboot (no Reset Endpoint); no Disable Slot.
- `identify_boot_stick` has no overall deadline: a hostile USB device can delay boot by about 48 s (6.4 min with 8 devices); to fix before M5.
- "amnesisch: ja" does not take the armed stick watch into account (`l_core::wipe::Amnesia` has no `armed` input); today arming follows the seal directly and does not fail under QEMU.
- Unverified: behavior on real hardware (UEFI boot, USB chipsets, real sticks and keyboards, ACPI); everything above was run under QEMU only.

## Reporting
No disclosure process exists yet. Open a private issue/contact the maintainer.
