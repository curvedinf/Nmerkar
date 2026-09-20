use crate::ast::*;
use crate::lex::*;
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

// dense -> text: mnemonics; glyph names "v57" are already valid text idents
pub fn emit_text(toks: &[Tok]) -> String {
    let mut o = String::new();
    let mut in_weave = false;
    for t in toks {
        match t {
            Tok::Op(name) => o.push_str(&format!("{} ", text_mnemonic(op_index(name).expect("op")))),
            Tok::PushI(v) => o.push_str(&format!("{} ", v)),
            Tok::PushF(v) => o.push_str(&format!("{:?} ", v)),
            Tok::PushS(s) => o.push_str(&format!("\"{}\" ", escape_str(s))),
            Tok::Jump(op, l) => o.push_str(&format!("{} {} ", text_mnemonic(op_index(op).expect("jump")), l)),
            Tok::SetV(n) => o.push_str(&format!("^{}! ", n)),
            Tok::GetV(n) => o.push_str(&format!("^{}@ ", n)),
            Tok::LocalSet(n) => o.push_str(&format!("{}! ", n)),
            Tok::LocalGet(n) => o.push_str(&format!("{}@ ", n)),
            Tok::IncLocal(n) => o.push_str(&format!("{}++ ", n)),
            Tok::AddLocal(n) => o.push_str(&format!("{}+= ", n)),
            Tok::IncGlobal(n) => o.push_str(&format!("^{}++ ", n)),
            Tok::AddGlobal(n) => o.push_str(&format!("^{}+= ", n)),
            Tok::Discard => o.push_str("_! "),
            Tok::Import(im) | Tok::ManifestImport(im) => o.push_str(&format!("import c\"{}\"({})->{} ", im.name, im.params.join(","), im.ret)),
            Tok::Export(n) => o.push_str(&format!("export \"{}\" ", escape_str(n))),
            Tok::Extern(n) => o.push_str(&format!("extern \"{}\" ", escape_str(n))),
            Tok::Use(n) => o.push_str(&format!("use \"{}\" ", escape_str(n))),
            Tok::Mod(n) => o.push_str(&format!("mod \"{}\" ", escape_str(n))),
            Tok::Pub => o.push_str("pub "),
            Tok::Method(n) => o.push_str(&format!("method {}: ", n)),
            Tok::MacroDef(n, body) => o.push_str(&format!("macro {} {{ {} }} ", n, emit_text(body))),
            Tok::StructDef(n, fields) => {
                let fs: Vec<String> = fields.iter().map(|(f, t)| format!("{}:{}", f, t)).collect();
                o.push_str(&format!("struct {} {{ {} }} ", n, fs.join(", ")));
            }
            Tok::Sys(n) => o.push_str(&format!("_sys {} ", n)),
            Tok::Ident(n) => o.push_str(&format!("{} ", n)),
            Tok::LabelDef(n) => o.push_str(&format!("{}: ", n)),
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

pub fn emit_dense(toks: &[Tok]) -> String {
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
            panic!("emit-dense: more than 64 distinct label/var names");
        }
        names.insert(n.to_string(), i);
        slot_glyphs(i)
    }
    // Separators: in dense mode, v-glyphs fold into names and l-glyphs fold
    // into numbers. A space is needed ONLY between two adjacent v-runs or two
    // adjacent l-runs. Everything else (opcodes, ^, !, @, ", ', digits, etc.)
    // is read independently and needs no separator.
    fn sep_v(o: &mut String) {
        if o.chars().last().map_or(false, |c| is_v(c as u32)) { o.push(' '); }
    }
    fn sep_l(o: &mut String) {
        if o.chars().last().map_or(false, |c| is_l(c as u32)) { o.push(' '); }
    }
    fn rec(toks: &[Tok], names: &mut HashMap<String, u32>, next: &mut u32, weave: &mut bool, o: &mut String) {
        for t in toks {
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
                    let idx = op_index(op).expect("jump");
                    if *op == "ADDR" {
                        o.push('\'');
                        o.push_str(&nm(l, names, next));
                    } else if *op == "JE" {
                        o.push_str("=");
                        o.push_str(&nm(l, names, next));
                    } else {
                        o.push(glyph_of(idx));
                        o.push_str(&nm(l, names, next));
                    }
                }
                Tok::SetV(n) => { sep_v(o); o.push_str(&format!("^{}!", nm(n, names, next))); }
                Tok::GetV(n) => { sep_v(o); o.push_str(&format!("^{}@", nm(n, names, next))); }
                Tok::LocalSet(n) => { sep_v(o); o.push_str(&format!("{}!", nm(n, names, next))); }
                Tok::LocalGet(n) => { sep_v(o); o.push_str(&format!("{}@", nm(n, names, next))); }
                Tok::IncLocal(n) => { sep_v(o); o.push_str(&format!("{}++", nm(n, names, next))); }
                Tok::AddLocal(n) => { sep_v(o); o.push_str(&format!("{}+=", nm(n, names, next))); }
                Tok::IncGlobal(n) => { sep_v(o); o.push_str(&format!("^{}++", nm(n, names, next))); }
                Tok::AddGlobal(n) => { sep_v(o); o.push_str(&format!("^{}+=", nm(n, names, next))); }
                Tok::Discard => { /* _! has no dense representation */ }
                Tok::Import(im) | Tok::ManifestImport(im) => o.push_str(&format!("{}c\"{}\"({})->{}", glyph_of(51), im.name, im.params.join(","), im.ret)),
                Tok::Export(n) => o.push_str(&format!("{}\"{}\"", glyph_of(52), escape_str(n))),
                Tok::Extern(n) => o.push_str(&format!("{}\"{}\"", glyph_of(53), escape_str(n))),
                Tok::Use(n) => o.push_str(&format!("{}\"{}\"", glyph_of(78), escape_str(n))),
                Tok::Mod(n) => o.push_str(&format!("{}\"{}\"", glyph_of(79), escape_str(n))),
                Tok::Pub => o.push_str(&format!("{}", glyph_of(80))),
                Tok::Method(_) => panic!("emit-dense: METHOD was removed in v10"),
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
                Tok::Ident(n) => o.push_str(&format!("{}", n)),
                Tok::LabelDef(n) => { sep_v(o); o.push_str(&format!("{}\n", nm(n, names, next))); }
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
