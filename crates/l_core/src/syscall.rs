//! System-call ABI shared by the kernel and (future) user programs.
//!
//! Convention (`int 0x80`): `rax` = number, `rdi`/`rsi`/`rdx` = arguments,
//! result in `rax` as a signed 64-bit value (negative = error). All other
//! registers are preserved.

use crate::policy::{Decision, DenyReason};
use crate::wipe::Amnesia;

pub const SYS_EXIT: u64 = 0;
/// `write(console_handle, ptr, len)`: handle needs `Rights::WRITE`.
pub const SYS_WRITE: u64 = 1;
/// `net_check(net_handle, host_ptr, host_len, port)`: handle needs `Rights::USE`;
/// returns the policy decision. It does not open a connection (no network stack yet).
pub const SYS_NET_CHECK: u64 = 2;
/// `yield()`: let another ready process run.
pub const SYS_YIELD: u64 = 3;
/// `close(handle)`
pub const SYS_CLOSE: u64 = 4;
/// `send(endpoint, ptr, len, xfer_handle|NO_HANDLE, rights_mask)`: endpoint needs
/// `SEND`; a transferred handle needs `GRANT` and arrives with rights ∩ mask.
pub const SYS_SEND: u64 = 5;
/// `recv(endpoint, ptr, max_len)`: endpoint needs `RECV`; blocks while empty.
/// Result: `len | has_handle << 16 | handle << 32`.
pub const SYS_RECV: u64 = 6;

/// `spawn(spawner, program_id, grants_ptr, grants_count, budget_ticks)`:
/// starts a built-in program. Each grant is `(parent_handle, rights_mask)` (two
/// u64); the child receives those handles, in order, with rights ∩ mask. The
/// child's budget is capped by the parent's remaining budget. Returns the pid.
pub const SYS_SPAWN: u64 = 7;
/// `wait(pid)`: block until the child `pid` ended; returns its `Outcome::to_raw()`.
pub const SYS_WAIT: u64 = 8;
/// `read_key(keyboard_handle)`: block until a raw scancode arrives; returns it.
pub const SYS_READ_KEY: u64 = 9;
/// `sysinfo(system_handle, what)`: 0 = ticks since boot, 1 = CPU feature bits.
pub const SYS_SYSINFO: u64 = 10;

/// `random(random_handle, ptr, len)`: fill a user buffer with hardware randomness
/// (at most 256 bytes per call); fails if no hardware generator works.
pub const SYS_RANDOM: u64 = 11;
/// `net_connect(net_handle, host_ptr, host_len, port)`: open a TCP connection to
/// `host:port`. Needs a `NetClient` handle AND a live grant for exactly that
/// destination (and, for names, for the DNS resolver). Returns a socket handle.
pub const SYS_NET_CONNECT: u64 = 12;
/// `sock_send(sock, ptr, len)`: returns bytes accepted (>0).
pub const SYS_SOCK_SEND: u64 = 13;
/// `sock_recv(sock, ptr, max_len)`: returns bytes read; 0 = connection closed.
pub const SYS_SOCK_RECV: u64 = 14;
/// `netctl(manager, op, ptr, len, arg)`: management by the trusted shell.
pub const SYS_NETCTL: u64 = 15;

pub const NETCTL_STATUS: u64 = 0;
pub const NETCTL_DEVICE: u64 = 1;
pub const NETCTL_GRANT: u64 = 2;
pub const NETCTL_REVOKE: u64 = 3;
pub const NETCTL_REVOKE_ALL: u64 = 4;
pub const NETCTL_LIST: u64 = 5;
pub const NETCTL_STATS: u64 = 6;
pub const NETCTL_SET_RESOLVER: u64 = 7;
pub const NETCTL_GET_RESOLVER: u64 = 8;

pub const NET_STATUS_PRESENT: u64 = 1;
pub const NET_STATUS_UP: u64 = 2;

pub const MAX_RANDOM_LEN: u64 = 256;

pub const SYSINFO_TICKS: u64 = 0;
pub const SYSINFO_CPU: u64 = 1;
/// Wall-clock seconds since the Unix epoch from the RTC, or an error if the
/// RTC is unreadable or implausible.
pub const SYSINFO_WALL_CLOCK: u64 = 2;
/// The amnesia status for the shell's `security` command, packed by
/// [`pack_amnesia`] (spec E7). Read from atomics only.
pub const SYSINFO_AMNESIA: u64 = 3;
pub const CPU_SMEP: u64 = 1;
pub const CPU_SMAP: u64 = 2;

pub const MAX_SPAWN_GRANTS: u64 = 8;

/// Not a live handle in the caller's table (also: forged, closed, stale).
pub const ERR_BAD_HANDLE: i64 = -1;
pub const ERR_BAD_POINTER: i64 = -2;
pub const ERR_INVALID: i64 = -3;
pub const ERR_NO_SYSCALL: i64 = -4;
pub const ERR_WOULD_BLOCK: i64 = -5;
pub const ERR_NO_RIGHTS: i64 = -6;
pub const ERR_TABLE_FULL: i64 = -7;
/// No free process slot.
pub const ERR_NO_SLOT: i64 = -8;
/// Not a child of the caller (or already reaped).
pub const ERR_NOT_CHILD: i64 = -9;
/// DNS lookup needed but the resolver itself is not authorized.
pub const DENIED_DNS_RESOLVER: i64 = -13;
pub const ERR_NO_DEVICE: i64 = -14;
pub const ERR_DNS_FAILED: i64 = -15;
pub const ERR_CONNECT_FAILED: i64 = -16;
pub const ERR_TIMEOUT: i64 = -17;
/// The authorization for this connection ended (revoked or expired).
pub const ERR_REVOKED: i64 = -18;
pub const ERR_NO_ENTROPY: i64 = -19;
pub const ERR_NO_CLOCK: i64 = -20;
pub const DENIED_NO_GRANT: i64 = -10;
pub const DENIED_EXPIRED: i64 = -11;
pub const DENIED_INVALID_DESTINATION: i64 = -12;

pub const MAX_WRITE_LEN: u64 = 256;
pub const MAX_HOST_ARG_LEN: u64 = 253;

/// Top of every process's stack (the stack grows down from here).
pub const USER_STACK_TOP: u64 = 0x_7000_0100_0000; // 16 MiB above the image base
/// Program arguments live at a fixed spot in the top stack page: a
/// little-endian u64 length followed by that many bytes.
pub const ARGS_ADDR: u64 = USER_STACK_TOP - 512;
pub const ARGS_MAX: usize = 500;

/// First address that is not user space (canonical lower half ends here).
pub const USER_SPACE_END: u64 = 0x0000_8000_0000_0000;

pub fn decision_to_ret(d: Decision) -> i64 {
    match d {
        Decision::Allow => 0,
        Decision::Deny(DenyReason::NoGrant) => DENIED_NO_GRANT,
        Decision::Deny(DenyReason::Expired) => DENIED_EXPIRED,
        Decision::Deny(DenyReason::InvalidDestination) => DENIED_INVALID_DESTINATION,
    }
}

const AM_STICK_SEALED: u64 = 1;
const AM_PLAN_READY: u64 = 1 << 1;
const AM_SERIAL_MIRROR: u64 = 1 << 2;
const AM_KEYBOARD: u64 = 1 << 3;
const AM_ACPI_POWEROFF: u64 = 1 << 4;
/// `stick_reads` occupies bits 8..40, so a packed value never has bit 63
/// set (it is returned as a non-negative `i64`).
const AM_READS_SHIFT: u32 = 8;

/// What `sysinfo(SYSINFO_AMNESIA)` reports, unpacked by [`unpack_amnesia`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmnesiaStatus {
    pub amnesia: Amnesia,
    /// A USB boot keyboard was enumerated.
    pub keyboard: bool,
    /// Bulk-only commands sent to mass storage since boot (saturated).
    pub stick_reads: u32,
    /// The ACPI S5 power-off values were found.
    pub acpi_poweroff: bool,
}

/// Bits 0..=4: stick sealed, plan ready, serial mirror, keyboard, ACPI
/// power-off; bits 8..40: `stick_reads`. Always below `1 << 40`.
pub fn pack_amnesia(a: Amnesia, keyboard: bool, stick_reads: u32, acpi: bool) -> u64 {
    let flag = |b: bool, bit: u64| if b { bit } else { 0 };
    flag(a.stick_sealed, AM_STICK_SEALED)
        | flag(a.plan_ready, AM_PLAN_READY)
        | flag(a.serial_mirror, AM_SERIAL_MIRROR)
        | flag(keyboard, AM_KEYBOARD)
        | flag(acpi, AM_ACPI_POWEROFF)
        | u64::from(stick_reads) << AM_READS_SHIFT
}

/// Inverse of [`pack_amnesia`]; unknown bits are ignored.
pub fn unpack_amnesia(v: u64) -> AmnesiaStatus {
    AmnesiaStatus {
        amnesia: Amnesia {
            stick_sealed: v & AM_STICK_SEALED != 0,
            plan_ready: v & AM_PLAN_READY != 0,
            serial_mirror: v & AM_SERIAL_MIRROR != 0,
        },
        keyboard: v & AM_KEYBOARD != 0,
        stick_reads: (v >> AM_READS_SHIFT) as u32,
        acpi_poweroff: v & AM_ACPI_POWEROFF != 0,
    }
}

/// Does `[ptr, ptr+len)` lie entirely below the kernel/user split, without
/// overflow, and is it non-empty? (Page-level mapping checks happen in the
/// kernel; this is the address-range part.)
pub fn range_in_user_space(ptr: u64, len: u64) -> bool {
    match ptr.checked_add(len) {
        Some(end) => len > 0 && ptr != 0 && end <= USER_SPACE_END,
        None => false,
    }
}

/// How a user program ended. Encoded as a `u64` for the asm return path:
/// exit codes occupy the low 16 bits, every kernel-detected end is >= 0x1_0000.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Exited(u16),
    PageFault,
    GeneralProtection,
    InvalidOpcode,
    DivideError,
    /// Any other CPU exception raised by the process (alignment, FPU, overflow, ...).
    OtherFault,
    /// Blocked forever: nothing left that could wake it.
    Deadlock,
    /// Killed because it ran past its time budget.
    Timeout,
}

impl Outcome {
    pub const fn to_raw(self) -> u64 {
        match self {
            Outcome::Exited(c) => c as u64,
            Outcome::PageFault => 0x1_0001,
            Outcome::GeneralProtection => 0x1_0002,
            Outcome::InvalidOpcode => 0x1_0003,
            Outcome::DivideError => 0x1_0004,
            Outcome::OtherFault => 0x1_0005,
            Outcome::Deadlock => 0x1_0006,
            Outcome::Timeout => 0x1_00ff,
        }
    }

    /// Unknown raw values map to `GeneralProtection` (a kill), never to a
    /// successful exit.
    pub const fn from_raw(raw: u64) -> Outcome {
        match raw {
            0..=0xffff => Outcome::Exited(raw as u16),
            0x1_0001 => Outcome::PageFault,
            0x1_0003 => Outcome::InvalidOpcode,
            0x1_0004 => Outcome::DivideError,
            0x1_0005 => Outcome::OtherFault,
            0x1_0006 => Outcome::Deadlock,
            0x1_00ff => Outcome::Timeout,
            _ => Outcome::GeneralProtection,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_roundtrip() {
        for o in [
            Outcome::Exited(0), Outcome::Exited(7), Outcome::Exited(u16::MAX), Outcome::PageFault,
            Outcome::GeneralProtection, Outcome::InvalidOpcode, Outcome::DivideError, Outcome::OtherFault, Outcome::Deadlock, Outcome::Timeout,
        ] {
            assert_eq!(Outcome::from_raw(o.to_raw()), o);
        }
        assert_eq!(Outcome::from_raw(u64::MAX), Outcome::GeneralProtection);
    }

    #[test]
    fn user_cannot_spoof_a_kernel_kill_with_exit_code() {
        // Exit codes are truncated to 16 bits by the kernel before encoding.
        assert_ne!(Outcome::Exited(0xffff).to_raw(), Outcome::PageFault.to_raw());
        assert!(Outcome::Exited(u16::MAX).to_raw() < 0x1_0000);
    }

    #[test]
    fn user_range_checks() {
        assert!(range_in_user_space(0x1000, 16));
        assert!(!range_in_user_space(0, 16)); // null
        assert!(!range_in_user_space(0x1000, 0)); // empty
        assert!(!range_in_user_space(USER_SPACE_END - 8, 16)); // straddles split
        assert!(range_in_user_space(USER_SPACE_END - 16, 16)); // ends exactly at split
        assert!(!range_in_user_space(0xffff_8000_0000_0000, 8)); // kernel half
        assert!(!range_in_user_space(u64::MAX - 4, 16)); // overflow
        assert!(!range_in_user_space(0x4444_4444_0000 | (1 << 63), 8));
    }

    #[test]
    fn amnesia_pack_roundtrip() {
        use crate::wipe::Amnesia;
        for bits in 0..32u8 {
            let a = Amnesia {
                stick_sealed: bits & 1 != 0,
                plan_ready: bits & 2 != 0,
                serial_mirror: bits & 4 != 0,
            };
            let (kbd, acpi) = (bits & 8 != 0, bits & 16 != 0);
            for reads in [0u32, 1, 3, 0x7fff_ffff, 0x8000_0000, u32::MAX - 1, u32::MAX] {
                let v = pack_amnesia(a, kbd, reads, acpi);
                // Must survive the syscall return path: never negative.
                assert!((v as i64) >= 0, "{v:#x}");
                let s = unpack_amnesia(v);
                assert_eq!(s.amnesia, a);
                assert_eq!((s.keyboard, s.stick_reads, s.acpi_poweroff), (kbd, reads, acpi));
                assert_eq!(s.amnesia.label(), a.label());
            }
        }
        // Unknown bits are ignored; garbage never yields "ja" by accident
        // unless the three status bits say so.
        let s = unpack_amnesia(u64::MAX);
        assert!(s.amnesia.serial_mirror && !s.amnesia.armed());
        let s = unpack_amnesia(0);
        assert!(!s.amnesia.armed() && !s.keyboard && !s.acpi_poweroff && s.stick_reads == 0);
        let s = unpack_amnesia(0b11 | 1 << 60);
        assert!(s.amnesia.armed());
    }

    #[test]
    fn decisions_map_to_distinct_negative_codes() {
        assert_eq!(decision_to_ret(Decision::Allow), 0);
        let denied = [
            decision_to_ret(Decision::Deny(DenyReason::NoGrant)),
            decision_to_ret(Decision::Deny(DenyReason::Expired)),
            decision_to_ret(Decision::Deny(DenyReason::InvalidDestination)),
        ];
        assert!(denied.iter().all(|&r| r < 0));
        assert!(denied[0] != denied[1] && denied[1] != denied[2] && denied[0] != denied[2]);
    }
}
