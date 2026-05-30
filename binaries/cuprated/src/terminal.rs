//! Terminal
//!
//! `cuprated`'s interactive prompt, command handling, and stdout routing.

pub mod commands;

/// The outcome of reading one line from the prompt.
pub enum Line {
    /// A line of user input.
    Input(String),
    /// The user requested shutdown.
    Shutdown,
}

#[cfg(feature = "readline")]
mod readline;
#[cfg(feature = "readline")]
pub use readline::{try_init, CommandOutput, Reader, SUPPRESSED_LOG_TARGETS};

#[cfg(not(feature = "readline"))]
mod fallback;
#[cfg(not(feature = "readline"))]
pub use fallback::{try_init, CommandOutput, Reader, SUPPRESSED_LOG_TARGETS};
