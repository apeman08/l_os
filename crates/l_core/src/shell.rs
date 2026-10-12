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
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError<'a> {
    Unknown(&'a str),
    UnexpectedArguments(&'a str),
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
    fn parse_rejects_bad_input() {
        assert_eq!(parse("rm -rf /"), Err(ParseError::Unknown("rm")));
        assert_eq!(parse("HELP"), Err(ParseError::Unknown("HELP")));
        assert_eq!(parse("help me"), Err(ParseError::UnexpectedArguments("help")));
        assert_eq!(parse("net request example.com:443"), Err(ParseError::Unknown("net")));
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
