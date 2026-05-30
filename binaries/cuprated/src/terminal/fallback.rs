//! Fallback terminal
use std::{
    io::{self, Write},
    thread::sleep,
    time::Duration,
};

use tracing_subscriber::fmt::writer::BoxMakeWriter;

use super::Line;

pub const SUPPRESSED_LOG_TARGETS: &[&str] = &[];

/// The interactive command prompt.
pub struct Reader {
    stdin: io::Stdin,
}

impl Reader {
    pub fn read_line(&mut self) -> Line {
        loop {
            let mut line = String::new();
            match self.stdin.read_line(&mut line) {
                Ok(0) => return Line::Shutdown,
                Ok(_) => return Line::Input(line),
                Err(e) => {
                    eprintln!("Failed to read from stdin: {e}");
                    sleep(Duration::from_secs(1));
                }
            }
        }
    }
}

/// Handle for writing command output to stdout.
pub struct CommandOutput(io::Stdout);

impl CommandOutput {
    pub fn write_fmt(&self, args: std::fmt::Arguments<'_>) {
        self.0.lock().write_fmt(args).ok();
    }
}

/// Initialize the interactive prompt and the stdout side of `tracing`.
pub fn try_init() -> (Option<(Reader, CommandOutput)>, BoxMakeWriter) {
    let writer = BoxMakeWriter::new(io::stdout);
    if io::IsTerminal::is_terminal(&io::stdin()) {
        (
            Some((Reader { stdin: io::stdin() }, CommandOutput(io::stdout()))),
            writer,
        )
    } else {
        (None, writer)
    }
}
