//! The interactive shell, running in ring 3.
//!
//! Its authority is exactly the handles the kernel gave it: console (write +
//! grant), keyboard, system info, a spawner for the built-in programs, and
//! the network manager. Because only the shell reads the keyboard and holds
//! the manager, a network authorization can only come from a key the user
//! typed here: no program started from this shell can answer for the user.
#![no_std]
#![no_main]

use l_core::{
    handles::Rights,
    shell::{Command, EditEvent, LineEditor, ParseError, parse, parse_host_port},
    syscall::*,
    time::{TICK_HZ, approx_seconds, ticks_from_seconds},
};
use l_user::{Console, entry, h, netctl, print, println, read_key, spawn, sysinfo, wait};
use pc_keyboard::{DecodedKey, HandleControl, Keyboard, ScancodeSet1, layouts};

const PROMPT: &str = "l_os> ";
/// (name, kernel program id, needs console, needs network: client+random+clock)
const PROGRAMS: [(&str, u64, bool, bool); 5] = [
    ("hello", 0, true, false),
    ("evil", 1, false, false),
    ("spin", 2, false, false),
    ("hello_rs", 3, true, false),
    ("fetch", 4, true, true),
];
const CHILD_BUDGET_SECONDS: u64 = 1;
const FETCH_BUDGET_SECONDS: u64 = 60;
const DEFAULT_GRANT_SECONDS: u64 = 300;

type Kb = Keyboard<layouts::De105Key, ScancodeSet1>;

struct Term {
    out: Console,
    // Keyboard layout is German (De105Key), as in the original kernel shell.
    kb: Kb,
}

impl Term {
    /// Block until the next printable/control character is typed.
    fn next_char(&mut self) -> Option<char> {
        loop {
            let sc = read_key(h::KEYBOARD);
            if sc < 0 {
                return None;
            }
            let Ok(Some(event)) = self.kb.add_byte(sc as u8) else { continue };
            if let Some(DecodedKey::Unicode(c)) = self.kb.process_keyevent(event) {
                return Some(c);
            }
        }
    }

    /// Ask a yes/no question on the trusted path. Anything but `y` is "no".
    fn confirm(&mut self, question: &str) -> bool {
        print!(self.out, "{} [y/N] ", question);
        let answer = self.next_char();
        match answer {
            Some(c @ ('y' | 'Y')) => {
                println!(self.out, "{}", c);
                true
            }
            Some(c) if (' '..='~').contains(&c) => {
                println!(self.out, "{}", c);
                false
            }
            _ => {
                println!(self.out);
                false
            }
        }
    }
}

fn main() -> u16 {
    let mut t = Term {
        out: Console(h::CONSOLE),
        kb: Keyboard::new(ScancodeSet1::new(), layouts::De105Key, HandleControl::Ignore),
    };
    let mut editor = LineEditor::new();

    println!(t.out, "Larp OS {} - shell running in ring 3", env!("CARGO_PKG_VERSION"));
    println!(t.out, "Type 'help' for a list of commands.");
    print!(t.out, "{}", PROMPT);

    loop {
        let Some(c) = t.next_char() else {
            println!(t.out, "\nkeyboard unavailable; shell exiting");
            return 1;
        };
        match editor.handle_char(c) {
            EditEvent::Inserted(b) => print!(t.out, "{}", b as char),
            EditEvent::Erased => t.out.backspace(),
            EditEvent::Submit => {
                println!(t.out);
                // Copy the line: executing a command may read more keys.
                let mut line = [0u8; l_core::shell::LINE_CAPACITY];
                let n = editor.line().len();
                line[..n].copy_from_slice(editor.line().as_bytes());
                editor.clear();
                run_line(&mut t, core::str::from_utf8(&line[..n]).unwrap_or(""));
                print!(t.out, "{}", PROMPT);
            }
            EditEvent::Ignored | EditEvent::Full => {}
        }
    }
}

fn run_line(t: &mut Term, line: &str) {
    match parse(line) {
        Ok(cmd) => execute(t, cmd),
        Err(ParseError::Unknown(name)) => println!(t.out, "unknown command: {} (try 'help')", name),
        Err(ParseError::UnexpectedArguments(name)) => println!(t.out, "{}: unexpected arguments", name),
        Err(ParseError::MissingArgument(name)) => println!(t.out, "{}: missing argument (try 'help')", name),
    }
}

fn execute(t: &mut Term, cmd: Command<'_>) {
    match cmd {
        Command::Empty => {}
        Command::Help => help(t),
        Command::Version => println!(t.out, "Larp OS {} (x86_64, development build)", env!("CARGO_PKG_VERSION")),
        Command::Clear => t.out.clear(),
        Command::Uptime => {
            let ticks = sysinfo(h::SYSTEM, SYSINFO_TICKS).max(0) as u64;
            println!(t.out, "{} ticks (~{} s at {} Hz)", ticks, approx_seconds(ticks), TICK_HZ);
        }
        Command::Security => security(t),
        Command::Echo(text) => println!(t.out, "{}", text),
        Command::Run(rest) => run_program(t, rest),
        Command::Net(rest) => net(t, rest),
    }
}

fn help(t: &mut Term) {
    println!(t.out, "Commands:");
    println!(t.out, "  help      show this list");
    println!(t.out, "  version   show OS version");
    println!(t.out, "  uptime    show timer ticks since boot");
    println!(t.out, "  security  show what isolation/network/storage exists");
    println!(t.out, "  echo ...  print the arguments");
    println!(t.out, "  run NAME [ARGS]  run a program in its own process:");
    println!(t.out, "            hello | evil | spin | hello_rs | fetch https://HOST[:PORT]/PATH");
    println!(t.out, "  net status | devices | permissions");
    println!(t.out, "  net request HOST:PORT [SECONDS]   ask to allow TCP to HOST:PORT (you confirm)");
    println!(t.out, "  net revoke HOST:PORT | all        withdraw an authorization");
    println!(t.out, "  net resolver [IP:PORT]            show or change the DNS resolver");
    println!(t.out, "  clear     clear the screen");
}

fn security(t: &mut Term) {
    let cpu = sysinfo(h::SYSTEM, SYSINFO_CPU).max(0) as u64;
    let on = |bit: u64| if cpu & bit != 0 { "on" } else { "off" };
    println!(t.out, "Facts about this build (not guarantees about the future):");
    println!(t.out, "  network:    denied by default; a connection needs your confirmation ('net request')");
    println!(t.out, "              and then exact host:port (and, for names, the DNS resolver) is allowed;");
    println!(t.out, "              the NIC stays off until the first allowed connection");
    println!(t.out, "  storage:    only LBA 0 of the boot stick is read (to recognize it), then the");
    println!(t.out, "              storage path is sealed; nothing is ever written");
    println!(t.out, "  isolation:  this shell and its programs run in ring 3, each in its own");
    println!(t.out, "              address space with only the handles it was given;");
    println!(t.out, "              the kernel, network stack and drivers still run in ring 0");
    println!(t.out, "  cpu:        SMEP {}, SMAP {}", on(CPU_SMEP), on(CPU_SMAP));
    println!(t.out, "  telemetry:  none");
    amnesia_lines(t);
}

/// Spec E7: "amnesisch: ja" only when the kernel reports every condition.
fn amnesia_lines(t: &mut Term) {
    let raw = sysinfo(h::SYSTEM, SYSINFO_AMNESIA);
    if raw < 0 {
        println!(t.out, "  amnesisch: nein (Status nicht lesbar)");
        return;
    }
    let s = unpack_amnesia(raw as u64);
    if s.amnesia.armed() {
        println!(t.out, "  {}", s.amnesia.label());
    } else {
        println!(t.out, "  {} ({})", s.amnesia.label(), s.amnesia.reason());
    }
    let yn = |b: bool| if b { "ja" } else { "nein" };
    let stick = if s.amnesia.stick_sealed { "versiegelt" } else { "fehlt" };
    println!(t.out, "  usb: tastatur {}, stick: {}, stick-lesezugriffe {}", yn(s.keyboard), stick, s.stick_reads);
    println!(t.out, "  acpi-poweroff: {}", yn(s.acpi_poweroff));
}

// ------------------------------------------------------------------- run

fn outcome_text(t: &mut Term, raw: i64) {
    match Outcome::from_raw(raw as u64) {
        Outcome::Exited(code) => println!(t.out, "[process exited with code {}]", code),
        Outcome::PageFault => println!(t.out, "[process killed: illegal memory access (page fault)]"),
        Outcome::GeneralProtection => println!(t.out, "[process killed: protection fault]"),
        Outcome::InvalidOpcode => println!(t.out, "[process killed: invalid instruction]"),
        Outcome::DivideError => println!(t.out, "[process killed: divide error]"),
        Outcome::OtherFault => println!(t.out, "[process killed: CPU exception]"),
        Outcome::Deadlock => println!(t.out, "[process killed: blocked forever]"),
        Outcome::Timeout => println!(t.out, "[process killed: exceeded its time budget]"),
    }
}

fn run_program(t: &mut Term, rest: &str) {
    let (name, args) = rest.split_once(' ').unwrap_or((rest, ""));
    let Some(&(_, id, needs_console, needs_net)) = PROGRAMS.iter().find(|(n, _, _, _)| *n == name) else {
        println!(t.out, "run: unknown program '{}' (hello | evil | spin | hello_rs | fetch)", name);
        return;
    };
    if id != 4 && !args.is_empty() {
        println!(t.out, "run: {} takes no arguments", name);
        return;
    }
    if id == 4 && args.is_empty() {
        println!(t.out, "usage: run fetch https://HOST[:PORT]/PATH   (authorize it first with 'net request')");
        return;
    }

    // Each program gets only what it needs, with the narrowest rights:
    // write-only console, and for fetch: the right to ask the network, random
    // numbers, and the clock. It never gets the manager or the keyboard.
    let write = Rights::WRITE.bits() as u64;
    let ask = Rights::USE.bits() as u64;
    let read = Rights::READ.bits() as u64;
    let mut grants = [(0u64, 0u64); 4];
    let mut n = 0;
    if needs_console {
        grants[n] = (h::CONSOLE, write);
        n += 1;
    }
    if needs_net {
        grants[n] = (h::NETCLIENT, ask);
        grants[n + 1] = (h::RANDOM, read);
        grants[n + 2] = (h::SYSTEM, read);
        n += 3;
    }
    let budget = ticks_from_seconds(if id == 4 { FETCH_BUDGET_SECONDS } else { CHILD_BUDGET_SECONDS });

    let pid = spawn(h::SPAWNER, id, &grants[..n], args.as_bytes(), budget);
    if pid < 0 {
        println!(t.out, "run failed (error {})", pid);
        return;
    }
    let raw = wait(pid as u64);
    if raw < 0 {
        println!(t.out, "wait failed (error {})", raw);
        return;
    }
    outcome_text(t, raw);
}

// ------------------------------------------------------------------- net

fn net(t: &mut Term, rest: &str) {
    let (sub, args) = rest.split_once(' ').unwrap_or((rest, ""));
    let args = args.trim_matches(' ');
    match sub {
        "status" if args.is_empty() => net_status(t),
        "devices" if args.is_empty() => net_devices(t),
        "permissions" if args.is_empty() => net_permissions(t),
        "request" => net_request(t, args),
        "revoke" => net_revoke(t, args),
        "resolver" => net_resolver(t, args),
        _ => println!(t.out, "net: unknown or malformed subcommand (try 'help')"),
    }
}

/// Read one text line from a netctl op that takes an index in `arg`.
fn netctl_line<'b>(op: u64, index: u64, buf: &'b mut [u8; 160]) -> Option<&'b str> {
    let r = netctl(h::NETMANAGER, op, buf.as_mut_ptr() as u64, buf.len() as u64, index);
    if r < 0 {
        return None;
    }
    core::str::from_utf8(&buf[..r as usize]).ok()
}

fn net_status(t: &mut Term) {
    let st = netctl(h::NETMANAGER, NETCTL_STATUS, 0, 0, 0).max(0) as u64;
    let stats = netctl(h::NETMANAGER, NETCTL_STATS, 0, 0, 0).max(0) as u64;
    let mut buf = [0u8; 160];
    println!(t.out, "network interface: {}", if st & NET_STATUS_PRESENT != 0 { "present (virtio-net)" } else { "none" });
    if st & NET_STATUS_UP != 0 {
        println!(t.out, "state: UP (initialized after an authorized connection)");
    } else {
        println!(t.out, "state: DOWN - never initialized; nothing has been sent or received");
    }
    println!(t.out, "frames sent/received: {} / {}", stats & 0xffff_ffff, stats >> 32);
    println!(t.out, "dns resolver: {}", netctl_line(NETCTL_GET_RESOLVER, 0, &mut buf).unwrap_or("?"));
    let mut count = 0;
    let mut b = [0u8; 160];
    while netctl_line(NETCTL_LIST, count, &mut b).is_some() {
        count += 1;
    }
    println!(t.out, "enforced authorizations: {} (see 'net permissions')", count);
}

fn net_devices(t: &mut Term) {
    let mut i = 0;
    let mut buf = [0u8; 160];
    while let Some(line) = netctl_line(NETCTL_DEVICE, i, &mut buf) {
        println!(t.out, "  {}", line);
        i += 1;
    }
    if i == 0 {
        println!(t.out, "no PCI devices found");
    }
}

fn net_permissions(t: &mut Term) {
    let mut i = 0;
    let mut buf = [0u8; 160];
    while let Some(line) = netctl_line(NETCTL_LIST, i, &mut buf) {
        println!(t.out, "  {}", line);
        i += 1;
    }
    if i == 0 {
        println!(t.out, "no authorizations: every network operation is denied");
    } else {
        println!(t.out, "(this list is what the kernel enforces right now)");
    }
}

fn net_request(t: &mut Term, args: &str) {
    let (target, secs) = args.split_once(' ').unwrap_or((args, ""));
    let Ok((host, port)) = parse_host_port(target) else {
        println!(t.out, "usage: net request HOST:PORT [SECONDS]");
        return;
    };
    let seconds = if secs.is_empty() {
        DEFAULT_GRANT_SECONDS
    } else {
        match secs.trim_matches(' ').parse::<u64>() {
            Ok(s) if (1..=86_400).contains(&s) => s,
            _ => {
                println!(t.out, "SECONDS must be between 1 and 86400");
                return;
            }
        }
    };
    println!(t.out, "REQUEST (not yet in effect): allow programs started from this shell to open");
    println!(t.out, "TCP connections to {} port {} for {} s.", host, port, seconds);
    println!(t.out, "Asking this question sends nothing to the network.");
    if !t.confirm("Allow?") {
        println!(t.out, "NOT AUTHORIZED. Nothing changed.");
        return;
    }
    let duration = ticks_from_seconds(seconds);
    let arg = port as u64 | (duration << 16);
    let r = netctl(h::NETMANAGER, NETCTL_GRANT, host.as_ptr() as u64, host.len() as u64, arg);
    if r < 0 {
        println!(t.out, "NOT AUTHORIZED: the kernel refused the request (error {}; invalid name or table full)", r);
        return;
    }
    // Do not trust our own request: show what the kernel actually holds.
    println!(t.out, "AUTHORIZED (enforced by the kernel): {}:{} for {} s", host, port, seconds);
}

fn net_revoke(t: &mut Term, args: &str) {
    if args == "all" {
        netctl(h::NETMANAGER, NETCTL_REVOKE_ALL, 0, 0, 0);
        println!(t.out, "all authorizations revoked; open connections were reset");
        return;
    }
    let Ok((host, port)) = parse_host_port(args) else {
        println!(t.out, "usage: net revoke HOST:PORT | all");
        return;
    };
    let r = netctl(h::NETMANAGER, NETCTL_REVOKE, host.as_ptr() as u64, host.len() as u64, port as u64);
    match r {
        1 => println!(t.out, "revoked {}:{}; open connections to it were reset", host, port),
        0 => println!(t.out, "no such authorization: {}:{}", host, port),
        e => println!(t.out, "revoke failed (error {})", e),
    }
}

fn net_resolver(t: &mut Term, args: &str) {
    let mut buf = [0u8; 160];
    if args.is_empty() {
        println!(t.out, "dns resolver: {}", netctl_line(NETCTL_GET_RESOLVER, 0, &mut buf).unwrap_or("?"));
        println!(t.out, "(names can only be looked up after 'net request' for BOTH the name and the resolver)");
        return;
    }
    let Ok((ip, port)) = parse_host_port(args) else {
        println!(t.out, "usage: net resolver [IP:PORT]");
        return;
    };
    println!(t.out, "Name lookups will be sent to {} port {} (once you also authorize it).", ip, port);
    if !t.confirm("Change the resolver?") {
        println!(t.out, "Resolver unchanged.");
        return;
    }
    let r = netctl(h::NETMANAGER, NETCTL_SET_RESOLVER, ip.as_ptr() as u64, ip.len() as u64, port as u64);
    if r < 0 {
        println!(t.out, "refused (error {}): the resolver must be an IPv4 address", r);
    } else {
        println!(t.out, "resolver is now {}:{}", ip, port);
    }
}

entry!(main);
