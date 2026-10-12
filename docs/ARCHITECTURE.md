# Architecture

Status legend: **[impl]** implemented and exercised, **[partial]** exists but incomplete, **[planned]** not started, **[assumed]** believed but unverified.

## 1. Current state (assessed 2026-10-12)

Target: `x86_64-l_os.json` (bare metal x86_64, soft-float, no red zone, panic=abort). BIOS boot via the `bootloader` 0.9 crate and `bootimage`. Requires nightly Rust (`build-std`, `json-target-spec`, `abi_x86_interrupt`); pinned in `rust-toolchain.toml`. Emulator: QEMU `qemu-system-x86_64`. Stable Rust cannot build this repository.

| Area | State | Notes |
|---|---|---|
| Boot (BIOS, bootloader 0.9) | [impl] | Boots in QEMU. UEFI and Secure Boot: [planned], no chain of trust today. |
| VGA text output | [impl] | `src/vga_buffer.rs`; bottom-row writer with scroll, backspace, clear. Non-ASCII bytes render as `0xfe`. |
| Serial (COM1) | [impl] | Used only for test output. |
| GDT/TSS, IST for double fault | [impl] | Kernel code segment only. No user segments. |
| IDT: breakpoint, page fault, double fault | [partial] | Other CPU exceptions (GPF, invalid opcode, ...) have no handler and escalate to double fault. Page fault handler prints and halts. |
| PIC + PIT timer | [impl] | Tick counter in `src/time.rs`. PIT rate is **[assumed]** default (~18.2 Hz); observed ~18 Hz under QEMU only. |
| PS/2 keyboard | [impl] | ISR only enqueues raw scancodes (`src/input.rs`, lock-free SPSC ring, drops on overflow). Decoding (German `De105Key` layout) happens in the shell loop. USB keyboards: [planned]. |
| Shell | [impl] | `src/shell.rs`: bounded line editor (printable ASCII only), parser, commands `help version uptime security echo clear`. No heap used. |
| Paging / frame allocator / heap | [planned] | Only the bootloader's page tables exist; the kernel reads CR3 but does not manage memory. |
| User mode, processes, syscalls | [planned] | Everything runs in ring 0. |
| Capabilities / policy | [planned] | No authorization code exists yet. |
| Storage | [planned] | No storage driver; nothing can be written to disk (RAM-only by absence, not by enforcement). |
| Network | [planned] | No NIC driver, no stack. |

## 2. Testing layers (be precise about what each proves)

- **In-kernel tests** (`cargo test`): the test binary is a kernel image booted in QEMU; results are reported over serial and `isa-debug-exit`. This is the only test layer today. Unit tests for the shell parser, line editor, scancode queue, and tick math run here.
- **QEMU smoke test** (`scripts/qemu_shell_smoke.py`): boots the real `l_os` image, types through the emulated PS/2 keyboard via the QEMU monitor, dumps VGA memory and asserts on text. Exercises ISR → queue → decoder → editor → parser → VGA.
- **Host-side tests**: none yet. Portable logic (future policy engine, HTML parser, URL parsing) should move into `no_std` crates testable with `cargo test` on the host.

## 3. Kernel architecture decision (provisional)

Options considered:

| | Minimal monolithic | Microkernel |
|---|---|---|
| Trusted code | Larger: drivers/stack/FS in ring 0 | Smaller: scheduler, memory, IPC, capabilities |
| Isolation of faulty/hostile drivers | None | Strong (separate address spaces) |
| Performance | Best | IPC cost; fine for a terminal OS |
| Implementation cost | Lowest | High: needs user mode, address spaces, IPC *before* any service |
| Fit with capability model | Possible but easy to bypass in-kernel | Natural: capabilities are the IPC authority |

**Decision:** target a capability-based microkernel design, but build in stages so the project keeps working:

1. Kernel owns memory, interrupts, timer, and (temporarily) the console/keyboard.
2. Introduce user mode, address spaces, syscalls, and kernel-enforced capability handles.
3. Move services (network, filesystem, browser) out of the kernel as unprivileged processes holding only the capabilities they were given.

Until step 2 exists, **no isolation or authorization claim may be made**: any "policy" in ring 0 code is advisory, not a security boundary. This ordering is a judgment call and should be revisited if user-mode bring-up proves disproportionately costly.

## 4. Module map

```
src/main.rs        entry point (_start), panic handler, boots shell
src/lib.rs         init(), test harness, QEMU exit
src/gdt.rs         GDT/TSS
src/interrupts.rs  IDT, PIC, timer + keyboard ISRs (ISRs do minimal work)
src/input.rs       lock-free scancode queue
src/time.rs        tick counter
src/vga_buffer.rs  text console
src/serial.rs      COM1 (tests)
src/shell.rs       line editor, parser, commands
```
