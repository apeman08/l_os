# Architecture

Status legend: **[impl]** implemented and exercised, **[partial]** exists but incomplete, **[planned]** not started, **[assumed]** believed but unverified.

## 1. Current state (assessed 2026-10-12)

Target: `x86_64-l_os.json` (bare metal x86_64, soft-float, no red zone, panic=abort). BIOS boot via the `bootloader` 0.9 crate and `bootimage`. Requires nightly Rust (`build-std`, `json-target-spec`, `abi_x86_interrupt`); pinned in `rust-toolchain.toml`. Emulator: QEMU `qemu-system-x86_64`. Stable Rust cannot build this repository.

| Area | State | Notes |
|---|---|---|
| Boot (BIOS, bootloader 0.9, `map_physical_memory`) | [impl] | Boots in QEMU. UEFI and Secure Boot: [planned], no chain of trust today. |
| VGA text output | [impl] | `src/vga_buffer.rs`; bottom-row writer with scroll, backspace, clear. Non-ASCII bytes render as `0xfe`. |
| Serial (COM1) | [impl] | Used only for test output. |
| GDT/TSS | [impl] | Kernel code/data, user code/data (ring 3), mutable TSS: IST stack for double fault and a per-process ring-0 stack set on every context switch. |
| IDT / exceptions | [impl] | #DE #UD #NMI #OF #BR #NM #TS #NP #SS #GP #PF #AC #MF/#XM #MC #DF #BP handled. From ring 3 they end only the user process; from ring 0 they panic (fail-stop). Panic output never blocks on a held console lock. Not handled: virtualization/CET/VMM exceptions. |
| PIC + PIT timer | [impl] | Tick counter in `src/time.rs`. PIT rate is **[assumed]** default (~18.2 Hz); observed ~18 Hz under QEMU only. |
| CPU hardening | [impl, conditional] | SMEP and SMAP enabled when CPUID reports them (QEMU needs `-cpu max`; the default `qemu64` model has neither). `security` shows the actual state. |
| PS/2 keyboard | [impl] | ISR only enqueues raw scancodes (`src/input.rs`); decoding (German `De105Key`) happens in the shell loop. USB keyboards: [planned]. |
| Shell | [impl] | `help version uptime security echo clear run`. Runs in ring 0 (moving it to ring 3 needs the user-space toolchain). No heap use. |
| Physical frames, paging, heap | [impl] | Frame allocator with free list (frames are zeroed on alloc and free; bitmap detects double free and panics; only the first 4 GiB of RAM is tracked). `OffsetPageTable`, 1 MiB heap (`linked_list_allocator`). No allocation in ISRs. |
| Processes (ring 3) | [impl, limited] | Up to 8 processes, each with its own address space (private user L4 slot, kernel mappings shared as supervisor-only), own kernel stack, preemptive round-robin on the PIT tick, CPU-tick budgets, W^X code page and NX stack, memory fully reclaimed on exit (tested over 120 runs). Programs are raw machine code (no loader); no dynamic spawn from user space; no user-space Rust toolchain yet. |
| Syscalls | [impl, limited] | `int 0x80`: `exit write net_check yield close send recv`. Every user pointer is validated against the calling process's page tables; copies run in a SMAP window. |
| Capabilities | [impl, limited] | Per-process handle table (generation-checked, unforgeable integers), objects `Console`, `NetClient`, `Endpoint`; rights `WRITE USE SEND RECV GRANT`. Delegation only over endpoints, only with `GRANT`, always `rights AND mask`. No revocation of already-delegated handles; no object types for devices/files yet. |
| Net policy | [partial] | `l_core::policy::NetPolicy`: default deny, exact host:port, expiry, revoke. Consulted by `SYS_NET_CHECK` (needs a `NetClient` handle). There is **no network stack**, so it gates nothing real, and no user-facing way to grant at runtime (deliberately). |
| Storage | [planned] | No storage driver; nothing can be written to disk (RAM-only by absence, not by enforcement). |
| Network | [planned] | No NIC driver, no stack. |

## 2. Testing layers (be precise about what each proves)

- **In-kernel tests** (`cargo test`): the test binary is a kernel image booted in QEMU; results are reported over serial and `isa-debug-exit`. This is the only test layer today. Unit tests for the shell parser, line editor, scancode queue, and tick math run here.
- **QEMU smoke test** (`scripts/qemu_shell_smoke.py`): boots the real `l_os` image, types through the emulated PS/2 keyboard via the QEMU monitor, dumps VGA memory and asserts on text. Exercises ISR → queue → decoder → editor → parser → VGA.
- **Host-side tests** (`scripts/host_tests.sh`): the `l_core` crate (`#![forbid(unsafe_code)]`: shell parser/editor, frame cursor, `NetPolicy`, capabilities, syscall ABI helpers). These run on the host, not in the kernel, so they prove the logic only, not that the kernel uses it correctly.
- **Kernel integration tests** (`tests/*.rs`, ~65 results): exceptions through the real IDT, heap/paging/frame reuse and double free, SMEP/SMAP violations (each with a passing control case), and ring-3 suites: `usermode` (kernel-memory isolation, privileged instructions, pointer validation, policy outcomes), `processes` (separate address spaces, preemption, crash containment, no frame leaks, table-driven pointer probe) and `capabilities` (forged/closed handles, delegation, attenuation, deadlock cleanup). Selected tests were checked by mutation (breaking the protection makes the test fail).

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

1. Kernel owns memory, interrupts, timer, and (temporarily) the console/keyboard. **Done.**
2. Introduce user mode, address spaces, syscalls, and kernel-enforced capability handles. **Done** for hand-assembled programs: separate address spaces, handles, IPC.
3. Move services (network, filesystem, browser, shell) out of the kernel as unprivileged processes holding only the capabilities they were given. **Not started**; blocked on a user-space build pipeline (Rust programs for ring 3) and dynamic spawn.

Until step 2 is complete, isolation claims are limited to what `tests/usermode.rs` demonstrates. The shell and every service still run in ring 0, so a "policy" consulted only from ring-0 code remains advisory; only checks on the syscall path are a boundary. This ordering is a judgment call and should be revisited if user-mode bring-up proves disproportionately costly.

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
