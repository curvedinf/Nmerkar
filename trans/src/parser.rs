// parser.rs — recursive descent over the token stream, producing the AST.
// Expression precedence mirrors C for the supported subset:
//   assignment (right-assoc) → || → && → equality → relational
//   → additive → multiplicative → unary → postfix → primary
// Division and modulo are rejected ("no division in subset").

use crate::ast::*;
use crate::error::{Error, Result};
use crate::token::{Kind, Token};

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    loop_depth: usize,
}

impl Parser {
    pub fn new(toks: Vec<Token>) -> Self {
        Parser { toks, pos: 0, loop_depth: 0 }
    }

    pub fn parse_program(mut self) -> Result<Program> {
        let mut functions = Vec::new();
        while self.peek().kind != Kind::Eof {
            functions.push(self.function()?);
        }
        Ok(Program { functions })
    }

    // ---- cursor helpers ----

    fn peek(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn peek2(&self) -> &Token {
        self.toks.get(self.pos + 1).unwrap_or(self.toks.last().unwrap())
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn here(&self) -> (usize, usize) {
        (self.peek().line, self.peek().col)
    }

    fn expect_punct(&mut self, s: &str) -> Result<()> {
        if self.peek().is_punct(s) {
            self.bump();
            Ok(())
        } else {
            let (l, c) = self.here();
            Err(Error::new(l, c, format!("expected '{}', got '{}'", s, self.peek().text)))
        }
    }

    fn expect_ident(&mut self) -> Result<Token> {
        if self.peek().kind == Kind::Ident {
            Ok(self.bump())
        } else {
            let (l, c) = self.here();
            Err(Error::new(l, c, format!("expected identifier, got '{}'", self.peek().text)))
        }
    }

    // ---- declarations & functions ----

    /// Consume a sequence of type keywords and `*`s.
    fn type_spec(&mut self) {
        while self.peek().is_type_word() || self.peek().is_punct("*") {
            self.bump();
        }
    }

    fn function(&mut self) -> Result<Function> {
        let (l, c) = self.here();
        self.type_spec();
        let name = self.expect_ident()?.text;
        self.expect_punct("(")?;
        let mut params = Vec::new();
        loop {
            if self.peek().is_punct(")") {
                break;
            }
            if self.peek().is_punct(",") || self.peek().is_type_word() || self.peek().is_punct("*") {
                self.bump();
            } else {
                params.push(self.expect_ident()?.text);
            }
        }
        self.expect_punct(")")?;
        self.expect_punct("{")?;
        // block_stmts consumes the closing brace.
        let body = self.block_stmts()?;
        Ok(Function { name, params, body, line: l, col: c })
    }

    // ---- statements ----

    /// `{ stmt* }` — assumes the opening brace is consumed; consumes the brace.
    fn block_stmts(&mut self) -> Result<Vec<Stmt>> {
        let mut stmts = Vec::new();
        while !self.peek().is_punct("}") && self.peek().kind != Kind::Eof {
            stmts.push(self.stmt()?);
        }
        self.expect_punct("}")?;
        Ok(stmts)
    }

    fn stmt(&mut self) -> Result<Stmt> {
        let t = self.peek().clone();
        if t.is_type_word() {
            return self.declaration();
        }
        if t.kind == Kind::Ident {
            match t.text.as_str() {
                "return" => return self.return_stmt(),
                "if" => return self.if_stmt(),
                "while" => return self.while_stmt(),
                "for" => return self.for_stmt(),
                "do" => return self.do_stmt(),
                "break" => {
                    self.bump();
                    self.expect_punct(";")?;
                    if self.loop_depth == 0 {
                        return Err(Error::new(t.line, t.col, "break outside loop"));
                    }
                    return Ok(Stmt::Break { line: t.line, col: t.col });
                }
                "continue" => {
                    self.bump();
                    self.expect_punct(";")?;
                    if self.loop_depth == 0 {
                        return Err(Error::new(t.line, t.col, "continue outside loop"));
                    }
                    return Ok(Stmt::Continue { line: t.line, col: t.col });
                }
                _ => {}
            }
        }
        if t.is_punct("{") {
            self.bump();
            return Ok(Stmt::Block(self.block_stmts()?));
        }
        if t.is_punct(";") {
            self.bump();
            return Ok(Stmt::Empty);
        }
        let e = self.expr()?;
        self.expect_punct(";")?;
        Ok(Stmt::Expr(e))
    }

    fn declaration(&mut self) -> Result<Stmt> {
        self.type_spec();
        let mut decls = vec![self.declarator()?];
        while self.peek().is_punct(",") {
            self.bump();
            decls.push(self.declarator()?);
        }
        self.expect_punct(";")?;
        Ok(Stmt::Decl(decls))
    }

    fn declarator(&mut self) -> Result<Declarator> {
        let t = self.expect_ident()?;
        let init = if self.peek().is_punct("=") {
            self.bump();
            Some(self.expr()?)
        } else {
            None
        };
        Ok(Declarator { name: t.text, init, line: t.line, col: t.col })
    }

    fn return_stmt(&mut self) -> Result<Stmt> {
        let kw = self.bump();
        let value = self.expr()?;
        self.expect_punct(";")?;
        Ok(Stmt::Return { value, line: kw.line, col: kw.col })
    }

    fn if_stmt(&mut self) -> Result<Stmt> {
        self.bump();
        self.expect_punct("(")?;
        let cond = self.expr()?;
        self.expect_punct(")")?;
        let then = Box::new(self.stmt()?);
        let else_ = if self.peek().is_ident("else") {
            self.bump();
            Some(Box::new(self.stmt()?))
        } else {
            None
        };
        Ok(Stmt::If { cond, then, else_ })
    }

    fn while_stmt(&mut self) -> Result<Stmt> {
        self.bump();
        self.expect_punct("(")?;
        let cond = self.expr()?;
        self.expect_punct(")")?;
        self.loop_depth += 1;
        let body = Box::new(self.stmt()?);
        self.loop_depth -= 1;
        Ok(Stmt::While { cond, body })
    }

    fn do_stmt(&mut self) -> Result<Stmt> {
        self.bump();
        self.loop_depth += 1;
        let body = Box::new(self.stmt()?);
        self.loop_depth -= 1;
        if !self.peek().is_ident("while") {
            let (l, c) = self.here();
            return Err(Error::new(l, c, "expected 'while' after do-body"));
        }
        self.bump();
        self.expect_punct("(")?;
        let cond = self.expr()?;
        self.expect_punct(")")?;
        self.expect_punct(";")?;
        Ok(Stmt::DoWhile { body, cond })
    }

    fn for_stmt(&mut self) -> Result<Stmt> {
        self.bump();
        self.expect_punct("(")?;

        let init = if self.peek().is_punct(";") {
            self.bump();
            None
        } else if self.peek().is_type_word() {
            self.type_spec();
            let t = self.expect_ident()?;
            self.expect_punct("=")?;
            let value = self.expr()?;
            self.expect_punct(";")?;
            Some(ForInit::Decl(Declarator { name: t.text, init: Some(value), line: t.line, col: t.col }))
        } else {
            let e = self.expr()?;
            self.expect_punct(";")?;
            Some(ForInit::Expr(e))
        };

        let cond = if self.peek().is_punct(";") {
            self.bump();
            None
        } else {
            let e = self.expr()?;
            self.expect_punct(";")?;
            Some(e)
        };

        let post = if self.peek().is_punct(")") {
            None
        } else {
            Some(self.expr()?)
        };
        self.expect_punct(")")?;

        self.loop_depth += 1;
        let body = Box::new(self.stmt()?);
        self.loop_depth -= 1;
        Ok(Stmt::For { init, cond, post, body })
    }

    // ---- expressions ----

    pub fn expr(&mut self) -> Result<Expr> {
        self.assignment()
    }

    fn assignment(&mut self) -> Result<Expr> {
        // An assignment target must be a bare identifier directly followed
        // by an assignment operator.
        let next_is_assign = self.peek2().kind == Kind::Punct && Self::is_assign_op(&self.peek2().text);
        if self.peek().kind == Kind::Ident && next_is_assign {
            let name_tok = self.bump();
            let op_tok = self.bump();
            if op_tok.text == "/=" || op_tok.text == "%=" {
                return Err(Error::new(op_tok.line, op_tok.col, "no division in subset"));
            }
            let op = match op_tok.text.as_str() {
                "+=" => Some(BinOp::Add),
                "-=" => Some(BinOp::Sub),
                "*=" => Some(BinOp::Mul),
                _ => None,
            };
            let value = Box::new(self.assignment()?);
            return Ok(Expr {
                kind: ExprKind::Assign { name: name_tok.text, op, value },
                line: name_tok.line,
                col: name_tok.col,
            });
        }
        self.logical_or()
    }

    fn is_assign_op(s: &str) -> bool {
        matches!(s, "=" | "+=" | "-=" | "*=" | "/=" | "%=")
    }

    fn logical_or(&mut self) -> Result<Expr> {
        let mut lhs = self.logical_and()?;
        while self.peek().is_punct("||") {
            self.bump();
            let rhs = self.logical_and()?;
            lhs = Expr { kind: ExprKind::Logical { op: LogicOp::Or, lhs: Box::new(lhs), rhs: Box::new(rhs) }, line: 0, col: 0 };
        }
        Ok(lhs)
    }

    fn logical_and(&mut self) -> Result<Expr> {
        let mut lhs = self.bit_or()?;
        while self.peek().is_punct("&&") {
            self.bump();
            let rhs = self.bit_or()?;
            lhs = Expr { kind: ExprKind::Logical { op: LogicOp::And, lhs: Box::new(lhs), rhs: Box::new(rhs) }, line: 0, col: 0 };
        }
        Ok(lhs)
    }

    fn bit_or(&mut self) -> Result<Expr> {
        let mut lhs = self.bit_xor()?;
        while self.peek().is_punct("|") {
            self.bump();
            let rhs = self.bit_xor()?;
            lhs = Expr { kind: ExprKind::Binary { op: BinOp::BitOr, lhs: Box::new(lhs), rhs: Box::new(rhs) }, line: 0, col: 0 };
        }
        Ok(lhs)
    }

    fn bit_xor(&mut self) -> Result<Expr> {
        let mut lhs = self.bit_and()?;
        while self.peek().is_punct("^") {
            self.bump();
            let rhs = self.bit_and()?;
            lhs = Expr { kind: ExprKind::Binary { op: BinOp::BitXor, lhs: Box::new(lhs), rhs: Box::new(rhs) }, line: 0, col: 0 };
        }
        Ok(lhs)
    }

    fn bit_and(&mut self) -> Result<Expr> {
        let mut lhs = self.equality()?;
        while self.peek().is_punct("&") {
            self.bump();
            let rhs = self.equality()?;
            lhs = Expr { kind: ExprKind::Binary { op: BinOp::BitAnd, lhs: Box::new(lhs), rhs: Box::new(rhs) }, line: 0, col: 0 };
        }
        Ok(lhs)
    }

    fn equality(&mut self) -> Result<Expr> {
        let mut lhs = self.relational()?;
        while self.peek().is_punct("==") || self.peek().is_punct("!=") {
            let op = if self.bump().text == "==" { BinOp::Eq } else { BinOp::Ne };
            let rhs = self.relational()?;
            lhs = Expr { kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, line: 0, col: 0 };
        }
        Ok(lhs)
    }

    fn relational(&mut self) -> Result<Expr> {
        let mut lhs = self.shift()?;
        while matches!(self.peek().text.as_str(), "<" | "<=" | ">" | ">=") && self.peek().kind == Kind::Punct {
            let op = match self.bump().text.as_str() {
                "<" => BinOp::Lt,
                "<=" => BinOp::Le,
                ">" => BinOp::Gt,
                _ => BinOp::Ge,
            };
            let rhs = self.shift()?;
            lhs = Expr { kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, line: 0, col: 0 };
        }
        Ok(lhs)
    }

    fn shift(&mut self) -> Result<Expr> {
        let mut lhs = self.additive()?;
        while self.peek().is_punct("<<") || self.peek().is_punct(">>") {
            let op = if self.bump().text == "<<" { BinOp::Shl } else { BinOp::Shr };
            let rhs = self.additive()?;
            lhs = Expr { kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, line: 0, col: 0 };
        }
        Ok(lhs)
    }

    fn additive(&mut self) -> Result<Expr> {
        let mut lhs = self.multiplicative()?;
        while self.peek().is_punct("+") || self.peek().is_punct("-") {
            let op = if self.bump().text == "+" { BinOp::Add } else { BinOp::Sub };
            let rhs = self.multiplicative()?;
            lhs = Expr { kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, line: 0, col: 0 };
        }
        Ok(lhs)
    }

    fn multiplicative(&mut self) -> Result<Expr> {
        let mut lhs = self.unary()?;
        loop {
            let t = self.peek();
            if t.kind != Kind::Punct {
                break;
            }
            match t.text.as_str() {
                "*" => {
                    self.bump();
                    let rhs = self.unary()?;
                    lhs = Expr { kind: ExprKind::Binary { op: BinOp::Mul, lhs: Box::new(lhs), rhs: Box::new(rhs) }, line: 0, col: 0 };
                }
                "/" | "%" => {
                    return Err(Error::new(t.line, t.col, "no division in subset"));
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr> {
        let t = self.peek().clone();
        if t.kind == Kind::Punct {
            match t.text.as_str() {
                "-" => {
                    self.bump();
                    let operand = Box::new(self.unary()?);
                    return Ok(Expr { kind: ExprKind::Unary { op: UnOp::Neg, operand }, line: t.line, col: t.col });
                }
                "!" => {
                    self.bump();
                    let operand = Box::new(self.unary()?);
                    return Ok(Expr { kind: ExprKind::Unary { op: UnOp::Not, operand }, line: t.line, col: t.col });
                }
                "~" => {
                    self.bump();
                    let operand = Box::new(self.unary()?);
                    return Ok(Expr { kind: ExprKind::Unary { op: UnOp::BitNot, operand }, line: t.line, col: t.col });
                }
                "+" => {
                    self.bump();
                    return self.unary();
                }
                "++" | "--" => {
                    self.bump();
                    let name = self.expect_ident()?;
                    return Ok(Expr {
                        kind: ExprKind::Pre { name: name.text, delta: if t.text == "++" { 1 } else { -1 } },
                        line: t.line,
                        col: t.col,
                    });
                }
                _ => {}
            }
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr> {
        let e = self.primary()?;
        let t = self.peek().clone();
        if (t.is_punct("++") || t.is_punct("--")) && matches!(e.kind, ExprKind::Var(_)) {
            self.bump();
            let name = match e.kind {
                ExprKind::Var(n) => n,
                _ => unreachable!(),
            };
            return Ok(Expr {
                kind: ExprKind::Post { name, delta: if t.text == "++" { 1 } else { -1 } },
                line: t.line,
                col: t.col,
            });
        }
        if t.is_punct("++") || t.is_punct("--") {
            return Err(Error::new(t.line, t.col, "postfix ++ needs a variable"));
        }
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr> {
        let t = self.peek().clone();
        let (l, c) = (t.line, t.col);
        match t.kind {
            Kind::Int => {
                self.bump();
                Ok(Expr { kind: ExprKind::Int(t.text), line: l, col: c })
            }
            Kind::Str => {
                self.bump();
                Ok(Expr { kind: ExprKind::Str(t.text), line: l, col: c })
            }
            Kind::Char => {
                self.bump();
                Ok(Expr { kind: ExprKind::Char(t.text), line: l, col: c })
            }
            Kind::Punct if t.text == "(" => {
                self.bump();
                let e = self.expr()?;
                self.expect_punct(")")?;
                Ok(e)
            }
            Kind::Ident => {
                self.bump();
                match t.text.as_str() {
                    "argc" => Ok(Expr { kind: ExprKind::Argc, line: l, col: c }),
                    "stdout" => Ok(Expr { kind: ExprKind::Stream("stdout"), line: l, col: c }),
                    "stdin" => Ok(Expr { kind: ExprKind::Stream("stdin"), line: l, col: c }),
                    "stderr" => Ok(Expr { kind: ExprKind::Stream("stderr"), line: l, col: c }),
                    "argv" => {
                        self.expect_punct("[")?;
                        let index = Box::new(self.expr()?);
                        self.expect_punct("]")?;
                        Ok(Expr { kind: ExprKind::Argv(index), line: l, col: c })
                    }
                    "__byte" => {
                        self.expect_punct("(")?;
                        let inner = Box::new(self.expr()?);
                        self.expect_punct(")")?;
                        Ok(Expr { kind: ExprKind::Byte(inner), line: l, col: c })
                    }
                    "NULL" => Ok(Expr { kind: ExprKind::Null, line: l, col: c }),
                    "EOF" => Ok(Expr { kind: ExprKind::EofLit, line: l, col: c }),
                    _ => {
                        if self.peek().is_punct("(") {
                            self.bump();
                            let mut args = Vec::new();
                            if !self.peek().is_punct(")") {
                                args.push(self.expr()?);
                                while self.peek().is_punct(",") {
                                    self.bump();
                                    args.push(self.expr()?);
                                }
                            }
                            self.expect_punct(")")?;
                            Ok(Expr { kind: ExprKind::Call { name: t.text, args }, line: l, col: c })
                        } else if self.peek().is_punct("[") {
                            self.bump();
                            let index = Box::new(self.expr()?);
                            self.expect_punct("]")?;
                            Ok(Expr { kind: ExprKind::Index { name: t.text, index }, line: l, col: c })
                        } else {
                            Ok(Expr { kind: ExprKind::Var(t.text), line: l, col: c })
                        }
                    }
                }
            }
            _ => Err(Error::new(l, c, format!("unexpected token '{}'", t.text))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn parse_expr(src: &str) -> Expr {
        let toks = lex(src).unwrap();
        Parser::new(toks).expr().unwrap()
    }

    #[test]
    fn assignment_is_right_associative() {
        let e = parse_expr("a = b = 3");
        match e.kind {
            ExprKind::Assign { name, value, .. } => {
                assert_eq!(name, "a");
                assert!(matches!(value.kind, ExprKind::Assign { .. }));
            }
            _ => panic!("expected assignment"),
        }
    }

    #[test]
    fn precedence_add_binds_tighter_than_relational() {
        let e = parse_expr("a + b < c");
        match e.kind {
            ExprKind::Binary { op: BinOp::Lt, .. } => {}
            _ => panic!("expected < at top"),
        }
    }

    #[test]
    fn rejects_division() {
        let toks = lex("a / b").unwrap();
        assert!(Parser::new(toks).expr().is_err());
    }

    #[test]
    fn dangling_else_binds_inner() {
        let mut toks = lex("if (a) if (b) x = 1; else x = 2;").unwrap();
        // drop Eof, re-add so the cursor can stop cleanly after one stmt
        toks.pop();
        toks.push(Token { kind: Kind::Eof, text: String::new(), line: 0, col: 0 });
        let mut p = Parser::new(toks);
        let s = p.stmt().unwrap();
        match s {
            Stmt::If { else_: None, .. } => {}
            _ => panic!("outer if must not capture the else"),
        }
    }
}
