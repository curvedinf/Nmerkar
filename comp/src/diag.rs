//! Structured compiler diagnostics.
//! User-facing compile errors are carried as `Diagnostic` payloads (via
//! `panic_any`) so the driver can present them cleanly with a stable exit
//! code; internal invariants keep plain `panic!`/`expect` and exit 101.

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiagKind {
    User,
    Internal,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub kind: DiagKind,
    pub stage: &'static str, // "lex" | "parse" | "gen" | "gen_js" | "emit" | "sandbox" | "driver"
    pub line: Option<u32>,   // source line when known
    pub message: String,     // full human message — same text as the historical panic payloads
}

impl Diagnostic {
    pub fn user(stage: &'static str, line: Option<u32>, message: String) -> Self {
        Diagnostic { kind: DiagKind::User, stage, line, message }
    }

    /// One-line machine-readable form for `--diag=json` (no external crates:
    /// a local escaper covers quotes, backslash, and control characters).
    pub fn to_json(&self) -> String {
        let message = json_escape(&self.message);
        match self.line {
            Some(line) => format!(
                "{{\"kind\":\"error\",\"stage\":\"{}\",\"line\":{},\"message\":\"{}\"}}",
                self.stage, line, message
            ),
            None => format!(
                "{{\"kind\":\"error\",\"stage\":\"{}\",\"message\":\"{}\"}}",
                self.stage, message
            ),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// Abort compilation with a Diagnostic payload. Unwinds exactly like a
/// `panic!`, but the driver's hook stays quiet and its catch presents the
/// message with the stable user-error exit code.
pub fn fail(d: Diagnostic) -> ! {
    std::panic::panic_any(d)
}

/// Extract a human message from a panic payload: `Diagnostic` first (return
/// its `.message`), then the `String`/`&str` payloads of plain `panic!`s.
/// Shared by the CLI driver and the wasm panic protocol.
pub fn payload_message(p: &(dyn std::any::Any + Send)) -> Option<String> {
    if let Some(d) = p.downcast_ref::<Diagnostic>() {
        return Some(d.message.clone());
    }
    if let Some(s) = p.downcast_ref::<String>() {
        return Some(s.clone());
    }
    if let Some(s) = p.downcast_ref::<&str>() {
        return Some(s.to_string());
    }
    None
}

/// Raise a user-facing compile error at `stage`, optionally carrying the
/// source `line`, with the same message text a `panic!` would have shown.
#[macro_export]
macro_rules! user_err {
    ($stage:expr, $line:expr, $($arg:tt)+) => {
        $crate::diag::fail($crate::diag::Diagnostic::user($stage, $line, format!($($arg)+)))
    };
}
