//! Errors from reading, writing, and replaying traces.

use std::fmt;

/// Something went wrong with a trace.
#[derive(Debug)]
pub struct Error {
    message: String,
}

impl Error {
    /// Build an error.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The explanation.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<watoots::Error> for Error {
    fn from(err: watoots::Error) -> Self {
        Self::new(err.message())
    }
}

impl From<Error> for watoots::Error {
    fn from(err: Error) -> Self {
        Self::invalid_argument(err.message)
    }
}

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;
