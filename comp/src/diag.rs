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

/// Catch a compile error and return its `Diagnostic` payload (test-only).
/// The panic hook is silenced for the duration — the hook swap is
/// process-global, so callers serialize on one lock.
#[cfg(test)]
pub(crate) fn catch_diag<F: FnOnce() + std::panic::UnwindSafe>(f: F) -> Diagnostic {
    static HOOK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = HOOK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(f);
    std::panic::set_hook(prev);
    match r {
        Ok(()) => panic!("expected a compile error, but the call returned"),
        Err(p) => *p.downcast::<Diagnostic>().expect("panic payload is a Diagnostic"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_json_escapes_message_and_omits_absent_line() {
        let d = Diagnostic::user("lex", Some(3), "a\"b\\c\nd\te\u{1f}f".to_string());
        assert_eq!(
            d.to_json(),
            "{\"kind\":\"error\",\"stage\":\"lex\",\"line\":3,\"message\":\"a\\\"b\\\\c\\nd\\te\\u001ff\"}"
        );
        let d = Diagnostic::user("gen", None, "boom".to_string());
        assert_eq!(d.to_json(), "{\"kind\":\"error\",\"stage\":\"gen\",\"message\":\"boom\"}");
        assert!(!d.to_json().contains("\"line\""), "line: None must omit the field");
    }

    #[test]
    fn display_prints_only_the_message() {
        let d = Diagnostic::user("parse", Some(7), "something broke".to_string());
        assert_eq!(format!("{}", d), "something broke");
        assert!(!format!("{}", d).contains("parse"));
    }

    #[test]
    fn payload_message_prefers_diagnostic_then_strings() {
        // one branch of the downcast chain per payload type
        let d = Diagnostic::user("lex", None, "diag msg".to_string());
        assert_eq!(payload_message(&d), Some("diag msg".to_string()));
        let s = String::from("plain panic");
        assert_eq!(payload_message(&s), Some("plain panic".to_string()));
        let r = &"static str panic"[..];
        assert_eq!(payload_message(&r), Some("static str panic".to_string()));
        // any other payload type yields None
        assert_eq!(payload_message(&42i64), None);
    }
}

