# Architecture

Status legend: **[impl]** implemented and exercised, **[partial]** exists but incomplete, **[planned]** not started, **[assumed]** believed but unverified.

## 1. Current state (assessed 2026-10-12)

Target: `x86_64-l_os.json` (bare metal x86_64, soft-float, no red zone, panic=abort). UEFI boot via the `bootloader` 0.11 crate (kernel side: `bootloader_api`; image builder: host crate `tools/image`, cargo runner `scripts/qemu_runner.sh`, firmware OVMF in QEMU). The kernel is a static position-independent executable; the bootloader picks its base address. Requires nightly Rust (`build-std`, `json-target-spec`, `abi_x86_interrupt`); pinned in `rust-toolchain.toml`. Emulator: QEMU `qemu-system-x86_64`. Stable Rust cannot build this repository.

| Area | State | Notes |
|---|---|---|
| Boot (UEFI via OVMF, bootloader 0.11) | [impl] | Boots in QEMU. `BOOTLOADER_CONFIG` (`src/lib.rs`): all physical memory at fixed `0x1000_0000_0000` (L4 slot 32), dynamic bootloader mappings below L4 slot 128 (heap: 136, user: 224), 2 MiB kernel stack. Framebuffer taken over (`src/framebuffer.rs`) and drawn on by the console. Secure Boot: [planned], no chain of trust today. |
| Framebuffer console | [impl] | `src/fb_console.rs` (replaces the VGA text console). An `l_core::cell::Grid` of 8x16 cells (128x48 at QEMU's 1024x768) drawn by `l_core::render` in `AURORA` colours; geometry, stride and Rgb/Bgr come from the firmware. Cursor starts top left; newline, wrap, scroll, backspace, clear (form feed from `write`); control characters show as the replacement glyph. After each print call only changed rows are rendered; a scroll moves the drawn pixels up with one copy per print call. Measured (`tests/framebuffer.rs`, QEMU TCG, debug kernel, `l_core` at `opt-level = 3`): 2000 `println!` lines in 78-89 ticks at 18.2 Hz (~4-5 s) and 450-582 ticks at 100 Hz (~4.5-5.8 s), 3953 text rows rendered; with unoptimized `l_core` the same took 1267 ticks at 18.2 Hz (~70 s). Panic output never blocks: with the console lock held it is painted straight onto the bottom rows (with the chrome on, over the hint bar). Checked pixel-exactly against host rendering in `tests/framebuffer.rs` and the smoke test. **Aurora chrome** (opt-in, `fb_console::enable_chrome`, turned on by the kernel entry through `src/aurora.rs`; test kernels without the call keep every row as text): status bar on the first row ("Larp OS" left, "amnesisch: nein" right) and hint bar on the last row, drawn with `l_core::widgets::{status_bar, HintBar}` in `AURORA`; the text region in between holds all console output. `tests/aurora.rs` checks: cursor starts at the region's first row; 200 lines, clear (form feed) and backspace at column 0 never change the bar cells or pixels; the bars fade in from the background colour (`l_core::anim::Fade`, 400 ms, `hlt` between timer ticks, each frame re-renders only the two bar rows: measured 34 frames in 36 ticks) and end exactly in the final AURORA colours; with `REDUCE_MOTION` (const, default false) the final frame is drawn at once. Hints are limited to keys the shell really has (`Enter` ausführen, `Backspace` löschen, `help` Befehle; at most three, enforced by `HintBar::new`); no Tab completion or command palette exists, so none is shown. The smoke test reads both bars back from the screendump and compares every row, bars included, pixel-exactly with the host rendering. Real hardware: [assumed] to work; scrolling reads framebuffer memory back, which can be slow on real GPUs. |
| Serial (COM1) | [impl] | Test output; panic messages; and, with the cargo feature `serial-console` (on by default), a mirror of every console write. |
| GDT/TSS | [impl] | Kernel code/data, user code/data (ring 3), mutable TSS: IST stack for double fault and a per-process ring-0 stack set on every context switch. |
| IDT / exceptions | [impl] | #DE #UD #NMI #OF #BR #NM #TS #NP #SS #GP #PF #AC #MF/#XM #MC #DF #BP handled. From ring 3 they end only the user process; from ring 0 they panic (fail-stop). Panic output never blocks on a held console lock. Not handled: virtualization/CET/VMM exceptions. |
| PIC + PIT timer | [impl] | Tick counter in `src/time.rs`; `time::init_pit` programs PIT channel 0 for `l_core::time::TICK_HZ` = 100 Hz (mode 2, divisor 11932, nominally 99.9985 Hz). Measured by `tests/timer.rs` against the RTC under QEMU (UEFI/OVMF, TCG): 499-500 ticks in 5 RTC seconds (~100 Hz); the test fails outside 100 Hz +-10 %. Real hardware: not measured. Durations are written with `l_core::time` helpers (`ticks_from_seconds`, `ticks_from_ms`, `ticks_to_ms`). |
| CPU hardening | [impl, conditional] | SMEP and SMAP enabled when CPUID reports them (QEMU needs `-cpu max`; the default `qemu64` model has neither). `security` shows the actual state. |
| PS/2 keyboard | [impl] | ISR only enqueues raw scancodes (`src/input.rs`); decoding (German `De105Key`) happens in the shell loop. USB keyboards: [planned]. |
| Shell | [impl] | A Rust program running in **ring 3** (`user/src/bin/shell.rs`), supervised and restarted by the kernel. It reuses the host-tested parser/editor from `l_core`, reads raw scancodes through a `Keyboard` handle (layout decoding with `pc-keyboard` happens in user space), prints through a `Console` handle and starts programs through a `Spawner` handle. Commands: `help version uptime security echo clear run`. |
| Physical frames, paging, heap | [impl] | Frame allocator with free list (frames are zeroed on alloc and free; bitmap detects double free and panics; only the first 4 GiB of RAM is tracked). `OffsetPageTable`, 1 MiB heap (`linked_list_allocator`). No allocation in ISRs. |
| Processes (ring 3) | [impl, limited] | Up to 8 processes, each with its own address space (private user L4 slot, kernel mappings shared as supervisor-only), own 64 KiB kernel stack, preemptive round-robin on the PIT tick, CPU-tick budgets, 32 KiB stack with an unmapped guard page, memory fully reclaimed on exit. Programs: validated static ELF executables built from `user/` (W^X enforced per segment) or raw machine code for the hand-assembled tests. `spawn`/`wait` from user space; idle in `hlt` while everything waits for the keyboard. |
| Syscalls | [impl, limited] | `int 0x80`: `exit write net_check yield close send recv spawn wait read_key sysinfo random net_connect sock_send sock_recv netctl`. Every user pointer is validated against the calling process's page tables; copies run in a SMAP window. |
| Capabilities | [impl, limited] | Per-process handle table (generation-checked, unforgeable integers). Objects: `Console`, `Keyboard`, `System`, `Spawner(mask)`, `NetClient`, `NetManager`, `Socket`, `Random`, `Endpoint`; rights `WRITE USE SEND RECV GRANT READ`. Delegation over endpoints and via `spawn` grants only, only with `GRANT`, always `rights AND mask`. No revocation of already-delegated handles; no object types for devices/files yet. |
| Net policy | [partial] | `l_core::policy::NetPolicy`: default deny, exact host:port, expiry, revoke. Consulted by `SYS_NET_CHECK` (needs a `NetClient` handle). There is **no network stack**, so it gates nothing real, and no user-facing way to grant at runtime (deliberately). |
| Storage | [planned] | No storage driver; nothing can be written to disk (RAM-only by absence, not by enforcement). |
| Network | [impl, limited] | virtio-net (QEMU only) + smoltcp in the kernel, DNS client, TCP sockets as handles, all behind `NetPolicy`; NIC off until the first authorized connection. See docs/NETWORK.md. |
| TLS / `fetch` | [impl, limited] | `crates/l_tls` (rustls 0.23.45, TLS 1.3 only, client only) in ring 3; `run fetch https://...`. Success path verified with a test CA; not shown against a public site (sandbox intercepts TLS). |
| RNG / clock | [impl] | RDSEED/RDRAND without fallback; unauthenticated CMOS RTC with a plausibility window. |

## 2. Testing layers (be precise about what each proves)

- **In-kernel tests** (`cargo test`): the test binary is a kernel image booted in QEMU; results are reported over serial and `isa-debug-exit`. This is the only test layer today. Unit tests for the shell parser, line editor, scancode queue, and tick math run here.
- **QEMU smoke test** (`scripts/qemu_shell_smoke.py`): boots the real `l_os` image, types through the emulated PS/2 keyboard via the QEMU monitor, reads the console from the serial mirror and asserts on text, and compares a QEMU `screendump` of the framebuffer console pixel-exactly with the same lines rendered on the host by `l_core::render` (`crates/l_core/examples/render_text.rs`, which also draws the status and hint bars with the `l_core` widgets); check `shell_screen_has_status_and_at_most_three_hints` reads the first and last screen rows back as text by matching each cell against the font. A second screendump checks a screen that output has overflowed: the prompt is on the last text row above the hint bar. Exercises ISR → queue → decoder → editor → parser → console → framebuffer.
- **Host-side tests** (`scripts/host_tests.sh`): the `l_core` crate (`#![forbid(unsafe_code)]`: shell parser/editor, frame cursor, `NetPolicy`, capabilities, syscall ABI helpers). These run on the host, not in the kernel, so they prove the logic only, not that the kernel uses it correctly.
- **Kernel integration tests** (`tests/*.rs`, ~70 results): exceptions through the real IDT, heap/paging/frame reuse and double free, SMEP/SMAP violations (each with a passing control case), and ring-3 suites: `usermode` (kernel-memory isolation, privileged instructions, pointer validation, policy outcomes), `processes` (separate address spaces, preemption, crash containment, no frame leaks, table-driven pointer probe) `capabilities` (forged/closed handles, delegation, attenuation, deadlock cleanup) and `userland` (Rust ELF programs: W^X of the loaded image, corrupt-image rejection, spawn/wait limits, keyboard handle). Selected tests were checked by mutation (breaking the protection makes the test fail).

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
3. Move services (shell, network, filesystem, browser) out of the kernel as unprivileged processes holding only the capabilities they were given. **Started**: the shell now runs in ring 3 with a user-space build pipeline and dynamic spawn; network, storage and the console/keyboard "services" are still kernel objects, not processes.

Until step 2 is complete, isolation claims are limited to what `tests/usermode.rs` demonstrates. The shell and every service still run in ring 0, so a "policy" consulted only from ring-0 code remains advisory; only checks on the syscall path are a boundary. This ordering is a judgment call and should be revisited if user-mode bring-up proves disproportionately costly.

## 4. Module map

```
src/main.rs        entry point (_start), panic handler, boots shell
src/lib.rs         init(), test harness, QEMU exit
src/gdt.rs         GDT/TSS
src/interrupts.rs  IDT, PIC, timer + keyboard ISRs (ISRs do minimal work)
src/input.rs       lock-free scancode queue
src/time.rs        PIT setup (100 Hz) and tick counter
src/fb_console.rs  text console on the framebuffer (l_core grid + renderer)
src/framebuffer.rs framebuffer description from the bootloader
src/serial.rs      COM1 (tests, panics, console mirror)
src/shell.rs       line editor, parser, commands
```

## 5. Font

`crates/l_core/src/font.rs` embeds an 8x16 bitmap font for the framebuffer terminal. The glyphs come from Terminus Font 4.49.1 (`ter-u16n.bdf`, https://terminus-font.sourceforge.net/), copyright (C) 2020 Dimitar Toshkov Zhekov, licensed under the SIL Open Font License 1.1. The licence text is in `crates/l_core/LICENSE-font`. The glyph table `crates/l_core/src/font_data.rs` is generated from the BDF by `scripts/gen_font.py` and must not be edited by hand. It covers ASCII, Latin-1, box drawing, block elements and a few symbols, sorted by codepoint; any other codepoint renders as a hollow-box replacement glyph. Terminus lacks the dashed box-drawing variants and a few others (U+2504-250B, U+254C-254F, U+2594-2595); these are not covered and fall back to the replacement glyph. The replacement glyph itself is hand-drawn (not from Terminus).
