//! Minimal runtime for Larp OS ring-3 programs: syscall wrappers, console
//! output, entry macro, and a panic handler that ends only this process.
#![no_std]

use core::arch::asm;
use core::fmt;
pub use l_core;
use l_core::syscall as abi;

/// Handles a freshly started program finds in its table, in this order, when
/// the kernel (or its parent) grants them. A program must not assume a handle
/// exists; use [`h`] values only for programs whose spawner documents them.
pub mod h {
    pub const CONSOLE: u64 = 0;
    pub const KEYBOARD: u64 = 1;
    pub const SYSTEM: u64 = 2;
    pub const SPAWNER: u64 = 3;
    pub const NETMANAGER: u64 = 4;
    pub const NETCLIENT: u64 = 5;
    pub const RANDOM: u64 = 6;
}

/// Raw `int 0x80`. All registers except `rax` are preserved by the kernel.
#[inline(always)]
pub fn syscall(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> i64 {
    let ret: i64;
    // SAFETY: int 0x80 enters the kernel's syscall gate; the kernel validates
    // every argument and preserves all registers but rax.
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") num as i64 => ret,
            in("rdi") a0, in("rsi") a1, in("rdx") a2, in("r10") a3, in("r8") a4,
            options(nostack),
        );
    }
    ret
}

pub fn exit(code: u16) -> ! {
    syscall(abi::SYS_EXIT, code as u64, 0, 0, 0, 0);
    // exit never returns; if it somehow did, fault instead of running on.
    loop {
        // SAFETY: ud2 raises #UD, which ends this process.
        unsafe { asm!("ud2", options(noreturn)) }
    }
}

pub fn yield_now() {
    syscall(abi::SYS_YIELD, 0, 0, 0, 0, 0);
}

/// Write bytes to a console handle (in chunks the kernel accepts).
pub fn write(console: u64, bytes: &[u8]) -> i64 {
    let mut total = 0;
    for chunk in bytes.chunks(abi::MAX_WRITE_LEN as usize) {
        let r = syscall(abi::SYS_WRITE, console, chunk.as_ptr() as u64, chunk.len() as u64, 0, 0);
        if r < 0 {
            return r;
        }
        total += r;
    }
    total
}

pub struct Console(pub u64);

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if write(self.0, s.as_bytes()) < 0 { Err(fmt::Error) } else { Ok(()) }
    }
}

impl Console {
    /// Erase the previous character on the line.
    pub fn backspace(&self) {
        write(self.0, &[0x08]);
    }
    pub fn clear(&self) {
        write(self.0, &[0x0c]);
    }
}

#[macro_export]
macro_rules! print {
    ($c:expr, $($arg:tt)*) => {{ use core::fmt::Write as _; let _ = write!($c, $($arg)*); }};
}

#[macro_export]
macro_rules! println {
    ($c:expr) => {{ $crate::print!($c, "\n"); }};
    ($c:expr, $($arg:tt)*) => {{ $crate::print!($c, "{}\n", format_args!($($arg)*)); }};
}

/// Start a built-in program. `grants` are `(my_handle, rights_mask)` pairs the
/// child receives (narrowed); `args` is copied to the child's argument area.
pub fn spawn(spawner: u64, program: u64, grants: &[(u64, u64)], args: &[u8], budget_ticks: u64) -> i64 {
    let mut buf = [0u8; abi::MAX_SPAWN_GRANTS as usize * 16 + abi::ARGS_MAX];
    let n = grants.len().min(abi::MAX_SPAWN_GRANTS as usize);
    for (i, (h, m)) in grants.iter().take(n).enumerate() {
        buf[16 * i..16 * i + 8].copy_from_slice(&h.to_le_bytes());
        buf[16 * i + 8..16 * i + 16].copy_from_slice(&m.to_le_bytes());
    }
    let a = args.len().min(abi::ARGS_MAX);
    buf[16 * n..16 * n + a].copy_from_slice(&args[..a]);
    syscall(abi::SYS_SPAWN, spawner, program, buf.as_ptr() as u64, (n as u64) | ((a as u64) << 8), budget_ticks)
}

/// The bytes this program was started with (empty if none).
pub fn args() -> &'static [u8] {
    // SAFETY: the kernel maps the top stack page of every process and writes a
    // length (<= ARGS_MAX) followed by the bytes at ARGS_ADDR before the first
    // instruction runs; nothing else in the program writes there.
    unsafe {
        let len = (abi::ARGS_ADDR as *const u64).read_unaligned() as usize;
        let len = len.min(abi::ARGS_MAX);
        core::slice::from_raw_parts((abi::ARGS_ADDR + 8) as *const u8, len)
    }
}

pub fn close(handle: u64) -> i64 {
    syscall(abi::SYS_CLOSE, handle, 0, 0, 0, 0)
}

/// Hardware random bytes (the whole buffer or an error).
pub fn random(random: u64, buf: &mut [u8]) -> i64 {
    let mut done = 0;
    for chunk in buf.chunks_mut(abi::MAX_RANDOM_LEN as usize) {
        let r = syscall(abi::SYS_RANDOM, random, chunk.as_mut_ptr() as u64, chunk.len() as u64, 0, 0);
        if r < 0 {
            return r;
        }
        done += r;
    }
    done
}

/// Open an authorized TCP connection; returns a socket handle or a negative error.
pub fn net_connect(net: u64, host: &str, port: u16) -> i64 {
    syscall(abi::SYS_NET_CONNECT, net, host.as_ptr() as u64, host.len() as u64, port as u64, 0)
}

/// Send some bytes (returns how many were accepted, > 0).
pub fn sock_send(sock: u64, data: &[u8]) -> i64 {
    let n = data.len().min(2048);
    syscall(abi::SYS_SOCK_SEND, sock, data.as_ptr() as u64, n as u64, 0, 0)
}

/// Receive bytes; 0 means the peer closed the connection.
pub fn sock_recv(sock: u64, buf: &mut [u8]) -> i64 {
    let n = buf.len().min(2048);
    syscall(abi::SYS_SOCK_RECV, sock, buf.as_mut_ptr() as u64, n as u64, 0, 0)
}

pub fn netctl(manager: u64, op: u64, ptr: u64, len: u64, arg: u64) -> i64 {
    syscall(abi::SYS_NETCTL, manager, op, ptr, len, arg)
}

pub fn wait(pid: u64) -> i64 {
    syscall(abi::SYS_WAIT, pid, 0, 0, 0, 0)
}

pub fn read_key(keyboard: u64) -> i64 {
    syscall(abi::SYS_READ_KEY, keyboard, 0, 0, 0, 0)
}

pub fn sysinfo(system: u64, what: u64) -> i64 {
    syscall(abi::SYS_SYSINFO, system, what, 0, 0, 0)
}

/// Define the program entry point: `entry!(main)` where `main() -> u16` is the
/// exit code.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn _start() -> ! {
            let code: u16 = $main();
            $crate::exit(code)
        }
    };
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // No allocation or formatting here; just end this process distinctly.
    exit(0xdead)
}
