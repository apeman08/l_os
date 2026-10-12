//! Shell line editor and command parser (pure logic).

pub const LINE_CAPACITY: usize = 128;

// ---------------------------------------------------------------- line editor

#[derive(Debug, PartialEq, Eq)]
pub enum EditEvent {
    /// A printable character was appended and should be echoed.
    Inserted(u8),
    /// The last character was removed.
    Erased,
    /// Enter was pressed; the line is ready.
    Submit,
    /// Input was rejected (non-printable / non-ASCII, or backspace on empty).
    Ignored,
    /// The buffer is full; the character was rejected.
    Full,
}

pub struct LineEditor {
    buf: [u8; LINE_CAPACITY],
    len: usize,
}

impl LineEditor {
    pub const fn new() -> Self {
        Self { buf: [0; LINE_CAPACITY], len: 0 }
    }

    pub fn handle_char(&mut self, c: char) -> EditEvent {
        match c {
            '\n' | '\r' => EditEvent::Submit,
            '\u{8}' | '\u{7f}' => {
                if self.len == 0 {
                    EditEvent::Ignored
                } else {
                    self.len -= 1;
                    EditEvent::Erased
                }
            }
            ' '..='~' => {
                if self.len >= LINE_CAPACITY {
                    EditEvent::Full
                } else {
                    self.buf[self.len] = c as u8;
                    self.len += 1;
                    EditEvent::Inserted(c as u8)
                }
            }
            _ => EditEvent::Ignored,
        }
    }

    pub fn line(&self) -> &str {
        // Only printable ASCII is ever stored, so this cannot fail.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    pub fn clear(&mut self) {
        self.buf = [0; LINE_CAPACITY];
        self.len = 0;
    }
}

/// Split `host:port` (port 1..=65535). Only the shape is checked here; the
/// kernel validates the host name strictly before it stores a grant.
pub fn parse_host_port(s: &str) -> Result<(&str, u16), ()> {
    let (host, port) = s.rsplit_once(':').ok_or(())?;
    if host.is_empty() || host.contains(':') || port.is_empty() || !port.bytes().all(|c| c.is_ascii_digit()) {
        return Err(());
    }
    match port.parse::<u16>() {
        Ok(p) if p != 0 => Ok((host, p)),
        _ => Err(()),
    }
}

// --------------------------------------------------------------------- parser

#[derive(Debug, PartialEq, Eq)]
pub enum Command<'a> {
    Empty,
    Help,
    Version,
    Clear,
    Uptime,
    Security,
    Echo(&'a str),
    /// `run <program> [args...]`: start a built-in program in its own process.
    /// The payload is everything after `run `.
    Run(&'a str),
    /// `net <subcommand> ...`: network management. The payload is everything
    /// after `net `.
    Net(&'a str),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError<'a> {
    Unknown(&'a str),
    UnexpectedArguments(&'a str),
    MissingArgument(&'a str),
}

pub fn parse(line: &str) -> Result<Command<'_>, ParseError<'_>> {
    let line = line.trim_matches(' ');
    if line.is_empty() {
        return Ok(Command::Empty);
    }
    let (name, rest) = match line.find(' ') {
        Some(i) => (&line[..i], line[i..].trim_start_matches(' ')),
        None => (line, ""),
    };
    let no_args = |cmd| {
        if rest.is_empty() { Ok(cmd) } else { Err(ParseError::UnexpectedArguments(name)) }
    };
    match name {
        "help" => no_args(Command::Help),
        "version" => no_args(Command::Version),
        "clear" => no_args(Command::Clear),
        "uptime" => no_args(Command::Uptime),
        "security" => no_args(Command::Security),
        "echo" => Ok(Command::Echo(rest)),
        "run" => if rest.is_empty() { Err(ParseError::MissingArgument(name)) } else { Ok(Command::Run(rest)) },
        "net" => if rest.is_empty() { Err(ParseError::MissingArgument(name)) } else { Ok(Command::Net(rest)) },
        _ => Err(ParseError::Unknown(name)),
    }
}


#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn parse_basic_commands() {
        assert_eq!(parse(""), Ok(Command::Empty));
        assert_eq!(parse("   "), Ok(Command::Empty));
        assert_eq!(parse("help"), Ok(Command::Help));
        assert_eq!(parse("  uptime  "), Ok(Command::Uptime));
        assert_eq!(parse("echo   a  b "), Ok(Command::Echo("a  b")));
        assert_eq!(parse("echo"), Ok(Command::Echo("")));
    }

    #[test]
    fn parse_run_and_net() {
        assert_eq!(parse("run hello"), Ok(Command::Run("hello")));
        assert_eq!(parse("run"), Err(ParseError::MissingArgument("run")));
        assert_eq!(parse("run fetch https://x.example/a"), Ok(Command::Run("fetch https://x.example/a")));
        assert_eq!(parse("net request a.example:443 60"), Ok(Command::Net("request a.example:443 60")));
        assert_eq!(parse("net"), Err(ParseError::MissingArgument("net")));
    }

    #[test]
    fn host_port_parsing() {
        assert_eq!(parse_host_port("example.com:443"), Ok(("example.com", 443)));
        assert_eq!(parse_host_port("10.0.2.2:18080"), Ok(("10.0.2.2", 18080)));
        for bad in ["example.com", ":443", "example.com:", "example.com:0", "example.com:65536", "example.com:44x", "a:b:443", "example.com:-1", ""] {
            assert!(parse_host_port(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert_eq!(parse("rm -rf /"), Err(ParseError::Unknown("rm")));
        assert_eq!(parse("HELP"), Err(ParseError::Unknown("HELP")));
        assert_eq!(parse("help me"), Err(ParseError::UnexpectedArguments("help")));
        assert_eq!(parse("netx request example.com:443"), Err(ParseError::Unknown("netx")));
    }

    #[test]
    fn editor_edits_and_submits() {
        let mut e = LineEditor::new();
        assert_eq!(e.handle_char('h'), EditEvent::Inserted(b'h'));
        assert_eq!(e.handle_char('x'), EditEvent::Inserted(b'x'));
        assert_eq!(e.handle_char('\u{8}'), EditEvent::Erased);
        assert_eq!(e.handle_char('i'), EditEvent::Inserted(b'i'));
        assert_eq!(e.line(), "hi");
        assert_eq!(e.handle_char('\n'), EditEvent::Submit);
        e.clear();
        assert_eq!(e.line(), "");
        assert_eq!(e.handle_char('\u{8}'), EditEvent::Ignored);
    }

    #[test]
    fn editor_rejects_control_and_non_ascii() {
        let mut e = LineEditor::new();
        for c in ['\u{1b}', '\t', '\u{0}', 'ä', '€', '\u{9b}'] {
            assert_eq!(e.handle_char(c), EditEvent::Ignored);
        }
        assert_eq!(e.line(), "");
    }

    #[test]
    fn editor_bounded() {
        let mut e = LineEditor::new();
        for _ in 0..LINE_CAPACITY {
            assert!(matches!(e.handle_char('a'), EditEvent::Inserted(_)));
        }
        assert_eq!(e.handle_char('b'), EditEvent::Full);
        assert_eq!(e.line().len(), LINE_CAPACITY);
    }

}
