# SDD ledger — plan: docs/superpowers/plans/2026-10-13-m2-amnesisch.md
Note: qemu+ovmf installed via apt in session; kernel tests runnable.
Task 1: complete (commits 69f37ca..2d4e8e5, docs-only spike; no code review needed). Findings: no IDE fallback needed; Task 11 must use no usb-kbd; BAR0 64-bit @0xC000000000 use own uncached window, 32-bit cap reads; FADT rev1 116B check length; _S5=0 PM1a_CNT=0xB004 write 0x2000; dump eval over Usable p_paddr only. See notes/2026-10-13-usb-qemu-spike.md
Task 2: fix round 1/5 (4 addressed, 0 open; commits 6b2e5b2..bb4e05d)
Task 2: minor (deferred): smoke helpers glyph_masks/build_render_helper run twice; tests/network.rs:97 tx counter flake seen once (1471 vs 1470), not reproduced in 4 reruns.
Task 2: complete (commits 2d4e8e5..bb4e05d, review clean)
Task 3: minor (deferred): no test for advance past Halted / full 14-stage walk / trigger after advance; call sites must set serial_mirror=true when test-hooks on (spec E7).
Task 3: complete (commits bb4e05d..255d1eb, review clean)
Task 4: fix round 1/5 (2 addressed, 0 open; commits b679d2f..a6dfdfe)
Task 4: minor (deferred): coalesce_pages debug_assert alignment — Task 9 caller must pass page-aligned addrs; zeroize black_box best-effort, kernel wipe (Task 10) must use volatile writes; push copies array, unallocated_runs O(frames).
Task 4: complete (commits 255d1eb..a6dfdfe, review clean)
Task 5: minor (deferred): total_us==kernel_us (time to KernelZeroed, lower bound; excludes island wipe/poweroff) — Task 10/25 docs must state what it covers. tsc_hz(tick_hz=0)=Some(0): boot code must treat 0 as uncalibrated. Task 11 must require total_us>0.
Task 5: complete (commits a6dfdfe..0cbac08, review clean)
Task 6: minor (deferred): s5_at pkg_len only lower-bounded; find_s5 doesn't require NameOp; strict X_PM1b failure; Mem::read test helper unchecked add.
Task 6: complete (commits 0cbac08..67853b1, review clean)
Task 7: minor (deferred): HotkeyDetector::new/from_bits/to_bits not const fn (Task 10 may need it - make const then); lost break code leaves modifier stuck (document); feed lacks #[must_use].
Task 7: complete (commits 67853b1..ea9045e, review clean)
Task 8: minor (deferred): src/acpi.rs:117 vec![0;len] aborts on OOM (use try_reserve_exact); PHYS_LIMIT assumes bootloader maps holes below max region end; no kernel tests with malformed tables.
Task 8: complete (commits ea9045e..b5f228c, review clean)
Task 9: ruling (implementer): load_segments skips file-range checks (trusted bootloader kernel ELF only); >64 Usable regions => arm fails => "amnesisch: nein" (M5 concern); tsc_hz=0 until Task 10 calibrates; Plan has kernel_l4+phys_offset; fb is virt addr+len. Task 10 must read plan in place (5.3KB, not onto 16KiB island stack).
Task 9: fix round 1/5 (2 addressed, 0 open; commits c342d76..dd608eb)
Task 9: minor (deferred): check_offset_window leaf boundary computed from phys not virt (ok while phys_offset 1G-aligned); FrameBitmap in l_core now dead code for kernel.
Task 9: complete (commits b5f228c..dd608eb, review clean)
Task 10: ruling (implementer): degraded wipe without plan = screen+DMA+keys+poweroff, RAM not zeroed; panic during wipe -> poweroff/reset (RAM may be partly unzeroed; docs must say so); wipe flag in island; lock-free PCI/ACPI mirrors; NMI masked in stage 1; slot 0 via rng::fill w/ 0xA5 fallback.
Task 10: minor (deferred): time.rs wait_tick_after bounds hlt count not time -> boot hang if PIT dead (fix before M5; use rdtsc bound); second trigger parks powered on (unreachable); degraded WIPE-REPORT marks done stages w/o work -> Task 11 must not accept report alone, check bytes= and dump; finish asm SAFETY note; ~50ms boot cost for TSC calibration; Task 19 must add xHCI halt at Real::stop_dma; Task 25 docs: panic-in-wipe trade-off.
Task 10: complete (commits dd608eb..623514d, review clean)
Task 11: Ruling: skipping UserZeroed alone fails no dump check because AllocatedZeroed (stage 7) zeroes all user frames (user frames subset of allocated) — accepted as defense in depth, brief's mutation expectation not met; stronger probe (skip both) fails 3 checks — costs: stage ordering/speed purpose of UserZeroed is untested.
Task 11: minor (deferred): elf_segments no sanity checks (struct.error -> traceback exit 1); 32-byte key prefix lacks pre-dump positive control; dead armed branch; hotkey wipe measured 0.94-1.48 s at 256M (WIPE-REPORT), host 1.08-1.60 s.
Task 11: complete (commits 623514d..3fd156d, review clean). PART A DONE (hotkey wipe with dump acceptance).
Task 12: minor (deferred): enqueue has no full-ring check (driver must bound in-flight); add ProducerRing::cycle() when needed (Task 19); ISP set on IN data/normal -> driver must treat completion code 13 (short packet) as non-error; driver write order: TRB body, cycle bit, link, doorbell; tests lack exact-control-word and ISP assertions; ProducerRing derives Copy.
Task 12: complete (commits 3fd156d..f2c0661, review clean)
Task 13: Ruling: reviewer Important (classify returns Nothing for CCS=1+CSC with unknown PSI, could hide a connect) is not fixed in l_core because the brief fixes the PortChange enum and pins the behavior; instead Task 20/23 driver MUST NOT blindly ack CSC on Nothing with ccs&&csc — log raw speed and issue reset_request to learn speed; also re-check slot state after ResetDone (disconnect/reconnect during reset). Cost if wrong: unknown-PSI device on real HW not enumerated (M5).
Task 13: minor (deferred): ack_changes/reset_request doc "caller checks is_all_ones first"; from_psi only default PSI table; ep_interval interrupt-only doc; FS EP0 max packet 8 initially.
Task 13: complete (commits f2c0661..5a45fef, review clean)
Task 14: minor (deferred): parse_prefix doesn't validate bMaxPacketSize0 (Task 20 driver must reject 0/invalid); fuzz test weak (bytes %8); no test for alt!=0 skip / interface len<9.
Task 14: complete (commits 5a45fef..a1688ff, review clean)
Task 15: Ruling: brief's "releases first" vs "modifiers pressed first, released last" resolved as modifier presses, key releases, key presses, modifier releases (satisfies both).
Task 15: minor (deferred): Task 22 must keep prev = last GOOD report (rollover error produces no output; consider stateful wrapper owning prev); Pause dropped, PrintScreen without fake-shift; table is physical set-1 (no German decoder exists in repo).
Task 15: complete (commits a1688ff..531883f, review clean)
Task 16: minor (deferred): marker_fits_in_mbr test tautological; blocks==0 READ10 allowed; parse_csw doesn't check residue vs data_len (caller). Task 17 must ensure exactly ONE copy of PLACEHOLDER in the kernel binary (no extra const copies in non-test code => Duplicate) and the patch tool patches only the unpatched image; LBA0 marker overwrites bytes 0..32 of the MBR bootstrap area (spec decision, verify the stick still boots).
Task 16: complete (commits 531883f..9a72344, review clean)
Task 17: Ruling: kernel-side recompute of its own boot id is infeasible (kernel file in RAM is modified in place, 3/175 64KiB blocks differ) -> end-to-end 'kernel recognizes its own stick' rests on host chain test + Task 21/24 tests (stick_is_identified_by_boot_marker, unplug). Cost if wrong: a stale/mismatched ID could only be detected at Task 21.
Task 17: fix round 1/5 (3 addressed, 0 open; commits 4c35570..b9ff309)
Task 17: minor (deferred): host chain test leaves temp dir on assertion failure; #[used] on rlib static vs used(linker).
Task 17: complete (commits 9a72344..b9ff309, review clean)
Task 18: ruling (implementer): Mmio ops return Result<_,OutOfBounds>; write64 = lo then hi 32-bit halves; L3 for window created in init_memory (process L4s copy existing slots); map refuses RAM overlap & >16MiB; stage 7 of wipe zeroes window's table frames harmlessly (window unused after stage 3).
Task 18: minor (deferred): bar64/bar_size on the upper-half index of a 64-bit BAR returns wrong data (no caller yet; Task 19 uses idx 0); mmio::map not idempotent (aliases; doc); overlaps_ram only Usable+Bootloader; WB alias of low BARs via bootloader offset map (predates).
Task 18: complete (commits b9ff309..3840d60, review clean)
Task 19: fix round 1/5 (2 addressed, 0 open; commits 95352d6..6d7b0dc)
Task 19: carry to Task 20+: command() holds DRIVER lock while waiting -> ISR polling must use try_lock (or IF=0 ownership); caps/pop_event/dropped_events lock, never call from ISR. init() restarts after stale/Busy. Real-HW (M5): no HCRST settle delay, no USBLEGSUP BIOS handoff, scratchpad path untested in QEMU. Event queue 32 slots keeps oldest.
Task 19: complete (commits 3840d60..6d7b0dc, review clean)
Task 20: Ruling (implementer, confirmed by reviewer): Configure Endpoint BEFORE SET_CONFIGURATION (xHCI 4.3.5); Task 25 must amend spec §7.1 text. §7.3 abort criterion not triggered (18/18 x4 runs). Observed: stick port1 super-speed vendor/product 0x46f4/0x0001 EP0 512; kbd port6 high-speed 0x0627/0x0001 EP0 64; PORTSC 0x1203/0x0e03.
Task 20: minor (deferred): refuse bConfigurationValue 0 (usb.rs:436) — do in Task 21/22; SET_IDLE stall marks EP0 broken (no Reset Endpoint; M5); no Disable Slot (M5); enumerate holds DRIVER with IF=1 up to ~seconds at boot; pop_event still has 1 queued PortStatusChange (port 6) for Task 23 to drain; test configuration_selected... vacuous on empty list; Task 22 needs Normal-TRB enqueue + doorbell + in-flight count.
Task 20: complete (commits 6d7b0dc..5443329, review clean)
Task 21: implementer commit 6930e96; review found 2 Important (seal enforced only in usb.rs while xhci::bulk is unsealed; a post-seal xhci restart/enumerate would re-address the stick and foreign MSC devices) + minors (Short doc, boot-delay doc, test-hooks reason wording). Fix round 1/5 was IN PROGRESS when the user paused the session; agent stopped, partial uncommitted work saved as task-21-fix1-wip.patch (unverified), tree reset to 6930e96. RESUME: re-dispatch fix round 1 with these findings (optionally starting from the WIP patch), then scoped re-review, then Task 22.
Task 21: fix round 1/5 (2 Important + minors addressed, 0 open; commits 6930e96..8fe23b9)
Task 21: ruling: test-hooks reason stays "Wipe-Plan fehlt" (documented; naming it needs an Amnesia field + pack change). Post-seal restart only rescans ports that held a non-storage device at seal time (keyboard); storage on a former keyboard port is addressed and its descriptors read but never configured (documented). Control transfers to the stick's existing slot remain possible in principle until next restart (nothing issues them).
Task 21: minor (deferred): identify_boot_stick worst-case hostile delay ~48 s/device (6.4 min w/ 8 devices) — add overall deadline (~10-15 s) before M5; stick_reads counts commands attempted.
Task 21: complete (commits 5443329..8fe23b9, review clean)
Task 22: ruling (implementer): keyboard ring keeps 8 TDs in flight (exception to one-in-flight; 8 of 63 slots); keyboard reconnect (CSC with CCS=1) also stops keyboard, no wipe; stall => keyboard dead until reboot (no Reset Endpoint, M5); security gained field tastatur-berichte (bits 40..62). Hold-Ctrl-then-unplug check uses Alt+Backspace over PS/2 (shell ignores Ctrl for letters). USB hotkey wipe verified only ad hoc, not CI.
Task 22: fix round 1/5 (1 Important + minors addressed, 0 open; commits ba7c3d1..786b776)
Task 22: carry to Task 23: PSC events fire only on 0->1 change-bit transitions: classify PORTSC LEVEL (was_connected + current CCS), don't rely on new events once CSC stays set; poll (Driver::poll) must be extended for stick port changes, controller loss, USBSTS; port changes on other ports consumed but not acked; counting new connections.
Task 22: minor (deferred): SHUTDOWN wait 5 s in unplug check could false-pass on very slow wipe; kbd_transfer copies Kbd per event.
Task 22: complete (commits 8fe23b9..786b776, review clean)
Task 23: ruling (implementer, reviewer-confirmed): fail-closed extras: USBSTS.HCH after arming => ControllerLost; CSC=1 with CCS=1 on stick port => StickRemoved; watch_check runs lock-free (atomics+volatile PORTSC) every tick before DRIVER.try_lock; Task 25 must write these into spec §7.1/§7.2. "amnesisch: ja" is computed from stick_sealed, not "watch armed" (arming right after seal; failure unreachable today) — Task 25 should add an `armed` input to Amnesia or make arm failure => "nein". Real-HW (M5): spurious CSC / SuperSpeed fallback would wipe.
Task 23: minor (deferred): no kernel test drives StickRemoved through poll_tick on the real stick port (Task 24 --trigger unplug MUST close this); test-order coupling (name-sorted test cases); op_read # Safety contract stale (USBCMD/USBSTS or PORTSC); new_connections() is lower bound; unchecked: PICS.lock() in timer ISR before poll_tick contention could starve watch (outside diff, check in Task 24/final review).
Task 23: complete (commits 786b776..318088c, review clean)
Task 24: minors (deferred to Task 25 docs): soften "hotkey/unplug differences within noise" (only supported at 256M; 1G/4G single runs), mark design assertions in measurements doc as not measured, ROADMAP/spec still say "[assumed] replaced" without pointer to docs/superpowers/notes/2026-10-14-m2-wipe-measurements.md; UNPLUG_TIMEOUT comment. Measured (TCG, host time cmd->SHUTDOWN): 256M hotkey 1.20 s/unplug 1.11 s; 1G 3.14/3.94 s; 4G 6.85/7.01 s; highest usable range ends 0x140000000 zero. Default --boot usb for hotkey flow too.
Task 24: complete (commits 318088c..f835d02, review clean)
