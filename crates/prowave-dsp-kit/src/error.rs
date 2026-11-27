//! Errors, with the wording of the original's messages.

use std::fmt;

/// What can go wrong when an effect is made from its arguments.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// `failed to parse <param>: <text>`
    Parse {
        effect: String,
        param: String,
        text: String,
    },
    /// `<param> out of range`
    Range { effect: String, param: String },
    /// Wrong number or form of arguments; carries the effect's usage line.
    Usage { effect: String, usage: String },
    /// Any other message.
    Message(String),
    /// A feature of the original that this port does not have yet.
    Unsupported(String),
    /// No effect has this name.
    UnknownEffect(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Parse {
                effect,
                param,
                text,
            } => write!(f, "{effect}: failed to parse {param}: {text}"),
            Error::Range { effect, param } => write!(f, "{effect}: {param}: value out of range"),
            Error::Usage { effect, usage } => write!(f, "usage: {effect} {usage}"),
            Error::Message(m) => f.write_str(m),
            Error::Unsupported(m) => write!(f, "not supported yet: {m}"),
            Error::UnknownEffect(n) => write!(f, "unknown effect: {n}"),
        }
    }
}

impl std::error::Error for Error {}

/// `Result` with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
