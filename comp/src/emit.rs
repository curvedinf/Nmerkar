use crate::ast::*;
use crate::lex::*;
use crate::user_err;
use std::collections::HashMap;

// ---------------- round-trip emitters (v9) ----------------
pub fn escape_str(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            '\0' => o.push_str("\\0"),
            c => o.push(c),
        }
    }
    o
}

// glyph codepoint for an opcode index (now in lex.rs; re-exported here)
fn glyph_of(idx: usize) -> char {
    crate::lex::glyph_of(idx)
}

// v15: text-mode bare names arrive as Tok::Ident (macro-or-load is resolved
// at parse time). Encoding converters work on lexed tokens, so resolve here:
// '@'-sentinels and macro names stay Ident; every other bare name becomes an
// implicit LocalGet (dense slot-encodes it; text prints it bare).
pub fn resolve_implicit_loads(toks: &[(u32, Tok)]) -> Vec<(u32, Tok)> {
    fn collect_macros(toks: &[(u32, Tok)], out: &mut std::collections::HashSet<String>) {
        for (_, t) in toks {
            match t {
                Tok::MacroDef(n, _) => {
                    out.insert(n.clone());
                }
                Tok::List(b) | Tok::Dict(b) => collect_macros(b, out),
                _ => {}
            }
        }
    }
    let mut macros = std::collections::HashSet::new();
    collect_macros(toks, &mut macros);
    fn walk(toks: &[(u32, Tok)], macros: &std::collections::HashSet<String>) -> Vec<(u32, Tok)> {
        toks.iter()
            .map(|(ln, t)| (*ln, match t {
                Tok::Ident(n) if !n.starts_with('@') && !macros.contains(n) => Tok::LocalGet(n.clone()),
                Tok::List(b) => Tok::List(walk(b, macros)),
                Tok::Dict(b) => Tok::Dict(walk(b, macros)),
                Tok::Task { name, count, body } => Tok::Task {
                    name: name.clone(),
                    count: *count,
                    body: walk(body, macros),
                },
                other => other.clone(),
            }))
            .collect()
    }
    walk(toks, &macros)
}

// dense -> text: mnemonics; glyph names "v57" are already valid text idents
pub fn emit_text(toks: &[(u32, Tok)]) -> String {
    let mut o = String::new();
    let mut in_weave = false;
    let mut i = 0;
    while i < toks.len() {
        let (_, t) = &toks[i];
        i += 1;
        match t {
            Tok::Op(name) => o.push_str(&format!("{} ", text_mnemonic(op_index(name).expect("op")))),
            Tok::PushI(v) => o.push_str(&format!("{} ", v)),
            Tok::PushBool(v) => o.push_str(if *v { "true " } else { "false " }),
            Tok::PushF(v) => o.push_str(&format!("{:?} ", v)),
            Tok::PushS(s) => o.push_str(&format!("\"{}\" ", escape_str(s))),
            Tok::Jump(op, l) => match op.as_ref() {
                // v16: 'name is the call form; 'recv.method is the send-only
                // method reference; a bare ADDR jump (no producer since v16)
                // degrades to the implicit-load spelling
                "CALL" => o.push_str(&format!("'{} ", l)),
                "ADDR" if l.contains('.') => o.push_str(&format!("'{} ", l)),
                "ADDR" => o.push_str(&format!("{} ", l)),
                _ => o.push_str(&format!("{} {} ", text_mnemonic(op_index(op).expect("jump")), l)),
            },
            Tok::SetV(n) => o.push_str(&format!("^{}! ", n)),
            Tok::GetV(n) => o.push_str(&format!("^{}@ ", n)),
            Tok::LocalSet(n) => o.push_str(&format!("{}! ", n)),
            // v15: bare names load implicitly
            Tok::LocalGet(n) => o.push_str(&format!("{} ", n)),
            Tok::IncLocal(n) => o.push_str(&format!("{}++ ", n)),
            Tok::AddLocal(n) => o.push_str(&format!("{}+= ", n)),
            Tok::IncGlobal(n) => o.push_str(&format!("^{}++ ", n)),
            Tok::AddGlobal(n) => o.push_str(&format!("^{}+= ", n)),
            Tok::Discard => o.push_str("_! "),
            Tok::Destr(n, k) => match k {
                Some(k) => o.push_str(&format!("{}#{} ", n, k)),
                None => o.push_str(&format!("{}# ", n)),
            },
            Tok::DestrDiscard => o.push_str("_# "),
            Tok::Import(im) | Tok::ManifestImport(im) => o.push_str(&format!("import c\"{}\"({})->{} ", im.name, im.params.join(","), im.ret)),
            Tok::Export(n) => o.push_str(&format!("export \"{}\" ", escape_str(n))),
            Tok::Extern(n) => o.push_str(&format!("extern \"{}\" ", escape_str(n))),
            Tok::Use(spec) => {
                o.push_str("use ");
                if let Some(a) = &spec.alias {
                    o.push_str(a);
                    o.push(' ');
                }
                o.push_str(&format!("\"{}\"", escape_str(&spec.source)));
                if let Some(p) = &spec.policy {
                    o.push_str(&format!(" policy {}", p));
                }
                if !spec.allows.is_empty() {
                    o.push_str(&format!(" allow {}", spec.allows.join(" ")));
                }
                if !spec.denies.is_empty() {
                    o.push_str(&format!(" deny {}", spec.denies.join(" ")));
                }
                o.push(' ');
            }
            Tok::Method(n) => o.push_str(&format!("@{} ", n)),
            Tok::Mod(n) => o.push_str(&format!("mod \"{}\" ", escape_str(n))),
            Tok::Pub => o.push_str("pub "),
            Tok::Method(n) => o.push_str(&format!("method {}: ", n)),
            Tok::MacroDef(n, body) => o.push_str(&format!("macro {} {{ {} }} ", n, emit_text(body))),
            Tok::StructDef(n, fields) => {
                let fs: Vec<String> = fields.iter().map(|(f, t)| format!("{}:{}", f, t)).collect();
                o.push_str(&format!("struct {} {{ {} }} ", n, fs.join(", ")));
            }
            Tok::Sys(n) => o.push_str(&format!("_sys {} ", n)),
            Tok::Ident(n) => {
                // v15: translate internal sentinels back to source mnemonics
                // (objsize/cast carry a following runtime op that folds into
                // the same mnemonic)
                if let Some(rest) = n.strip_prefix("@sizeof:") {
                    o.push_str(&format!("_size_of {} ", rest));
                } else if let Some(rest) = n.strip_prefix("@offset:") {
                    o.push_str(&format!("_offset {} ", rest));
                } else if let Some(rest) = n.strip_prefix("@objsize:") {
                    o.push_str(&format!("_obj {} ", rest));
                    if matches!(toks.get(i), Some((_, Tok::Op(op))) if *op == "OBJ") { i += 1; }
                } else if let Some(rest) = n.strip_prefix("@cast:") {
                    o.push_str(&format!("_cast {} ", rest));
                    if matches!(toks.get(i), Some((_, Tok::Op(op))) if *op == "CAST") { i += 1; }
                } else {
                    o.push_str(&format!("{} ", n));
                }
            }
            Tok::LabelDef(n) => o.push_str(&format!("{}: ", n)),
            Tok::AnonDef => o.push_str(": "),
            Tok::AnonCall => o.push_str("': "),
            Tok::Entry => o.push_str("entry: "),
            Tok::Task { name, count, body } => {
                if !in_weave {
                    o.push_str("weave\n  ");
                    in_weave = true;
                }
                if let Some(c) = count {
                    o.push_str(&format!("{} ", c));
                }
                o.push_str(&format!("task {}: ", name));
                o.push_str(&emit_text(body));
            }
            Tok::TaskEnd(_) => {}
            Tok::Wrun(terminal) => {
                match terminal {
                    Some(t) => o.push_str(&format!("run {}\n", t)),
                    None => o.push_str("run\n"),
                }
                in_weave = false;
            }
            Tok::List(body) => {
                o.push_str("[");
                o.push_str(&emit_text(body));
                o.push_str("] ");
            }
            Tok::Dict(body) => {
                o.push_str("{ ");
                o.push_str(&emit_text(body));
                o.push_str("} ");
            }
        }
    }
    o.push('\n');
    o
}

// text -> dense: custom glyphs; labels/vars become v-space slots, assigned
// deterministically by first use (names already of the form v<N> keep slot N)
fn lrun_u(mut v: u64) -> String {
    let mut ds = vec![l_glyph((v % 64) as u32)];
    v /= 64;
    while v > 0 {
        ds.push(l_glyph((v % 64) as u32));
        v /= 64;
    }
    ds.iter().rev().collect()
}

/// Encode an f64 as SPEC l-space (`lrun '.' lrun`) when the reconstruction
/// `ip + frac` is bit-identical. Otherwise the caller keeps LIT+ASCII.
fn emit_lfloat(v: f64) -> Option<String> {
    if !v.is_finite() {
        return None;
    }
    let bits = v.to_bits();
    let neg = v.is_sign_negative();
    let a = v.abs();
    let mut scale = 1.0f64;
    for k in 0..=10 {
        let n = (a * scale).round();
        if n >= 9007199254740992.0 {
            break;
        }
        let recon = n / scale;
        let r = if neg { -recon } else { recon };
        if r.to_bits() == bits {
            let ni = n as u64;
            let mut p = 1u64;
            for _ in 0..k {
                p = p.saturating_mul(64);
            }
            let ip = ni / p;
            let fp = ni % p;
            let mut s = String::new();
            if neg {
                s.push('-');
            }
            s.push_str(&lrun_u(ip));
            s.push('.');
            if k == 0 {
                s.push(l_glyph(0));
            } else {
                let mut digits = Vec::with_capacity(k);
                let mut x = fp;
                for _ in 0..k {
                    digits.push(l_glyph((x % 64) as u32));
                    x /= 64;
                }
                digits.reverse();
                s.extend(digits);
            }
            return Some(s);
        }
        scale *= 64.0;
    }
    None
}

pub fn emit_dense(toks: &[(u32, Tok)]) -> String {
    fn is_slot(n: &str) -> Option<u32> {
        n.strip_prefix('v').and_then(|d| d.parse::<u32>().ok()).filter(|&i| i < 64)
    }
    fn slot_glyphs(i: u32) -> String {
        v_glyph(i).to_string()
    }
    fn lrun(mut v: u64) -> String {
        let mut ds = vec![l_glyph((v % 64) as u32)];
        v /= 64;
        while v > 0 {
            ds.push(l_glyph((v % 64) as u32));
            v /= 64;
        }
        ds.iter().rev().collect()
    }
    fn nm(n: &str, names: &mut HashMap<String, u32>, next: &mut u32) -> String {
        if let Some(i) = is_slot(n) {
            return slot_glyphs(i);
        }
        if let Some(&i) = names.get(n) {
            return slot_glyphs(i);
        }
        let i = *next;
        *next += 1;
        if i >= 64 {
            user_err!("emit", None, "emit-dense: more than 64 distinct label/var names");
        }
        names.insert(n.to_string(), i);
        slot_glyphs(i)
    }
    // Separators: in dense mode, v-glyphs fold into names and l-glyphs fold
    // into numbers. A space is needed ONLY between two adjacent v-runs or two
    // adjacent l-runs. Everything else (opcodes, !, 🏷, tags, ", ', digits,
    // etc.) is read independently and needs no separator.
    fn sep_v(o: &mut String) {
        if o.chars().last().map_or(false, |c| is_v(c as u32)) { o.push(' '); }
    }
    fn sep_l(o: &mut String) {
        if o.chars().last().map_or(false, |c| is_l(c as u32)) { o.push(' '); }
    }
    fn rec(toks: &[(u32, Tok)], names: &mut HashMap<String, u32>, next: &mut u32, weave: &mut bool, o: &mut String) {
        for (_, t) in toks {
            match t {
                Tok::Op(name) => {
                    let idx = op_index(name).expect("op");
                    o.push(glyph_of(idx));
                }
                Tok::PushI(v) => {
                    if *v >= 0 {
                        sep_l(o);
                        o.push_str(&lrun(*v as u64));
                    } else {
                        o.push(glyph_of(0)); // LIT
                        o.push_str(&format!("{}", v));
                    }
                }
                Tok::PushBool(v) => {
                    sep_l(o);
                    o.push_str(&lrun(if *v { 1 } else { 0 }));
                    o.push(glyph_of(27)); // _cast with the bool type immediate
                    o.push(char::from_u32(TYPE_BASE + 7).unwrap());
                }
                Tok::PushF(v) => {
                    if let Some(ls) = emit_lfloat(*v) {
                        sep_l(o);
                        o.push_str(&ls);
                    } else {
                        o.push(glyph_of(0)); // LIT + ASCII, for values l-space cannot round-trip
                        o.push_str(&format!("{:?}", v));
                    }
                }
                Tok::PushS(s) => o.push_str(&format!("\"{}\"", escape_str(s))),
                Tok::Jump(op, l) => {
                    if *op == "ADDR" {
                        if l.contains('.') {
                            // method reference (recv.method): ASCII, the same
                            // spelling as the struct immediates
                            o.push('\'');
                            o.push_str(l);
                        } else {
                            // v16: a bare label reference is an implicit fn
                            // load — the same v-run spelling as a var load
                            // (the parser resolves it)
                            sep_v(o);
                            o.push_str(&nm(l, names, next));
                        }
                    } else if *op == "JE" {
                        o.push_str("=");
                        o.push_str(&nm(l, names, next));
                    } else {
                        let idx = op_index(op).expect("jump");
                        o.push(glyph_of(idx));
                        o.push_str(&nm(l, names, next));
                    }
                }
                Tok::SetV(n) => { sep_v(o); o.push_str(&format!("^{}!", nm(n, names, next))); }
                Tok::GetV(n) => { sep_v(o); o.push_str(&format!("^{}@", nm(n, names, next))); }
                Tok::LocalSet(n) => { sep_v(o); o.push_str(&format!("{}!", nm(n, names, next))); }
                Tok::LocalGet(n) => { sep_v(o); o.push_str(&format!("{}", nm(n, names, next))); }
                Tok::IncLocal(n) => { sep_v(o); o.push_str(&format!("{}++", nm(n, names, next))); }
                Tok::AddLocal(n) => { sep_v(o); o.push_str(&format!("{}+=", nm(n, names, next))); }
                Tok::IncGlobal(n) => { sep_v(o); o.push_str(&format!("^{}++", nm(n, names, next))); }
                Tok::AddGlobal(n) => { sep_v(o); o.push_str(&format!("^{}+=", nm(n, names, next))); }
                Tok::Discard => { /* _! has no dense representation */ }
                // v15.1: name + '#' + optional attached ASCII key; the trailing
                // space keeps a following l-run or ASCII ident from attaching
                // to the key (and marks keyless binds before a digit)
                Tok::Destr(n, k) => {
                    sep_v(o);
                    o.push_str(&nm(n, names, next));
                    o.push('#');
                    if let Some(k) = k {
                        o.push_str(k);
                    }
                    o.push(' ');
                }
                Tok::DestrDiscard => { sep_v(o); o.push_str("_# "); }
                Tok::Import(im) | Tok::ManifestImport(im) => o.push_str(&format!("{}c\"{}\"({})->{}", glyph_of(51), im.name, im.params.join(","), im.ret)),
                Tok::Export(n) => o.push_str(&format!("{}\"{}\"", glyph_of(52), escape_str(n))),
                Tok::Extern(n) => o.push_str(&format!("{}\"{}\"", glyph_of(53), escape_str(n))),
                Tok::Use(spec) => {
                    if spec.alias.is_some() || spec.policy.is_some() || !spec.allows.is_empty() || !spec.denies.is_empty() {
                        user_err!("emit", None, "use aliases and grant clauses are text-mode spellings — convert with --to-text (dense emits the plain package form)");
                    }
                    o.push_str(&format!("{}\"{}\"", glyph_of(78), escape_str(&spec.source)));
                }
                Tok::Mod(n) => o.push_str(&format!("{}\"{}\"", glyph_of(79), escape_str(n))),
                Tok::Method(n) => o.push_str(&format!("{}{}", crate::lex::GLYPH_METHOD, escape_str(n))),
                Tok::Pub => o.push_str(&format!("{}", glyph_of(80))),
                Tok::Method(_) => user_err!("emit", None, "emit-dense: METHOD was removed in v10"),
                Tok::MacroDef(n, body) => {
                    o.push_str(&format!("{}{} {{ ", glyph_of(28), n));
                    rec(body, names, next, weave, o);
                    o.push_str("} ");
                }
                Tok::StructDef(n, fields) => {
                    let fs: Vec<String> = fields.iter().map(|(f, t)| format!("{}:{}", f, t)).collect();
                    o.push_str(&format!("{}{} {{ {} }}", glyph_of(46), n, fs.join(", ")));
                }
                Tok::Sys(n) => o.push_str(&format!("{}{}", glyph_of(49), n)),
                Tok::Ident(n) => {
                    // v15: struct-name immediates get dedicated tag glyphs
                    if let Some(rest) = n.strip_prefix("@sizeof:") {
                        o.push(GLYPH_TAG_SIZEOF);
                        o.push_str(rest);
                    } else if let Some(rest) = n.strip_prefix("@offset:") {
                        o.push(GLYPH_TAG_OFFSET);
                        o.push_str(rest);
                    } else if let Some(rest) = n.strip_prefix("@objsize:") {
                        o.push(GLYPH_TAG_OBJSIZE);
                        o.push_str(rest);
                    } else if let Some(rest) = n.strip_prefix("@cast:") {
                        o.push(GLYPH_TAG_CAST);
                        o.push_str(rest);
                    } else {
                        o.push_str(&format!("{}", n));
                    }
                }
                // v15: dense label definitions carry the 🏷 marker (bare
                // v-runs are implicit loads now)
                Tok::LabelDef(n) => { sep_v(o); o.push_str(&format!("{}{}\n", nm(n, names, next), GLYPH_LABEL_DEF)); }
                Tok::AnonDef => { sep_v(o); o.push(GLYPH_ANON_DEF); o.push(' '); }
                Tok::AnonCall => { o.push(glyph_of(17)); o.push_str(&format!("{}", GLYPH_ANON_DEF)); }
                Tok::Entry => o.push_str(&format!("{}\n", glyph_of(206))),
                Tok::Task { name, count, body } => {
                    if !*weave {
                        o.push_str(&format!("{}\n", glyph_of(81)));
                        *weave = true;
                    }
                    if let Some(c) = count {
                        sep_l(o);
                        o.push_str(&lrun(*c as u64));
                        o.push(' ');
                    }
                    o.push_str(&format!("{}{}:\n", glyph_of(82), nm(name, names, next)));
                    rec(body, names, next, weave, o);
                }
                Tok::TaskEnd(_) => {}
                Tok::Wrun(terminal) => {
                    o.push_str(&format!("{}", glyph_of(84)));
                    if let Some(t) = terminal {
                        sep_v(o);
                        o.push_str(&nm(t, names, next));
                    }
                    o.push('\n');
                    *weave = false;
                }
                Tok::List(body) => {
                    o.push('[');
                    rec(body, names, next, weave, o);
                    o.push_str("] ");
                }
                Tok::Dict(body) => {
                    o.push_str("{ ");
                    rec(body, names, next, weave, o);
                    o.push_str("} ");
                }
            }
        }
    }
    let mut o = String::new();
    let mut names: HashMap<String, u32> = HashMap::new();
    let mut next = 0u32;
    let mut weave = false;
    rec(toks, &mut names, &mut next, &mut weave, &mut o);
    o.push('\n');
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    // The conversion contract (the same pipeline the driver's --to-dense /
    // --to-text runs): lex → resolve implicit loads → emit. Text → dense →
    // text must reproduce the canonical text form (the emit_text spelling —
    // single-space separated, trailing newline), and the dense output must
    // be a fixed point of re-encoding. Names must be vN-shaped because dense
    // encodes them as v-space slots (other names are canonicalized to slots
    // on the way through).
    #[test]
    fn text_dense_text_round_trip() {
        let src = "entry:\n  3 4 add print\n  \"a\\nb\" print\n  5 v0!\n  v0 print\n  [1 2 3] length print\n  ret\n";
        let toks = resolve_implicit_loads(&lex_source(src));
        let text = emit_text(&toks);
        let dense = emit_dense(&toks);

        let toks2 = resolve_implicit_loads(&lex_source(&dense));
        assert_eq!(emit_text(&toks2), text, "text -> dense -> text must reproduce the canonical text");
        let dense2 = emit_dense(&resolve_implicit_loads(&lex_source(&emit_text(&toks2))));
        assert_eq!(dense2, dense, "dense must be a fixed point of the round trip");
    }

    // v16: 'name is the call form; a bare label name is its callable
    // (implicit fn load). Both spellings must survive the round trip (names
    // must be vN-shaped — dense canonicalizes other names to slots).
    #[test]
    fn call_and_fn_ref_round_trip() {
        let src = "3 v0 'v1 print\n4 v0 v1 filter\n5 v2 v3 while\n'v0.area send print\nret\nv1: v4! v5!\n  ret v4 v5 add\n";
        let toks = resolve_implicit_loads(&lex_source(src));
        assert!(toks.iter().any(|(_, t)| matches!(t, Tok::Jump("CALL", l) if l == "v1")));
        assert!(toks.iter().any(|(_, t)| matches!(t, Tok::LocalGet(n) if n == "v1" || n == "v2" || n == "v3")));
        let text = emit_text(&toks);
        assert!(text.contains("'v1 "), "call form must re-emit as 'label: {}", text);
        assert!(text.contains(" v1 "), "fn ref must re-emit bare: {}", text);
        let dense = emit_dense(&toks);
        let toks2 = resolve_implicit_loads(&lex_source(&dense));
        assert_eq!(emit_text(&toks2), text, "call/fn-ref round trip must be stable");
        let dense2 = emit_dense(&resolve_implicit_loads(&lex_source(&text)));
        assert_eq!(dense2, dense, "dense must be a fixed point");
    }

    // v16: ':' / '': anonymous functions round-trip through dense (🔖;
    // CALL glyph + 🔖 for the apply form)
    #[test]
    fn anon_round_trip() {
        let src = "v0 : v1 2 mul ret filter\n[v1] ': v2! v2 v2 mul ret print\nret\n";
        let toks = resolve_implicit_loads(&lex_source(src));
        assert!(toks.iter().any(|(_, t)| matches!(t, Tok::AnonDef)));
        assert!(toks.iter().any(|(_, t)| matches!(t, Tok::AnonCall)));
        let text = emit_text(&toks);
        assert!(text.contains(": "), "anon def must re-emit as ':': {}", text);
        assert!(text.contains("': "), "anon call must re-emit as ':'': {}", text);
        let dense = emit_dense(&toks);
        let toks2 = resolve_implicit_loads(&lex_source(&dense));
        assert_eq!(emit_text(&toks2), text, "anon round trip must be stable");
        let dense2 = emit_dense(&resolve_implicit_loads(&lex_source(&text)));
        assert_eq!(dense2, dense, "dense must be a fixed point");
    }
}
