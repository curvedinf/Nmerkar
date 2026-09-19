use crate::ast::*;
use crate::prelude::PRELUDE;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

// Set by gen() when --debug is active; read by emit_range() to skip register caching.
static UF_DEBUG: AtomicBool = AtomicBool::new(false);

// v13.1 strict arity: describe the guarded param pop at instruction pc as
// (label name, declared arity, param name) for the runtime die message.
fn sb_param_info(p: &Parsed, pc: usize) -> Option<(String, usize, String)> {
    let off = *p.param_pcs.get(&pc)?;
    let mut best_lpc = None;
    for (&lpc, _) in &p.label_params {
        if lpc <= pc && best_lpc.map_or(true, |b: usize| lpc > b) {
            best_lpc = Some(lpc);
        }
    }
    let lpc = best_lpc?;
    let params = p.label_params.get(&lpc)?;
    let name = params.get(off).map(|pr| match pr {
        Param::Local(n) | Param::Global(n) => n.clone(),
        Param::Discard => "_!".to_string(),
    })?;
    let label = p.labels.iter().find(|(_, &v)| v == lpc).map(|(k, _)| k.clone())?;
    Some((label, params.len(), name))
}

fn sb_die_expr(p: &Parsed, pc: usize) -> String {
    match sb_param_info(p, pc) {
        Some((label, arity, name)) => format!(
            "die(\"label '{}': missing parameter '{}' ({} parameter(s) declared, fewer present at runtime — null-fill was removed)\")",
            label, name, arity
        ),
        None => "die(\"missing label parameter at runtime — null-fill was removed\")".to_string(),
    }
}

// ---------------- codegen ----------------
pub fn c_type(t: &str) -> &'static str {
    match t {
        "int" => "int64_t",
        "float" => "double",
        "ptr" | "handle" => "void*",
        "byte" => "char",
        "void" => "void",
        _ => panic!("unknown C type {}", t),
    }
}

// Return type used in the generated extern function-pointer declaration.
// µFlux cells are 64-bit, but a libc function declared `->int` returns a C
// int (32-bit) in eax; declaring the pointer with C `int` makes the call
// site sign-extend the result into the 64-bit cell (e.g. fgetc's EOF).
pub fn c_retty(t: &str) -> &'static str {
    match t {
        "int" => "int",
        "float" => "double",
        "ptr" | "handle" => "void*",
        "byte" => "char",
        "void" => "void",
        _ => panic!("unknown C type {}", t),
    }
}

pub fn c_escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            '\0' => o.push_str("\\0"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\x{:02x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

// ---------------- basic-block stack virtualization with type specialization (v11) ----------
// Within a straight-line block, stack pushes/pops become C locals instead of
// traffic on the runtime ds. The virtual stack is flushed (spilled to the real
// ds, in order) before any instruction that is a jump target, transfers control,
// or otherwise needs the real stack. All fused operations go through the
// same uf_c* helpers the op_* functions use, so tag/float semantics are
// unchanged.
//
// Type specialization: each virtual stack entry carries an inferred type
// (Float, Int, or Unknown). When both operands of an arithmetic op are known,
// the codegen emits raw C double/int64_t arithmetic — no Cell tag checks,
// no branches. This lets the C compiler keep values in SSE registers and
// vectorize loops, matching C++ performance for numeric kernels.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VType {
    Float,
    Int,
    FloatArr, // marks a local holding a float-typed array handle
    Unknown,
}

#[derive(Clone)]
pub struct VEntry {
    pub expr: String,  // C expression (e.g. "t0" or "t0.i" or a raw double)
    pub ty: VType,
}

pub fn vpop(e: &mut String, vs: &mut Vec<VEntry>, n: &mut usize) -> VEntry {
    if let Some(t) = vs.pop() {
        t
    } else {
        let t = format!("t{}", *n);
        *n += 1;
        e.push_str(&format!("Cell {}=pop(cx);", t));
        VEntry { expr: t, ty: VType::Unknown }
    }
}
pub fn vpush(e: &mut String, vs: &mut Vec<VEntry>, n: &mut usize, init: &str, ty: VType) -> VEntry {
    let t = format!("t{}", *n);
    *n += 1;
    e.push_str(&format!("{} {}={};", c_type_of(ty), t, init));
    vs.push(VEntry { expr: t.clone(), ty });
    VEntry { expr: t, ty }
}
// Emit a Cell-typed push (for backward compatibility with ops that expect Cell)
pub fn vpush_cell(e: &mut String, vs: &mut Vec<VEntry>, n: &mut usize, init: &str) -> VEntry {
    vpush(e, vs, n, init, VType::Unknown)
}

fn c_type_of(ty: VType) -> &'static str {
    match ty {
        VType::Float => "double",
        VType::Int => "int64_t",
        VType::FloatArr | VType::Unknown => "Cell",
    }
}

// Extract the C expression to read a value from a VEntry for Cell-context ops
fn cell_of(v: &VEntry) -> String {
    match v.ty {
        VType::Unknown | VType::FloatArr => v.expr.clone(),
        VType::Float => format!("uf_mkf({})", v.expr),
        VType::Int => format!("uf_mki({})", v.expr),
    }
}

// Result type of a binary op given operand types
fn result_type(a: VType, b: VType) -> VType {
    match (a, b) {
        (VType::Float, VType::Float) => VType::Float,
        (VType::Int, VType::Int) => VType::Int,
        (VType::Float, VType::Int) | (VType::Int, VType::Float) => VType::Float,
        _ => VType::Unknown,
    }
}

// If the expression is a constant integer literal "NLL" where N is a power of 2,
// return the shift amount. Used for strength-reducing division/modulo.
fn parse_const_pow2(s: &str) -> Option<u32> {
    let s = s.trim_end_matches("LL");
    let n: i64 = s.parse().ok()?;
    if n > 0 && (n & (n - 1)) == 0 {
        Some(n.trailing_zeros())
    } else {
        None
    }
}

// An expression is "simple" if it has no parens: a single identifier, literal,
// or array subscript. Simple expressions can be safely inlined into compound
// expressions without creating a C temp variable, letting cc see constants and
// strength-reduce (e.g. (x / 2LL) -> (x >> 1)).
fn is_simple_expr(s: &str) -> bool {
    !s.contains('(') && !s.contains(')')
}

// If the vstack entry at `idx` has a compound expression, materialize it into a
// C temp so it isn't evaluated multiple times (dup/ovr copy the entry).
fn materialize_at(e: &mut String, vs: &mut Vec<VEntry>, n: &mut usize, idx: usize) {
    if idx < vs.len() && !is_simple_expr(&vs[idx].expr) {
        let name = format!("t{}", *n);
        *n += 1;
        e.push_str(&format!("{} {}={};", c_type_of(vs[idx].ty), name, vs[idx].expr));
        vs[idx].expr = name;
    }
}

// Push a binary-op result: inline (no temp) when both operands are simple,
// otherwise materialize as a temp variable as before.
fn vpush_cond(e: &mut String, vs: &mut Vec<VEntry>, n: &mut usize,
              result: String, ty: VType, a_expr: &str, b_expr: &str) {
    if is_simple_expr(a_expr) && is_simple_expr(b_expr) {
        vs.push(VEntry { expr: result, ty });
    } else {
        vpush(e, vs, n, &result, ty);
    }
}

// C expression reading a VEntry as a double, converting if needed.
// Unknown cells go through uf_f (tag dispatch: float bits / int -> double).
fn f64_expr(v: &VEntry) -> String {
    match v.ty {
        VType::Float => v.expr.clone(),
        VType::Int => format!("((double)({}))", v.expr),
        _ => format!("uf_f({})", v.expr),
    }
}

// Inference-only result type (used by the type pre-pass, never by codegen):
// comparisons always yield Int; in arithmetic a float operand forces float
// semantics at runtime (float dominates); bitwise ops require both Int.
fn infer_bin_type(h: &str, a: VType, b: VType) -> VType {
    match h {
        "op_lt" | "op_gt" | "op_lte" | "op_gte" | "op_eq" => VType::Int,
        "op_and" | "op_or" | "op_xor" => {
            if a == VType::Int && b == VType::Int { VType::Int } else { VType::Unknown }
        }
        _ => match (a, b) {
            // v13.1: arithmetic is polymorphic (arrays/matrices dispatch at
            // runtime), so an Unknown operand forces an Unknown result —
            // typing it Float would miscompile the handle as a double.
            (VType::Float, VType::Float) | (VType::Float, VType::Int) | (VType::Int, VType::Float) => VType::Float,
            (VType::Int, VType::Int) => VType::Int,
            _ => VType::Unknown,
        },
    }
}

// vcache: deferred variable stores — (var name, temp, dirty). Within a
// block, SetV writes only the cache and GetV reads from it; dirty temps are
// stored back to their globals at flush time. Distinct var globals cannot
// alias each other, so deferring/reordering the stores is unobservable
// inside the block.

// vcache: deferred variable stores — (var name, temp, dirty). Within a
// block, SetV writes only the cache and GetV reads from it; dirty temps are
// stored back to their globals at flush time. Distinct var globals cannot
// alias each other, so deferring/reordering the stores is unobservable
// inside the block.
pub fn vflush(e: &mut String, vs: &mut Vec<VEntry>, vc: &mut Vec<(String, String, bool)>) {
    for (v, t, d) in vc.drain(..) {
        if d {
            e.push_str(&format!("var_{}={};", v, t));
        }
    }
    for t in vs.drain(..) {
        match t.ty {
            VType::Unknown | VType::FloatArr => e.push_str(&format!("pushc(cx,{});", t.expr)),
            VType::Float => e.push_str(&format!("pushf(cx,{});", t.expr)),
            VType::Int => e.push_str(&format!("pushi(cx,{});", t.expr)),
        }
    }
}
// v12 boundary discard: write back dirty variable caches and drop unbound values.
pub fn vdiscard(e: &mut String, vs: &mut Vec<VEntry>, vc: &mut Vec<(String, String, bool)>) {
    for (v, t, d) in vc.drain(..) {
        if d {
            e.push_str(&format!("var_{}={};", v, t));
        }
    }
    vs.clear();
}

pub fn plab(prefix: &str, i: usize) -> String {
    if prefix.is_empty() {
        format!("L_{}", i)
    } else {
        format!("{}L{}", prefix, i)
    }
}

// v13: the data-stack pointer a body's guarded param pops compare against —
// the caller's saved sp (the just-pushed cs entry), or the outline's own
// pre-arg pointer, or 0 at the top level.
fn param_base(prefix: &str) -> &'static str {
    if prefix.starts_with("OB") {
        "_osp"
    } else {
        "(cx->csp>0?cx->rsps[cx->csp-1]:0)"
    }
}

// guard expression for the k-th reversed param pop: fires only while the
// caller's stack still holds cells above base + (arity-1-k)
fn base_off(base: &str, off: usize) -> String {
    if off > 0 {
        format!("{}+{}", base, off)
    } else {
        base.to_string()
    }
}

// A FOR body starting at instruction `bs` is inlinable if its terminating
// RET is the first RET in the range (no early returns) and no internal jump
// leaves the range. Returns the exclusive end (= the RET's index).
pub fn for_body_range(ins: &[Ins], bs: usize) -> Option<usize> {
    let mut j = bs;
    while j < ins.len() {
        if matches!(ins[j], Ins::Ret) {
            return Some(j);
        }
        j += 1;
    }
    None
}
pub fn inlinable_for(p: &Parsed, bs: usize, be: usize) -> bool {
    for (_j, ins) in p.ins.iter().enumerate().take(be).skip(bs) {
        match ins {
            Ins::Ret => return false,
            _ => {}
        }
    }
    true
}

// A label whose body (up to its first RET) is only BREAK/CONT is a trivial
// loop-exit continuation: `cond 'exit if` can be compiled as a direct
// conditional break/cont with no code-address push and no indirect jump.
pub fn trivial_loop_exit(p: &Parsed, l: &str) -> bool {
    match p.labels.get(l) {
        Some(&tb) => match for_body_range(&p.ins, tb) {
            Some(tbe) => tbe > tb && (tb..tbe).all(|k| matches!(p.ins[k], Ins::Break | Ins::Cont)),
            None => false,
        },
        None => false,
    }
}

// Emit instructions [start, end) as C, prefixing every label (including K_
// continuations) with `prefix` — used to inline FOR bodies as renamed copies
// so their internal jumps stay local to the copy.
#[allow(clippy::too_many_arguments)]
pub fn emit_range(
    o: &mut String,
    p: &Parsed,
    targets: &std::collections::HashSet<usize>,
    inline_fors: &HashMap<usize, (usize, usize)>,
    inline_ffolds: &HashMap<usize, (usize, usize)>,
    inline_whiles: &HashMap<usize, (usize, usize, usize, usize)>,
    outlined_bodies: &HashMap<usize, (usize, usize)>,
    suppress: &std::collections::HashSet<usize>,
    ext_idx: &HashMap<&str, usize>,
    start: usize,
    end: usize,
    prefix: &str,
    depth: usize,
    local_types: &mut HashMap<usize, VType>,
    ins_body: &[usize],
    reg: &HashMap<usize, (String, VType)>,
    numeric: &std::collections::HashSet<usize>,
    arr_ptr: &HashMap<String, String>,
    suppress_flush: bool,
) {
    let resolve = |name: &str| -> usize {
        *p.labels.get(name).unwrap_or_else(|| panic!("undefined label {}", name))
    };
    let mut vstack: Vec<VEntry> = Vec::new();
    let mut vcache: Vec<(String, String, bool)> = Vec::new();
    let mut vtmp = 0usize;
    // v13: stack of saved literal-start ds pointers (LIFO — literals nest)
    let mut lit_starts: Vec<usize> = Vec::new();
    for (i, ins) in p.ins.iter().enumerate().take(end).skip(start) {
        let mut e = String::new();
        if i > start && targets.contains(&i) {
            vdiscard(&mut e, &mut vstack, &mut vcache);
        }
        e.push_str(&format!("{}: ", plab(prefix, i)));
        if suppress.contains(&i) {
            // PushAddr feeding an inlined FOR: the address is compile-time
            // known, so the push is elided entirely.
            o.push_str(&e);
            continue;
        }
        // v13.1: fused weave-task GPU dispatch. At a compilable task's entry,
        // try one kernel launch over the task's inputs (peeked from the ds);
        // on success perform the ret epilogue (drain to the caller's saved
        // sp, jump back), on decline fall through to the CPU body below.
        if let Some((kidx, nin)) = crate::compute::task_kernel_at(i) {
            vflush(&mut e, &mut vstack, &mut vcache);
            e.push_str(&format!(
                "{{uf_cur_op=\"task_gpu\";Cell _g=uf_gpu_task(cx,{},{});if(!(_g.tag==T_INT&&!_g.i)){{{{if(cx->csp==0){{pushc(cx,_g);return;}}cx->csp--;const void* _gr=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_g);if(!_gr)return;goto *_gr;}}}}}}\n",
                kidx, nin
            ));
        }
        match ins {
            Ins::PushI(v) => {
                // Push literal directly: no C temp. This lets cc see constants
                // and strength-reduce (e.g. (x / 2LL) -> (x >> 1)).
                vstack.push(VEntry { expr: format!("{}LL", v), ty: VType::Int });
            }
            Ins::PushF(v) => {
                vstack.push(VEntry { expr: format!("{:?}", v), ty: VType::Float });
            }
            Ins::PushS(idx) => {
                vpush_cell(&mut e, &mut vstack, &mut vtmp, &format!("uf_mkp((void*)&uf_sl{})", idx));
            }
            Ins::PushAddr(l) => {
                // Peephole (with the Ins::If arm): `'exit if` where `exit` is
                // a trivial loop exit emits no address push — the If compiles
                // to a direct conditional break/cont.
                if matches!(p.ins.get(i + 1), Some(Ins::If))
                    && trivial_loop_exit(p, l)
                    && !targets.contains(&i)
                {
                    o.push_str(&e);
                    continue;
                }
                // v12: flush (do not discard) so values like the if-condition
                // that precede the address are materialized on the real stack.
                vflush(&mut e, &mut vstack, &mut vcache);
                // The target may live outside an inlined body range (e.g. a
                // nested FOR body); only in-range targets get the prefix.
                let t = resolve(l);
                let lab = if t >= start && t < end && !prefix.is_empty() {
                    plab(prefix, t)
                } else {
                    plab("", t)
                };
                e.push_str(&format!("pushp(cx,(void*)&&{});\n", lab))
            }
            Ins::Simple(h) => {
                let bin = match *h {
                    "op_add" => Some("uf_cadd"),
                    "op_sub" => Some("uf_csub"),
                    "op_mul" => Some("uf_cmul"),
                    "op_and" => Some("uf_cand"),
                    "op_div" => Some("uf_cdiv"),
                    "op_rem" => Some("uf_crem"),
                    "op_lt"  => Some("uf_clt"),
                    "op_gt"  => Some("uf_cgt"),
                    "op_lte" => Some("uf_clte"),
                    "op_gte" => Some("uf_cgte"),
                    "op_eq"  => Some("uf_ceq"),
                    "op_or"  => Some("uf_cor"),
                    "op_xor" => Some("uf_cxor"),
                    _ => None,
                };
                let un = match *h {
                    "op_shr" => Some("uf_cshr"),
                    "op_inc" => Some("uf_cinc"),
                    "op_dec" => Some("uf_cdec"),
                    "op_not" => Some("uf_cnot"),
                    _ => None,
                };
                if let Some(f) = bin {
                    let b = vpop(&mut e, &mut vstack, &mut vtmp);
                    let a = vpop(&mut e, &mut vstack, &mut vtmp);
                    let rt = result_type(a.ty, b.ty);
                    let is_arith = matches!(*h, "op_add"|"op_sub"|"op_mul"|"op_div"|"op_rem");
                    let is_cmp = matches!(*h, "op_lt"|"op_gt"|"op_lte"|"op_gte"|"op_eq");
                    // v13.1: raw-double fusion requires BOTH operand types known.
                    // An Unknown operand may be an array/matrix handle whose
                    // polymorphic dispatch lives in the uf_c* helpers.
                    if (is_arith || is_cmp)
                        && (a.ty == VType::Float || b.ty == VType::Float)
                        && a.ty != VType::Unknown && b.ty != VType::Unknown
                        && a.ty != VType::FloatArr && b.ty != VType::FloatArr
                    {
                        // Float-dominant: a known float operand forces float
                        // semantics at runtime, so emit raw double arithmetic
                        // and convert the other operand inline (no Cell helper).
                        let fa = f64_expr(&a);
                        let fb = f64_expr(&b);
                        match *h {
                            "op_add" => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("({} + {})", fa, fb), VType::Float, &fa, &fb); }
                            "op_sub" => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("({} - {})", fa, fb), VType::Float, &fa, &fb); }
                            "op_mul" => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("({} * {})", fa, fb), VType::Float, &fa, &fb); }
                            "op_div" => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("({} / {})", fa, fb), VType::Float, &fa, &fb); }
                            "op_rem" => { vpush(&mut e, &mut vstack, &mut vtmp, &format!("fmod({}, {})", fa, fb), VType::Float); }
                            "op_lt"  => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("(({} < {}) ? 1 : 0)", fa, fb), VType::Int, &fa, &fb); }
                            "op_gt"  => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("(({} > {}) ? 1 : 0)", fa, fb), VType::Int, &fa, &fb); }
                            "op_lte" => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("(({} <= {}) ? 1 : 0)", fa, fb), VType::Int, &fa, &fb); }
                            "op_gte" => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("(({} >= {}) ? 1 : 0)", fa, fb), VType::Int, &fa, &fb); }
                            "op_eq"  => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("(({} == {}) ? 1 : 0)", fa, fb), VType::Int, &fa, &fb); }
                            _ => unreachable!(),
                        }
                    } else if rt != VType::Unknown && a.ty != VType::Unknown && b.ty != VType::Unknown
                        && a.ty != VType::FloatArr && b.ty != VType::FloatArr
                        && (!matches!(*h, "op_and"|"op_or"|"op_xor") || rt == VType::Int) {
                        // Peephole: Int division/modulo by a constant power-of-2
                        // literal. GCC can't strength-reduce idivq→shift inside
                        // our monolithic function, so we do it ourselves. The
                        // dividend is non-negative in the overwhelmingly common
                        // case (loop induction math), so >> matches /.
                        if rt == VType::Int && matches!(*h, "op_div"|"op_rem") {
                            if let Some(shift) = parse_const_pow2(&b.expr) {
                                if *h == "op_div" {
                                    vpush_cond(&mut e, &mut vstack, &mut vtmp,
                                        format!("({} >> {})", a.expr, shift), VType::Int, &a.expr, &b.expr);
                                } else {
                                    vpush_cond(&mut e, &mut vstack, &mut vtmp,
                                        format!("({} & {})", a.expr, (1i64 << shift) - 1), VType::Int, &a.expr, &b.expr);
                                }
                                o.push_str(&e);
                                continue;
                            }
                        }
                        // Integer division / remainder must call runtime helpers
                        // so that division-by-zero is caught (try/retry) rather
                        // than constant-folded by the C compiler into SIGFPE.
                        if rt == VType::Int && matches!(*h, "op_div"|"op_rem") {
                            vpush_cell(&mut e, &mut vstack, &mut vtmp,
                                &format!("{}({},{})", f, cell_of(&a), cell_of(&b)));
                            o.push_str(&e);
                            continue;
                        }
                        let op_str = match *h {
                            "op_add" => Some("+"), "op_sub" => Some("-"),
                            "op_mul" => Some("*"),
                            "op_and" => Some("&"), "op_or" => Some("|"),
                            "op_xor" => Some("^"),
                            _ => None,
                        };
                        let cmp_str = match *h {
                            "op_lt" => Some("<"), "op_gt" => Some(">"),
                            "op_lte" => Some("<="), "op_gte" => Some(">="),
                            "op_eq" => Some("=="),
                            _ => None,
                        };
                        if let Some(op) = op_str {
                            vpush_cond(&mut e, &mut vstack, &mut vtmp,
                                format!("({} {} {})", a.expr, op, b.expr), rt, &a.expr, &b.expr);
                        } else if let Some(op) = cmp_str {
                            vpush_cond(&mut e, &mut vstack, &mut vtmp,
                                format!("(({} {} {}) ? 1 : 0)", a.expr, op, b.expr), VType::Int, &a.expr, &b.expr);
                        } else {
                            // rem: use fmod for float, % for int
                            if rt == VType::Float {
                                vpush(&mut e, &mut vstack, &mut vtmp,
                                    &format!("fmod({}, {})", a.expr, b.expr), VType::Float);
                            } else {
                                vpush(&mut e, &mut vstack, &mut vtmp,
                                    &format!("({} % {})", a.expr, b.expr), VType::Int);
                            }
                        }
                    } else {
                        // Fallback: unknown types, use Cell helpers
                        vpush_cell(&mut e, &mut vstack, &mut vtmp,
                            &format!("{}({},{})", f, cell_of(&a), cell_of(&b)));
                    }
                } else if let Some(f) = un {
                    let a = vpop(&mut e, &mut vstack, &mut vtmp);
                    if a.ty == VType::Float {
                        let op_str = match *h {
                            "op_inc" => Some("+1.0"),
                            "op_dec" => Some("-1.0"),
                            _ => None,
                        };
                        if let Some(op) = op_str {
                            vpush(&mut e, &mut vstack, &mut vtmp,
                                &format!("({} {})", a.expr, op), VType::Float);
                        } else {
                            // Wrap back to Cell for unsupported float ops
                            vpush_cell(&mut e, &mut vstack, &mut vtmp,
                                &format!("{}(uf_mkf({}))", f, a.expr));
                        }
                    } else if a.ty == VType::Int {
                        let op_str = match *h {
                            "op_inc" => Some("+1"),
                            "op_dec" => Some("-1"),
                            "op_shr" => Some(">>1"),
                            _ => None,
                        };
                        if let Some(op) = op_str {
                            vpush(&mut e, &mut vstack, &mut vtmp,
                                &format!("({} {})", a.expr, op), VType::Int);
                        } else if *h == "op_not" {
                            // !int → (int == 0) ? 1 : 0
                            vpush(&mut e, &mut vstack, &mut vtmp,
                                &format!("(({} == 0) ? 1 : 0)", a.expr), VType::Int);
                        } else {
                            // Wrap back to Cell for unsupported int ops
                            vpush_cell(&mut e, &mut vstack, &mut vtmp,
                                &format!("{}(uf_mki({}))", f, a.expr));
                        }
                    } else {
                        vpush_cell(&mut e, &mut vstack, &mut vtmp, &format!("{}({})", f, cell_of(&a)));
                    }
                } else {
                    match *h {
                        "op_idx" => {
                            let ix = vpop(&mut e, &mut vstack, &mut vtmp);
                            let hh = vpop(&mut e, &mut vstack, &mut vtmp);
                            let ix_c = match ix.ty {
                                VType::Unknown => format!("({}).i", ix.expr),
                                _ => ix.expr.clone(),
                            };
                            vpush_cell(&mut e, &mut vstack, &mut vtmp, &format!("uf_cidx({},{})", cell_of(&hh), ix_c));
                        }
                        "op_seti" => {
                            let v = vpop(&mut e, &mut vstack, &mut vtmp);
                            let ix = vpop(&mut e, &mut vstack, &mut vtmp);
                            let hh = vpop(&mut e, &mut vstack, &mut vtmp);
                            let ix_c = match ix.ty {
                                VType::Unknown => format!("({}).i", ix.expr),
                                _ => ix.expr.clone(),
                            };
                            e.push_str(&format!("uf_cseti({},{},{});", cell_of(&hh), ix_c, cell_of(&v)));
                        }
                        "op_vget" => {
                            let idx = vpop(&mut e, &mut vstack, &mut vtmp);
                            let hh = vpop(&mut e, &mut vstack, &mut vtmp);
                            let idx_c = match idx.ty {
                                VType::Unknown => format!("({}).i", idx.expr),
                                _ => idx.expr.clone(),
                            };
                            if hh.ty == VType::FloatArr {
                                // Float-typed array: raw double load, no Cell
                                if let Some(p) = arr_ptr.get(&hh.expr) {
                                    // Loop-hoisted element pointer
                                    vpush(&mut e, &mut vstack, &mut vtmp,
                                        &format!("({})[{}]", p, idx_c), VType::Float);
                                } else {
                                    vpush(&mut e, &mut vstack, &mut vtmp,
                                        &format!("((double*)uf_data((Hdr*)({}).i))[{}]", hh.expr, idx_c), VType::Float);
                                }
                            } else {
                                vpush_cell(&mut e, &mut vstack, &mut vtmp, &format!("uf_cvget({},{})", cell_of(&hh), idx_c));
                            }
                        }
                        "op_vset" => {
                            let v = vpop(&mut e, &mut vstack, &mut vtmp);
                            let idx = vpop(&mut e, &mut vstack, &mut vtmp);
                            let hh = vpop(&mut e, &mut vstack, &mut vtmp);
                            let idx_c = match idx.ty {
                                VType::Unknown => format!("({}).i", idx.expr),
                                _ => idx.expr.clone(),
                            };
                            if hh.ty == VType::FloatArr {
                                // Float-typed array: raw double store, no Cell
                                if let Some(p) = arr_ptr.get(&hh.expr) {
                                    e.push_str(&format!("({})[{}]={};", p, idx_c, f64_expr(&v)));
                                } else {
                                    e.push_str(&format!("((double*)uf_data((Hdr*)({}).i))[{}]={};", hh.expr, idx_c, f64_expr(&v)));
                                }
                            } else {
                                e.push_str(&format!("uf_cvset({},{},{});", cell_of(&hh), idx_c, cell_of(&v)));
                            }
                        }
                        _ => {
                            let is_inlined_ffold = (*h == "op_ffold" || *h == "op_fsplit" || *h == "op_rangefold") && depth < 8 && inline_ffolds.contains_key(&i);
                            if is_inlined_ffold {
                                if let Some((bs, be)) = inline_ffolds.get(&i).copied() {
                                    vflush(&mut e, &mut vstack, &mut vcache);
                                    if *h == "op_ffold" {
                                        /* inlined FFOLD: getline loop + line string + callback */
                                        e.push_str("{Cell _ff_acc=pop(cx),_ff_p=pop(cx);uf_fs_gate(uf_sptr(_ff_p),0);FILE*_fp=fopen(uf_sptr(_ff_p),\"r\");if(!_fp)die(\"FFOLD: cannot open file\");char*_line=0;size_t _ncap=0;ssize_t m;long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_FF_C_");
                                        e.push_str(&format!("{}{};cx->loops[fr].end=&&K_FF_E_{}{};long _ff_base=cx->sp;while((m=getline(&_line,&_ncap,_fp))>=0){{while(m>0&&(_line[m-1]=='\\n'||_line[m-1]=='\\r'))_line[--m]=0;Cell _ls=uf_str_new(_line,(size_t)m);pushc(cx,_ff_acc);pushc(cx,_ls);\n", prefix, i, prefix, i));
                                        let inner = format!("{}FF{}_", prefix, i);
                                        emit_range(&mut e, p, targets, inline_fors, inline_ffolds, inline_whiles, outlined_bodies, suppress, ext_idx, bs, be, &inner, depth + 1, local_types, ins_body, &HashMap::new(), &std::collections::HashSet::new(), &HashMap::new(), false);
                                        e.push_str(&format!("K_FF_C_{}{}:;_ff_acc=pop(cx);cx->sp=_ff_base+1;}}K_FF_E_{}{}:;cx->lsp=fr;free(_line);fclose(_fp);pushc(cx,_ff_acc);}}\n", prefix, i, prefix, i));
                                    } else if *h == "op_fsplit" {
                                        /* inlined FSPLIT: getline loop + in-place split + field offsets + callback */
                                        e.push_str("{Cell _ff_acc=pop(cx),_ff_sep=pop(cx),_ff_p=pop(cx);const char*_E=uf_sptr(_ff_sep);if(!*_E)die(\"FSPLIT: empty separator\");size_t _el=strlen(_E);uf_fs_gate(uf_sptr(_ff_p),0);FILE*_fp=fopen(uf_sptr(_ff_p),\"r\");if(!_fp)die(\"FSPLIT: cannot open file\");char*_line=0;size_t _ncap=0;ssize_t m;long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_FF_C_");
                                        e.push_str(&format!("{}{};cx->loops[fr].end=&&K_FF_E_{}{};long _ff_base=cx->sp;while((m=getline(&_line,&_ncap,_fp))>=0){{\n", prefix, i, prefix, i));
                                        /* set up fsplit thread-locals for fget/fatoi/fsget/fbyte */
                                        e.push_str("while(m>0&&(_line[m-1]=='\\n'||_line[m-1]=='\\r'))_line[--m]=0;\n");
                                        e.push_str("uf_fsplit_line=_line;uf_fsplit_parent=(Hdr*)_line;\n");
                                        e.push_str("uf_fsplit_nfields=0;char*_cur=_line;\n");
                                        e.push_str("while(uf_fsplit_nfields<128){char*_sp=strstr(_cur,_E);if(!_sp){uf_fsplit_offsets[uf_fsplit_nfields*2]=(int64_t)(_cur-_line);uf_fsplit_offsets[uf_fsplit_nfields*2+1]=(int64_t)strlen(_cur);uf_fsplit_nfields++;break;}*_sp=0;uf_fsplit_offsets[uf_fsplit_nfields*2]=(int64_t)(_cur-_line);uf_fsplit_offsets[uf_fsplit_nfields*2+1]=(int64_t)(_sp-_cur);uf_fsplit_nfields++;_cur=_sp+_el;}\n");
                                        e.push_str("pushc(cx,_ff_acc);pushi(cx,uf_fsplit_nfields);\n");
                                        let inner = format!("{}FF{}_", prefix, i);
                                        emit_range(&mut e, p, targets, inline_fors, inline_ffolds, inline_whiles, outlined_bodies, suppress, ext_idx, bs, be, &inner, depth + 1, local_types, ins_body, &HashMap::new(), &std::collections::HashSet::new(), &HashMap::new(), false);
                                        e.push_str(&format!("K_FF_C_{}{}:;_ff_acc=pop(cx);cx->sp=_ff_base+1;}}K_FF_E_{}{}:;cx->lsp=fr;free(_line);fclose(_fp);uf_fsplit_line=0;pushc(cx,_ff_acc);}}\n", prefix, i, prefix, i));
                                    } else if *h == "op_rangefold" {
                                        /* inlined RANGEFOLD: count loop + callback */
                                        e.push_str(&format!("{{Cell _rf_acc=pop(cx);int64_t _rf_cnt=uf_i(pop(cx));long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_RF_C_{}{};cx->loops[fr].end=&&K_RF_E_{}{};long _rf_base=cx->sp;for(int64_t _rf_k=0;_rf_k<_rf_cnt;_rf_k++){{pushc(cx,_rf_acc);pushi(cx,_rf_k);\n", prefix, i, prefix, i));
                                        let inner = format!("{}RF{}_", prefix, i);
                                        emit_range(&mut e, p, targets, inline_fors, inline_ffolds, inline_whiles, outlined_bodies, suppress, ext_idx, bs, be, &inner, depth + 1, local_types, ins_body, &HashMap::new(), &std::collections::HashSet::new(), &HashMap::new(), false);
                                        e.push_str(&format!("K_RF_C_{}{}:;_rf_acc=pop(cx);cx->sp=_rf_base+1;}}K_RF_E_{}{}:;cx->lsp=fr;pushc(cx,_rf_acc);}}\n", prefix, i, prefix, i));
                                    }
                                }
                            } else {
                                if *h == "op_drop" {
                                    if let Some(off) = p.param_pcs.get(&i).copied() {
                                        // v13.1: `_!` parameter — pop the cell or
                                        // die (null-fill removed)
                                        let base = param_base(prefix);
                                        let diee = sb_die_expr(p, i);
                                        e.push_str(&format!("if(cx->sp>{}&&cx->sp>0){{pop(cx);}}else{{{}}}\n", base_off(base, off), diee));
                                        o.push_str(&e);
                                        continue;
                                    }
                                }
                                vflush(&mut e, &mut vstack, &mut vcache);
                                e.push_str(&format!("uf_cur_op=\"{}\";{}(cx);\n", h, h));
                            }
                        }
                    }
                }
            }
            Ins::For => {
                // v12: flush virtual stack so the count/address pushed by
                // PushAddr are on the real stack, then pop from there.
                vflush(&mut e, &mut vstack, &mut vcache);
                let inl = if depth < 8 {
                    inline_fors.get(&i).copied()
                } else {
                    None
                };
                match inl {
                    Some((bs, be)) => {
                        // Compile-time-known body: direct C loop over a
                        // renamed inline copy; no call-stack or indirect
                        // jumps per iteration. The iteration index is pushed
                        // on the real ds per iteration, exactly as the
                        // subroutine path does. A dynamic loop frame lets
                        // BREAK/CONT unwind out of the C loop.
                        //
                        // Register-cache typed locals across the loop (same
                        // scheme as inlined WHILE): safe when the body makes
                        // no calls and takes no indirect jumps except IFs
                        // whose continuation is a trivial loop exit
                        // (break/cont then ret) that never touches locals.
                        // Inherit parent's register cache so nested for loops
                        // share the same C locals without cx->locals[] round-trips.
                        let inherited: std::collections::HashSet<usize> = reg.keys().copied().collect();
                        let inherited_arr: HashMap<String, String> = arr_ptr.clone();
                        let mut reg: HashMap<usize, (String, VType)> = reg.clone();
                        {
                            let trivial_exit = |tb: usize| -> bool {
                                match for_body_range(&p.ins, tb) {
                                    Some(tbe) => (tb..tbe).all(|k| {
                                        matches!(p.ins[k], Ins::Break | Ins::Cont)
                                    }),
                                    None => false,
                                }
                            };
                            let escapable = UF_DEBUG.load(Ordering::Relaxed) || (bs..be).any(|k| match &p.ins[k] {
                                Ins::Call(_) | Ins::CallExt(_) | Ins::Sys(_) |
                                Ins::Weave(_) | Ins::Send | Ins::Goto(_) | Ins::While => true,
                                Ins::For => !inline_fors.contains_key(&k),
                                Ins::If => {
                                    if k > bs {
                                        match &p.ins[k - 1] {
                                            Ins::PushAddr(l) => {
                                                match p.labels.get(l) {
                                                    Some(&tb) => !trivial_exit(tb),
                                                    None => true,
                                                }
                                            }
                                            _ => true,
                                        }
                                    } else {
                                        true
                                    }
                                }
                                Ins::PushAddr(_) => {
                                    match p.ins.get(k + 1) {
                                        Some(Ins::If) => false,
                                        Some(Ins::For) => !inline_fors.contains_key(&(k + 1)),
                                        _ => true,
                                    }
                                }
                                Ins::Simple(h) => *h == "op_ffold" || *h == "op_fsplit",
                                _ => false,
                            });
                            if !escapable {
                                for k in bs..be {
                                    let id = match &p.ins[k] {
                                        Ins::LocalGetI(id) | Ins::LocalSetI(id) => *id,
                                        _ => continue,
                                    };
                                    if reg.contains_key(&id) { continue; }
                                    let key = ins_body[k] * 1000000 + id;
                                    let ty = local_types.get(&key).copied().unwrap_or(VType::Unknown);
                                    if ty == VType::Int || ty == VType::Float {
                                        reg.insert(id, (format!("_r{}", id), ty));
                                    } else if numeric.contains(&key) {
                                        // Proven numeric-only slot: cache as a
                                        // plain Cell C local (no pointers, so
                                        // GC-safe), skipping the locals memory
                                        reg.insert(id, (format!("_r{}", id), VType::Unknown));
                                    }
                                }
                            }
                        }
                        let mut regs: Vec<_> = reg.iter().collect();
                        regs.sort_by_key(|(id, _)| *id);
                        // Hoist float-array element pointers: same logic as the
                        // while-inlining path. FloatArr slots never reassigned
                        // inside the loop get a raw double* C local.
                        let mut arr_ptr: HashMap<String, String> = HashMap::new();
                        {
                            let escapable2 = UF_DEBUG.load(Ordering::Relaxed) || (bs..be).any(|k| match &p.ins[k] {
                                Ins::Break | Ins::Cont | Ins::Call(_) | Ins::CallExt(_) | Ins::Sys(_) |
                                Ins::Weave(_) | Ins::Send | Ins::Goto(_) | Ins::While |
                                Ins::If | Ins::IfElse => true,
                                Ins::For => !inline_fors.contains_key(&k),
                                Ins::PushAddr(_) => {
                                    match p.ins.get(k + 1) {
                                        Some(Ins::If) => false,
                                        Some(Ins::For) => inline_fors.contains_key(&(k + 1)),
                                        _ => true,
                                    }
                                }
                                Ins::Simple(h) => *h == "op_ffold" || *h == "op_fsplit",
                                _ => false,
                            });
                            if !escapable2 {
                                let mut reassigned: std::collections::HashSet<usize> = std::collections::HashSet::new();
                                for k in bs..be {
                                    if let Ins::LocalSetI(id) = &p.ins[k] { reassigned.insert(*id); }
                                }
                                for k in bs..be {
                                    if let Ins::LocalGetI(id) = &p.ins[k] {
                                        if reassigned.contains(id) { continue; }
                                        let key = ins_body[k] * 1000000 + id;
                                        if local_types.get(&key).copied() == Some(VType::FloatArr) {
                                            arr_ptr.insert(
                                                format!("cx->locals[cx->local_base+{}]", id),
                                                format!("_a{}", id));
                                        }
                                    }
                                }
                            }
                        }
                        let mut arrps: Vec<_> = arr_ptr.iter().collect();
                        arrps.sort();
                        e.push_str(&format!("{{long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_FC_{}{};cx->loops[fr].end=&&K_FE_{}{};long _sp0=cx->sp;\n", prefix, i, prefix, i));
                        // Only declare registers new to this loop scope;
                        // inherited ones are already in C locals from the parent.
                        for (id, (name, ty)) in &regs {
                            if inherited.contains(id) { continue; }
                            match ty {
                                VType::Int => e.push_str(&format!("int64_t {}=uf_i(cx->locals[cx->local_base+{}]);\n", name, id)),
                                VType::Float => e.push_str(&format!("double {}=uf_f(cx->locals[cx->local_base+{}]);\n", name, id)),
                                _ => e.push_str(&format!("Cell {}=cx->locals[cx->local_base+{}];\n", name, id)),
                            }
                        }
                        for (expr, name) in &arrps {
                            if inherited_arr.contains_key(expr.as_str()) { continue; }
                            e.push_str(&format!("double* {}=(double*)uf_data((Hdr*)({}).i);\n", name, expr));
                        }
                        e.push_str("{int64_t cnt=pop(cx).i;");
                        // If the body immediately stores the index into a
                        // register-cached Int local, assign uf_k directly
                        // and skip the push/pop/store.
                        let reg_store = if let Ins::LocalSetI(id) = &p.ins[bs] {
                            if let Some((name, VType::Int)) = reg.get(id) {
                                Some((name, *id))
                            } else { None }
                        } else { None };
                        if let Some((name, _)) = &reg_store {
                            e.push_str(&format!("for(int64_t uf_k=0;uf_k<cnt;uf_k++){{{}=uf_k;\n", name));
                        } else {
                            e.push_str(&format!("for(int64_t uf_k=0;uf_k<cnt;uf_k++){{pushi(cx,uf_k);\n"));
                        }
                        let eff_bs = if reg_store.is_some() { bs + 1 } else { bs };
                        let inner = format!("{}F{}_", prefix, i);
                        emit_range(&mut e, p, targets, inline_fors, inline_ffolds, inline_whiles, outlined_bodies, suppress, ext_idx, eff_bs, be, &inner, depth + 1, local_types, ins_body, &reg, numeric, &arr_ptr, false);
                        e.push_str(&format!("K_FC_{}{}:;cx->sp=_sp0;}}\nK_FE_{}{}:;", prefix, i, prefix, i));
                        for (id, (name, ty)) in &regs {
                            if inherited.contains(id) { continue; }
                            match ty {
                                VType::Int => e.push_str(&format!("cx->locals[cx->local_base+{}]=uf_mki({});", id, name)),
                                VType::Float => e.push_str(&format!("cx->locals[cx->local_base+{}]=uf_mkf({});", id, name)),
                                _ => e.push_str(&format!("cx->locals[cx->local_base+{}]={};", id, name)),
                            }
                        }
                        e.push_str("cx->lsp=fr;}}\n");
                    }
                    None => {
                        e.push_str(&format!(
                        "{{const void* t=((void*)pop(cx).i);int64_t cnt=pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_FC_{}{};cx->loops[fr].end=&&K_FE_{}{};for(int64_t k=0;k<cnt;k++){{pushi(cx,k);uf_cspush(cx,&&K_{}{},cx->sp-1);goto *t;K_{}{}:;if(cx->sp>0)pop(cx);K_FC_{}{}:;}}K_FE_{}{}:;cx->lsp=fr;}}\n",
                        prefix, i, prefix, i, prefix, i, prefix, i, prefix, i, prefix, i
                        ));
                    }
                }
            }
            Ins::Call(l) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                // Outlined bodies run as separate C functions; the body computes
                // its own drain target (_osp) from its declared arity.
                if let Some(&(bs, _be)) = outlined_bodies.get(&i) {
                    if UF_DEBUG.load(Ordering::Relaxed) {
                        e.push_str(&format!("cx->call_pcs[cx->call_csp++]={};", bs));
                    }
                    e.push_str(&format!("uf_ob_{}(cx);\n", bs));
                    o.push_str(&e);
                    continue;
                }
                // v13: save the caller's data-stack pointer just below the
                // arguments (clamped to 0). The callee's RET restores sp to
                // this value and pushes the single return value, so the call
                // boundary is self-contained (stack draining).
                let target_pc = resolve(l);
                let arity = p.label_params.get(&target_pc).map(|v| v.len()).unwrap_or(0);
                let lc = p.local_counts.get(&target_pc).copied().unwrap_or(0);
                let call_pc_push = if UF_DEBUG.load(Ordering::Relaxed) { format!("cx->call_pcs[cx->call_csp++]={};", target_pc) } else { String::new() };
                let call_pc_pop = if UF_DEBUG.load(Ordering::Relaxed) { "cx->call_csp--;".to_string() } else { String::new() };
                e.push_str(&format!(
                    "cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+={};{}uf_cspush(cx,&&K_{}{},cx->sp>{}?cx->sp-{}:0);goto L_{};K_{}{}:;cx->local_base=cx->local_frames[--cx->local_fsp];{}\n",
                    lc,
                    call_pc_push,
                    prefix,
                    i,
                    arity,
                    arity,
                    target_pc,
                    prefix,
                    i,
                    call_pc_pop
                ))
            }
            Ins::Ret => {
                // v13: return-value rules, then stack draining.
                //   * vstack holds exactly one value  -> return it
                //   * vstack holds several            -> build a list, return it
                //   * vstack empty                    -> return the top of the
                //     real stack above the frame base (v12-style bare `ret`
                //     compat), else null
                // The return value is computed into a C temp first (the real
                // stack is still available for the fallback read), then the
                // data stack is drained back to the caller's saved pointer and
                // the return value is pushed on top.
                let vals = std::mem::take(&mut vstack);
                vdiscard(&mut e, &mut vstack, &mut vcache);
                let outlined = prefix.starts_with("OB");
                let base = if outlined { "_osp" } else { "(cx->csp>0?cx->rsps[cx->csp-1]:0)" };
                let rv = format!("_rv{}", vtmp);
                vtmp += 1;
                let rr = format!("_r{}", vtmp);
                vtmp += 1;
                let mut pre = String::new();
                if vals.is_empty() {
                    // real-stack fallback, read before the drain changes sp
                    pre.push_str(&format!("Cell {}=(cx->sp>{}&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);", rv, base));
                } else if vals.len() == 1 {
                    let v = &vals[0];
                    match v.ty {
                        VType::Unknown | VType::FloatArr => pre.push_str(&format!("Cell {}={};", rv, v.expr)),
                        VType::Float => pre.push_str(&format!("Cell {}=uf_mkf({});", rv, v.expr)),
                        VType::Int => pre.push_str(&format!("Cell {}=uf_mki({});", rv, v.expr)),
                    }
                } else {
                    // multi-value ret: root the values on the ds, build a fresh
                    // list from them, then drain
                    for v in &vals {
                        match v.ty {
                            VType::Unknown | VType::FloatArr => pre.push_str(&format!("pushc(cx,{});", v.expr)),
                            VType::Float => pre.push_str(&format!("pushf(cx,{});", v.expr)),
                            VType::Int => pre.push_str(&format!("pushi(cx,{});", v.expr)),
                        }
                    }
                    pre.push_str(&format!("Cell {}=uf_list_build(cx,{});", rv, vals.len()));
                }
                if outlined {
                    let pop = if UF_DEBUG.load(Ordering::Relaxed) { "cx->call_csp--;" } else { "" };
                    e.push_str(&format!(
                        "{}{{cx->sp=_osp;pushc(cx,{});cx->local_base=cx->local_frames[--cx->local_fsp];{}return;}}\n",
                        pre, rv, pop
                    ));
                } else {
                    e.push_str(&format!(
                        "{}{{if(cx->csp==0){{pushc(cx,{});return;}}cx->csp--;const void*{}=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,{});if(!{})return;goto *{};}}\n",
                        pre, rv, rr, rv, rr, rr
                    ));
                }
            }
            Ins::Goto(l) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str(&format!("goto {};\n", plab(prefix, resolve(l))))
            }
            // v10 structured control flow: operands are code addresses on the
            // ds, CALLed via the threaded call stack (like FOR's path).
            Ins::If => {
                // Peephole (with the Ins::PushAddr arm): `'exit if` where
                // `exit`'s body is only break/cont becomes a direct
                // conditional loop exit — the condition stays a C expression
                // (no ds round trip) and no continuation is pushed.
                if i > start {
                    if let Ins::PushAddr(l) = &p.ins[i - 1] {
                        if trivial_loop_exit(p, l) && !targets.contains(&(i - 1)) {
                            let tb = resolve(l);
                            let is_break = matches!(p.ins[tb], Ins::Break);
                            let c = vpop(&mut e, &mut vstack, &mut vtmp);
                            let test = match c.ty {
                                VType::Int | VType::Float => format!("({})!=0", c.expr),
                                _ => format!("!uf_zero({})", c.expr),
                            };
                            if is_break {
                                e.push_str(&format!("if({}){{if(cx->lsp<=0)die(\"break outside loop\");cx->lsp--;cx->csp=cx->loops[cx->lsp].cspl;goto *cx->loops[cx->lsp].end;}}\n", test));
                            } else {
                                e.push_str(&format!("if({}){{if(cx->lsp<=0)die(\"cont outside loop\");cx->csp=cx->loops[cx->lsp-1].cspl;goto *cx->loops[cx->lsp-1].cont;}}\n", test));
                            }
                            o.push_str(&e);
                            continue;
                        }
                    }
                }
                vflush(&mut e, &mut vstack, &mut vcache);
                // v13: the body's declared arity sets the drain point (the
                // caller's cells below the branch's arguments stay put)
                let if_arity = if i > start {
                    match &p.ins[i - 1] {
                        Ins::PushAddr(l) => p.label_params.get(&resolve(l)).map(|v| v.len()).unwrap_or(0),
                        _ => 0,
                    }
                } else { 0 };
                e.push_str(&format!(
                    "{{const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){{uf_cspush(cx,&&K_{}{},cx->sp>{}?cx->sp-{}:0);goto *b;K_{}{}:;}}}}\n",
                    prefix, i, if_arity, if_arity, prefix, i
                ))
            }
            Ins::IfElse => {
                vflush(&mut e, &mut vstack, &mut vcache);
                // v13: each branch may declare its own arity; the drain point
                // is saved per branch before jumping
                let (th_ar, el_ar) = if i > start {
                    match (&p.ins[i - 2], &p.ins[i - 1]) {
                        (Ins::PushAddr(tl), Ins::PushAddr(el)) => (
                            p.label_params.get(&resolve(tl)).map(|v| v.len()).unwrap_or(0),
                            p.label_params.get(&resolve(el)).map(|v| v.len()).unwrap_or(0),
                        ),
                        _ => (0, 0),
                    }
                } else { (0, 0) };
                // v13: if the if_else is the last instruction of its body (the
                // next instruction is a label target), the continuation must
                // return — otherwise it would fall through into the next
                // label's body. The branch's return value is on the stack
                // (above the body's drain point), so the fallback ret
                // re-returns it. Only in real bodies (top level or outlined) —
                // inside an inlined loop/streaming copy the continuation just
                // falls through to the loop's accumulator pop.
                let mut cont = String::new();
                let cont_ok = (prefix.is_empty() || prefix.starts_with("OB")) && targets.contains(&(i + 1));
                if cont_ok {
                    let rv = format!("_rv{}", vtmp);
                    vtmp += 1;
                    let rr = format!("_r{}", vtmp);
                    vtmp += 1;
                    cont.push_str(&format!("Cell {}=(cx->sp>{}&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);", rv, param_base(prefix)));
                    if prefix.starts_with("OB") {
                        cont.push_str(&format!("{{cx->sp=_osp;pushc(cx,{});cx->local_base=cx->local_frames[--cx->local_fsp];return;}}", rv));
                    } else {
                        cont.push_str(&format!("{{if(cx->csp==0){{pushc(cx,{});return;}}cx->csp--;const void*{}=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,{});if(!{})return;goto *{};}}", rv, rr, rv, rr, rr));
                    }
                }
                e.push_str(&format!(
                    "{{const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){{uf_cspush(cx,&&K_{}{},cx->sp>{}?cx->sp-{}:0);goto *th;}}else{{uf_cspush(cx,&&K_{}{},cx->sp>{}?cx->sp-{}:0);goto *el;}}K_{}{}:;{}}}\n",
                    prefix, i, th_ar, th_ar, prefix, i, el_ar, el_ar, prefix, i, cont
                ))
            }
            Ins::While => {
                vflush(&mut e, &mut vstack, &mut vcache);
                // v13: cond/body declared arities set the per-call drain points
                let (cond_arity, body_arity) = if i >= 2 {
                    match (&p.ins[i - 2], &p.ins[i - 1]) {
                        (Ins::PushAddr(cl), Ins::PushAddr(bl)) => (
                            p.label_params.get(&resolve(cl)).map(|v| v.len()).unwrap_or(0),
                            p.label_params.get(&resolve(bl)).map(|v| v.len()).unwrap_or(0),
                        ),
                        _ => (0, 0),
                    }
                } else { (0, 0) };
                if depth < 8 {
                    if let Some((bbs, bbe, cbs, cbe)) = inline_whiles.get(&i).copied() {
                        let inner_c = format!("{}WC{}_", prefix, i);
                        let inner_b = format!("{}WB{}_", prefix, i);
                        let cbe_trim = if cbe > cbs + 1 && matches!(p.ins[cbe - 1], Ins::PushI(_)) { cbe - 1 } else { cbe };
                        let bbe_trim = if bbe > bbs + 1 && matches!(p.ins[bbe - 1], Ins::PushI(_)) { bbe - 1 } else { bbe };
                        // Register-cache typed locals across the loop: when the
                        // inlined cond+body make no calls, take no indirect
                        // jumps and nest no further control flow, Int/Float
                        // slots can live in C locals for the loop's duration
                        // and be written back once at loop exit.
                        let mut reg: HashMap<usize, (String, VType)> = HashMap::new();
                        {
                            let escapable = UF_DEBUG.load(Ordering::Relaxed) || (cbs..cbe_trim).chain(bbs..bbe_trim).any(|k| match &p.ins[k] {
                                Ins::Break | Ins::Cont | Ins::Call(_) | Ins::CallExt(_) | Ins::Sys(_) |
                                Ins::Weave(_) | Ins::Send | Ins::Goto(_) | Ins::While | Ins::For |
                                Ins::If | Ins::IfElse | Ins::PushAddr(_) => true,
                                Ins::Simple(h) => *h == "op_ffold" || *h == "op_fsplit",
                                _ => false,
                            });
                            if !escapable {
                                for k in (cbs..cbe_trim).chain(bbs..bbe_trim) {
                                    let id = match &p.ins[k] {
                                        Ins::LocalGetI(id) | Ins::LocalSetI(id) => *id,
                                        _ => continue,
                                    };
                                    if reg.contains_key(&id) { continue; }
                                    let key = ins_body[k] * 1000000 + id;
                                    let ty = local_types.get(&key).copied().unwrap_or(VType::Unknown);
                                    if ty == VType::Int || ty == VType::Float {
                                        reg.insert(id, (format!("_r{}", id), ty));
                                    } else if numeric.contains(&key) {
                                        // Proven numeric-only slot: cache as a
                                        // plain Cell C local (no pointers, so
                                        // GC-safe), skipping the locals memory
                                        reg.insert(id, (format!("_r{}", id), VType::Unknown));
                                    }
                                }
                            }
                        }
                        let mut regs: Vec<_> = reg.iter().collect();
                        regs.sort_by_key(|(id, _)| *id);
                        // Hoist float-array element pointers: FloatArr slots
                        // never reassigned inside the loop can keep a raw
                        // double* in a C local (non-moving GC; the memory cell
                        // keeps the object rooted).
                        let mut arr_ptr: HashMap<String, String> = HashMap::new();
                        {
                            let escapable2 = (cbs..cbe_trim).chain(bbs..bbe_trim).any(|k| match &p.ins[k] {
                                Ins::Break | Ins::Cont | Ins::Call(_) | Ins::CallExt(_) | Ins::Sys(_) |
                                Ins::Weave(_) | Ins::Send | Ins::Goto(_) | Ins::While | Ins::For |
                                Ins::If | Ins::IfElse | Ins::PushAddr(_) => true,
                                Ins::Simple(h) => *h == "op_ffold" || *h == "op_fsplit",
                                _ => false,
                            });
                            if !escapable2 {
                                let mut reassigned: std::collections::HashSet<usize> = std::collections::HashSet::new();
                                for k in (cbs..cbe_trim).chain(bbs..bbe_trim) {
                                    if let Ins::LocalSetI(id) = &p.ins[k] { reassigned.insert(*id); }
                                }
                                for k in (cbs..cbe_trim).chain(bbs..bbe_trim) {
                                    if let Ins::LocalGetI(id) = &p.ins[k] {
                                        if reassigned.contains(id) { continue; }
                                        let key = ins_body[k] * 1000000 + id;
                                        if local_types.get(&key).copied() == Some(VType::FloatArr) {
                                            arr_ptr.insert(
                                                format!("cx->locals[cx->local_base+{}]", id),
                                                format!("_a{}", id));
                                        }
                                    }
                                }
                            }
                        }
                        let mut arrps: Vec<_> = arr_ptr.iter().collect();
                        arrps.sort();
                        e.push_str(&format!("{{long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_{}{};cx->loops[fr].end=&&K_WE_{}{};long _sp0=cx->sp;\n", prefix, i, prefix, i));
                        for (id, (name, ty)) in &regs {
                            match ty {
                                VType::Int => e.push_str(&format!("int64_t {}=uf_i(cx->locals[cx->local_base+{}]);\n", name, id)),
                                VType::Float => e.push_str(&format!("double {}=uf_f(cx->locals[cx->local_base+{}]);\n", name, id)),
                                _ => e.push_str(&format!("Cell {}=cx->locals[cx->local_base+{}];\n", name, id)),
                            }
                        }
                        for (expr, name) in &arrps {
                            e.push_str(&format!("double* {}=(double*)uf_data((Hdr*)({}).i);\n", name, expr));
                        }
                        e.push_str(&format!("K_WC_{}{}:;{{Cell _wc;{{\n", prefix, i));
                        // Emit the cond into a side buffer; if it ends with a
                        // plain typed value push, peel it into a direct C test
                        // and skip the data-stack round trip entirely.
                        let mut ce = String::new();
                        emit_range(&mut ce, p, targets, inline_fors, inline_ffolds, inline_whiles, outlined_bodies, suppress, ext_idx, cbs, cbe_trim, &inner_c, depth + 1, local_types, ins_body, &reg, numeric, &arr_ptr, false);
                        let mut direct_cond: Option<String> = None;
                        if ce.ends_with(");") {
                            for tag in ["pushi(cx,", "pushf(cx,"] {
                                if let Some(pos) = ce.rfind(tag) {
                                    let expr = &ce[pos + tag.len()..ce.len() - 2];
                                    // single expression: balanced parens, no
                                    // semicolons or string literals inside
                                    let mut dp = 0i32;
                                    let mut ok = !expr.contains('"') && !expr.contains(';');
                                    if ok {
                                        for ch in expr.chars() {
                                            match ch { '(' => dp += 1, ')' => dp -= 1, _ => {} }
                                            if dp < 0 { ok = false; break; }
                                        }
                                    }
                                    if ok && dp == 0 {
                                        direct_cond = Some(expr.to_string());
                                        ce.truncate(pos);
                                        break;
                                    }
                                }
                            }
                        }
                        e.push_str(&ce);
                        match direct_cond {
                            // keep the test inside the block where the cond
                            // temps are declared
                            Some(expr) => e.push_str(&format!("if(({})==0)goto K_WE_{}{};}};\n", expr, prefix, i)),
                            None => e.push_str(&format!("}}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_{}{};\n", prefix, i)),
                        }
                        e.push_str("{\n");
                        // Dead-flush suppression: the loop's `cx->sp=_sp0`
                        // discards everything the body pushed, so in a
                        // register-cached (non-escapable) body the @flush and
                        // end-of-range pushes are pure overhead (each is a real
                        // pushc call + a memory round trip for the accumulator).
                        // Suppress them only when nothing can read the ds:
                        // no list/dict literals (they build from the ds) and the
                        // ret operand starts with a push (a leading bare pop
                        // would read the @flush strays).
                        let body_esc = UF_DEBUG.load(Ordering::Relaxed) || (bbs..bbe_trim).any(|k| match &p.ins[k] {
                            Ins::Break | Ins::Cont | Ins::Call(_) | Ins::CallExt(_) | Ins::Sys(_) |
                            Ins::Weave(_) | Ins::Send | Ins::Goto(_) | Ins::While | Ins::For |
                            Ins::If | Ins::IfElse | Ins::PushAddr(_) => true,
                            Ins::Simple(h) => *h == "op_ffold" || *h == "op_fsplit",
                            _ => false,
                        });
                        let last_flush = (bbs..bbe_trim).rev().find(|&k| matches!(p.ins[k], Ins::Flush));
                        let operand_push_first = match last_flush {
                            Some(f) => f + 1 >= bbe_trim || matches!(p.ins[f + 1],
                                Ins::PushI(_) | Ins::PushF(_) | Ins::PushS(_) |
                                Ins::LocalGetI(_) | Ins::PushAddr(_) | Ins::GetV(_)),
                            None => true,
                        };
                        let body_suppress = !body_esc && operand_push_first
                            && !(bbs..bbe_trim).any(|k| matches!(p.ins[k], Ins::ListLit | Ins::DictLit));
                        emit_range(&mut e, p, targets, inline_fors, inline_ffolds, inline_whiles, outlined_bodies, suppress, ext_idx, bbs, bbe_trim, &inner_b, depth + 1, local_types, ins_body, &reg, numeric, &arr_ptr, body_suppress);
                        // With dead-flush suppression active the body never
                        // touches the data stack (no pushes, and a `pop(cx)`
                        // would mean a vpop-empty the suppression guards
                        // against), so cx->sp stays == _sp0 for the whole loop
                        // and the per-iteration reset is a redundant memory
                        // store that also forces GCC to spill loop
                        // accumulators. Drop it; sp is already correct at exit.
                        let sp_stable = body_suppress && !e.contains("pop(cx)");
                        if sp_stable {
                            e.push_str(&format!("}}goto K_WC_{}{};}}\nK_WE_{}{}:;", prefix, i, prefix, i));
                        } else {
                            e.push_str(&format!("}}cx->sp=_sp0;goto K_WC_{}{};}}\nK_WE_{}{}:;", prefix, i, prefix, i));
                        }
                        for (id, (name, ty)) in &regs {
                            match ty {
                                VType::Int => e.push_str(&format!("cx->locals[cx->local_base+{}]=uf_mki({});", id, name)),
                                VType::Float => e.push_str(&format!("cx->locals[cx->local_base+{}]=uf_mkf({});", id, name)),
                                _ => e.push_str(&format!("cx->locals[cx->local_base+{}]={};", id, name)),
                            }
                        }
                        e.push_str("cx->lsp=fr;}\n");
                        o.push_str(&e);
                        continue;
                    }
                }
                e.push_str(&format!(
                    "{{const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;\n\
                     K_WT_{}{}:;cx->loops[fr].cont=&&K_WT_{}{};cx->loops[fr].end=&&K_WE_{}{};\n\
                     uf_cspush(cx,&&K_WC_{}{},cx->sp>{}?cx->sp-{}:0);goto *cnd;K_WC_{}{}:;\n\
                     if(uf_zero(pop(cx)))goto K_WE_{}{};\n\
                     uf_cspush(cx,&&K_WB_{}{},cx->sp>{}?cx->sp-{}:0);goto *bod;K_WB_{}{}:;pop(cx);\n\
                     goto K_WT_{}{};\n\
                     K_WE_{}{}:;cx->lsp=fr;}}\n",
                    prefix, i, prefix, i, prefix, i,
                    prefix, i, cond_arity, cond_arity, prefix, i,
                    prefix, i,
                    prefix, i, body_arity, body_arity, prefix, i,
                    prefix, i,
                    prefix, i
                ))
            }
            Ins::Break => {
                vdiscard(&mut e, &mut vstack, &mut vcache);
                e.push_str("{if(cx->lsp<=0)die(\"break outside loop\");cx->lsp--;cx->csp=cx->loops[cx->lsp].cspl;goto *cx->loops[cx->lsp].end;}\n")
            }
            Ins::Cont => {
                vdiscard(&mut e, &mut vstack, &mut vcache);
                e.push_str("{if(cx->lsp<=0)die(\"cont outside loop\");cx->csp=cx->loops[cx->lsp-1].cspl;goto *cx->loops[cx->lsp-1].cont;}\n")
            }
            Ins::SetV(v) => {
                let par_off = p.param_pcs.get(&i).copied();
                let is_param_bind = par_off.is_some();
                let t = if let Some(off) = par_off {
                    let base = param_base(prefix);
                    let tmp = format!("_pp{}", vtmp);
                    vtmp += 1;
                    e.push_str(&format!("Cell {}=(cx->sp>{}&&cx->sp>0)?pop(cx):({},uf_mki(0));", tmp, base_off(base, off), sb_die_expr(p, i)));
                    VEntry { expr: tmp.clone(), ty: VType::Unknown }
                } else {
                    vpop(&mut e, &mut vstack, &mut vtmp)
                };
                // Wrap typed values into Cell for global storage
                let cell_expr = match t.ty {
                    VType::Unknown | VType::FloatArr => t.expr.clone(),
                    VType::Float => format!("uf_mkf({})", t.expr),
                    VType::Int => format!("uf_mki({})", t.expr),
                };
                if let Some(ent) = vcache.iter_mut().find(|(n, _, _)| n == v) {
                    ent.1 = cell_expr;
                    ent.2 = true;
                } else {
                    vcache.push((v.clone(), cell_expr, true));
                }
                // v12: assignment is pass-through; leave value on stack
                // (param binds consume their cell and are not pass-through)
                if !is_param_bind {
                    vstack.push(t);
                }
            }
            Ins::GetV(v) => {
                if let Some((_, t, _)) = vcache.iter().find(|(n, _, _)| n == v) {
                    let t = t.clone();
                    vstack.push(VEntry { expr: t, ty: VType::Unknown });
                } else {
                    vpush_cell(&mut e, &mut vstack, &mut vtmp, &format!("var_{}", v));
                    vcache.push((v.clone(), format!("var_{}", v), false));
                }
            }
            Ins::LocalSetI(id) => {
                // v13: param pops (instruction pcs recorded by the parser) pop
                // with a guard against the caller's saved stack pointer, so
                // missing arguments bind null instead of underflowing. They are
                // not pass-through.
                let par_off = p.param_pcs.get(&i).copied();
                let is_param_bind = par_off.is_some();
                let t = if let Some(off) = par_off {
                    let base = param_base(prefix);
                    let tmp = format!("_pp{}", vtmp);
                    vtmp += 1;
                    e.push_str(&format!("Cell {}=(cx->sp>{}&&cx->sp>0)?pop(cx):({},uf_mki(0));", tmp, base_off(base, off), sb_die_expr(p, i)));
                    VEntry { expr: tmp.clone(), ty: VType::Unknown }
                } else {
                    vpop(&mut e, &mut vstack, &mut vtmp)
                };
                if let Some((name, ty)) = reg.get(id) {
                    // Register-cached slot: raw C assignment, no memory traffic
                    let rhs = if *ty == VType::Int {
                        match t.ty {
                            VType::Int => t.expr.clone(),
                            _ => format!("uf_i({})", cell_of(&t)),
                        }
                    } else if *ty == VType::Float {
                        f64_expr(&t)
                    } else {
                        // Cell-cached numeric slot
                        cell_of(&t)
                    };
                    e.push_str(&format!("{}={};", name, rhs));
                    if !is_param_bind {
                        // v12: assignment is pass-through; leave value on stack
                        vstack.push(VEntry { expr: name.clone(), ty: *ty });
                    }
                    o.push_str(&e);
                    continue;
                }
                match t.ty {
                    VType::Unknown | VType::FloatArr => {
                        e.push_str(&format!("cx->locals[cx->local_base+{}]={};", id, t.expr));
                    }
                    VType::Float => {
                        e.push_str(&format!("cx->locals[cx->local_base+{}]=uf_mkf({});", id, t.expr));
                    }
                    VType::Int => {
                        e.push_str(&format!("cx->locals[cx->local_base+{}]=uf_mki({});", id, t.expr));
                    }
                }
                if !is_param_bind {
                    // v12: assignment is pass-through; leave value on stack
                    vstack.push(t);
                }
            }
            Ins::LocalGetI(id) => {
                if let Some((name, ty)) = reg.get(id) {
                    // Register-cached slot: plain C local read
                    vstack.push(VEntry { expr: name.clone(), ty: *ty });
                    o.push_str(&e);
                    continue;
                }
                let key = ins_body[i] * 1000000 + id;
                let ty = local_types.get(&key).copied().unwrap_or(VType::Unknown);
                match ty {
                    VType::Unknown => {
                        vpush_cell(&mut e, &mut vstack, &mut vtmp,
                            &format!("cx->locals[cx->local_base+{}]", id));
                    }
                    VType::Float => {
                        vpush(&mut e, &mut vstack, &mut vtmp,
                            &format!("uf_f(cx->locals[cx->local_base+{}])", id), VType::Float);
                    }
                    VType::Int => {
                        vpush(&mut e, &mut vstack, &mut vtmp,
                            &format!("uf_i(cx->locals[cx->local_base+{}])", id), VType::Int);
                    }
                    VType::FloatArr => {
                        let slot_expr = format!("cx->locals[cx->local_base+{}]", id);
                        if arr_ptr.contains_key(&slot_expr) {
                            // Loop-hoisted pointer exists: push raw expression
                            // (not a temp) so vget's arr_ptr lookup matches
                            vstack.push(VEntry { expr: slot_expr, ty: VType::FloatArr });
                        } else {
                            vpush(&mut e, &mut vstack, &mut vtmp, &slot_expr, VType::FloatArr);
                        }
                    }
                }
            }
            // Unresolved locals (should have been resolved by resolve_locals)
            Ins::LocalSet(n) => panic!("unresolved local {} at pc {}", n, i),
            Ins::LocalGet(n) => panic!("unresolved local {} at pc {}", n, i),
            Ins::Extern(name) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                let xi = ext_idx[name.as_str()];
                e.push_str(&format!("pushp(cx,(void*)&uf_x{});\n", xi));
            }
            Ins::Sys(arity) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str("{int64_t num=pop(cx).i;");
                for k in (0..*arity).rev() {
                    e.push_str(&format!("int64_t sa{}=pop(cx).i;", k));
                }
                if *arity == 0 {
                    e.push_str("pushi(cx,syscall(num));");
                } else {
                    let args: Vec<String> = (0..*arity).map(|k| format!("sa{}", k)).collect();
                    e.push_str(&format!("pushi(cx,syscall(num,{}));", args.join(",")));
                }
                e.push_str("}\n");
            }
            Ins::Flush => {
                // v13: ret-operand boundary — spill the vstack to the real
                // stack so the ret's count rule sees only the operand's values
                if !suppress_flush {
                    vflush(&mut e, &mut vstack, &mut vcache);
                }
            }
            Ins::ListStart | Ins::DictStart => {
                // v13: save the ds pointer at the literal start. Values pushed
                // before the literal (e.g. earlier dict key/values) are flushed
                // to the real stack FIRST, so the saved pointer sits below the
                // literal's own cells and the closing build counts only them.
                vflush(&mut e, &mut vstack, &mut vcache);
                let t = vtmp;
                vtmp += 1;
                e.push_str(&format!("long _ls{}=cx->sp;", t));
                lit_starts.push(t);
            }
            Ins::ListLit => {
                // v13: [expr ...] — flush the element values to the real stack
                // (rooted against GC), build the list from the cells above the
                // saved literal start, and keep the handle on the virtual stack
                // so nested literals work
                let vals = std::mem::take(&mut vstack);
                for v in &vals {
                    match v.ty {
                        VType::Unknown | VType::FloatArr => e.push_str(&format!("pushc(cx,{});", v.expr)),
                        VType::Float => e.push_str(&format!("pushf(cx,{});", v.expr)),
                        VType::Int => e.push_str(&format!("pushi(cx,{});", v.expr)),
                    }
                }
                let start = lit_starts.pop().expect("ListLit without ListStart");
                let tmp = format!("t{}", vtmp);
                vtmp += 1;
                e.push_str(&format!("Cell {}=uf_list_build(cx,cx->sp-_ls{});", tmp, start));
                vstack.push(VEntry { expr: tmp.clone(), ty: VType::Unknown });
            }
            Ins::DictLit => {
                // v13: { k v ... } — alternating key/value cells above the start
                let vals = std::mem::take(&mut vstack);
                for v in &vals {
                    match v.ty {
                        VType::Unknown | VType::FloatArr => e.push_str(&format!("pushc(cx,{});", v.expr)),
                        VType::Float => e.push_str(&format!("pushf(cx,{});", v.expr)),
                        VType::Int => e.push_str(&format!("pushi(cx,{});", v.expr)),
                    }
                }
                let start = lit_starts.pop().expect("DictLit without DictStart");
                let tmp = format!("t{}", vtmp);
                vtmp += 1;
                e.push_str(&format!("Cell {}=uf_dict_build(cx,cx->sp-_ls{});", tmp, start));
                vstack.push(VEntry { expr: tmp.clone(), ty: VType::Unknown });
            }
            Ins::CallExt(ii) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                let im = &p.imports[*ii];
                e.push_str(&format!("uf_cur_op=\"{}\";", im.name));
                e.push_str(&gen_call_ext(im, &format!("uf_im{}", ii)));
            }
            Ins::Send => {
                vdiscard(&mut e, &mut vstack, &mut vcache);
                // v13: the method's declared arity sets the drain point
                let mut snd_arity = 0usize;
                for (_, _, l) in &p.methods {
                    if let Some(&pc) = p.labels.get(l) {
                        if let Some(v) = p.label_params.get(&pc) {
                            snd_arity = snd_arity.max(v.len());
                        }
                    }
                }
                e.push_str(&format!(
                "{{int64_t mid=pop(cx).i;Cell rc=pop(cx);Hdr*h=(Hdr*)rc.i;int64_t tk=(h->tag==HT_OBJ)?1000+(int64_t)h->len:(int64_t)h->tag;const void*lab=0;for(unsigned long q=0;q<sizeof(uf_mt)/sizeof(uf_mt[0]);q++)if(uf_mt[q].tk==tk&&uf_mt[q].mh==mid){{lab=uf_mt[q].lab;break;}}if(!lab)die(\"SEND: no such method\");pushc(cx,rc);uf_cspush(cx,&&K_{}{},cx->sp>{}?cx->sp-{}:0);goto *lab;K_{}{}:;}}\n",
                prefix, i, snd_arity, snd_arity, prefix, i
                ))
            }
            Ins::Weave(tasks) => {
                vdiscard(&mut e, &mut vstack, &mut vcache);
                let n = tasks.len();
                e.push_str(&format!("{{WeaveTask uf_wt[{}];\n", n));
                for (k, t) in tasks.iter().enumerate() {
                    let ins: Vec<String> = t.inputs.iter().map(|j| j.to_string()).collect();
                    e.push_str(&format!("int uf_wi{}[]={{{}}};\n", k, ins.join(",")));
                    e.push_str(&format!(
                        "uf_wt[{}]=(WeaveTask){{{}, {}, uf_wi{}, {}, {{0,0}}, 0, 0,0,0,0,0}};\n",
                        k,
                        t.pc,
                        t.inputs.len(),
                        k,
                        t.count
                    ));
                }
                e.push_str(&format!("uf_weave(cx,uf_wt,{},uflux_run);\n", n));
                for (k, t) in tasks.iter().enumerate() {
                    e.push_str(&format!("var_{}=uf_wt[{}].result;\n", t.name, k));
                }
                e.push_str(&format!("pushc(cx,uf_wt[{}].result);\n", n - 1));
                e.push_str("}\n");
            }
        }
        o.push_str(&e);
    }
    // end of range: make the ds/var state exact for whoever follows
    let mut e = String::new();
    if prefix.is_empty() {
        // v12 top-level: auto-discard any unbound values left on the stack
        vdiscard(&mut e, &mut vstack, &mut vcache);
    } else if suppress_flush {
        // Inlined register-cached loop body: values left on the vstack are
        // provably dead (the loop's `cx->sp=_sp0` discards them right after),
        // so skip the ds pushes entirely. Dirty var caches must still be
        // written back (vflush drains both).
        for (v, t, d) in vcache.drain(..) {
            if d {
                e.push_str(&format!("var_{}={};", v, t));
            }
        }
    } else {
        vflush(&mut e, &mut vstack, &mut vcache);
    }
    o.push_str(&e);
}

// Pre-pass: determine local variable types by simulating the virtual stack.
// For each call body, tracks what type each LocalSetI stores. If all stores
// to a slot agree, the slot is typed. Conflicts → Unknown.
fn compute_local_types(p: &Parsed) -> (HashMap<usize, VType>, Vec<usize>, std::collections::HashSet<usize>) {
    // Determine call-entry labels (same logic as resolve_locals)
    let mut call_entries: std::collections::HashSet<usize> = std::collections::HashSet::new();
    call_entries.insert(0);
    if let Some(el) = &p.entry_label {
        if let Some(&pc) = p.labels.get(el) { call_entries.insert(pc); }
    }
    for ins in &p.ins {
        if let Ins::Call(l) = ins { if let Some(&pc) = p.labels.get(l) { call_entries.insert(pc); } }
    }
    for ins in &p.ins {
        if let Ins::Weave(tasks) = ins { for t in tasks { call_entries.insert(t.pc); } }
    }
    for (_, l) in &p.exports { if let Some(&pc) = p.labels.get(l) { call_entries.insert(pc); } }

    let mut all_label_pcs: Vec<usize> = p.labels.values().copied().collect();
    all_label_pcs.push(0);
    all_label_pcs.sort();
    all_label_pcs.dedup();

    // Initial body assignment by position
    let mut label_body: HashMap<usize, usize> = HashMap::new();
    {
        let mut cur_body = 0usize;
        for &pc in &all_label_pcs {
            if call_entries.contains(&pc) { cur_body = pc; }
            label_body.insert(pc, cur_body);
        }
    }
    // Propagate through PushAddr — union-find merge (a naive reassign loop
    // oscillates forever on mutually-referenced continuation labels)
    {
        let mut parent: HashMap<usize, usize> = HashMap::new();
        for &pc in &all_label_pcs { parent.insert(pc, pc); }
        fn ufind(parent: &mut HashMap<usize, usize>, x: usize) -> usize {
            let mut root = x;
            loop { let p = *parent.get(&root).unwrap_or(&root); if p == root { break; } root = p; }
            let mut cur = x;
            while cur != root { let next = *parent.get(&cur).unwrap_or(&root); parent.insert(cur, root); cur = next; }
            root
        }
        let ranges: Vec<(usize, usize)> = all_label_pcs.iter().copied().zip({
            let mut nexts = all_label_pcs.iter().copied().skip(1).collect::<Vec<_>>();
            nexts.push(p.ins.len());
            nexts.into_iter()
        }).collect();
        for &(lpc, end) in &ranges {
            let lroot = ufind(&mut parent, lpc);
            for i in lpc..end.min(p.ins.len()) {
                if let Ins::PushAddr(target) = &p.ins[i] {
                    if let Some(&target_pc) = p.labels.get(target) {
                        let troot = ufind(&mut parent, target_pc);
                        if troot != lroot { parent.insert(troot, lroot); }
                    }
                }
            }
        }
        let mut comp_body: HashMap<usize, usize> = HashMap::new();
        for &pc in &all_label_pcs {
            let root = ufind(&mut parent, pc);
            let pos = *label_body.get(&pc).unwrap_or(&0);
            let e = comp_body.entry(root).or_insert(pos);
            if pos < *e { *e = pos; }
        }
        for &pc in &all_label_pcs {
            let root = ufind(&mut parent, pc);
            if let Some(&b) = comp_body.get(&root) { label_body.insert(pc, b); }
        }
    }

    // Map instruction index to body
    let mut ins_body: Vec<usize> = vec![0usize; p.ins.len()];
    {
        let mut cur_body = 0usize;
        for i in 0..p.ins.len() {
            if all_label_pcs.contains(&i) {
                if let Some(&b) = label_body.get(&i) { cur_body = b; }
            }
            ins_body[i] = cur_body;
        }
    }

    // Continuation labels (PushAddr targets): while/if/for/quotation bodies
    // whose RET returns to a dispatcher that consumes exactly one value.
    // Their RET candidates must drop the top value — it never reaches the
    // function's caller.
    let mut cont_pcs: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for ins in &p.ins {
        if let Ins::PushAddr(t) = ins {
            if let Some(&pc) = p.labels.get(t) { cont_pcs.insert(pc); }
        }
    }

    // For each body, simulate the stack to infer types of LocalSetI operands.
    // Process ALL instructions in the body as one continuous stream — types
    // flow from one continuation label to the next within the same body.

    // Group instruction indices by body, preserving source order
    let mut body_to_ins: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..p.ins.len() {
        body_to_ins.entry(ins_body[i]).or_default().push(i);
    }
    let bodies: Vec<(usize, Vec<usize>)> = body_to_ins.into_iter().collect();
    // Sort by body start PC for deterministic fixed-point convergence
    let mut bodies = bodies;
    bodies.sort_by_key(|(b, _)| *b);

    // Function prologue: consecutive LocalSetI at a body start pop call
    // parameters (params[0] = top of caller stack, params[1] = next, ...).
    let mut prologue: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut prologue_pcs: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (body, _) in &bodies {
        let mut params = Vec::new();
        let mut j = *body;
        while j < p.ins.len() {
            if let Ins::LocalSetI(id) = p.ins[j] {
                params.push(id);
                prologue_pcs.insert(j);
                j += 1;
            } else {
                break;
            }
        }
        prologue.insert(*body, params);
    }

    // Fixed-point iteration: repeat until no type changes.
    // Key is (body_start_pc, slot_id) — slots in different call frames
    // are independent and must not be merged.
    // The OUTER loop adds interprocedural propagation: call-site stack
    // snapshots seed callee parameter slots, and callee RET stacks type
    // the values a CALL leaves on the caller's stack.
    let mut result: HashMap<(usize, usize), VType> = HashMap::new();
    let mut ret_types: HashMap<usize, Vec<VType>> = HashMap::new();
    let mut numeric_slots: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // conflicted: slots with irreconcilable provable store types (sticky).
    // seeded: parameter slots typed by call-site evidence.
    let mut conflicted: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut seeded: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();

    // Pre-seed for-body iteration index slots: the `for` opcode pushes an
    // integer index, so the first LocalSetI at the start of any for-body
    // label receives an Int. This lets register caching fire for loop
    // counters even in continuation bodies (which share the parent
    // function's body key).
    for j in 1..p.ins.len() {
        if let (Ins::PushAddr(l), Ins::For) = (&p.ins[j - 1], &p.ins[j]) {
            if let Some(&bs) = p.labels.get(l) {
                // v13: only single-parameter bodies receive the loop index as
                // their (single) argument — multi-param bodies bind the index
                // to the first declared parameter, which the reversed emission
                // does not place first.
                if p.label_params.get(&bs).map(|v| v.len() == 1).unwrap_or(false) {
                    // The first LocalSetI at bs receives the iteration index.
                    if let Some(Ins::LocalSetI(id)) = p.ins.get(bs) {
                        let body = ins_body[bs];
                        let key = (body, *id);
                        result.insert(key, VType::Int);
                        seeded.insert(key);
                    }
                }
            }
        }
    }
    let mut changed_outer = true;
    let mut outer_iter = 0;
    while changed_outer && outer_iter < 10 {
        outer_iter += 1;
        changed_outer = false;
        // Parameter slots without call-site evidence stay opaque: in-body
        // stores alone can never prove a parameter's type.
        let mut param_unseeded: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
        for (body, params) in &prologue {
            for &slot in params {
                if !seeded.contains(&(*body, slot)) {
                    param_unseeded.insert((*body, slot));
                }
            }
        }
        // Caller stack snapshots at each CALL (target body pc, stack before
        // the call) and per-body stacks at each RET — recorded during the
        // last inner pass and consumed below.
        let mut call_snaps: Vec<(usize, Vec<VType>)> = Vec::new();
        let mut ret_cands: HashMap<usize, Vec<Vec<VType>>> = HashMap::new();
        // Slots proven to hold only numeric (int/float) cells — no pointers,
        // so they are safe to keep in C locals across loop iterations.
        let mut round_numeric: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut changed = true;
        let mut max_iter = 0;
        while changed && max_iter < 20 {
            max_iter += 1;
            changed = false;
            call_snaps.clear();
            ret_cands.clear();
            round_numeric.clear();
            for (body, indices) in &bodies {
            // Simulate with opacity tracking. A stack entry is (type, opaque):
            // "opaque" means the value came from a source the inference cannot
            // see through (globals, extern calls, unknown-ety arrays, stack
            // underflow, unseeded parameters). Storing an opaque value poisons
            // the slot for this round; a slot commits to a type only when ALL
            // its stores carry that exact provable type.
            let mut records: HashMap<usize, Vec<VType>> = HashMap::new();
            let mut poisoned: std::collections::HashSet<usize> = std::collections::HashSet::new();
            let mut type_stack: Vec<(VType, bool)> = Vec::new();
            let mut in_cont = false;

            for &i in indices {
                if call_entries.contains(&i) {
                    in_cont = false;
                } else if cont_pcs.contains(&i) {
                    in_cont = true;
                }
            match &p.ins[i] {
                Ins::PushI(_) => type_stack.push((VType::Int, false)),
                Ins::PushF(_) => type_stack.push((VType::Float, false)),
                Ins::PushS(_) | Ins::PushAddr(_) => type_stack.push((VType::Unknown, true)),
                Ins::Simple(h) => {
                    let bin = matches!(*h, "op_add"|"op_sub"|"op_mul"|"op_and"|"op_div"|"op_rem"|"op_lt"|"op_gt"|"op_lte"|"op_gte"|"op_eq"|"op_or"|"op_xor");
                    let un = matches!(*h, "op_shr"|"op_inc"|"op_dec"|"op_not");
                    if bin {
                        let b = type_stack.pop().unwrap_or((VType::Unknown, true));
                        let a = type_stack.pop().unwrap_or((VType::Unknown, true));
                        let rt = infer_bin_type(h, a.0, b.0);
                        type_stack.push((rt, rt == VType::Unknown && (a.1 || b.1)));
                    } else if un {
                        let a = type_stack.pop().unwrap_or((VType::Unknown, true));
                        // inc/dec preserve type; others return Int
                        let r = if matches!(*h, "op_inc"|"op_dec") { a.0 } else { VType::Int };
                        type_stack.push((r, r == VType::Unknown && a.1));
                    } else if matches!(*h, "op_drop") {
                        type_stack.pop();
                    } else if matches!(*h, "op_vget"|"op_idx") {
                        type_stack.pop(); // index
                        // FloatArr handle (float-array local or propagated
                        // parameter) means vget yields a raw Float.
                        let hty = type_stack.pop().unwrap_or((VType::Unknown, true));
                        type_stack.push(if hty.0 == VType::FloatArr { (VType::Float, false) } else { (VType::Unknown, true) });
                    } else if matches!(*h, "op_vset"|"op_seti") {
                        type_stack.pop(); type_stack.pop(); type_stack.pop();
                    } else if matches!(*h, "op_atoi"|"op_len") {
                        // Result provably Int regardless of input
                        type_stack.pop();
                        type_stack.push((VType::Int, false));
                    } else if matches!(*h, "op_atof") {
                        type_stack.pop();
                        type_stack.push((VType::Float, false));
                    } else if matches!(*h, "op_get") {
                        type_stack.pop(); type_stack.pop();
                        type_stack.push((VType::Unknown, true));
                    } else if matches!(*h, "op_argi") {
                        // ARGI: index -> int (argv[index] parsed as integer)
                        type_stack.pop();
                        type_stack.push((VType::Int, false));
                    } else if matches!(*h, "op_hasargs") {
                        // HASARGS: -> int (1 if any CLI args present)
                        type_stack.push((VType::Int, false));
                    } else if matches!(*h, "op_argv") {
                        type_stack.push((VType::Unknown, true));
                    } else {
                        // Unknown op: flush stack conservatively
                        type_stack.clear();
                    }
                }
                Ins::LocalSetI(id) => {
                    // Float-array handle pattern:
                    //   <len> PushI(1) ARR LocalSetI(id)
                    let mut is_farr = false;
                    if i >= 2 {
                        if let (Ins::PushI(ty), Ins::Simple(h)) = (&p.ins[i-2], &p.ins[i-1]) {
                            if *h == "op_arr" && *ty == 1 { is_farr = true; }
                        }
                    }
                    if is_farr {
                        type_stack.pop();
                        records.entry(*id).or_default().push(VType::FloatArr);
                        continue;
                    }
                    match type_stack.pop() {
                        None => {
                            // Stack underflow: the value comes from outside the
                            // simulated stream. Prologue pops are the call
                            // parameters — seeded separately, so skip them.
                            if !prologue_pcs.contains(&i) {
                                poisoned.insert(*id);
                            }
                        }
                        Some((t, opq)) => {
                            if t != VType::Unknown {
                                records.entry(*id).or_default().push(t);
                            } else {
                                // v13.1: an Unknown-typed store must block
                                // committing a concrete type to this slot —
                                // polymorphic arithmetic may store array or
                                // matrix handles where a scalar was inferred.
                                poisoned.insert(*id);
                            }
                            let _ = opq;
                        }
                    }
                }
                Ins::LocalGetI(id) => {
                    if param_unseeded.contains(&(*body, *id)) {
                        // Unproven parameter: opaque
                        type_stack.push((VType::Unknown, true));
                    } else {
                        let ty = result.get(&(*body, *id)).copied().unwrap_or(VType::Unknown);
                        type_stack.push((ty, false));
                    }
                }
                Ins::SetV(_) => { type_stack.pop(); }
                Ins::GetV(_) => { type_stack.push((VType::Unknown, true)); }
                Ins::Ret => {
                    // v13: a body leaves exactly one return value — the top of
                    // the stack (or null), or a fresh list when several values
                    // are named. Continuation RETs feed a dispatcher that
                    // consumes it — drop it from the caller's view.
                    let mut cand: Vec<VType> = if type_stack.len() >= 2 {
                        vec![VType::Unknown] // multi-value ret: a list handle
                    } else {
                        vec![type_stack.last().map(|e| e.0).unwrap_or(VType::Unknown)]
                    };
                    if in_cont && !cand.is_empty() { cand.pop(); }
                    ret_cands.entry(*body).or_default().push(cand);
                    type_stack.clear();
                }
                Ins::If | Ins::IfElse | Ins::While | Ins::For | Ins::Break | Ins::Cont | Ins::Goto(_) => {
                    type_stack.clear();
                }
                Ins::Call(l) => {
                    if let Some(&tpc) = p.labels.get(l) {
                        call_snaps.push((tpc, type_stack.iter().map(|e| e.0).collect()));
                        if let Some(rts) = ret_types.get(&tpc) {
                            // Model the call: callee pops its prologue params
                            // and pushes its (previously inferred) returns.
                            let np = prologue.get(&tpc).map(|v| v.len()).unwrap_or(0);
                            for _ in 0..np { type_stack.pop(); }
                            for &t in rts { type_stack.push((t, t == VType::Unknown)); }
                        } else {
                            // Callee returns not yet known — be conservative
                            type_stack.clear();
                        }
                    } else {
                        type_stack.clear();
                    }
                }
                Ins::CallExt(_) => {
                    type_stack.clear();
                }
                Ins::Sys(_) | Ins::Weave(_) | Ins::Send => { type_stack.clear(); }
                Ins::Flush | Ins::ListStart | Ins::DictStart => { type_stack.clear(); }
                Ins::ListLit | Ins::DictLit => { type_stack.clear(); type_stack.push((VType::Unknown, false)); }
                Ins::Extern(_) => { type_stack.push((VType::Unknown, true)); }
                Ins::LocalSet(_) | Ins::LocalGet(_) => { type_stack.push((VType::Unknown, true)); }
            }
        }

            // Commit slot types from this round's evidence. A slot commits
            // only when every store carries the same provable type; opaque
            // stores block commitment; conflicting provable types poison the
            // slot permanently.
            let mut slots: std::collections::HashSet<usize> = records.keys().copied().collect();
            slots.extend(poisoned.iter().copied());
            for slot in slots {
                let key = (*body, slot);
                if conflicted.contains(&key) { continue; }
                if param_unseeded.contains(&key) { continue; } // params: seeds only
                let known: Vec<VType> = records.get(&slot).cloned().unwrap_or_default();
                if known.is_empty() { continue; } // fully opaque or untouched
                if !poisoned.contains(&slot)
                    && known.iter().all(|&t| t == VType::Int || t == VType::Float)
                {
                    round_numeric.insert(*body * 1000000 + slot);
                }
                // When all stores are numeric (Int and/or Float), promote to
                // Float if any Float is present — Int→Float is lossless, so a
                // slot that receives both is provably Float. Only non-numeric
                // mixing is a genuine conflict.
                let all_numeric = known.iter().all(|&t| t == VType::Int || t == VType::Float);
                if !all_numeric && known.iter().any(|&t| t != known[0]) {
                    // Genuine non-numeric type conflict — permanently Unknown
                    conflicted.insert(key);
                    if result.get(&key).copied() != Some(VType::Unknown) {
                        result.insert(key, VType::Unknown);
                        changed = true;
                    }
                    continue;
                }
                let resolved = if all_numeric && known.iter().any(|&t| t == VType::Float) {
                    VType::Float
                } else {
                    known[0]
                };
                if poisoned.contains(&slot) { continue; } // opaque store present
                // Merge with existing type: numeric types merge to Float
                let cur = result.get(&key).copied();
                let merged = match (cur, resolved) {
                    (None, _) | (Some(VType::Unknown), _) => Some(resolved),
                    (Some(a), b) if a == b => Some(a),
                    (Some(VType::Int), VType::Float) | (Some(VType::Float), VType::Int) => Some(VType::Float),
                    _ => None,
                };
                match merged {
                    None => {
                        conflicted.insert(key);
                        result.insert(key, VType::Unknown);
                        changed = true;
                    }
                    Some(m) if cur == Some(m) => {}
                    Some(m) => {
                        result.insert(key, m);
                        changed = true;
                    }
                }
            }
        }
        }

        // Finalize return-stack types. Dispatcher (continuation) returns are
        // always shorter than real function returns, so merge only the
        // max-length candidates; per position, a known type beats Unknown,
        // two different known types conflict back to Unknown.
        let mut new_ret_types: HashMap<usize, Vec<VType>> = HashMap::new();
        for (body, cands) in &ret_cands {
            if cands.is_empty() { continue; }
            let nvals = cands.iter().map(|c| c.len()).max().unwrap();
            let top: Vec<&Vec<VType>> = cands.iter().filter(|c| c.len() == nvals).collect();
            let mut rt = vec![VType::Unknown; nvals];
            for c in top {
                for k in 0..nvals {
                    if c[k] != VType::Unknown {
                        if rt[k] == VType::Unknown { rt[k] = c[k]; }
                        else if rt[k] != c[k] { rt[k] = VType::Unknown; }
                    }
                }
            }
            new_ret_types.insert(*body, rt);
        }
        if new_ret_types != ret_types {
            ret_types = new_ret_types;
            changed_outer = true;
        }

        // Seed callee parameter slots from call-site stack snapshots.
        // Conflicting call sites poison the slot (left Unknown).
        let mut seeds: HashMap<(usize, usize), VType> = HashMap::new();
        let mut poison: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
        for (tpc, snap) in &call_snaps {
            if let Some(params) = prologue.get(tpc) {
                for (k, &slot) in params.iter().enumerate() {
                    let ty = if snap.len() > k { snap[snap.len() - 1 - k] } else { VType::Unknown };
                    if ty == VType::Unknown { continue; }
                    let key = (*tpc, slot);
                    if poison.contains(&key) { continue; }
                    match seeds.get(&key) {
                        None => { seeds.insert(key, ty); }
                        Some(&prev) if prev == ty => {}
                        _ => { seeds.remove(&key); poison.insert(key); }
                    }
                }
            }
        }
        for (key, ty) in seeds {
            if conflicted.contains(&key) { continue; }
            let cur = result.get(&key).copied().unwrap_or(VType::Unknown);
            if cur == VType::Unknown {
                result.insert(key, ty);
                seeded.insert(key);
                changed_outer = true;
            }
        }
        numeric_slots = round_numeric;
    }

    if std::env::var("UF_DEBUG_TYPES").is_ok() {
        let mut dbg: Vec<_> = result.iter().collect();
        dbg.sort_by_key(|(k, _)| *k);
        for ((b, s), t) in dbg {
            eprintln!("TYPE body={} slot={} {:?}", b, s, t);
        }
        eprintln!("prologue: {:?}", prologue);
        eprintln!("conflicted: {:?}", conflicted);
        eprintln!("ret_types: {:?}", ret_types);
        eprintln!("seeded: {:?}", seeded);
    }

    (
        result.into_iter().map(|((b, s), t)| (b * 1000000 + s, t)).collect(),
        ins_body,
        numeric_slots,
    )
}

pub fn gen(p: &Parsed, structs: &StructMap, debug: bool) -> String {
    UF_DEBUG.store(debug, Ordering::Relaxed);
    let mut o = String::new();
    o.push_str(PRELUDE);
    // reflection: struct layouts sorted by sid, consumed by op_fields
    let mut by_sid: Vec<(&String, &(Vec<(String, i64)>, i64, i64))> = structs.iter().collect();
    by_sid.sort_by_key(|(_, v)| v.2);
    if !by_sid.is_empty() {
        o.push_str(&format!(
            "static const int64_t uf_sids_v[]={{{}}};\nstatic const int64_t uf_nf_v[]={{{}}};\n",
            by_sid.iter().map(|(_, v)| v.2.to_string()).collect::<Vec<_>>().join(","),
            by_sid.iter().map(|(_, v)| v.0.len().to_string()).collect::<Vec<_>>().join(",")
        ));
        let mut fnames = Vec::new();
        let mut foffs = Vec::new();
        for (i, (_, v)) in by_sid.iter().enumerate() {
            o.push_str(&format!(
                "static const char* uf_f_{}[]={{{}}};\n",
                i,
                v.0.iter().map(|(n, _)| format!("\"{}\"", n)).collect::<Vec<_>>().join(",")
            ));
            o.push_str(&format!(
                "static const int64_t uf_o_{}[]={{{}}};\n",
                i,
                v.0.iter().map(|(_, off)| off.to_string()).collect::<Vec<_>>().join(",")
            ));
            fnames.push(format!("uf_f_{}", i));
            foffs.push(format!("uf_o_{}", i));
        }
        o.push_str(&format!(
            "static const char** uf_fields_v[]={{{}}};\nstatic const int64_t* uf_offs_v[]={{{}}};\nstatic void uf_init_reflection(void){{ uf_st_n={}; uf_st_sids=uf_sids_v; uf_st_nf=uf_nf_v; uf_st_fields=uf_fields_v; uf_st_offs=uf_offs_v; }}\n",
            fnames.join(","),
            foffs.join(","),
            by_sid.len()
        ));
    } else {
        o.push_str("static void uf_init_reflection(void){}\n");
    }
    // string literals: GC-registered tag-9 str objects (pinned), so every
    // string in the language is a first-class str handle
    for (i, s) in p.strings.iter().enumerate() {
        let blen = s.len();
        o.push_str(&format!(
            "static struct {{ void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[{}]; }} uf_sl{} = {{0,0,0,9,{},1,0,0,0,\"{}\"}};\n",
            blen + 1,
            i,
            blen,
            c_escape(s)
        ));
    }
    if !p.strings.is_empty() {
        o.push_str(&format!(
            "static void* uf_lits[] = {{{}}};\n",
            (0..p.strings.len()).map(|i| format!("(void*)&uf_sl{}", i)).collect::<Vec<_>>().join(",")
        ));
    }
    // extern globals (asm alias avoids clashing with libc declarations)
    for (i, name) in p.externs.iter().enumerate() {
        o.push_str(&format!("extern char uf_x{}[] __asm__(\"{}\");\n", i, name));
    }
    let ext_idx: HashMap<&str, usize> = p.externs.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
    // declarations for imported C functions (unprototyped: args are cast at the
    // call; asm alias avoids clashing with libc prototypes, e.g. printf)
    for (i, im) in p.imports.iter().enumerate() {
        o.push_str(&format!(
            "extern {} uf_im{}() __asm__(\"{}\");\n",
            c_type(&im.ret),
            i,
            im.name
        ));
    }
    // variables
    for v in &p.vars {
        o.push_str(&format!("static Cell var_{};\n", v));
    }
    // GC roots: all variables are precise roots
    if !p.vars.is_empty() {
        o.push_str(&format!(
            "static Cell* uf_vroots[] = {{{}}};\n",
            p.vars.iter().map(|v| format!("&var_{}", v)).collect::<Vec<_>>().join(",")
        ));
    }
    // ---- debug metadata (--debug / -D) ----
    if debug {
        // global variable names
        if !p.vars.is_empty() {
            o.push_str(&format!(
                "static const char* uf_vnames_v[] = {{{}}};\n",
                p.vars.iter().map(|v| format!("\"{}\"", v)).collect::<Vec<_>>().join(",")
            ));
        } else {
            o.push_str("static const char** uf_vnames_v = 0;\n");
        }
        // PC -> label name table (inverse of p.labels)
        let n_dbg = p.ins.len() + 1;
        o.push_str(&format!("static const char* uf_labnames_v[{}];\n", n_dbg));
        let mut pc_to_name: std::collections::HashMap<usize, &str> = std::collections::HashMap::new();
        for (name, &pc) in &p.labels {
            pc_to_name.entry(pc).or_insert(name.as_str());
        }
        o.push_str("static void uf_init_labnames(void){");
        for (&pc, &name) in &pc_to_name {
            o.push_str(&format!("uf_labnames_v[{}]=\"{}\";", pc, name));
        }
        if !pc_to_name.contains_key(&0) {
            o.push_str("uf_labnames_v[0]=\"<entry>\";");
        }
        o.push_str("uf_labnames=uf_labnames_v;}\n");
        o.push_str(&format!("static long uf_labnames_n = {};\n", n_dbg));
        // local names per body: emit name arrays at file scope, then init function
        o.push_str(&format!("static const char** uf_ln_tab_v[{}];\nstatic long uf_ln_cnt_v[{}];\n", n_dbg, n_dbg));
        for (&body_pc, names) in &p.local_names {
            if names.is_empty() { continue; }
            o.push_str(&format!(
                "static const char* uf_ln_b{}[]={{{}}};\n",
                body_pc,
                names.iter().map(|n| format!("\"{}\"", n)).collect::<Vec<_>>().join(",")
            ));
        }
        o.push_str("static void uf_init_local_names(void){");
        for (&body_pc, names) in &p.local_names {
            if names.is_empty() { continue; }
            o.push_str(&format!("uf_ln_tab_v[{}]=uf_ln_b{};uf_ln_cnt_v[{}]={};", body_pc, body_pc, body_pc, names.len()));
        }
        o.push_str("uf_ln_tab=uf_ln_tab_v;uf_ln_cnt=uf_ln_cnt_v;}\n");
    }
    // v11: per-label local frame sizes. We emit a flat array indexed by
    // instruction PC (sparse, but simple and fast). uf_local_counts[pc] gives
    // the frame size for the call-entry label starting at pc.
    let n_ins = p.ins.len();
    o.push_str(&format!("static long uf_lc_v[{}];\n", n_ins + 1));
    if !p.local_counts.is_empty() {
        o.push_str("static void uf_init_locals(void){");
        for (&pc, &count) in &p.local_counts {
            o.push_str(&format!("uf_lc_v[{}]={};", pc, count));
        }
        o.push_str("}\n");
    } else {
        o.push_str("static void uf_init_locals(void){}\n");
    }
    o.push_str("static long uf_lc(long pc){ return (pc>=0&&(unsigned long)pc<(unsigned long)(sizeof(uf_lc_v)/sizeof(uf_lc_v[0])))?uf_lc_v[pc]:0; }\n");
    // label -> instruction index
    let resolve = |name: &str| -> usize {
        *p.labels.get(name).unwrap_or_else(|| panic!("undefined label {}", name))
    };
    o.push_str("\nstatic void uflux_run(Ctx*cx, long pc){\n  uf_current_ctx=cx;\n  if(pc<0){ goto *(void*)uf_entry_addr; }\n  /* v11: set up the entry label's local frame (v13: capacity-checked) */\n  cx->local_frames[cx->local_fsp++]=cx->local_base; cx->local_base+=uf_lc(pc); if(cx->local_base>cx->local_cap)die(\"local frame overflow\");\n");
    let n = p.ins.len();
    // Only labels that can be entered dynamically (initial pc, FOR bodies via
    // PushAddr, weave task entries, SEND methods, exports) go into labtab.
    // Every other label is reached exclusively by direct gotos, so leaving it
    // out of any address-taken context lets cc -O2 optimize across the static
    // control flow (loops, conditionals) instead of treating each label as an
    // irreducible indirect-branch target.
    let mut dyn_idx: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    dyn_idx.insert(0);
    for ins in &p.ins {
        match ins {
            Ins::PushAddr(l) => {
                dyn_idx.insert(resolve(l));
            }
            Ins::Weave(tasks) => {
                for t in tasks {
                    dyn_idx.insert(t.pc);
                }
            }
            _ => {}
        }
    }
    for (_, _, l) in &p.methods {
        dyn_idx.insert(resolve(l));
    }
    for (_, l) in &p.exports {
        dyn_idx.insert(resolve(l));
    }
    // Add init-TU entry points to dyn_idx so their labels are in labtab
    for &ipc in &p.init_pcs {
        dyn_idx.insert(ipc);
    }
    o.push_str("  static const void* labtab[] = {");
    for &i in &dyn_idx {
        o.push_str(&format!("[{}]=&&L_{},", i, i));
    }
    if dyn_idx.is_empty() {
        o.push_str("[0]=&&L_0,");
    }
    o.push_str("};\n");
    // init-TU spawns: each init.uf entry point gets a detached pthread,
    // spawned exactly once when main_cx enters at pc 0. We run this before
    // the labtab dispatch so it executes on the initial call with pc=0.
    if !p.init_pcs.is_empty() {
        o.push_str("  if(pc==0 && cx==main_cx) {");
        for &ipc in &p.init_pcs {
            o.push_str(&format!(
                "{{ pthread_t th; if(pthread_create(&th,0,uf_init_worker,(void*)&&L_{})) die(\"init thread\"); pthread_detach(th); }}",
                ipc
            ));
        }
        o.push_str("  }\n");
    }
    // ENTRY: if the program has an entry label, pc 0 jumps to it (replaces jmp main)
    if let Some(el) = &p.entry_label {
        let eidx = resolve(el);
        o.push_str(&format!("  if(pc==0) goto L_{};\n", eidx));
    }
    o.push_str("  goto *labtab[pc];\n");
    // method dispatch table (SEND): (type key, name hash) -> code address
    o.push_str("  static const struct { int64_t tk; int64_t mh; const void* lab; } uf_mt[] = {");
    if p.methods.is_empty() {
        o.push_str("{0,0,&&L_0}");
    } else {
        for (tk, mh, l) in &p.methods {
            o.push_str(&format!("{{{},{}LL,&&L_{}}},", tk, mh, resolve(l)));
        }
    }
    o.push_str("};\n");
    // Dispatch emission: basic-block stack virtualization (see emit_range
    // above) — jump targets start basic blocks.
    let targets: std::collections::HashSet<usize> = p.labels.values().copied().collect();
    // FOR inlining: a For immediately preceded by PushAddr of a compile-time
    // known label, whose body is structurally inlinable (no early RET, no
    // internal jump leaving the body range), is emitted as a direct C loop
    // over a renamed inline copy of the body (emit_range, recursively for
    // nested FORs). The PushAddr feeding it is elided. Any other FOR
    // (computed address, weird layout, deep recursion) keeps the subroutine
    // path.
    let mut inline_fors: HashMap<usize, (usize, usize)> = HashMap::new();
    let mut inline_ffolds: HashMap<usize, (usize, usize)> = HashMap::new();
    let mut inline_whiles: HashMap<usize, (usize, usize, usize, usize)> = HashMap::new();
    let mut suppress: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for j in 1..p.ins.len() {
        if let (Ins::PushAddr(l), Ins::For) = (&p.ins[j - 1], &p.ins[j]) {
            if targets.contains(&(j - 1)) {
                continue; // control can jump onto the PushAddr; keep it
            }
            let bs = resolve(l);
            if let Some(be) = for_body_range(&p.ins, bs) {
                if inlinable_for(p, bs, be) {
                    inline_fors.insert(j, (bs, be));
                    suppress.insert(j - 1);
                }
            }
        }
        // FFOLD inlining: detect PushAddr(label), Simple("op_ffold")
        if let (Ins::PushAddr(l), Ins::Simple(h)) = (&p.ins[j - 1], &p.ins[j]) {
            if *h == "op_ffold" && !targets.contains(&(j - 1)) {
                let bs = resolve(l);
                if let Some(be) = for_body_range(&p.ins, bs) {
                    if inlinable_for(p, bs, be) {
                        inline_ffolds.insert(j, (bs, be));
                        suppress.insert(j - 1);
                    }
                }
            }
            // FSPLIT inlining: same pattern as FFOLD
            if *h == "op_fsplit" && !targets.contains(&(j - 1)) {
                let bs = resolve(l);
                if let Some(be) = for_body_range(&p.ins, bs) {
                    if inlinable_for(p, bs, be) {
                        inline_ffolds.insert(j, (bs, be)); // reuse the same map
                        suppress.insert(j - 1);
                    }
                }
            }
            // RANGEFOLD inlining: same pattern as FFOLD
            if *h == "op_rangefold" && !targets.contains(&(j - 1)) {
                let bs = resolve(l);
                if let Some(be) = for_body_range(&p.ins, bs) {
                    if inlinable_for(p, bs, be) {
                        inline_ffolds.insert(j, (bs, be));
                        suppress.insert(j - 1);
                    }
                }
            }
        }
        // CALL inlining was removed in v13: every call now goes through the
        // standard cs mechanism so the callee's RET can drain the data stack
        // back to the caller's saved pointer.
        // IR order: PushAddr(cond_label) at j-2, PushAddr(body_label) at j-1
        if j >= 2 {
            if let (Ins::PushAddr(cl), Ins::PushAddr(bl), Ins::While) = (&p.ins[j - 2], &p.ins[j - 1], &p.ins[j]) {
                if !targets.contains(&(j - 2)) && !targets.contains(&(j - 1)) {
                    let bbs = resolve(bl);
                    let cbs = resolve(cl);
                    if let (Some(bbe), Some(cbe)) = (for_body_range(&p.ins, bbs), for_body_range(&p.ins, cbs)) {
                        if inlinable_for(p, bbs, bbe) && inlinable_for(p, cbs, cbe) {
                            inline_whiles.insert(j, (bbs, bbe, cbs, cbe));
                            suppress.insert(j - 2);
                            suppress.insert(j - 1);
                        }
                    }
                }
            }
        }
    }
    // Compute types and body mapping BEFORE outlined-body detection so we
    // can find the full extent of each call body (including continuation labels).
    let (mut local_types, ins_body, numeric_slots) = compute_local_types(p);

    // Outlined call bodies: detect call bodies that use locals and contain
    // while loops but don't call other uf bodies (only externs). These are
    // emitted as separate C functions so GCC's register allocator handles
    // the hot inner loops without being overwhelmed by the monolithic
    // dispatch function's register pressure.
    let mut outlined_bodies: HashMap<usize, (usize, usize)> = HashMap::new(); // call_pc -> (body_start, body_end)
    for j in 1..p.ins.len() {
        if let Ins::Call(l) = &p.ins[j] {
            let bs = resolve(l);
            // Find the full body extent using ins_body: all instructions k
            // where ins_body[k] == bs form the body (including continuation
            // labels like while-cond/while-body that belong to this body).
            let be = {
                let mut end = bs + 1;
                while end < p.ins.len() && ins_body[end] == bs { end += 1; }
                end
            };
            // Must use locals
            let has_locals = (bs..be).any(|k| matches!(p.ins[k], Ins::LocalSetI(_) | Ins::LocalGetI(_)));
            if !has_locals { continue; }
            // Must contain at least one while or inlined-for loop (the hot pattern)
            let has_loop = (bs..be).any(|k| {
                matches!(p.ins[k], Ins::While) ||
                (matches!(p.ins[k], Ins::For) && inline_fors.contains_key(&k))
            });
            if !has_loop { continue; }
            // Must not call other uf bodies (only externs/simple ops)
            let calls_others = (bs..be).any(|k| matches!(&p.ins[k], Ins::Call(_)));
            if calls_others { continue; }
            // Must not have unsuppressed PushAddr (inlined-while PushAddrs are
            // suppressed and fine — they never use the call-stack mechanism)
            let has_pushaddr = (bs..be).any(|k| matches!(&p.ins[k], Ins::PushAddr(_)) && !suppress.contains(&k));
            if has_pushaddr { continue; }
            // Outline EVERY call site of the label (not just the first): the
            // threaded `goto L_` fallback runs the loop inside the monolithic
            // uflux_run where GCC's register allocator degrades, so any
            // additional call site must also go through the small function.
            outlined_bodies.insert(j, (bs, be));
        }
    }

    // Emit outlined body functions BEFORE uflux_run (one per body_start, even
    // if multiple call sites share it)
    let mut outlined_fns = String::new();
    let mut outlined_emitted: std::collections::HashSet<usize> = std::collections::HashSet::new(); // body_starts already emitted
    for (&_call_pc, &(bs, be)) in &outlined_bodies {
        if outlined_emitted.contains(&bs) { continue; }
        outlined_emitted.insert(bs);
        let fname = format!("uf_ob_{}", bs);
        let lc = p.local_counts.get(&bs).copied().unwrap_or(0);
        let oarity = p.label_params.get(&bs).map(|v| v.len()).unwrap_or(0);
        outlined_fns.push_str(&format!(
            "static void {}(Ctx*cx){{cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+={};\n",
            fname, lc
        ));
        // v13: the outline's own pre-arg data-stack pointer — its RET drains
        // back to it and pushes the return value
        outlined_fns.push_str(&format!("long _osp=cx->sp>{}?cx->sp-{}:0;\n", oarity, oarity));
        let ob_prefix = format!("OB{}_", bs);
        // Emit the body code — use empty reg/arr_ptr maps; the inlined while
        // loops within the body will do their own register caching.
        emit_range(&mut outlined_fns, p, &targets, &inline_fors, &inline_ffolds, &inline_whiles, &outlined_bodies, &suppress, &ext_idx, bs, be, &ob_prefix, 0, &mut local_types, &ins_body, &HashMap::new(), &numeric_slots, &HashMap::new(), false);
        // If the body has no explicit RET at the end (shouldn't happen, but
        // be safe), restore the frame.
        outlined_fns.push_str(&format!("cx->local_base=cx->local_frames[--cx->local_fsp];{}}}\n", if UF_DEBUG.load(Ordering::Relaxed) { "cx->call_csp--;" } else { "" }));
    }
    // Insert outlined functions after uf_lc but before uflux_run's body
    if !outlined_fns.is_empty() {
        // Find the uflux_run function definition (not the forward declaration).
        // The forward declaration is "static void uflux_run(Ctx*cx, long pc);"
        // while the definition is "static void uflux_run(Ctx*cx, long pc){".
        let search = "static void uflux_run(Ctx*cx, long pc){";
        let insert_pos = o.find(search).unwrap_or_else(|| {
            // Fallback: find the last occurrence of uflux_run
            o.rfind("static void uflux_run").unwrap_or(o.len())
        });
        o.insert_str(insert_pos, &outlined_fns);
    }
    emit_range(&mut o, p, &targets, &inline_fors, &inline_ffolds, &inline_whiles, &outlined_bodies, &suppress, &ext_idx, 0, n, "", 0, &mut local_types, &ins_body, &HashMap::new(), &numeric_slots, &HashMap::new(), false);
    o.push_str(&format!("L_{}: return;\n}}\n", n));

    // exported wrappers (fixed 4-arg C ABI trampoline, run on the main ctx)
    for (cname, label) in &p.exports {
        let lidx = resolve(label);
        o.push_str(&format!(
            "uint64_t {}(uint64_t a0,uint64_t a1,uint64_t a2,uint64_t a3){{Ctx*cx=main_cx;long base=cx->sp;long _lb=cx->local_base;pushp(cx,(void*)a0);pushp(cx,(void*)a1);pushp(cx,(void*)a2);pushp(cx,(void*)a3);uflux_run(cx,{});uint64_t r=(cx->sp>base)?(uint64_t)pop(cx).i:0;cx->sp=base;cx->local_base=_lb;return r;}}\n",
            cname, lidx
        ));
    }
    let lits_arg = if p.strings.is_empty() { "0,0".to_string() } else { format!("uf_lits,{}", p.strings.len()) };
    let roots_arg = if p.vars.is_empty() { "0,0".to_string() } else { format!("uf_vroots,{}", p.vars.len()) };
    let dbg_init = if debug {
        "uf_debug_mode=1;uf_vnames=uf_vnames_v;uf_init_labnames();uf_init_local_names();"
    } else { "" };
    o.push_str(&format!("int main(int argc,char**argv){{uf_argc=argc;uf_argv=(void*)argv;uf_init_reflection();uf_init_locals();{}uf_init_lits({});uf_gc_setroots({});uf_gc_init();uflux_run(main_cx,0);return 0;}}\n", dbg_init, lits_arg, roots_arg));
    o
}

pub fn gen_call_ext(im: &Import, sym: &str) -> String {
    let vararg = im.params.iter().any(|t| t == "...");
    let fixed: Vec<&String> = im.params.iter().filter(|t| *t != "...").collect();
    let mut o = String::from("{");
    if vararg {
        // printf-style: format string is fixed param 0 (ptr); count % directives
        o.push_str("int c=uf_vargc(cx);Cell ex[8];if(c>8)die(\"vararg: too many args\");for(int k=c-1;k>=0;k--){ex[k]=pop(cx);if(ex[k].tag==2&&ex[k].i&&uf_is_str(ex[k]))ex[k].i=(int64_t)uf_sptr(ex[k]);}");
        for (k, _) in fixed.iter().enumerate().rev() {
            o.push_str(&format!("Cell a{}=pop(cx);", k));
        }
        if im.ret == "void" {
            o.push_str("switch(c){");
        } else {
            o.push_str(&format!("{} r;switch(c){{", c_retty(&im.ret)));
        }
        let fty: Vec<&str> = fixed.iter().map(|t| c_type(t)).collect();
        for c in 0..=8 {
            let casts: Vec<String> = fixed
                .iter()
                .enumerate()
                .map(|(k, t)| arg_cast(c_type(t), &format!("a{}", k)))
                .collect();
            let mut callargs = casts;
            for k in 0..c {
                callargs.push(format!("ex[{}].i", k));
            }
            let dots = if c > 0 { ",..." } else { "" };
            o.push_str(&format!(
                "case {}: {}(({}(*)({}{})){})({});break;",
                c,
                if im.ret == "void" { "" } else { "r=" },
                c_retty(&im.ret),
                fty.join(","),
                dots,
                sym,
                callargs.join(",")
            ));
        }
        o.push_str("default: die(\"vararg: too many args\");}");
        if im.ret != "void" {
            o.push_str(&ret_push(&im.ret, "r"));
        }
        o.push_str("}\n");
        return o;
    }
    for (k, _) in fixed.iter().enumerate().rev() {
        o.push_str(&format!("Cell a{}=pop(cx);", k));
    }
    let fty: Vec<&str> = fixed.iter().map(|t| c_type(t)).collect();
    let casts: Vec<String> = fixed
        .iter()
        .enumerate()
        .map(|(k, t)| arg_cast(c_type(t), &format!("a{}", k)))
        .collect();
    if im.ret == "void" {
        o.push_str(&format!(
            "((void(*)({})){})({});",
            fty.join(","),
            sym,
            casts.join(",")
        ));
    } else {
        o.push_str(&format!(
            "{} r=(({}(*)({})){})({});",
            c_retty(&im.ret),
            c_retty(&im.ret),
            fty.join(","),
            sym,
            casts.join(",")
        ));
        o.push_str(&ret_push(&im.ret, "r"));
    }
    o.push_str("}\n");
    o
}

pub fn arg_cast(ct: &str, var: &str) -> String {
    match ct {
        "int64_t" => format!("(int64_t)({0}.tag==T_FLOAT?(int64_t)uf_f({0}):{0}.i)", var),
        "double" => format!("uf_f({})", var),
        "void*" => format!("(void*)uf_sptr({})", var),
        "char" => format!("(char){}.i", var),
        _ => var.to_string(),
    }
}

pub fn ret_push(ret: &str, var: &str) -> String {
    match ret {
        "int" => format!("pushi(cx,(int64_t){});", var),
        "float" => format!("pushf(cx,{});", var),
        "ptr" | "handle" => format!("pushp(cx,{});", var),
        "byte" => format!("pushi(cx,(int64_t){});", var),
        _ => String::new(),
    }
}

// ---------------- driver ----------------
