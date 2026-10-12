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
| Network denied by default | **Enforced**: `NetPolicy` is checked in `net::connect` before DNS, before the NIC is initialized, and on every send/receive; revoke/expiry reset open connections. Default is no grants. | Verified in QEMU: 10 refusal tests and an empty QEMU packet capture for the never-authorized run; functional run produces traffic (so the capture works). The kernel network stack and driver are in the TCB (ring 0), DMA is not isolated (no IOMMU). Only the emulated virtio-net is supported. |
| RAM-only | No storage driver exists | Holds by absence. No claim about forensic traces: RAM contents, firmware, and emulator disk images are outside our control. |
| Telemetry | None in code | Source-auditable; the project is small. |
| Memory safety | Rust; `unsafe` limited to hardware access, page-table edits, asm entry/exit, user-pointer copies, heap init | Kernel bugs in `unsafe` or logic remain possible. `l_core` forbids `unsafe`. |
| User/kernel and process/process isolation | **Demonstrated** for up to 8 hand-assembled ring-3 processes: separate address spaces (same virtual address, different contents), no access to kernel pages, no privileged instructions, kernel pointers rejected in syscalls, runaway processes preempted and killed, crashes contained, memory reclaimed. SMEP/SMAP enabled when the CPU offers them. | Single CPU only. SMEP/SMAP absent on QEMU's default CPU model. Kernel mappings are shared by all processes (copied L4 entries; new kernel L4 entries created after a process exists are not visible to it). No guard pages under kernel stacks. No fuzzing; side channels (Spectre/Meltdown class) not addressed. |
| Capabilities | Per-process handle tables; syscalls resolve a handle first (kind and right checked); delegation needs `GRANT` and can only narrow rights; stale/forged/closed handles rejected | No revocation of delegated handles; no capability for devices/memory yet; endpoint queues are unauthenticated beyond the handle; a malicious holder of `GRANT` can pass on what it has (by design, attenuated). |
| Input hardening | Shell accepts printable ASCII only, bounded 128 bytes; console writes from user are filtered to printable ASCII | Prevents screen-control injection. |
| Console output on COM1 | With the cargo feature `serial-console` (on by default, needed by the tests) every console write, including everything typed and shown, is also written to the serial port COM1; panic messages always are | Anyone who can read COM1 (the host, a serial cable, a VM log file) can record the console. Build with `--no-default-features` to keep normal console output off COM1. |
| The shell | Runs in **ring 3**, holds console (write+grant), keyboard, system-info (read) and a spawner limited to four built-in programs; a crash of the shell is contained and it is restarted | The shell can start the demo programs and pass them its console; it cannot touch memory, devices or other processes. Typed input is still restricted to printable ASCII; console writes from any holder of a console handle may use backspace/clear (full control of the text screen). |
| Exception handling | ring 3 faults kill the process; ring 0 faults panic | Fail-stop, no recovery for kernel faults; #NMI/#MC not handled. |

Known weaknesses specific to the process model: budgets depend on the assumed PIT rate; a process killed inside a syscall or ISR is abandoned together with its kernel stack, which is sound only if no lock is held at those points (tested, not proved); a process can fill its own handle table or endpoint queues but not others'; there are at most 8 endpoints per run; all drivers still run in ring 0.

Loader-specific: ELF images are embedded in the kernel and validated by `l_core::elf` (window, alignment, overlap, W^X, entry point, file bounds). They are trusted code from our own build, but the parser treats them as untrusted; `build.rs` runs a nested cargo build with a scrubbed environment, so the build host's toolchain and network (crates.io, locked by `user/Cargo.lock`) are part of the trusted computing base for the user programs.

Firmware and the NIC: the "nothing is sent" check in QEMU (`scripts/net_tests.sh`) depends on the boot disk having `bootindex=0`, so that OVMF connects only the disk and leaves the NIC alone; without it, OVMF's own network stack sent IPv6 frames (MLD report, neighbour solicitation carrying the MAC address) before the kernel started. Real UEFI firmware may likewise send frames (e.g. MLD/NS with the MAC, PXE/DHCP) before the kernel runs; the kernel cannot prevent that.

Network and TLS specifics: a connection needs a `NetClient` handle and an exact `host:port` grant (names also need the DNS resolver granted); only the shell can create grants, after a typed confirmation. TLS is TLS 1.3 only, client only, no fallback; certificate validity depends on an unauthenticated RTC; no revocation checking; the production trust store is the embedded Mozilla root list. The user-space TLS and HTTP code parses attacker-controlled data in ring 3 (a crash kills only `fetch`). A compromised shell is a compromised trusted path: it can grant anything.

## Principles for upcoming work
- Enforcement lives in the kernel at a boundary untrusted code cannot bypass (syscalls/IPC capability checks), never in the shell.
- DNS is a network operation and needs the same authorization as TCP.
- TLS and crypto only via vetted `no_std` libraries; certificate validation must work before any HTTPS feature is exposed; no HTTP fallback.
- Secrets: dedicated zeroizing type, minimal copies; documented as best-effort (registers, caches, swap-like copies cannot be guaranteed cleared).
- Persistent storage: separate, opt-in, authenticated encryption, explicit consent; design before code.
