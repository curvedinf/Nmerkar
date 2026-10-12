// JavaScript target codegen.
//
// Emits a self-contained ES5 script: the shared runtime (js_prelude.rs), the
// program's string table, one function per dynamic code segment, and the
// `__NK_API` handle consumed by node and the web glue.
//
// A segment is the instruction range [pc, next_dyn_pc) where next_dyn_pc is
// the next pc execution can re-enter: 0, any PushAddr/Call/Goto target, any
// weave task pc, or the entry label. Every C `goto L_x` becomes a JS function
// boundary, so control flow maps to plain calls/returns: break/cont unwind
// through a caught sentinel throw, and the C call-stack drain (rsps) is the
// `fr = {drain, base}` argument each function carries.
//
// Semantics follow the C target (gen.rs) instruction for instruction.
// Divergences: exports and init threads are compile-time rejections;
// syscalls are runtime dies; weave fanout runs items sequentially in item
// order; shell ops need node (in the browser, shp yields an empty channel);
// time ops accept the "unix" format only; cap/caps report the
// unrestricted-CPU model; the scan cursor reads env.stdin. See SPEC.md
// ("JavaScript target").

use crate::ast::*;
use crate::js_prelude::JS_PRELUDE;
use std::collections::{HashMap, HashSet};

// Ops with a runtime implementation in js_prelude.rs (the RT export list).
// Anything not listed is rejected at codegen time.
const JS_OPS: &[&str] = &[
    "op_add", "op_sub", "op_mul", "op_and", "op_or", "op_xor", "op_shl", "op_shr", "op_bnot",
    "op_inc", "op_dec", "op_pow", "op_sqrt", "op_div", "op_rem", "op_lt", "op_gt", "op_lte",
    "op_gte", "op_eq", "op_seq", "op_sne", "op_not", "op_orelse", "op_drop", "op_cast",
    "op_dcast", "op_dbg", "op_check", "op_timer", "op_memstats", "op_typeof", "op_len", "op_list", "op_dict", "op_get", "op_getq", "op_set",
    "op_del", "op_has", "op_keys", "op_cat", "op_slice", "op_push", "op_lpop", "op_range",
    "op_glob", "op_split", "op_join", "op_find", "op_repl", "op_trim", "op_up", "op_down",
    "op_starts", "op_ends", "op_atoi", "op_atof", "op_itoa", "op_ftoa", "op_match",
    "op_replace", "op_rsplit", "op_iter", "op_next", "op_collect", "op_imap", "op_ifilter",
    "op_sort", "op_sortkeys", "op_topn", "op_rangefold", "op_filter", "op_some", "op_every",
    "op_unique", "op_flat", "op_json", "op_unjson", "op_print", "op_fmt",
    "op_arr", "op_tensor", "op_vget", "op_vset", "op_vsum", "op_vfold",
    "op_vemax", "op_vemin", "op_veq", "op_vlt", "op_vgt", "op_vge", "op_vle",
    "op_vand", "op_vor", "op_vnot", "op_vcount", "op_vgather",
    "op_vmean", "op_vmin", "op_vmax", "op_vmap", "op_vargsort",
    "op_vsearchsorted", "op_vwhere", "op_transpose",
    "op_malloc", "op_free", "op_buf", "op_bufcopy", "op_loadx", "op_storex",
    "op_chan", "op_enq", "op_deq", "op_close", "op_atom", "op_aget", "op_aset",
    "op_aadd", "op_cas", "op_spawn",
    "op_addto", "op_agg", "op_argi", "op_argv", "op_badd", "op_bfs",
    "op_bloom", "op_btest", "op_chunk", "op_dfs", "op_exec", "op_faddto",
    "op_fatof", "op_fatoi", "op_fbyte", "op_feach", "op_femit", "op_ffold",
    "op_fget", "op_finc", "op_fmatch", "op_fsget", "op_fsplit", "op_gc",
    "op_group", "op_hasargs", "op_mmap", "op_now", "op_obj", "op_retry",
    "op_sh", "op_shp", "op_slurp", "op_spit", "op_time", "op_timef",
    "op_try", "op_wfind", "op_clone", "op_sizeof", "op_shutdown",
    "op_scan", "op_cap", "op_caps",
];

fn is_js_op(h: &str) -> bool {
    JS_OPS.contains(&h)
}

// f64 -> JS literal. Rust's debug of +/-inf is "inf"/"-inf"; JS spells them
// Infinity/-Infinity. Everything else (NaN, -0.0, decimals, exponents) is a
// valid JS numeric literal verbatim.
fn float_to_js(f: f64) -> String {
    let t = format!("{:?}", f);
    if t == "inf" {
        "Infinity".to_string()
    } else if t == "-inf" {
        "-Infinity".to_string()
    } else {
        t
    }
}

struct Ctx {
    ins_body: Vec<usize>,
    targets: HashSet<usize>, // every label pc
    discard: HashSet<usize>, // continuation-body labels (ret value dropped)
    weave_order: HashMap<usize, Vec<usize>>, // weave ins pc -> topo task order
}

const SB_DIE_FALLBACK: &str = "missing label parameter at runtime \u{2014} null-fill was removed";

// The runtime die text for a guarded param pop at instruction pc (mirrors
// gen.rs sb_die_expr, unwrapped: the message string, not a die("...") call).
fn sb_die_text(p: &Parsed, pc: usize) -> String {
    let off = match p.param_pcs.get(&pc) {
        Some(o) => *o,
        None => return SB_DIE_FALLBACK.to_string(),
    };
    let mut best_lpc = None;
    for (&lpc, _) in &p.label_params {
        if lpc <= pc && best_lpc.map_or(true, |b: usize| lpc > b) {
            best_lpc = Some(lpc);
        }
    }
    let (lpc, params) = match (best_lpc, best_lpc.and_then(|l| p.label_params.get(&l))) {
        (Some(l), Some(prs)) => (l, prs),
        _ => return SB_DIE_FALLBACK.to_string(),
    };
    let name = match params.get(off) {
        Some(Param::Local(n)) | Some(Param::Global(n)) => n.clone(),
        Some(Param::Discard) => "_!".to_string(),
        None => return SB_DIE_FALLBACK.to_string(),
    };
    let label = match p.labels.iter().find(|(_, &v)| v == lpc) {
        Some((k, _)) => k.clone(),
        None => return SB_DIE_FALLBACK.to_string(),
    };
    format!(
        "label '{}': missing parameter '{}' ({} parameter(s) declared, fewer present at runtime \u{2014} null-fill was removed)",
        label, name, params.len()
    )
}

// Guarded consume-pop for SetV / LocalSetI / op_drop param binds. `off` is
// the param_pcs entry (arity-1-k); the guard is the caller's saved stack
// pointer (fr.drain) plus the offset of the remaining slots. With `store`,
// the popped value is written to that frame slot (local params -> rt.L,
// global params -> the shared cell in rt.G, mirroring C's uf_sh_set);
// `_!` binds discard.
fn guard_pop(o: &mut String, x: usize, off: usize, p: &Parsed, store: Option<&str>) {
    let base = if off > 0 {
        format!("fr.drain+{}", off)
    } else {
        "fr.drain".to_string()
    };
    let r = match store {
        Some(s) => format!("{}=st.pop();", s),
        None => "st.pop();".to_string(),
    };
    o.push_str(&format!(
        "if(st.length>{}&&st.length>0){{{};}}else{{RT.die(rt,\"{}\");}}\n",
        base,
        r,
        sb_die_text(p, x)
    ));
}

fn resolve_label(p: &Parsed, name: &str) -> usize {
    *p.labels
        .get(name)
        .unwrap_or_else(|| panic!("undefined label {}", name))
}

// Find the ListStart/DictStart index that pairs with the literal at `i`.
// The C target keeps a LIFO `lit_starts` stack (gen.rs); here we rescan
// backward instead — starts are `(`, lits are `)`. Nested literals are
// balanced, so a lit seen while scanning back closes an opener further back
// and is counted, while the first start at depth 0 is the match.
fn find_lit_start(p: &Parsed, i: usize, seg_start: usize) -> usize {
    let mut depth = 0;
    let mut j = i;
    while j > seg_start {
        j -= 1;
        match &p.ins[j] {
            Ins::ListLit | Ins::DictLit => depth += 1,
            Ins::ListStart | Ins::DictStart => {
                if depth == 0 {
                    return j;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    panic!("list/dict literal at pc {} has no matching start in segment from pc {}", i, seg_start)
}

// Emit one instruction at index x of the segment starting at seg_start.
fn emit_ins(o: &mut String, i: usize, seg_start: usize, p: &Parsed, ctx: &Ctx) {
    // mark the instruction in flight: die() formats its source line (the
    // __SITES table mirrors the C uf_sites)
    o.push_str(&format!("rt.site={};\n", i));
    match &p.ins[i] {
        Ins::PushI(v) => {
            o.push_str(&format!("iv.push(RT.I({}n));\n", v));
        }
        Ins::PushF(f) => {
            o.push_str(&format!("iv.push(RT.F({}));\n", float_to_js(*f)));
        }
        Ins::PushS(idx) => {
            o.push_str(&format!("iv.push(RT.S(__STRS[{}]));\n", idx));
        }
        Ins::PushAddr(l) => {
            // C elides the PushAddrs feeding an inlined While/For: the
            // addresses never reach the data stack. Buffer them quietly; the
            // While/For arm pops them from the buffer (which it then
            // discards, vdiscard) so stray buffered values are killed too.
            let quiet = matches!(p.ins.get(i + 1), Some(Ins::While | Ins::For))
                || (matches!(p.ins.get(i + 1), Some(Ins::PushAddr(_)))
                    && matches!(p.ins.get(i + 2), Some(Ins::While)));
            if quiet {
                o.push_str(&format!("iv.push(__NKAD[{}]);\n", resolve_label(p, l)));
            } else {
                o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
                o.push_str(&format!("st.push(__NKAD[{}]);\n", resolve_label(p, l)));
            }
        }
        Ins::Simple(h) => {
            if !is_js_op(h) {
                panic!("{}: not supported by the JavaScript target", h);
            }
            // op_drop with a declared `_!` parameter: guarded consume-pop, no
            // flush (the C target continues before the vflush)
            if *h == "op_drop" {
                if let Some(off) = p.param_pcs.get(&i).copied() {
                    guard_pop(o, i, off, p, None);
                    return;
                }
            }
            let bin = matches!(
                *h,
                "op_add" | "op_sub" | "op_mul" | "op_and" | "op_div" | "op_rem" | "op_lt"
                    | "op_gt" | "op_lte" | "op_gte" | "op_eq" | "op_or" | "op_xor"
                    | "op_shr"
            );
            let un = matches!(*h, "op_inc" | "op_dec" | "op_not");
            if bin {
                // C materializes the WHOLE vstack before consuming anything,
                // so the buffer must be fully flushed before the pops:
                // iv == C's vstack at all times (ret and prelude framing
                // both rely on that). Every op call is awaited: sync ops
                // resolve on a microtask, blocking chan ops suspend the
                // whole task chain.
                o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
                o.push_str(&format!(
                    "var _b{}=RT.pop(rt,\"\");var _a{}=RT.pop(rt,\"\");st.push(_a{});st.push(_b{});await RT.{}(rt);iv.push(st.pop());\n",
                    i, i, i, i, h
                ));
            } else if un {
                o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
                o.push_str(&format!(
                    "var _a{}=RT.pop(rt,\"\");st.push(_a{});await RT.{}(rt);iv.push(st.pop());\n",
                    i, i, h
                ));
            } else {
                // generic op: materialize the buffer, run on the real stack
                o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
                o.push_str(&format!("rt.opName=\"{}\";await RT.{}(rt);\n", h, h));
            }
        }
        Ins::For => {
            // Mirrors C's inlined for. C's loop body is the body's ins
            // (minus the terminal ret) + vflush + `cx->sp=_sp0`, so the loop
            // leaves one cell in the count's slot: the value the FIRST
            // iteration last wrote there (the index push, the body's first
            // real push, or a pass-through vflush) — or the stale count cell
            // if the first iteration writes nothing. Later iterations work
            // one slot higher and never touch it, so at loop exit the depth
            // is restored and the top is that first-iteration cell (a zero
            // iteration for consumes the count and leaves nothing). The
            // noreturn flag suppresses the ret 0-fill so an empty body
            // leaves the slot to the index/stale count exactly as C does.
            o.push_str(&format!("var _b{}=iv.pop();\n", i));
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
            o.push_str(&format!("var _sp0{}=st.length;\n", i));
            o.push_str(&format!("var _ccell{}=st[_sp0{}-1];\n", i, i));
            o.push_str(&format!(
                "var _c{}=RT.asInt(_ccell{});st.length=_sp0{}-1;\n",
                i, i, i
            ));
            o.push_str(&format!(
                "var _f{}=rt.lsp;rt.lsp++;if(rt.lsp>=64)RT.die(rt,\"loops nested too deep\");\n",
                i
            ));
            o.push_str(&format!("var _X{}=_ccell{};\n", i, i));
            o.push_str(&format!(
                "for(var _k{}=0n;_k{}<_c{};_k{}++){{st.push(RT.I(_k{}));\n",
                i, i, i, i, i
            ));
            o.push_str(&format!(
                "try{{await _b{}.v(rt,{{drain:_sp0{}-1,base:fr.base,noreturn:1}});if(_k{}===0n&&st.length>_sp0{}-1)_X{}=st[_sp0{}-1];st.length=_sp0{};st[_sp0{}-1]=_X{};}}\n",
                i, i, i, i, i, i, i, i, i
            ));
            o.push_str(&format!(
                "catch(_e{}){{if(_e{}===RT.BREAK){{st.length=_sp0{};st[_sp0{}-1]=_X{};break;}}else if(_e{}===RT.CONT){{if(_k{}===0n&&st.length>_sp0{}-1)_X{}=st[_sp0{}-1];st.length=_sp0{};st[_sp0{}-1]=_X{};continue;}}else throw _e{};}}}}\n",
                i, i, i, i, i, i, i, i, i, i, i, i, i, i
            ));
            o.push_str(&format!("rt.lsp=_f{};\n", i));
        }
        Ins::Call(l) => {
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
            let t = resolve_label(p, l);
            let arity = p.label_params.get(&t).map(|v| v.len()).unwrap_or(0);
            // the callee frame is pushed past the CALLER's live slots (its
            // body's local count), not by the callee's own count
            let caller_n = p.local_counts.get(&ctx.ins_body[i]).copied().unwrap_or(0);
            o.push_str(&format!("var _lb{}=rt.lb;rt.lb+={};\n", i, caller_n));
            o.push_str(&format!(
                "await F_{}(rt,{{drain:st.length>{}?st.length-{}:0,base:rt.lb}});\n",
                t, arity, arity
            ));
            o.push_str(&format!("rt.lb=_lb{};\n", i));
        }
        Ins::CallExt(idx) => {
            // FFI call conventions (match the C target): a variadic import
            // (`...` in the param list) is called with the topmost cell the
            // vararg count, then the varargs (topmost first), then the fixed
            // params; a non-variadic import pushes only the fixed params —
            // no count cell. All FFI symbols are synchronous prelude
            // implementations, so no await.
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
            let im = &p.imports[*idx];
            let opn = format!("call {}", im.name);
            o.push_str(&format!("rt.opName=\"{}\";\n", opn));
            let c = opn.replace('"', "\\\"");
            let vararg = im.params.iter().any(|t| t.as_str() == "...");
            if vararg {
                o.push_str(&format!(
                    "var _cI=Number(RT.asInt(RT.pop(rt,\"{}\")));if(_cI<0||_cI>8)RT.die(rt,\"variadic call: vararg count out of range\");\n",
                    c
                ));
                o.push_str(&format!(
                    "var _exI=new Array(_cI);for(var _kI=_cI-1;_kI>=0;_kI--)_exI[_kI]=RT.pop(rt,\"{}\");\n",
                    c
                ));
            }
            o.push_str("var _fxI=[];\n");
            for _ in (0..im.params.iter().filter(|q| q.as_str() != "...").count()).rev() {
                o.push_str(&format!("_fxI.push(RT.pop(rt,\"{}\"));\n", c));
            }
            o.push_str("_fxI.reverse();\n");
            if vararg {
                o.push_str(&format!(
                    "var _allI=_fxI.concat(_exI);var _rI=RT.ffiCall(rt,\"{}\",_allI);if(_rI!==null)st.push(_rI);\n",
                    im.name
                ));
            } else {
                o.push_str(&format!(
                    "var _rI=RT.ffiCall(rt,\"{}\",_fxI);if(_rI!==null)st.push(_rI);\n",
                    im.name
                ));
            }
        }
        Ins::Ret => {
            // vstack snapshot, then the three-way return-value rule: empty ->
            // caller top or 0; single -> the value; multi -> a list, except in
            // a continuation body whose ret value the caller drops (C then
            // returns 0 instead of building a list)
            let mut drop = false;
            if let Some(&cur) = ctx.targets.iter().filter(|&&t| t <= i).max() {
                drop = ctx.discard.contains(&cur);
            }
            let multi: String = if drop {
                "RT.I(0n)".to_string()
            } else {
                format!("RT.L(_v{})", i)
            };
            // for-continuation bodies (fr.noreturn): C's inlined body ends
            // in a vflush + sp restore, not a ret push — flush the leftover
            // virtual values raw (no 0-fill, no list fold, no drain) and
            // leave depth/slot handling to the For wrapper.
            o.push_str(
                "if(fr.noreturn){if(iv.length)st.push.apply(st,iv.splice(0));return null;}\n",
            );
            o.push_str(&format!(
                "var _n{}=iv.length;var _v{}=iv.splice(0);var _rv{};if(_n{}===0){{_rv{}=(st.length>fr.drain&&st.length>0)?st[st.length-1]:RT.I(0n);}}else if(_n{}===1){{_rv{}=_v{}[0];}}else{{_rv{}={}}}\n",
                i, i, i, i, i, i, i, i, i, multi
            ));
            o.push_str(&format!(
                "st.length=fr.drain;st.push(_rv{});return _rv{};\n",
                i, i
            ));
        }
        Ins::SetV(v) => {
            if let Some(off) = p.param_pcs.get(&i).copied() {
                guard_pop(
                    o,
                    i,
                    off,
                    p,
                    Some(&format!("rt.G[\"{}\"]", v)),
                );
            } else {
                o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
                o.push_str(&format!(
                    "var _s{}=RT.pop(rt,\"\");rt.G[\"{}\"]=_s{};\n",
                    i, v, i
                ));
            }
        }
        Ins::GetV(v) => {
            o.push_str(&format!(
                "iv.push(rt.G[\"{}\"]===undefined?RT.I(0n):rt.G[\"{}\"]);\n",
                v, v
            ));
        }
        Ins::AtomicAdd(v, inc) => {
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
            o.push_str("rt.opName=\"atomic_add\";\n");
            o.push_str(&format!(
                "var _ov{}=rt.G[\"{}\"];if(_ov{}===undefined)_ov{}=RT.I(0n);\n",
                i, v, i, i
            ));
            let dv = if *inc {
                "RT.I(1n)".to_string()
            } else {
                "RT.pop(rt,\"\")".to_string()
            };
            o.push_str(&format!(
                "var _dv{}={};st.push(_ov{});st.push(_dv{});RT.op_add(rt);rt.G[\"{}\"]=st[st.length-1];\n",
                i, dv, i, i, v
            ));
        }
        Ins::Nop | Ins::Strict | Ins::Loose => {}
        Ins::IncLocal(_) | Ins::AddLocal(_) | Ins::IncGlobal(_) | Ins::AddGlobal(_) => {
            unreachable!("RMW placeholders are expanded by the parse scope pass")
        }
        Ins::LocalSetI(id) => {
            if let Some(off) = p.param_pcs.get(&i).copied() {
                guard_pop(
                    o,
                    i,
                    off,
                    p,
                    Some(&format!("rt.L[fr.base+{}]", id)),
                );
            } else {
                // pass-through: full flush, pop, store the slot, then push
                // the value back to the (now empty) buffer — C's vpop then
                // vpush
                o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
                o.push_str(&format!("var _p{}=RT.pop(rt,\"\");\n", i));
                o.push_str(&format!("rt.L[fr.base+{}]=_p{};\n", id, i));
                o.push_str(&format!("iv.push(_p{});\n", i));
            }
        }
        Ins::LocalGetI(id) => {
            o.push_str(&format!(
                "iv.push(rt.L[fr.base+{}]==undefined?RT.I(0n):rt.L[fr.base+{}]);\n",
                id, id
            ));
        }
        Ins::LocalSet(n) => panic!("unresolved local {} at pc {}", n, i),
        Ins::LocalGet(n) => panic!("unresolved local {} at pc {}", n, i),
        Ins::Extern(name) => {
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
            o.push_str(&format!("rt.opName=\"extern {}\";\n", name));
            o.push_str(&format!(
                "st.push(RT.externCell(rt,\"{}\"));\n",
                name.replace('"', "\\\"")
            ));
        }
        Ins::Send => {
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
            o.push_str("RT.die(rt,\"methods (send): not supported by the JavaScript target\");\n");
        }
        Ins::Sys(_) => {
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
            o.push_str("RT.die(rt,\"syscalls: not supported by the JavaScript target\");\n");
        }
        Ins::Flush => {
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
        }
        Ins::ListStart | Ins::DictStart => {
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
            o.push_str(&format!("var _ls{}=st.length;\n", i));
        }
        Ins::ListLit => {
            let ls = find_lit_start(p, i, seg_start);
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
            o.push_str(&format!(
                "var _lt{}=RT.L(st.splice(_ls{}));iv.push(_lt{});\n",
                i, ls, i
            ));
        }
        Ins::DictLit => {
            let ls = find_lit_start(p, i, seg_start);
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
            o.push_str(&format!(
                "var _dv{}=st.splice(_ls{});if(_dv{}.length%2)RT.die(rt,\"dict literal: element count must be even\");\n",
                i, ls, i
            ));
            o.push_str(&format!(
                "var _dd{}=RT.D(RT.mapNew());for(var _q{}=0;_q{}<_dv{}.length;_q{}+=2)RT.mapPut(_dd{}.v,_dv{}[_q{}],_dv{}[_q{}+1]);iv.push(_dd{});\n",
                i, i, i, i, i, i, i, i, i, i, i
            ));
        }
        Ins::If => {
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
            let if_arity = if i > seg_start {
                match &p.ins[i - 1] {
                    Ins::PushAddr(l) => p
                        .label_params
                        .get(&resolve_label(p, l))
                        .map(|v| v.len())
                        .unwrap_or(0),
                    _ => 0,
                }
            } else {
                0
            };
            // tail detection: everything between this if and the body's ret is
            // just the ret value -> the taken branch returns from the label
            // (the C scan is unbounded over the whole ins vector)
            let mut tail = false;
            let mut j = i + 1;
            while j < p.ins.len() {
                match &p.ins[j] {
                    Ins::Flush
                    | Ins::PushI(_)
                    | Ins::PushF(_)
                    | Ins::LocalGetI(_)
                    | Ins::LocalGet(_)
                    | Ins::GetV(_) => {
                        j += 1;
                    }
                    Ins::Ret => {
                        tail = true;
                        break;
                    }
                    _ => break,
                }
            }
            if tail {
                o.push_str(&format!(
                    "var _b{}=RT.pop(rt,\"\");var _c{}=RT.pop(rt,\"\");if(RT.truthy(_c{}))return _b{}.v(rt,fr);\n",
                    i, i, i, i
                ));
            } else {
                o.push_str(&format!(
                    "var _b{}=RT.pop(rt,\"\");var _c{}=RT.pop(rt,\"\");if(RT.truthy(_c{}))await _b{}.v(rt,{{drain:st.length>{}?st.length-{}:0,base:fr.base}});\n",
                    i, i, i, i, if_arity, if_arity
                ));
            }
        }
        Ins::IfElse => {
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
            let (th_ar, el_ar) = if i > seg_start {
                match (&p.ins[i - 2], &p.ins[i - 1]) {
                    (Ins::PushAddr(tl), Ins::PushAddr(el)) => (
                        p.label_params
                            .get(&resolve_label(p, tl))
                            .map(|v| v.len())
                            .unwrap_or(0),
                        p.label_params
                            .get(&resolve_label(p, el))
                            .map(|v| v.len())
                            .unwrap_or(0),
                    ),
                    _ => (0, 0),
                }
            } else {
                (0, 0)
            };
            o.push_str(&format!(
                "var _el{}=RT.pop(rt,\"\");var _th{}=RT.pop(rt,\"\");var _c{}=RT.pop(rt,\"\");\n",
                i, i, i
            ));
            o.push_str(&format!(
                "if(RT.truthy(_c{})){{await _th{}.v(rt,{{drain:st.length>{}?st.length-{}:0,base:fr.base}});}}else{{await _el{}.v(rt,{{drain:st.length>{}?st.length-{}:0,base:fr.base}});}}\n",
                i, i, th_ar, th_ar, i, el_ar, el_ar
            ));
            // if the if_else is the last ins of its body (the next ins is a
            // label target) the branches' return values must be returned,
            // otherwise the value would fall through into the next body
            if ctx.targets.contains(&(i + 1)) {
                o.push_str(&format!(
                    "var _rv{}=(st.length>fr.drain&&st.length>0)?st[st.length-1]:RT.I(0n);st.length=fr.drain;st.push(_rv{});return _rv{};\n",
                    i, i, i
                ));
            }
        }
        Ins::While => {
            // Mirrors C's inlined while: the two addresses come from the
            // buffer (quiet PushAddr), the buffer is vdiscard'd, and the real
            // stack is restored to the entry point (_sp0) after the cond
            // result is popped and after every body — so a while leaves the
            // stack exactly as it found it.
            o.push_str(&format!(
                "var _b{}=iv.pop();var _cnd{}=iv.pop();\n",
                i, i
            ));
            o.push_str("iv.length=0;\n");
            o.push_str(&format!("var _sp0{}=st.length;\n", i));
            o.push_str(&format!(
                "var _f{}=rt.lsp;rt.lsp++;if(rt.lsp>=64)RT.die(rt,\"loops nested too deep\");\n",
                i
            ));
            o.push_str("for(;;){try{\n");
            // noreturn:1 makes the cond's ret flush its virtual cells onto
            // the real stack instead of collapsing them: the inlined C
            // cond pops only the top cell as the loop value and leaves the
            // pass-through residual below it (the body's epilogue
            // truncates that residual; on the break path both C and JS
            // leave it). A collapsed list would survive RT.zero and run a
            // phantom body iteration on a drained stream.
            o.push_str(&format!(
                "await _cnd{}.v(rt,{{drain:_sp0{},base:fr.base,noreturn:1}});var _cv{}=RT.pop(rt,\"\");if(RT.zero(_cv{}))break;\n",
                i, i, i, i
            ));
            o.push_str(&format!(
                "await _b{}.v(rt,{{drain:_sp0{},base:fr.base}});st.length=_sp0{};\n",
                i, i, i
            ));
            o.push_str(&format!(
                "}}catch(_e{}){{if(_e{}===RT.BREAK){{st.length=_sp0{};break;}}else if(_e{}===RT.CONT){{st.length=_sp0{};continue;}}else throw _e{};}}}}\n",
                i, i, i, i, i, i
            ));
            o.push_str(&format!("rt.lsp=_f{};\n", i));
        }
        Ins::Break => {
            o.push_str("iv.length=0;\n");
            o.push_str(
                "if(rt.lsp<=0)RT.die(rt,\"break outside loop\");throw RT.BREAK;\n",
            );
        }
        Ins::Cont => {
            o.push_str("iv.length=0;\n");
            o.push_str("if(rt.lsp<=0)RT.die(rt,\"cont outside loop\");throw RT.CONT;\n");
        }
        Ins::Goto(l) => {
            o.push_str("if(iv.length)st.push.apply(st,iv.splice(0));\n");
            o.push_str(&format!("return F_{}(rt,fr);\n", resolve_label(p, l)));
        }
        Ins::Weave(tasks) => {
            // discard the virtual stack (C: vdiscard, not flush) and run the
            // tasks sequentially in topological order (the C target runs the
            // same DAG concurrently; the input ordering makes the sequential
            // result identical)
            o.push_str("iv.length=0;\n");
            o.push_str(&format!("var _res{}=new Array({});\n", i, tasks.len()));
            let order = ctx.weave_order.get(&i).expect("weave order");
            for &k in order {
                let t = &tasks[k];
                o.push_str(&format!(
                    "{{var _st{}{}=st.length;var _iv{}{}=iv.length;var _L{}{}=rt.L;var _lb{}{}=rt.lb;\n",
                    i, k, i, k, i, k, i, k
                ));
                o.push_str("rt.L=[];rt.lb=0;st.length=0;iv.length=0;\n");
                let n_task = ctx
                    .ins_body
                    .get(t.pc)
                    .and_then(|&b| p.local_counts.get(&b).copied())
                    .unwrap_or(0);
                if t.count > 1 {
                    // fanout: inputs[0] is the iterable; run the body once
                    // per item, sequentially in item order (C runs the
                    // items concurrently; the ordering divergence is
                    // documented in SPEC.md)
                    o.push_str(&format!(
                        "var _its{}{}=await RT.materialize(_res{}[{}],rt);var _col{}{}=[];\n",
                        i, k, i, t.inputs[0], i, k
                    ));
                    o.push_str(&format!(
                        "for(var _j{}{}=0;_j{}{}<_its{}{}.length;_j{}{}++){{rt.L=[];rt.lb=0;st.length=0;iv.length=0;st.push(_its{}{}[_j{}{}]);",
                        i, k, i, k, i, k, i, k, i, k, i, k
                    ));
                    for &d in &t.inputs[1..] {
                        o.push_str(&format!("st.push(_res{}[{}]);", i, d));
                    }
                    o.push_str(&format!(
                        "await F_{}(rt,{{drain:0,base:{}}});_col{}{}.push(st.length>0?st[st.length-1]:RT.I(0n));\n",
                        t.pc, n_task, i, k
                    ));
                    o.push_str(&format!("}}\n_res{}[{}]=RT.L(_col{}{});\n", i, k, i, k));
                } else {
                    for &d in &t.inputs {
                        o.push_str(&format!("st.push(_res{}[{}]);\n", i, d));
                    }
                    o.push_str(&format!(
                        "await F_{}(rt,{{drain:0,base:{}}});\n",
                        t.pc, n_task
                    ));
                    o.push_str(&format!(
                        "_res{}[{}]=st.length>0?st[st.length-1]:RT.I(0n);\n",
                        i, k
                    ));
                }
                o.push_str(&format!(
                    "st.length=_st{}{};iv.length=_iv{}{};rt.L=_L{}{};rt.lb=_lb{}{};}}\n",
                    i, k, i, k, i, k, i, k
                ));
            }
            // publish results to the shared vars in original task order,
            // then push the last task's result (C: pushc(wt[n-1].result))
            for (k, t) in tasks.iter().enumerate() {
                o.push_str(&format!(
                    "rt.G[\"{}\"]=_res{}[{}];\n",
                    t.name, i, k
                ));
            }
            o.push_str(&format!(
                "st.push(_res{}[{}]);\n",
                i,
                tasks.len() - 1
            ));
        }
    }
}

// Emit segment [start, end) as `function F_<start>(rt,fr){...}` plus the
// fall-through tail (C lays bodies out flat and falls into the next label;
// JS makes the fall-through an explicit call with the same frame).
fn emit_segment(o: &mut String, start: usize, end: usize, p: &Parsed, ctx: &Ctx) {
    // rt.curN carries this body's local count: prelude ops that invoke
    // callback code cells (fold/filter/...) frame them the same way a Call
    // does — callee base = caller base + caller's local count. The count is
    // keyed by BODY id, not pc: every instruction of the segment belongs to
    // the body of the segment's first instruction (bodies only change at
    // label pcs, and segments start at label pcs).
    let n = ctx
        .ins_body
        .get(start)
        .and_then(|&b| p.local_counts.get(&b).copied())
        .unwrap_or(0);
    o.push_str(&format!(
        "async function F_{}(rt,fr){{var st=rt.st,iv=rt.iv;var _cn{}=rt.curN;rt.curN={};rt.reg.gen++;try{{\n",
        start, start, n
    ));
    for x in start..end {
        emit_ins(o, x, start, p, ctx);
    }
    // segment tail: terminators already return; a non-terminator falls
    // through into the next segment (or off the end, C `L_n: return;`)
    match &p.ins[end - 1] {
        Ins::Ret | Ins::Goto(_) | Ins::Break | Ins::Cont | Ins::IfElse => {}
        _ => {
            if end < p.ins.len() {
                o.push_str(&format!("return F_{}(rt,fr);\n", end));
            }
        }
    }
    o.push_str(&format!("}}finally{{rt.curN=_cn{};}}}}\n", start));
}

pub fn gen_js(p: &Parsed, structs: &StructMap) -> String {
    // ---- compile-time rejections: constructs with no JS runtime model ----
    if !p.exports.is_empty() {
        panic!("exports: not supported by the JavaScript target");
    }
    if !p.methods.is_empty() {
        panic!("methods: not supported by the JavaScript target");
    }
    if !p.init_pcs.is_empty() {
        panic!("init threads: not supported by the JavaScript target");
    }

    // continuation-body labels (mirrors gen.rs DISCARD_RET_LABELS): every
    // PushAddr reference to the label is a loop/if body, so a multi-value ret
    // inside it drops its value instead of building a list
    let mut body_uses: HashMap<String, usize> = HashMap::new();
    for (i, ins) in p.ins.iter().enumerate() {
        if let Ins::PushAddr(l) = ins {
            let is_body = matches!(
                p.ins.get(i + 1),
                Some(Ins::While) | Some(Ins::For) | Some(Ins::If) | Some(Ins::IfElse)
            ) || (matches!(p.ins.get(i + 2), Some(Ins::While))
                && i > 0
                && matches!(&p.ins[i - 1], Ins::PushAddr(_)));
            if is_body {
                *body_uses.entry(l.clone()).or_insert(0) += 1;
            }
        }
    }
    let mut discard: HashSet<usize> = HashSet::new();
    for (l, uses) in body_uses.iter() {
        if p.ins.iter().filter(|x| matches!(x, Ins::PushAddr(g) if g == l)).count() == *uses {
            if let Some(&pc) = p.labels.get(l.as_str()) {
                discard.insert(pc);
            }
        }
    }

    // weave tasks: reject fanout, compute a deterministic topological order
    // (Kahn, ties broken by smallest task index); a cycle is a hard error
    // here where the C target simply hangs the DAG
    let mut weave_order: HashMap<usize, Vec<usize>> = HashMap::new();
    for (wi, ins) in p.ins.iter().enumerate() {
        if let Ins::Weave(tasks) = ins {
            let n = tasks.len();
            let mut indeg = vec![0usize; n];
            for (k, t) in tasks.iter().enumerate() {
                for &d in &t.inputs {
                    if d < n {
                        indeg[k] += 1;
                    }
                }
            }
            let mut done = vec![false; n];
            let mut order: Vec<usize> = Vec::with_capacity(n);
            for _ in 0..n {
                let k = (0..n).find(|&k| !done[k] && indeg[k] == 0);
                match k {
                    Some(k) => {
                        done[k] = true;
                        order.push(k);
                        for (m, t) in tasks.iter().enumerate() {
                            for &d in &t.inputs {
                                if d == k {
                                    indeg[m] -= 1;
                                }
                            }
                        }
                    }
                    None => {
                        panic!("weave task cycle: not supported by the JavaScript target")
                    }
                }
            }
            weave_order.insert(wi, order);
        }
    }

    let ins_body = crate::parse::compute_ins_body(p);
    let targets: HashSet<usize> = p.labels.values().copied().collect();

    // dynamic segment starts: every pc execution can re-enter
    let mut dyns: HashSet<usize> = HashSet::new();
    dyns.insert(0);
    for ins in &p.ins {
        match ins {
            Ins::PushAddr(l) | Ins::Call(l) | Ins::Goto(l) => {
                dyns.insert(resolve_label(p, l));
            }
            Ins::Weave(tasks) => {
                for t in tasks {
                    dyns.insert(t.pc);
                }
            }
            _ => {}
        }
    }
    // the entry label starts its own segment; main's prologue jumps straight
    // to it, so F_0's body is just the jump when it is not pc 0 itself
    let entry_pc = p
        .entry_label
        .as_ref()
        .map(|el| resolve_label(p, el))
        .filter(|&epc| epc != 0);
    if let Some(epc) = entry_pc {
        dyns.insert(epc);
    }
    let mut dyns: Vec<usize> = dyns.into_iter().collect();
    dyns.sort();

    // main's frame size: the entry body's local count (pc 0 when the program
    // has no entry label — C's nk_run sizes the frame from the entry pc)
    let n_main = ins_body
        .get(entry_pc.unwrap_or(0))
        .and_then(|&b| p.local_counts.get(&b).copied())
        .unwrap_or(0);

    let ctx = Ctx {
        ins_body,
        targets,
        discard,
        weave_order,
    };

    let mut o = String::new();
    o.push_str("// Nmerkar JavaScript target output.\n");
    o.push_str(JS_PRELUDE);
    // string table
    o.push_str("var __STRS=[");
    for (si, s) in p.strings.iter().enumerate() {
        if si > 0 {
            o.push(',');
        }
        o.push('"');
        o.push_str(&crate::gen::c_escape(s));
        o.push('"');
    }
    o.push_str("];\n");
    // struct reflection table (mirrors gen.rs uf_init_reflection): sid ->
    // { sid, esz, fields [[name, off], ...] }, sorted by sid. The prelude's
    // nksBySid/objOff readers consume it; empty when the program has no
    // structs.
    {
        let mut by_sid: Vec<(&String, &(Vec<(String, i64)>, i64, i64))> =
            structs.iter().collect();
        by_sid.sort_by_key(|(_, v)| v.2);
        o.push_str("var __NKS=[");
        for (si, (_, v)) in by_sid.iter().enumerate() {
            if si > 0 {
                o.push(',');
            }
            o.push_str(&format!("{{sid:{},esz:{},f:[", v.2, v.1));
            for (fi, (fname, off)) in v.0.iter().enumerate() {
                if fi > 0 {
                    o.push(',');
                }
                o.push_str(&format!("[\"{}\",{}]", fname, off));
            }
            o.push_str("]}");
        }
        o.push_str("];\n");
    }
    o.push_str("var rt=RT.makeRt();var st=rt.st;var iv=rt.iv;\n");
    // code-address table: every segment start reachable through a code cell
    let mut first = true;
    o.push_str("var __NKAD={");
    for &pc in &dyns {
        if !first {
            o.push(',');
        }
        first = false;
        o.push_str(&format!(
            "{}:RT.A(function(rt,fr){{return F_{}(rt,fr);}})",
            pc, pc
        ));
    }
    o.push_str("};\n");
    // error-site table (mirrors the C uf_sites_v): per instruction,
    // [innermost label name or null, file index, source line]. die() formats
    // it as "L<line> [label]"; __SF (multi-TU only) carries the file stems.
    {
        let mut disp: HashMap<usize, String> = HashMap::new();
        for (n, &pc) in &p.labels {
            disp.entry(pc)
                .or_insert_with(|| p.label_names.get(&pc).cloned().unwrap_or_else(|| n.clone()));
        }
        let mut labs: Vec<(usize, String)> = disp.into_iter().collect();
        labs.sort_by_key(|&(pc, _)| pc);
        let mut li = 0usize;
        let mut cur: Option<&String> = None;
        let mut entries = String::new();
        for i in 0..p.ins.len() {
            while li < labs.len() && labs[li].0 <= i {
                cur = Some(&labs[li].1);
                li += 1;
            }
            let line = p.ins_lines.get(i).copied().unwrap_or(0);
            let file = p.ins_files.get(i).copied().unwrap_or(0);
            let lab = match cur {
                Some(n) => format!("\"{}\"", n),
                None => "null".to_string(),
            };
            entries.push_str(&format!("[{},{},{}],", lab, file, line));
        }
        o.push_str(&format!("var __SITES=[{}];\n", entries));
        if p.file_names.len() > 1 {
            o.push_str(&format!(
                "var __SF=[{}];\n",
                p.file_names.iter().map(|f| format!("\"{}\"", f)).collect::<Vec<_>>().join(",")
            ));
        }
    }
    // segments, ascending (function declarations hoist, so forward calls work).
    // F_0 is always the REAL segment [0, next_dyn): with an entry label its
    // pc may be a plain label (e.g. a v0 body) and __NKAD[0] must reach it.
    for (si, &pc) in dyns.iter().enumerate() {
        let end = dyns.get(si + 1).copied().unwrap_or(p.ins.len());
        emit_segment(&mut o, pc, end, p, &ctx);
    }
    // The library surface: one `run(env)` per compiled program. env is
    // { argv: [..], stdin: "text", fs: { "name": "text" } }; the result is a
    // promise of { output, err, rc, error? } — output/err are the merged
    // stdout/stderr of the whole run, rc the exit code (0 clean, the exit op's
    // value, 1 on a runtime error), and error the die message when rc != 0.
    // execute children inherit the native fds; their writes hit the pipe
    // before the parent's block-buffered std streams flush, so the queued
    // child output (rt.reg.xout/xerr) is prepended to the parent output.
    o.push_str(
        "function nkOut(){ return (rt.reg.xout ? rt.reg.xout.join(\"\") : \"\") + rt.out.join(\"\"); }\n",
    );
    o.push_str(
        "function nkErr(){ return (rt.reg.xerr ? rt.reg.xerr.join(\"\") : \"\") + rt.reg.err.join(\"\"); }\n",
    );
    o.push_str("var __NK_API = {\n");
    o.push_str("  rt: rt,\n");
    o.push_str("  output: function(){ return nkOut(); },\n");
    o.push_str("  err: function(){ return nkErr(); },\n");
    o.push_str("  run: function(env){\n");
    o.push_str("    env = env || {};\n");
    o.push_str("    rt.reg.env = { argv: (env.argv && env.argv.length) ? env.argv : [\"nk\"], stdin: env.stdin || \"\", fs: env.fs || {} };\n");
    o.push_str("    rt.scanPos = 0;\n");
    o.push_str(&format!(
        "    var _lb = rt.lb; rt.lb += {};\n",
        n_main
    ));
    o.push_str(&format!("    var p = F_{}(rt, {{drain: 0, base: rt.lb}});\n", entry_pc.unwrap_or(0)));
    o.push_str("    p.then(function(){rt.lb=_lb;},function(){rt.lb=_lb;});\n");
    o.push_str("    return RT.pump(p, rt).then(function(){\n");
    o.push_str("      return { output: nkOut(), err: nkErr(), rc: 0 };\n");
    o.push_str("    },function(e){\n");
    o.push_str("      var r = { output: nkOut(), err: nkErr(), rc: 1 };\n");
    o.push_str("      if (e && e.nkExit !== undefined) { r.rc = e.nkExit; }\n");
    o.push_str("      else { r.error = (e && e.nkMsg) ? e.nkMsg : (e && e.message ? e.message : String(e)); }\n");
    o.push_str("      return r;\n");
    o.push_str("    });\n");
    o.push_str("  }\n");
    o.push_str("};\n");
    o.push_str(
        "if (typeof module !== \"undefined\") module.exports = __NK_API;\n",
    );
    // node auto-run: behaves like the native binary (output, `nk: <msg>` on
    // stderr, exit code). Piped stdin is read fully; a TTY is skipped so
    // interactive node use does not hang — documented divergence.
    o.push_str(
        "if (typeof require !== \"undefined\" && typeof module !== \"undefined\" && require.main === module) {\n",
    );
    o.push_str("  var __nkstdin = \"\";\n");
    o.push_str("  if (!process.stdin.isTTY) {\n");
    o.push_str(
        "    try { __nkstdin = require(\"fs\").readFileSync(0, \"utf8\"); } catch (e) { __nkstdin = \"\"; }\n",
    );
    o.push_str("  }\n");
    o.push_str(
        "  __NK_API.run({ argv: process.argv.slice(1), stdin: __nkstdin }).then(function(r){\n",
    );
    o.push_str("    process.stdout.write(r.output);\n");
    o.push_str("    if (r.err) process.stderr.write(r.err);\n");
    o.push_str("    if (r.error) process.stderr.write(\"nk: \" + r.error + \"\\n\");\n");
    o.push_str("    process.exit(r.rc & 255);\n");
    o.push_str("  });\n");
    o.push_str("}\n");
    o
}
