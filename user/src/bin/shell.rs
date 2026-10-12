//! The interactive shell, running in ring 3.
//!
//! Its authority is exactly the handles the kernel gave it:
//! console (write + grant), keyboard, system info, and a spawner for the
//! built-in demo programs. It cannot touch hardware or other processes.
#![no_std]
#![no_main]

use l_core::{
    handles::Rights,
    shell::{Command, EditEvent, LineEditor, ParseError, parse},
    syscall::{CPU_SMAP, CPU_SMEP, Outcome, SYSINFO_CPU, SYSINFO_TICKS},
    time::approx_seconds,
};
use l_user::{Console, entry, h, println, print, read_key, spawn, sysinfo, wait};
use pc_keyboard::{DecodedKey, HandleControl, Keyboard, ScancodeSet1, layouts};

const PROMPT: &str = "l_os> ";
/// (name, kernel program id, whether it needs the console)
const PROGRAMS: [(&str, u64, bool); 4] = [("hello", 0, true), ("evil", 1, false), ("spin", 2, false), ("hello_rs", 3, true)];
const CHILD_BUDGET_TICKS: u64 = 18; // about one second

fn main() -> u16 {
    let mut out = Console(h::CONSOLE);
    // Keyboard layout is German (De105Key), as in the original kernel shell.
    let mut keyboard = Keyboard::new(ScancodeSet1::new(), layouts::De105Key, HandleControl::Ignore);
    let mut editor = LineEditor::new();

    println!(out, "Larp OS {} - shell running in ring 3", env!("CARGO_PKG_VERSION"));
    println!(out, "Type 'help' for a list of commands.");
    print!(out, "{}", PROMPT);

    loop {
        let sc = read_key(h::KEYBOARD);
        if sc < 0 {
            println!(out, "\nkeyboard unavailable (error {}); shell exiting", sc);
            return 1;
        }
        let Ok(Some(event)) = keyboard.add_byte(sc as u8) else { continue };
        let Some(DecodedKey::Unicode(c)) = keyboard.process_keyevent(event) else { continue };

        match editor.handle_char(c) {
            EditEvent::Inserted(b) => print!(out, "{}", b as char),
            EditEvent::Erased => out.backspace(),
            EditEvent::Submit => {
                println!(out);
                run_line(&mut out, editor.line());
                editor.clear();
                print!(out, "{}", PROMPT);
            }
            EditEvent::Ignored | EditEvent::Full => {}
        }
    }
}

fn run_line(out: &mut Console, line: &str) {
    match parse(line) {
        Ok(cmd) => execute(out, cmd),
        Err(ParseError::Unknown(name)) => println!(out, "unknown command: {} (try 'help')", name),
        Err(ParseError::UnexpectedArguments(name)) => println!(out, "{}: unexpected arguments", name),
        Err(ParseError::MissingArgument(name)) => println!(out, "{}: missing argument", name),
    }
}

fn execute(out: &mut Console, cmd: Command<'_>) {
    match cmd {
        Command::Empty => {}
        Command::Help => {
            println!(out, "Commands:");
            println!(out, "  help      show this list");
            println!(out, "  version   show OS version");
            println!(out, "  uptime    show timer ticks since boot");
            println!(out, "  security  show what isolation/network/storage exists");
            println!(out, "  echo ...  print the arguments");
            println!(out, "  run NAME  run a program in its own process: hello | evil | spin | hello_rs");
            println!(out, "  clear     clear the screen");
        }
        Command::Version => println!(out, "Larp OS {} (x86_64, development build)", env!("CARGO_PKG_VERSION")),
        Command::Clear => out.clear(),
        Command::Uptime => {
            let t = sysinfo(h::SYSTEM, SYSINFO_TICKS).max(0) as u64;
            println!(out, "{} ticks (~{} s, assumes default PIT rate)", t, approx_seconds(t));
        }
        Command::Security => security(out),
        Command::Echo(text) => println!(out, "{}", text),
        Command::Run(name) => run_program(out, name),
    }
}

fn security(out: &mut Console) {
    let cpu = sysinfo(h::SYSTEM, SYSINFO_CPU).max(0) as u64;
    let on = |bit: u64| if cpu & bit != 0 { "on" } else { "off" };
    println!(out, "Facts about this build (not guarantees about the future):");
    println!(out, "  network:    no NIC driver or stack exists; no traffic is possible");
    println!(out, "  storage:    no storage driver exists; nothing is read or written");
    println!(out, "  isolation:  this shell and its programs run in ring 3, each in its own");
    println!(out, "              address space with only the handles it was given;");
    println!(out, "              the kernel and all drivers still run in ring 0");
    println!(out, "  cpu:        SMEP {}, SMAP {}", on(CPU_SMEP), on(CPU_SMAP));
    println!(out, "  telemetry:  none");
}

fn run_program(out: &mut Console, name: &str) {
    let Some(&(_, id, needs_console)) = PROGRAMS.iter().find(|(n, _, _)| *n == name) else {
        println!(out, "run: unknown program '{}' (hello | evil | spin | hello_rs)", name);
        return;
    };
    // Each program gets only what it needs: a console handle for the printing
    // ones (write right only, no grant), nothing for the others.
    let write_only = Rights::WRITE.bits() as u64;
    let grants: &[(u64, u64)] = if needs_console { &[(h::CONSOLE, write_only)] } else { &[] };

    let pid = spawn(h::SPAWNER, id, grants, &[], CHILD_BUDGET_TICKS);
    if pid < 0 {
        println!(out, "run failed (error {})", pid);
        return;
    }
    let raw = wait(pid as u64);
    if raw < 0 {
        println!(out, "wait failed (error {})", raw);
        return;
    }
    match Outcome::from_raw(raw as u64) {
        Outcome::Exited(code) => println!(out, "[process exited with code {}]", code),
        Outcome::PageFault => println!(out, "[process killed: illegal memory access (page fault)]"),
        Outcome::GeneralProtection => println!(out, "[process killed: protection fault]"),
        Outcome::InvalidOpcode => println!(out, "[process killed: invalid instruction]"),
        Outcome::DivideError => println!(out, "[process killed: divide error]"),
        Outcome::OtherFault => println!(out, "[process killed: CPU exception]"),
        Outcome::Deadlock => println!(out, "[process killed: blocked forever]"),
        Outcome::Timeout => println!(out, "[process killed: exceeded its time budget]"),
    }
}

entry!(main);
