// error.rs — positioned diagnostics shared by every phase.

use std::fmt;

#[derive(Debug, Clone)]
pub struct Error {
    pub line: usize,
    pub col: usize,
    pub msg: String,
}

impl Error {
    pub fn new(line: usize, col: usize, msg: impl Into<String>) -> Self {
        Error { line, col, msg: msg.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
