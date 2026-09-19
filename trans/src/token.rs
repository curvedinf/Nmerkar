// token.rs — token kinds and the token record.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Int,   // decimal or hex integer literal (raw text kept)
    Ident, // [A-Za-z_][A-Za-z0-9_]*
    Str,   // string literal, raw text including quotes
    Char,  // character literal, raw text including quotes
    Punct, // operator or punctuation, raw text
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: Kind,
    pub text: String,
    pub line: usize,
    pub col: usize,
}

impl Token {
    pub fn is_punct(&self, s: &str) -> bool {
        self.kind == Kind::Punct && self.text == s
    }

    pub fn is_ident(&self, s: &str) -> bool {
        self.kind == Kind::Ident && self.text == s
    }

    pub fn is_type_word(&self) -> bool {
        self.kind == Kind::Ident
            && matches!(
                self.text.as_str(),
                "int" | "char" | "void" | "long" | "short" | "unsigned" | "signed" | "const" | "static"
            )
    }
}

// Longest-match order matters: three-char operators before two-char, two-char
// before the single-char fallback in the lexer.
pub const MULTI_PUNCT: [&str; 17] = [
    "<<=", ">>=", "==", "!=", "<=", ">=", "&&", "||", "++", "--", "+=", "-=", "*=", "/=", "%=", "<<", ">>",
];
