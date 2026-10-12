//! Minimal terminal shell: line editor, command parser and dispatcher.
//!
//! No heap is used. Input is restricted to printable ASCII, so nothing a user
//! types can inject control characters into the VGA output.
//!
//! The shell is a plain kernel-mode loop today (there is no user mode). The
//! commands here only *report* state; none of them grants authority.

use crate::{input, print, println, time, usermode, vga_buffer};
pub use l_core::shell::{Command, EditEvent, LINE_CAPACITY, LineEditor, ParseError, parse};
use pc_keyboard::{DecodedKey, HandleControl, Keyboard, ScancodeSet1, layouts};

const PROMPT: &str = "l_os> ";

fn execute(cmd: Command<'_>) {
    match cmd {
        Command::Empty => {}
        Command::Help => {
            println!("Commands:");
            println!("  help      show this list");
            println!("  version   show OS version");
            println!("  uptime    show timer ticks since boot");
            println!("  security  show what isolation/network/storage exists");
            println!("  echo ...  print the arguments");
            println!("  run NAME  run a demo program in ring 3: hello | evil | spin");
            println!("  clear     clear the screen");
        }
        Command::Version => println!("Larp OS {} (x86_64, development build)", env!("CARGO_PKG_VERSION")),
        Command::Clear => vga_buffer::clear_screen(),
        Command::Uptime => {
            let t = time::ticks();
            println!("{} ticks (~{} s, assumes default PIT rate)", t, time::approx_seconds(t));
        }
        Command::Security => {
            println!("Facts about this build (not guarantees about the future):");
            println!("  network:    no NIC driver or stack exists; no traffic is possible");
            println!("  storage:    no storage driver exists; nothing is read or written");
            println!("  isolation:  ring 3 demo programs only (single process, no scheduler);");
            println!("              shell and kernel still share ring 0; see docs/THREAT_MODEL.md");
            println!("  cpu:        SMEP {}, SMAP {}",
                if crate::cpu::smep_enabled() { "on" } else { "off" },
                if crate::cpu::smap_enabled() { "on" } else { "off" });
            println!("  telemetry:  none");
        }
        Command::Echo(text) => println!("{}", text),
        Command::Run(name) => run_program(name),
    }
}

fn run_line(line: &str) {
    match parse(line) {
        Ok(cmd) => execute(cmd),
        Err(ParseError::Unknown(name)) => println!("unknown command: {} (try 'help')", name),
        Err(ParseError::UnexpectedArguments(name)) => println!("{}: unexpected arguments", name),
        Err(ParseError::MissingArgument(name)) => println!("{}: missing argument", name),
    }
}

fn run_program(name: &str) {
    use l_core::{caps::{Cap, Caps}, syscall::Outcome};

    // Each demo gets only the capabilities it needs; `evil` and `spin` get none.
    let (program, caps, budget) = match name {
        "hello" => (usermode::prog_hello(), Caps::NONE.with(Cap::Console), 18),
        "evil" => (usermode::prog_read_kernel(), Caps::NONE, 18),
        "spin" => (usermode::prog_spin(), Caps::NONE, 18),
        _ => {
            println!("run: unknown program '{}' (hello | evil | spin)", name);
            return;
        }
    };
    match usermode::run(program, caps, budget) {
        Ok(Outcome::Exited(code)) => println!("[process exited with code {}]", code),
        Ok(Outcome::PageFault) => println!("[process killed: illegal memory access (page fault)]"),
        Ok(Outcome::GeneralProtection) => println!("[process killed: protection fault]"),
        Ok(Outcome::InvalidOpcode) => println!("[process killed: invalid instruction]"),
        Ok(Outcome::DivideError) => println!("[process killed: divide error]"),
        Ok(Outcome::OtherFault) => println!("[process killed: CPU exception]"),
        Ok(Outcome::Timeout) => println!("[process killed: exceeded its time budget]"),
        Err(e) => println!("run failed: {:?}", e),
    }
}

// ------------------------------------------------------------------ main loop

/// Run the interactive shell forever. Interrupts must already be enabled.
pub fn run() -> ! {
    use x86_64::instructions::interrupts;

    // Keyboard layout is German (De105Key), as in the original implementation.
    let mut keyboard = Keyboard::new(ScancodeSet1::new(), layouts::De105Key, HandleControl::Ignore);
    let mut editor = LineEditor::new();

    println!("Type 'help' for a list of commands.");
    print!("{}", PROMPT);

    loop {
        // Avoid the lost-wakeup race between "queue empty?" and `hlt`.
        interrupts::disable();
        let scancode = match input::SCANCODES.pop() {
            Some(s) => s,
            None => {
                interrupts::enable_and_hlt();
                continue;
            }
        };
        interrupts::enable();

        let Ok(Some(event)) = keyboard.add_byte(scancode) else { continue };
        let Some(DecodedKey::Unicode(c)) = keyboard.process_keyevent(event) else { continue };

        match editor.handle_char(c) {
            EditEvent::Inserted(b) => print!("{}", b as char),
            EditEvent::Erased => vga_buffer::backspace(),
            EditEvent::Submit => {
                println!();
                run_line(editor.line());
                editor.clear();
                print!("{}", PROMPT);
            }
            EditEvent::Ignored | EditEvent::Full => {}
        }
    }
}

