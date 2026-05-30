//! Readline
use std::{
    io,
    sync::{Arc, Mutex, PoisonError},
    thread::sleep,
    time::Duration,
};

use nu_ansi_term::Color;
use rustyline::{
    error::ReadlineError, history::MemHistory, ColorMode, Config, Editor, ExternalPrinter,
};
use tracing_subscriber::fmt::{writer::BoxMakeWriter, MakeWriter};

use cuprated::logging::{ansi_enabled, eprintln_red};

use super::Line;

/// The interactive prompt string.
const PROMPT: &str = "cuprated> ";

/// Log targets emitted by `rustyline` that `cuprated` suppresses.
pub const SUPPRESSED_LOG_TARGETS: &[&str] = &["rustyline"];

type SharedPrinter = Arc<Mutex<dyn ExternalPrinter + Send>>;

/// The interactive command prompt.
pub struct Reader {
    editor: Editor<(), MemHistory>,
    prompt: String,
}

impl Reader {
    /// Read the next non-empty line of input, blocking until one is available.
    pub fn read_line(&mut self) -> Line {
        loop {
            match self.editor.readline(&(PROMPT, self.prompt.as_str())) {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    self.editor.add_history_entry(trimmed).ok();
                    return Line::Input(line);
                }
                Err(ReadlineError::Interrupted | ReadlineError::Eof) => return Line::Shutdown,
                Err(e) => {
                    eprintln!("Failed to read from stdin: {e}");
                    sleep(Duration::from_secs(1));
                }
            }
        }
    }
}

/// Handle for writing command output above the prompt.
pub struct CommandOutput(SharedPrinter);

impl CommandOutput {
    pub fn write_fmt(&self, args: std::fmt::Arguments<'_>) {
        let mut p = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        p.print(args.to_string()).ok();
    }
}

/// `MakeWriter` impl that routes `tracing` output through rustyline's
/// [`ExternalPrinter`].
struct Printer(SharedPrinter);

impl<'a> MakeWriter<'a> for Printer {
    type Writer = LineWriter;

    fn make_writer(&'a self) -> LineWriter {
        LineWriter {
            buf: Vec::new(),
            printer: Arc::clone(&self.0),
        }
    }
}

/// Per-event writer for `tracing`'s fmt layer.
struct LineWriter {
    buf: Vec<u8>,
    printer: SharedPrinter,
}

impl io::Write for LineWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for LineWriter {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.buf);
        let s = String::from_utf8_lossy(&bytes).into_owned();
        let mut p = self.printer.lock().unwrap_or_else(PoisonError::into_inner);
        p.print(s).ok();
    }
}

/// Initialize the interactive prompt and the stdout side of `tracing`.
pub fn try_init() -> (Option<(Reader, CommandOutput)>, BoxMakeWriter) {
    let Some((reader, shared)) = build_reader() else {
        return (None, BoxMakeWriter::new(io::stdout));
    };
    (
        Some((reader, CommandOutput(Arc::clone(&shared)))),
        BoxMakeWriter::new(Printer(shared)),
    )
}

fn build_reader() -> Option<(Reader, SharedPrinter)> {
    if !io::IsTerminal::is_terminal(&io::stdin()) {
        return None;
    }

    let warn = |e: &ReadlineError| {
        eprintln_red(&format!("Failed to start interactive prompt: {e}"));
    };

    let color_mode = if ansi_enabled(&io::stdout()) {
        ColorMode::Enabled
    } else {
        ColorMode::Disabled
    };
    let config = Config::builder().color_mode(color_mode).build();

    let mut editor: Editor<(), MemHistory> = Editor::with_history(config, MemHistory::new())
        .inspect_err(warn)
        .ok()?;
    editor.set_helper(Some(()));

    let printer = editor.create_external_printer().inspect_err(warn).ok()?;
    let shared: SharedPrinter = Arc::new(Mutex::new(printer));

    let prompt = Color::Blue.bold().paint(PROMPT).to_string();
    Some((Reader { editor, prompt }, shared))
}
