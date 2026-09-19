// lexer.rs — hand-rolled scanner for the C subset. Produces positioned
// tokens; whitespace, comments, and preprocessor lines are skipped.

use crate::error::{Error, Result};
use crate::token::{Kind, Token, MULTI_PUNCT};

pub fn lex(src: &str) -> Result<Vec<Token>> {
    let b = src.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut col = 1usize;

    macro_rules! bump {
        () => {{
            if b[i] == b'\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
            i += 1;
        }};
    }

    while i < b.len() {
        let (tl, tc) = (line, col);
        let c = b[i];

        // whitespace
        if c == b' ' || c == b'\t' || c == b'\r' || c == b'\n' {
            bump!();
            continue;
        }
        // // line comment
        if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                bump!();
            }
            continue;
        }
        // /* block comment */
        if c == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            bump!();
            bump!();
            loop {
                if i >= b.len() {
                    return Err(Error::new(tl, tc, "unterminated block comment"));
                }
                if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                    bump!();
                    bump!();
                    break;
                }
                bump!();
            }
            continue;
        }
        // preprocessor line
        if c == b'#' {
            while i < b.len() && b[i] != b'\n' {
                bump!();
            }
            continue;
        }
        // string literal: "..." with backslash escapes
        if c == b'"' {
            let start = i;
            bump!();
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    bump!();
                }
                if i >= b.len() {
                    break;
                }
                bump!();
            }
            if i >= b.len() {
                return Err(Error::new(tl, tc, "unterminated string literal"));
            }
            bump!(); // closing quote
            toks.push(Token {
                kind: Kind::Str,
                text: String::from_utf8_lossy(&b[start..i]).into_owned(),
                line: tl,
                col: tc,
            });
            continue;
        }
        // character literal: '...' with backslash escapes
        if c == b'\'' {
            let start = i;
            bump!();
            while i < b.len() && b[i] != b'\'' {
                if b[i] == b'\\' {
                    bump!();
                }
                if i >= b.len() {
                    break;
                }
                bump!();
            }
            if i >= b.len() {
                return Err(Error::new(tl, tc, "unterminated character literal"));
            }
            bump!(); // closing quote
            toks.push(Token {
                kind: Kind::Char,
                text: String::from_utf8_lossy(&b[start..i]).into_owned(),
                line: tl,
                col: tc,
            });
            continue;
        }
        // number: 0x hex or decimal
        if c.is_ascii_digit() {
            let start = i;
            if c == b'0' && i + 1 < b.len() && (b[i + 1] == b'x' || b[i + 1] == b'X') {
                bump!();
                bump!();
                while i < b.len() && b[i].is_ascii_hexdigit() {
                    bump!();
                }
                if i - start <= 2 {
                    return Err(Error::new(tl, tc, "malformed hex literal"));
                }
            } else {
                while i < b.len() && b[i].is_ascii_digit() {
                    bump!();
                }
            }
            toks.push(Token {
                kind: Kind::Int,
                text: String::from_utf8_lossy(&b[start..i]).into_owned(),
                line: tl,
                col: tc,
            });
            continue;
        }
        // identifier
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                bump!();
            }
            toks.push(Token {
                kind: Kind::Ident,
                text: String::from_utf8_lossy(&b[start..i]).into_owned(),
                line: tl,
                col: tc,
            });
            continue;
        }
        // multi-char operators, longest first
        let rest = &src[i..];
        if let Some(op) = MULTI_PUNCT.iter().find(|op| rest.starts_with(**op)) {
            let text = op.to_string();
            for _ in 0..text.len() {
                bump!();
            }
            toks.push(Token { kind: Kind::Punct, text, line: tl, col: tc });
            continue;
        }
        // any other single printable byte becomes a punct token
        if c.is_ascii_graphic() {
            let text = (c as char).to_string();
            bump!();
            toks.push(Token { kind: Kind::Punct, text, line: tl, col: tc });
            continue;
        }
        return Err(Error::new(tl, tc, format!("unexpected character 0x{:02x}", c)));
    }

    toks.push(Token { kind: Kind::Eof, text: String::new(), line, col });
    Ok(toks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(Kind, String)> {
        lex(src).unwrap().into_iter().filter(|t| t.kind != Kind::Eof).map(|t| (t.kind, t.text)).collect()
    }

    #[test]
    fn skips_comments_and_preprocessor() {
        let ks = kinds("// line\n/* block * still */ #include <x>\nint");
        assert_eq!(ks, vec![(Kind::Ident, "int".into())]);
    }

    #[test]
    fn lexes_multichar_ops_longest_first() {
        let ks = kinds("a <<= b <<= c");
        assert_eq!(ks[1], (Kind::Punct, "<<=".into()));
        assert_eq!(ks[2], (Kind::Ident, "b".into()));
    }

    #[test]
    fn lexes_hex_and_strings() {
        let ks = kinds("0x1F \"a\\\"b\" 'z'");
        assert_eq!(ks[0], (Kind::Int, "0x1F".into()));
        assert_eq!(ks[1], (Kind::Str, "\"a\\\"b\"".into()));
        assert_eq!(ks[2], (Kind::Char, "'z'".into()));
    }

    #[test]
    fn tracks_positions() {
        let toks = lex("a\n  bb").unwrap();
        assert_eq!((toks[1].line, toks[1].col), (2, 3));
    }

    #[test]
    fn rejects_unterminated_string() {
        assert!(lex("\"abc").is_err());
    }
}
