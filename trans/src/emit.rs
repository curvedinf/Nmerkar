// emit.rs — AST → Nmerkar text (.n).
//
// Shape of the generated program (behaviorally identical to the previous
// self-hosted transpiler):
//
//   - a fixed import preamble (libc + `extern "stdout"`)
//   - a top-level register prologue `0 rv! 0 fr! 0 frv! 0 pt! ret` — those
//     four are assigned from several label bodies, so v14 requires them
//     declared shared at top level
//   - one label per C function; `main` becomes `entry:`
//   - every C variable is a unique slot `vN`, local to the function's body
//     (construct labels merge into that body; `_call` entries get fresh
//     frames, so no save/restore is needed around calls)
//   - control flow uses quotation labels: conditions/bodies are separate
//     labels defined after the function body (so the body's `ret` comes
//     first), referenced by `if`/`if_else`/`while` instructions
//   - calls are direct: arguments evaluate inline in the caller's body,
//     the callee's param binds pop them, and its ret leaves the return
//     value on the stack
//   - a `for` post-expression is a generated `nN` label invoked by the loop
//     body tail and by `continue`, so `continue` still increments
//   - early `return` from inside control flow binds `rv`, sets `fr`, and
//     the remaining statements of the enclosing list are wrapped behind
//     `fr 'c 'b if_else`; loop conditions are `fr`-guarded so loops
//     exit; the function epilogue returns `fr rv mul` (early value when
//     flagged, else 0) and resets the flag
//   - `return e;` in `main` is `e _call exit` (process exit code)

use crate::ast::*;
use crate::error::{Error, Result};
use std::collections::HashMap;

const PRELUDE: &str = concat!(
    "import c\"printf\"(ptr,...)->int\n",
    "import c\"fprintf\"(ptr,ptr,...)->int\n",
    "import c\"fputc\"(int,ptr)->int\n",
    "import c\"ungetc\"(int,ptr)->int\n",
    "import c\"putchar\"(int)->int\n",
    "import c\"getchar\"()->int\n",
    "import c\"fputs\"(ptr,ptr)->int\n",
    "import c\"fwrite\"(ptr,int,int,ptr)->int\n",
    "import c\"strcmp\"(ptr,ptr)->int\n",
    "import c\"strncmp\"(ptr,ptr,int)->int\n",
    "import c\"strcpy\"(ptr,ptr)->ptr\n",
    "import c\"exit\"(int)->void\n",
    "import c\"fread\"(ptr,int,int,ptr)->int\n",
    "import c\"fopen\"(ptr,ptr)->ptr\n",
    "import c\"fclose\"(ptr)->int\n",
    "import c\"fgetc\"(ptr)->int\n",
    "import c\"strstr\"(ptr,ptr)->ptr\n",
    "extern \"stdout\"\n",
    "extern \"stdin\"\n",
    "extern \"stderr\"\n",
);

/// Variadic externs this emitter declares, mapped to their fixed (non-`...`)
/// parameter count. The runtime calling convention for variadic externs is
/// [fixed params..., varargs..., vararg-count] — the count is pushed as the
/// topmost cell so the runtime never has to scan the stack for the format
/// string (a scan can be hijacked by pointer cells left deeper on the stack).
fn vararg_fixed_params(name: &str) -> Option<usize> {
    match name {
        "printf" => Some(1),
        "fprintf" => Some(2),
        _ => None,
    }
}

pub fn emit(prog: &Program) -> Result<String> {
    let mut e = Emitter::new();
    e.out.push_str(PRELUDE);
    // v14: declare the cross-body registers once at top level (plain names;
    // a name assigned in several label bodies must be shared). rv/fr carry
    // the early-return value/flag, frv is the epilogue temp, pt holds a call
    // result for `ret pt`. C-variable slots stay frame-local: every _call
    // entry gets a fresh local frame, so no call-save stack is needed.
    if !prog.functions.is_empty() {
        e.out.push_str("0 rv! 0 fr! 0 frv! 0 pt!\nret\n");
    }
    for f in &prog.functions {
        e.function(f)?;
    }
    Ok(e.out.clone())
}

struct FnCtx {
    is_main: bool,
    slots: HashMap<String, String>,
    /// Innermost-loop stack: `Some(inc_label)` for `for` loops (whose
    /// `continue` must run the post-expression first), `None` for
    /// `while`/`do-while`.
    loops: Vec<Option<String>>,
}

struct Emitter {
    out: String,
    /// Function-level label buffers flushed after every construct label.
    /// k-call wrappers and for-post labels go to `hoisted`; early-return
    /// guard labels go to `hoisted_guards`. Nmerkar's break/continue
    /// validation is a linear scan that treats a `_call`-target label as a
    /// new non-loop context, so a guard body containing `continue`/`break`
    /// must be defined BEFORE any call-wrapper label: guards flush first.
    hoisted: String,
    hoisted_guards: String,
    next_label: usize,
    next_slot: usize,
}

/// Where a statement list lives — decides how a bare `return` terminates.
#[derive(Clone, Copy, PartialEq)]
enum ListCtx {
    /// The function's own body: `return` falls through to the epilogue.
    FnBody,
    /// Inside a generated label body (construct/guard): `return` ends the
    /// label with `0 ret`.
    LabelBody,
}

/// Output pair for statement-list emission: inline code plus label
/// definitions that must follow it at the function's top level.
struct Buf {
    code: String,
    labels: String,
}

impl Buf {
    fn new() -> Self {
        Buf { code: String::new(), labels: String::new() }
    }
}

impl Emitter {
    fn new() -> Self {
        Emitter { out: String::new(), hoisted: String::new(), hoisted_guards: String::new(), next_label: 0, next_slot: 0 }
    }

    fn fresh_label(&mut self) -> usize {
        self.next_label += 1;
        self.next_label
    }

    fn fresh_slot(&mut self) -> String {
        let s = format!("v{}", self.next_slot);
        self.next_slot += 1;
        s
    }

    fn function(&mut self, f: &Function) -> Result<()> {
        let is_main = f.name == "main";
        let mut header = if is_main {
            "entry:\n".to_string()
        } else {
            format!("{}:\n", f.name)
        };

        let mut ctx = FnCtx { is_main, slots: HashMap::new(), loops: Vec::new() };
        // main becomes `entry:` and runs with nothing on the stack: its
        // parameters (conventionally argc/argv, which the parser maps to
        // builtins anyway) are accepted but not bound.
        if !is_main {
            for p in &f.params {
                let slot = self.fresh_slot();
                ctx.slots.insert(p.clone(), slot.clone());
                header.push_str(&format!("{}!\n", slot));
            }
        }

        let mut body = Buf::new();
        self.hoisted.clear();
        self.hoisted_guards.clear();
        let terminated = self.stmt_list(&mut body, &f.body, &mut ctx, ListCtx::FnBody)?;
        debug_assert!(!terminated || !ctx.is_main, "main body ends in exit; list handled it");

        let epilogue = if is_main {
            "0 _call exit\nret\n"
        } else {
            "fr rv mul frv! 0 fr! ret frv\n"
        };

        self.out.push_str(&header);
        self.out.push_str(&body.code);
        self.out.push_str(epilogue);
        self.out.push_str(&body.labels);
        self.out.push_str(&self.hoisted_guards.clone());
        self.out.push_str(&self.hoisted.clone());
        Ok(())
    }

    /// Emit a statement list into `buf`. Returns Ok(true) when the list
    /// ended in a `return` — the enclosing label body already carries its
    /// `0 ret` terminator and everything after it was unreachable.
    fn stmt_list(&mut self, buf: &mut Buf, stmts: &[Stmt], ctx: &mut FnCtx, lctx: ListCtx) -> Result<bool> {
        let mut idx = 0;
        while idx < stmts.len() {
            let s = &stmts[idx];
            if matches!(s, Stmt::Return { .. }) {
                let terminated = self.return_stmt(buf, s, ctx, lctx)?;
                // Code after a top-level return is unreachable in C.
                return Ok(terminated);
            }
            self.stmt(buf, s, ctx, lctx)?;
            if s.contains_return() {
                // A return is reachable inside `s`; when it fired, the rest
                // of this list must be skipped via the early-return flag.
                let c = self.fresh_label();
                let b = self.fresh_label();
                buf.code.push_str(&format!("fr 'c{c} 'b{b} if_else\n"));
                let mut rest = Buf::new();
                let terminated = self.stmt_list(&mut rest, &stmts[idx + 1..], ctx, ListCtx::LabelBody)?;
                self.hoisted_guards.push_str(&format!("b{b}:\n{}", rest.code));
                if !terminated {
                    self.hoisted_guards.push_str("0 ret\n");
                }
                self.hoisted_guards.push_str(&rest.labels);
                self.hoisted_guards.push_str(&format!("c{c}:\n0 ret\n"));
                return Ok(false);
            }
            idx += 1;
        }
        Ok(false)
    }

    /// Emit a `return`. Returns true when the emitted text itself ends a
    /// label body with `ret` (only the non-main LabelBody form does —
    /// `main`'s `_call exit` never returns at runtime but is not a `ret`
    /// instruction, so its enclosing label body still needs a terminator).
    fn return_stmt(&mut self, buf: &mut Buf, s: &Stmt, ctx: &mut FnCtx, lctx: ListCtx) -> Result<bool> {
        let Stmt::Return { value, .. } = s else { unreachable!() };
        self.expr(buf, value, ctx)?;
        if ctx.is_main {
            buf.code.push_str("_call exit\n");
            Ok(false)
        } else if lctx == ListCtx::LabelBody {
            buf.code.push_str("rv! 1 fr! 0 ret\n");
            Ok(true)
        } else {
            buf.code.push_str("rv! 1 fr!\n");
            Ok(false)
        }
    }

    fn stmt(&mut self, buf: &mut Buf, s: &Stmt, ctx: &mut FnCtx, lctx: ListCtx) -> Result<()> {
        match s {
            Stmt::Decl(decls) => {
                for d in decls {
                    let slot = self.declare(ctx, d)?;
                    if let Some(init) = &d.init {
                        self.expr(buf, init, ctx)?;
                        buf.code.push_str(&format!("{}!\n", slot));
                    }
                }
            }
            Stmt::Expr(e) => {
                self.expr(buf, e, ctx)?;
                buf.code.push('\n');
            }
            Stmt::Empty => {}
            Stmt::Break { .. } => buf.code.push_str("break\n"),
            Stmt::Continue { .. } => match ctx.loops.last() {
                // PushAddr jump into the post label (same frame, like the
                // loop tail) — a `_call` would push a fresh frame and the
                // post's slot writes would miss the loop's locals
                Some(Some(inc)) => buf.code.push_str(&format!("1 '{inc} if continue\n")),
                _ => buf.code.push_str("continue\n"),
            },
            // Blocks are transparent: same buffers, same context. A return
            // inside is handled by this list's own recursion; the caller's
            // contains_return check guards the caller's remainder.
            Stmt::Block(stmts) => {
                self.stmt_list(buf, stmts, ctx, lctx)?;
            }
            Stmt::Return { .. } => unreachable!("handled in stmt_list"),
            Stmt::If { cond, then, else_ } => {
                // The condition evaluates once, at the instruction site.
                self.expr(buf, cond, ctx)?;
                let t = self.fresh_label();
                let e_label = else_.as_ref().map(|_| self.fresh_label());
                match e_label {
                    Some(e) => buf.code.push_str(&format!("'i{t} 'e{e} if_else\n")),
                    None => buf.code.push_str(&format!("'i{t} if\n")),
                }
                let mut then_buf = Buf::new();
                let terminated = self.stmt_list(&mut then_buf, stmt_of(then), ctx, ListCtx::LabelBody)?;
                buf.labels.push_str(&format!("i{t}:\n{}", then_buf.code));
                if !terminated {
                    buf.labels.push_str("0 ret\n");
                }
                buf.labels.push_str(&then_buf.labels);
                if let Some(els) = else_ {
                    let e = e_label.unwrap();
                    let mut else_buf = Buf::new();
                    let terminated = self.stmt_list(&mut else_buf, stmt_of(els), ctx, ListCtx::LabelBody)?;
                    buf.labels.push_str(&format!("e{e}:\n{}", else_buf.code));
                    if !terminated {
                        buf.labels.push_str("0 ret\n");
                    }
                    buf.labels.push_str(&else_buf.labels);
                }
            }
            Stmt::While { cond, body } => {
                let c = self.fresh_label();
                let b = self.fresh_label();
                let mut cond_buf = Buf::new();
                self.expr(&mut cond_buf, cond, ctx)?;
                buf.labels.push_str(&cond_buf.labels);
                buf.labels.push_str(&format!("c{c}:\nfr not {}and ret\n", cond_buf.code));
                ctx.loops.push(None);
                let mut body_buf = Buf::new();
                let terminated = self.stmt_list(&mut body_buf, stmt_of(body), ctx, ListCtx::LabelBody)?;
                ctx.loops.pop();
                buf.labels.push_str(&format!("b{b}:\n{}", body_buf.code));
                if !terminated {
                    buf.labels.push_str("0 ret\n");
                }
                buf.labels.push_str(&body_buf.labels);
                buf.code.push_str(&format!("'c{c} 'b{b} while\n"));
            }
            Stmt::DoWhile { body, cond } => {
                let df = self.fresh_label();
                let c = self.fresh_label();
                let b = self.fresh_label();
                // df=1 forces the first condition pass true — `'c 'b while`
                // checks before the first body run, do-while must not.
                buf.code.push_str(&format!("1 df{df}!\n"));
                ctx.loops.push(None);
                let mut body_buf = Buf::new();
                body_buf.code.push_str(&format!("0 df{df}!\n"));
                let terminated = self.stmt_list(&mut body_buf, stmt_of(body), ctx, ListCtx::LabelBody)?;
                ctx.loops.pop();
                buf.labels.push_str(&format!("b{b}:\n{}", body_buf.code));
                if !terminated {
                    buf.labels.push_str("0 ret\n");
                }
                buf.labels.push_str(&body_buf.labels);
                let mut cond_buf = Buf::new();
                self.expr(&mut cond_buf, cond, ctx)?;
                buf.labels.push_str(&cond_buf.labels);
                buf.labels.push_str(&format!("c{c}:\nfr not df{df} {}or and ret\n", cond_buf.code));
                buf.code.push_str(&format!("'c{c} 'b{b} while\n"));
            }
            Stmt::For { init, cond, post, body } => {
                match init {
                    Some(ForInit::Decl(d)) => {
                        let slot = self.declare(ctx, d)?;
                        if let Some(init_expr) = &d.init {
                            self.expr(buf, init_expr, ctx)?;
                            buf.code.push_str(&format!("{}!\n", slot));
                        }
                    }
                    Some(ForInit::Expr(e)) => {
                        self.expr(buf, e, ctx)?;
                        buf.code.push('\n');
                    }
                    None => {}
                }
                let c = self.fresh_label();
                let b = self.fresh_label();
                let n = self.fresh_label();
                let inc = format!("n{n}");
                let mut cond_buf = Buf::new();
                match cond {
                    Some(cond_expr) => self.expr(&mut cond_buf, cond_expr, ctx)?,
                    None => cond_buf.code.push_str("1 "),
                }
                buf.labels.push_str(&cond_buf.labels);
                buf.labels.push_str(&format!("c{c}:\nfr not {}and ret\n", cond_buf.code));
                ctx.loops.push(Some(inc.clone()));
                let mut body_buf = Buf::new();
                let terminated = self.stmt_list(&mut body_buf, stmt_of(body), ctx, ListCtx::LabelBody)?;
                ctx.loops.pop();
                body_buf.code.push_str(&format!("fr not '{inc} if\n"));
                buf.labels.push_str(&format!("b{b}:\n{}", body_buf.code));
                if !terminated {
                    buf.labels.push_str("0 ret\n");
                }
                buf.labels.push_str(&body_buf.labels);
                let mut post_buf = Buf::new();
                if let Some(post_expr) = post {
                    self.expr(&mut post_buf, post_expr, ctx)?;
                }
                self.hoisted.push_str(&post_buf.labels);
                self.hoisted.push_str(&format!("{inc}:\n{}0 ret\n", post_buf.code));
                buf.code.push_str(&format!("'c{c} 'b{b} while\n"));
            }
        }
        Ok(())
    }

    fn declare(&mut self, ctx: &mut FnCtx, d: &Declarator) -> Result<String> {
        if matches!(d.name.as_str(), "argc" | "argv" | "__byte" | "NULL" | "EOF" | "stdout" | "stdin" | "stderr") {
            return Err(Error::new(d.line, d.col, format!("cannot declare builtin '{}'", d.name)));
        }
        let slot = self.fresh_slot();
        ctx.slots.insert(d.name.clone(), slot.clone());
        Ok(slot)
    }

    fn slot_of(&self, ctx: &FnCtx, name: &str, line: usize, col: usize) -> Result<String> {
        ctx.slots
            .get(name)
            .cloned()
            .ok_or_else(|| Error::new(line, col, format!("undefined variable: {name}")))
    }

    /// Emit an expression's operand sequence into `code.code`; call
    /// subexpressions append their `kN` wrapper labels to `code.labels`.
    fn expr(&mut self, buf: &mut Buf, e: &Expr, ctx: &mut FnCtx) -> Result<()> {
        match &e.kind {
            ExprKind::Int(text) => buf.code.push_str(&format!("{text} ")),
            ExprKind::Str(text) => buf.code.push_str(&format!("{text} ")),
            ExprKind::Char(text) => {
                let v = decode_char(text).map_err(|m| Error::new(e.line, e.col, m))?;
                buf.code.push_str(&format!("{v} "));
            }
            ExprKind::Var(name) => {
                let slot = self.slot_of(ctx, name, e.line, e.col)?;
                buf.code.push_str(&format!("{slot} "));
            }
            ExprKind::Assign { name, op, value } => {
                let slot = self.slot_of(ctx, name, e.line, e.col)?;
                if let Some(op) = op {
                    buf.code.push_str(&format!("{slot} "));
                    self.expr(buf, value, ctx)?;
                    buf.code.push_str(&format!("{} ", binop_mnemonic(*op)));
                } else {
                    self.expr(buf, value, ctx)?;
                }
                buf.code.push_str(&format!("{slot}! "));
            }
            ExprKind::Binary { op, lhs, rhs } => {
                self.expr(buf, lhs, ctx)?;
                self.expr(buf, rhs, ctx)?;
                buf.code.push_str(&format!("{} ", binop_mnemonic(*op)));
            }
            ExprKind::Logical { op, lhs, rhs } => {
                // No short-circuit: both operands evaluate, then the
                // JS-style result is normalized to a C 0/1 bool.
                self.expr(buf, lhs, ctx)?;
                self.expr(buf, rhs, ctx)?;
                buf.code.push_str(match op {
                    LogicOp::And => "mul not not ",
                    LogicOp::Or => "or not not ",
                });
            }
            ExprKind::Unary { op, operand } => {
                match op {
                    UnOp::Neg => buf.code.push_str("0 "),
                    UnOp::BitNot => buf.code.push_str("-1 "),
                    UnOp::Not => {}
                }
                self.expr(buf, operand, ctx)?;
                buf.code.push_str(match op {
                    UnOp::Neg | UnOp::BitNot => "sub ",
                    UnOp::Not => "not ",
                });
            }
            ExprKind::Pre { name, delta } => {
                let slot = self.slot_of(ctx, name, e.line, e.col)?;
                let op = if *delta > 0 { "add" } else { "sub" };
                buf.code.push_str(&format!("{slot} 1 {op} {slot}! "));
            }
            ExprKind::Post { name, delta } => {
                let slot = self.slot_of(ctx, name, e.line, e.col)?;
                let op = if *delta > 0 { "add" } else { "sub" };
                // Net one cell (the old value): local stores pass through, so
                // the increment's store would leak a second cell under the
                // result — sink it into frv (free outside the epilogue).
                buf.code.push_str(&format!("{slot} pt! {slot} 1 {op} {slot}! frv! pt "));
            }
            ExprKind::Call { name, args } => {
                // malloc/free are Nmerkar opcodes (reserved words), not
                // importable call targets: emit the opcodes directly.
                if name == "malloc" && args.len() == 1 {
                    self.expr(buf, &args[0], ctx)?;
                    buf.code.push_str("malloc ");
                    return Ok(());
                }
                if name == "free" && args.len() == 1 {
                    self.expr(buf, &args[0], ctx)?;
                    buf.code.push_str("free ");
                    return Ok(());
                }
                // libc calls with exact Nmerkar-native equivalents are
                // emitted as ops — no FFI import, so they work under any
                // sandbox policy:
                //   strlen(s)    -> length   (op 75, str -> byte count)
                //   strcat(a,b)  -> concat   (op 36, returns a NEW string;
                //                the C destination is not mutated — use the
                //                return value, the subset convention)
                //   puts(s)      -> print    (op 54 prints the raw string
                //                and appends a newline — exactly puts; a 0
                //                stands in for the C return value)
                if name == "strlen" && args.len() == 1 {
                    self.expr(buf, &args[0], ctx)?;
                    buf.code.push_str("length ");
                    return Ok(());
                }
                if name == "strcat" && args.len() == 2 {
                    self.expr(buf, &args[0], ctx)?;
                    self.expr(buf, &args[1], ctx)?;
                    buf.code.push_str("concat ");
                    return Ok(());
                }
                if name == "puts" && args.len() == 1 {
                    self.expr(buf, &args[0], ctx)?;
                    buf.code.push_str("print 0 ");
                    return Ok(());
                }
                // v14: direct call. Arguments are evaluated inline in the
                // caller's body (same frame, so slot locals resolve); the
                // callee's param binds pop them and its ret leaves the single
                // return value on the stack. Fresh frames per _call make the
                // old kN wrapper (globals save/restore) unnecessary.
                for a in args {
                    self.expr(buf, a, ctx)?;
                }
                if let Some(fixed) = vararg_fixed_params(name) {
                    if args.len() < fixed {
                        return Err(Error::new(
                            e.line,
                            e.col,
                            format!("{name} expects at least {fixed} argument(s), got {}", args.len()),
                        ));
                    }
                    buf.code.push_str(&format!("{} ", args.len() - fixed));
                }
                buf.code.push_str(&format!("_call {name} "));
            }
            ExprKind::Index { name, index } => {
                let slot = self.slot_of(ctx, name, e.line, e.col)?;
                self.expr(buf, index, ctx)?;
                // Raw data pointer via strstr(s, ""), pointer+int arithmetic,
                // byte load, mask.
                buf.code.push_str(&format!("{slot} \"\" _call strstr add load 255 and "));
            }
            ExprKind::Argv(index) => {
                self.expr(buf, index, ctx)?;
                buf.code.push_str("8 mul extern \"nk_argv\" load add load ");
            }
            ExprKind::Argc => buf.code.push_str("extern \"nk_argc\" load "),
            ExprKind::Stream(name) => buf.code.push_str(&format!("extern \"{name}\" load ")),
            ExprKind::Byte(inner) => {
                self.expr(buf, inner, ctx)?;
                buf.code.push_str("load 255 and ");
            }
            ExprKind::Null => buf.code.push_str("0 "),
            ExprKind::EofLit => buf.code.push_str("-1 "),
        }
        Ok(())
    }
}

fn stmt_of(s: &Stmt) -> &[Stmt] {
    match s {
        Stmt::Block(stmts) => stmts,
        _ => std::slice::from_ref(s),
    }
}

fn binop_mnemonic(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Sub => "sub",
        BinOp::Mul => "mul",
        BinOp::Shl => "shl",
        BinOp::Shr => "shr",
        BinOp::Lt => "lt",
        BinOp::Le => "gt not",
        BinOp::Gt => "gt",
        BinOp::Ge => "lt not",
        BinOp::Eq => "eq",
        BinOp::Ne => "eq not",
        BinOp::BitAnd => "and",
        BinOp::BitOr => "or",
        BinOp::BitXor => "xor",
    }
}

fn decode_char(raw: &str) -> std::result::Result<i64, String> {
    let inner = &raw[1..raw.len().saturating_sub(1)];
    if inner.is_empty() {
        return Err("empty character literal".into());
    }
    if let Some(esc) = inner.strip_prefix('\\') {
        match esc {
            "n" => Ok(10),
            "t" => Ok(9),
            "r" => Ok(13),
            "0" => Ok(0),
            "\\" => Ok(92),
            "'" => Ok(39),
            _ => Err(format!("bad char escape: \\{esc}")),
        }
    } else {
        Ok(inner.as_bytes()[0] as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::Parser;

    fn transpile(src: &str) -> String {
        let toks = lex(src).unwrap();
        let prog = Parser::new(toks).parse_program().unwrap();
        emit(&prog).unwrap()
    }

    #[test]
    fn while_label_shape() {
        let out = transpile("int main() { int x = 0; while (x < 3) { x = x + 1; } return 0; }");
        assert!(out.contains("'c1 'b2 while"), "missing while instruction:\n{out}");
        assert!(out.contains("c1:\nfr not"), "missing guarded cond label:\n{out}");
        assert!(out.contains("b2:\n"), "missing body label:\n{out}");
    }

    #[test]
    fn calls_are_direct_v14() {
        let out = transpile("int g(int a) { return g(a); } int main() { return g(1); }");
        assert!(out.contains("1 _call g "), "missing direct call:\n{out}");
        assert!(!out.contains("k1:") && !out.contains("k2:"), "wrappers must be gone:\n{out}");
        assert!(!out.contains("svst"), "call-save stack must be gone:\n{out}");
        assert!(out.contains("0 rv! 0 fr! 0 frv! 0 pt!"), "missing register prologue:\n{out}");
    }

    #[test]
    fn early_return_guards_remainder() {
        let out = transpile("int f(int n) { if (n) { return 1; } return 0; }");
        assert!(out.contains("fr 'c"), "missing guard:\n{out}");
        assert!(out.contains("fr rv mul"), "missing epilogue:\n{out}");
    }

    #[test]
    fn for_continue_runs_post_label() {
        let out = transpile("int main() { int i; for (i = 0; i < 9; i++) { continue; } return 0; }");
        assert!(out.contains(" continue\n"), "missing continue:\n{out}");
        let def = out.find("n1:\n").or_else(|| out.find("n2:\n")).or_else(|| out.find("n3:\n"));
        assert!(def.is_some(), "missing post label definition:\n{out}");
        assert!(out.contains(" if continue\n"), "continue must run the post label frame-safely:\n{out}");
        assert!(!out.contains("_call n"), "continue must not _call the post label:\n{out}");
    }

    #[test]
    fn do_while_uses_df_flag() {
        let out = transpile("int main() { int u = 0; do { u = u + 1; } while (u < 1); return u; }");
        assert!(out.contains("1 df1!"), "missing df init:\n{out}");
        assert!(out.contains("0 df1!"), "missing df clear:\n{out}");
        assert!(out.contains("df1 "), "missing df cond guard:\n{out}");
    }
}
