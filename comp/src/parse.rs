use crate::ast::*;
use crate::lex::*;
use crate::sandbox::Caps;
use std::collections::{HashMap, HashSet, VecDeque};

// FNV-1a 64 over bytes (matches uf_fnv in the C prelude)
pub fn fnv64(s: &str) -> i64 {
    let mut h: u64 = 1469598103934665603;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h as i64
}

// method dispatch type key: struct names -> 1000+sid, container names -> HT_*,
// scalar type keywords -> 0..3
pub fn method_typekey(name: &str, structs: &StructMap) -> Option<i64> {
    if let Some((_, _, sid)) = structs.get(name) {
        return Some(1000 + sid);
    }
    match name {
        "int" => Some(0),
        "float" => Some(1),
        "ptr" | "handle" => Some(2),
        "byte" => Some(3),
        "arr" => Some(5),
        "tensor" => Some(6),
        "dyn" | "list" => Some(7),
        "map" | "dict" => Some(8),
        "ring" | "chan" => Some(9),
        "atom" => Some(10),
        "str" => Some(11),
        "obj" => Some(13),
        _ => None,
    }
}

// sandbox: compile-time capability gate for opcodes. Panics with the op,
// capability, active policy, and remedy when a gated op is denied.
fn gate_op(name: &str, caps: &Caps) {
    let (cap, need, mnem): (&str, &str, &str) = match name {
        "SH" => ("proc", "proc", "shell"),
        "SHP" => ("proc", "proc", "shell_stream"),
        "EXEC" => ("proc", "proc", "execute"),
        "SLURP" => ("fs.read", "fs.read", "read_file"),
        "SPIT" => ("fs.write", "fs.write", "write_file"),
        "MMAP" => ("fs.mmap", "fs.read + fs.write", "mmap"),
        "FEACH" => ("fs.read", "fs.read", "file_each_line"),
        "FFOLD" => ("fs.read", "fs.read", "file_fold_lines"),
        "FSPLIT" => ("fs.read", "fs.read", "file_split_lines"),
        "FMATCH" => ("fs.read", "fs.read", "file_match_lines"),
        "FEMIT" => ("fs.write", "fs.write", "file_emit"),
        "MALLOC" => ("raw.mem", "raw.mem", "malloc"),
        "FREE" => ("raw.mem", "raw.mem", "free"),
        "BUF" => ("raw.mem", "raw.mem", "buffer"),
        "BUFCOPY" => ("raw.mem", "raw.mem", "copy_memory"),
        "LOADX" => ("raw.mem", "raw.mem", "load"),
        "STOREX" => ("raw.mem", "raw.mem", "store"),
        "ARGV" => ("host.argv", "host.argv", "argv"),
        "HASARGS" => ("host.argv", "host.argv", "has_args"),
        "ARGI" => ("host.argv", "host.argv", "arg_index"),
        _ => return,
    };
    if !caps.cap(cap) {
        panic!(
            "sandbox: `{}` is disabled — capability `{}` is denied by policy '{}' ({}); allow it in a sandbox config, or run under a broader policy (e.g. `ufsb --policy build`)",
            mnem, need, caps.policy_name, caps.origin
        );
    }
}

pub fn parse(toks: Vec<Tok>, structs: &mut StructMap, caps: &Caps) -> Parsed {
    let mut macros: HashMap<String, Vec<Tok>> = HashMap::new();
    let imports: Vec<Import> = Vec::new();
    // v12: destructuring bind helpers
    fn is_multi_return(ins: Option<&Ins>, q: &VecDeque<Tok>) -> bool {
        match ins {
            // v13: a bind-run of >=2 (the current bind plus at least one more
            // in the queue) after a `_call` destructures the callee's returned
            // list (multi-value ret); a lone trailing bind stays plain
            Some(Ins::Call(_)) => {
                let mut n = 0usize;
                for t in q.iter().take(8) {
                    if is_bind(t) {
                        n += 1;
                    } else {
                        break;
                    }
                }
                n >= 1
            }
            _ => matches!(ins, Some(Ins::Simple(name)) if matches!(*name,
                "op_sh" | "op_try" | "op_retry" | "op_match" | "op_scan" | "op_next")),
        }
    }
    fn is_bind(t: &Tok) -> bool {
        matches!(t, Tok::LocalSet(_) | Tok::SetV(_) | Tok::Discard)
    }
    #[derive(Clone)]
    enum Bind { Local(String), Global(String), Discard }
    fn collect_destructure(first: Bind, q: &mut VecDeque<Tok>) -> Vec<Bind> {
        let mut pattern = vec![first];
        while let Some(t2) = q.front() {
            if !is_bind(t2) { break; }
            match q.pop_front().unwrap() {
                Tok::LocalSet(name) => pattern.push(Bind::Local(name)),
                Tok::SetV(name) => pattern.push(Bind::Global(name)),
                Tok::Discard => pattern.push(Bind::Discard),
                _ => break,
            }
        }
        pattern
    }
    fn emit_destructure(pattern: Vec<Bind>, p: &mut Parsed, dtemp_ctr: &mut usize) {
        let temp = format!("ufdt{}", *dtemp_ctr);
        *dtemp_ctr += 1;
        p.ins.push(Ins::LocalSet(temp.clone()));
        for (i, bind) in pattern.iter().enumerate() {
            p.ins.push(Ins::LocalGet(temp.clone()));
            p.ins.push(Ins::PushI(i as i64));
            p.ins.push(Ins::Simple("op_getq"));
            match bind {
                Bind::Local(name) => p.ins.push(Ins::LocalSet(name.clone())),
                Bind::Global(name) => {
                    if !p.vars.contains(name) {
                        p.vars.push(name.clone());
                    }
                    p.ins.push(Ins::SetV(name.clone()));
                }
                Bind::Discard => {}
            }
        }
    }
    let mut dtemp_ctr = 0usize;
    let mut p = Parsed {
        ins: Vec::new(),
        labels: HashMap::new(),
        imports: Vec::new(),
        exports: Vec::new(),
        externs: Vec::new(),
        strings: Vec::new(),
        vars: Vec::new(),
        uses: Vec::new(),
        modname: None,
        pubs: Vec::new(),
        methods: Vec::new(),
        init_pcs: Vec::new(),
        entry_label: None,
        local_counts: HashMap::new(),
        local_names: HashMap::new(),
        label_params: HashMap::new(),
        param_pcs: HashMap::new(),
    };
    let mut q: VecDeque<Tok> = toks.into();
    let mut pending_export: Option<String> = None;
    let mut pending_pub = false;
    let mut pending_method: Option<String> = None;
    let mut weave_ctr = 0usize;
    let mut cur_weave: Vec<(String, Vec<Param>, i64)> = Vec::new();
    let mut expand_depth = 0usize;
    // v13: `ret` is a prefix keyword whose operand is the following expression
    // tokens (e.g. `ret 0`, `ret a@ b@ add`, `ret out@ err@ code@`). The parser
    // rewrites `ret X...` into `X... ret` by pulling the operand tokens off the
    // queue and re-pushing them ahead of the RET. The operand runs until the
    // next statement boundary (label, directive, task, another ret, or a
    // literal closer). In valid v13 code the ret is the last statement of its
    // body, so the operand is exactly the return expression.
    fn is_stmt_boundary(t: &Tok) -> bool {
        is_hard_boundary(t)
            || matches!(t, Tok::List(_) | Tok::Dict(_))
    }
    fn is_hard_boundary(t: &Tok) -> bool {
        matches!(t,
            Tok::LabelDef(_) | Tok::Task { .. } | Tok::TaskEnd(_) | Tok::Wrun(_)
            | Tok::Entry | Tok::MacroDef(..) | Tok::StructDef(..) | Tok::Import(_)
            | Tok::Export(_) | Tok::Use(_) | Tok::Mod(_) | Tok::Pub
            | Tok::Extern(_) | Tok::Method(_)
            // control flow ends the ret's operand expression: `ret` is the
            // last statement, and `expr if_else ret` (v12 style) covers
            // branch-returning rets via the real-stack fallback
            | Tok::Op("RET" | "IF" | "IFELSE" | "WHILE" | "FOR")
        ) || matches!(t, Tok::Ident(n) if n.starts_with('@'))
    }
    while let Some(t) = q.pop_front() {
        match t {
            Tok::Op("RET") => {
                let mut operand: Vec<Tok> = Vec::new();
                let mut first = true;
                while let Some(t2) = q.front() {
                    if is_hard_boundary(t2) || (!first && matches!(t2, Tok::List(_) | Tok::Dict(_))) {
                        break;
                    }
                    first = false;
                    operand.push(q.pop_front().unwrap());
                }
                if operand.is_empty() {
                    // bare ret — return null
                    p.ins.push(Ins::Ret);
                } else {
                    // rewrite `ret X...` into `flush X... ret`: flush the
                    // values pushed before the operand to the real stack, so
                    // the ret's count rule sees only the values the operand
                    // itself leaves (strays get drained at the return)
                    q.push_front(Tok::Op("RET"));
                    for t2 in operand.into_iter().rev() {
                        q.push_front(t2);
                    }
                    q.push_front(Tok::Ident("@flush".to_string()));
                }
            }
            Tok::Op("FOR") => p.ins.push(Ins::For),
            Tok::Op("IF") => p.ins.push(Ins::If),
            Tok::Op("IFELSE") => p.ins.push(Ins::IfElse),
            Tok::Op("WHILE") => p.ins.push(Ins::While),
            Tok::Op("BREAK") => p.ins.push(Ins::Break),
            Tok::Op("CONT") => p.ins.push(Ins::Cont),
            Tok::Op(name) if name.starts_with('~') => {
                let retired_name = match name {
                    "~1" => "dup",
                    "~2" => "ovr",
                    "~3" => "drop",
                    "~4" => "swp",
                    "~5" => "pick",
                    _ => name,
                };
                panic!("'{}' is retired in v12 — use named variables instead", retired_name)
            }
            Tok::Op(name) => {
                gate_op(name, caps);
                p.ins.push(simple_ins(name))
            }
            Tok::PushI(v) => p.ins.push(Ins::PushI(v)),
            Tok::PushF(v) => p.ins.push(Ins::PushF(v)),
            Tok::PushS(s) => {
                let idx = p.strings.len();
                p.strings.push(s);
                p.ins.push(Ins::PushS(idx));
            }
            Tok::Jump(op, label) => match op {
                "ADDR" => p.ins.push(Ins::PushAddr(label)),
                "CALL" => {
                    if let Some(ii) = p.imports.iter().position(|im| im.name == label) {
                        p.ins.push(Ins::CallExt(ii));
                    } else {
                        p.ins.push(Ins::Call(label));
                    }
                }
                "JMP" => panic!("jmp removed — use entry/if/while/for"),
                "JZ" => panic!("jz removed — use if/ifelse/while"),
                "JE" => panic!("je removed — use if/ifelse"),
                _ => unreachable!(),
            },
            Tok::SetV(n) => {
                if is_multi_return(p.ins.last(), &q) {
                    // v12 destructuring bind: ^name! after a multi-return op
                    let pattern = collect_destructure(Bind::Global(n), &mut q);
                    emit_destructure(pattern, &mut p, &mut dtemp_ctr);
                } else {
                    if !p.vars.contains(&n) {
                        p.vars.push(n.clone());
                    }
                    p.ins.push(Ins::SetV(n));
                }
            }
            Tok::GetV(n) => {
                if !p.vars.contains(&n) {
                    p.vars.push(n.clone());
                }
                p.ins.push(Ins::GetV(n));
            }
            Tok::LocalSet(n) => {
                if is_multi_return(p.ins.last(), &q) {
                    // v12 destructuring bind: name! after a multi-return op
                    let pattern = collect_destructure(Bind::Local(n), &mut q);
                    emit_destructure(pattern, &mut p, &mut dtemp_ctr);
                } else {
                    p.ins.push(Ins::LocalSet(n));
                }
            }
            Tok::LocalGet(n) => {
                p.ins.push(Ins::LocalGet(n));
            }
            Tok::Discard => {
                if is_multi_return(p.ins.last(), &q) {
                    // v12 destructuring bind: leading _! after a multi-return op
                    let pattern = collect_destructure(Bind::Discard, &mut q);
                    emit_destructure(pattern, &mut p, &mut dtemp_ctr);
                } else {
                    panic!("_! is only valid inside a destructuring bind");
                }
            }
            Tok::IncLocal(n) => {
                p.ins.push(Ins::PushI(1));
                p.ins.push(Ins::LocalGet(n.clone()));
                p.ins.push(simple_ins("ADD"));
                p.ins.push(Ins::LocalSet(n));
            }
            Tok::AddLocal(n) => {
                p.ins.push(Ins::LocalGet(n.clone()));
                p.ins.push(simple_ins("ADD"));
                p.ins.push(Ins::LocalSet(n));
            }
            Tok::IncGlobal(n) => {
                p.ins.push(Ins::PushI(1));
                p.ins.push(Ins::GetV(n.clone()));
                p.ins.push(simple_ins("ADD"));
                p.ins.push(Ins::SetV(n));
            }
            Tok::AddGlobal(n) => {
                p.ins.push(Ins::GetV(n.clone()));
                p.ins.push(simple_ins("ADD"));
                p.ins.push(Ins::SetV(n));
            }
            Tok::Import(im) => {
                if !caps.cap("ffi.import") {
                    panic!(
                        "sandbox: `import c\"{}\"(...)` is disabled — capability `ffi.import` is denied by policy '{}' ({}); raw FFI declarations are only available under a policy that allows them",
                        im.name, caps.policy_name, caps.origin
                    );
                }
                if !p.imports.iter().any(|x| x.name == im.name) {
                    p.imports.push(im);
                }
            }
            Tok::ManifestImport(im) => {
                // imports inside an allowed module's .ufm manifest: loading the
                // module was already gated by the ffi.use module policy
                if !p.imports.iter().any(|x| x.name == im.name) {
                    p.imports.push(im);
                }
            }
            Tok::Export(n) => pending_export = Some(n),
            Tok::Use(n) => {
                if !caps.module_allowed(&n) {
                    let list = match &caps.module_allow {
                        Some(l) if !l.is_empty() => format!("; allowed modules: {}", l.join(", ")),
                        _ => String::new(),
                    };
                    panic!(
                        "sandbox: module `use \"{}\"` is denied by policy '{}' ({}){} — add it with `allow-module {}` in an allowed sandbox config, or run under a policy that permits it",
                        n, caps.policy_name, caps.origin, list, n
                    );
                }
                if !p.uses.contains(&n) {
                    p.uses.push(n);
                }
            }
            Tok::Mod(n) => p.modname = Some(n),
            Tok::Pub => pending_pub = true,
            Tok::Extern(n) => {
                if !caps.cap("ffi.import") {
                    panic!(
                        "sandbox: `extern \"{}\"` is disabled — capability `ffi.import` is denied by policy '{}' ({}); extern symbols are only available under a policy that allows them",
                        n, caps.policy_name, caps.origin
                    );
                }
                if !p.externs.contains(&n) {
                    p.externs.push(n.clone());
                }
                p.ins.push(Ins::Extern(n));
            }
            Tok::MacroDef(n, body) => {
                macros.insert(n, body);
            }
            Tok::StructDef(n, fields) => {
                // v13: obj fields are stored as full Cells (16 bytes: tag +
                // int64 payload), so the layout strides by sizeof(Cell) —
                // 8-byte strides made adjacent fields overlap.
                let mut off = 0i64;
                let mut fs = Vec::new();
                for (fname, fty) in &fields {
                    let sz = match type_id(fty) {
                        Some(_) => 16,
                        None => match structs.get(fty) {
                            Some((_, tsz, _)) => *tsz,
                            None => panic!("STRUCT {}: unknown field type {}", n, fty),
                        },
                    };
                    let align = sz.min(16);
                    off = (off + align - 1) / align * align;
                    fs.push((fname.clone(), off));
                    off += sz;
                }
                let sid = structs.len() as i64;
                structs.insert(n, (fs, off, sid));
            }
            Tok::Method(tname) => pending_method = Some(tname),
            Tok::Sys(n) => {
                if !caps.cap("raw.syscall") {
                    panic!(
                        "sandbox: `_syscall {}` is disabled — capability `raw.syscall` is denied by policy '{}' ({}); syscalls are never available in sandboxed policies",
                        n, caps.policy_name, caps.origin
                    );
                }
                p.ins.push(Ins::Sys(n))
            }
            Tok::Task { name, count, body } => {
                // v10 fanout: count literal 1..64; count > 1 requires exactly
                // one input (checked at RUN where the input is resolved)
                if let Some(c) = count {
                    if !(1..=64).contains(&c) {
                        panic!("WEAVE: task {} worker count {} out of range 1..64", name, c);
                    }
                }
                // v13: leading binding tokens in the body are the task's input
                // bindings (task name! makes that task's result a parameter)
                let mut inputs: Vec<Param> = Vec::new();
                let mut body: Vec<Tok> = body;
                loop {
                    match body.first() {
                        Some(Tok::LocalSet(nm)) => {
                            inputs.push(Param::Local(nm.clone()));
                            body.remove(0);
                        }
                        Some(Tok::SetV(nm)) => {
                            inputs.push(Param::Global(nm.clone()));
                            body.remove(0);
                        }
                        Some(Tok::Discard) => panic!("WEAVE: task {} input `_!` needs a task name", name),
                        _ => break,
                    }
                }
                let skip = format!("__wskip{}", weave_ctr);
                weave_ctr += 1;
                p.ins.push(Ins::Goto(skip.clone()));
                p.labels.insert(name.clone(), p.ins.len());
                // task bodies are self-contained: their labels are task-local,
                // so two tasks may reuse the same v-names
                let prefix = format!("{}/", name);
                let body = prefix_task_labels(body, &prefix);
                // input bindings are emitted reversed (guarded param pops)
                for (k, par) in inputs.iter().rev().enumerate() {
                    let pc = p.ins.len();
                    match par {
                        Param::Local(nm) => p.ins.push(Ins::LocalSet(nm.clone())),
                        Param::Global(nm) => {
                            if !p.vars.contains(nm) {
                                p.vars.push(nm.clone());
                            }
                            p.ins.push(Ins::SetV(nm.clone()));
                        }
                        Param::Discard => unreachable!(),
                    }
                    p.param_pcs.insert(pc, inputs.len() - 1 - k);
                }
                q.push_front(Tok::TaskEnd(skip));
                for t in body.into_iter().rev() {
                    q.push_front(t);
                }
                cur_weave.push((name, inputs, count.unwrap_or(1)));
            }
            Tok::TaskEnd(skip) => {
                // v13: the task body ends with the user's explicit `ret`;
                // the skip label just marks where the next unit begins
                p.labels.insert(skip, p.ins.len());
            }
            Tok::List(body) => {
                let mut lbody = body;
                lbody.push(Tok::Ident("@listlit".to_string()));
                lbody.insert(0, Tok::Ident("@liststart".to_string()));
                for t in lbody.into_iter().rev() {
                    q.push_front(t);
                }
            }
            Tok::Dict(body) => {
                let mut dbody = body;
                dbody.push(Tok::Ident("@dictlit".to_string()));
                dbody.insert(0, Tok::Ident("@dictstart".to_string()));
                for t in dbody.into_iter().rev() {
                    q.push_front(t);
                }
            }
            Tok::Wrun(terminal) => {
                // static DAG checks: inputs must name tasks in this weave; acyclic
                let names: Vec<&String> = cur_weave.iter().map(|(n, _, _)| n).collect();
                let mut metas: Vec<WeaveMeta> = Vec::new();
                for (n, ins, count) in &cur_weave {
                    let mut idxs = Vec::new();
                    for inp in ins {
                        let inp_name = match inp {
                            Param::Local(x) | Param::Global(x) => x,
                            Param::Discard => unreachable!(),
                        };
                        match names.iter().position(|x| *x == inp_name) {
                            Some(i) => idxs.push(i),
                            None => panic!("WEAVE: task {} has unknown input {}", n, inp_name),
                        }
                    }
                    if *count > 1 && idxs.is_empty() {
                        panic!("WEAVE: fanout task {} ({} workers) must have at least one input", n, count);
                    }
                    metas.push(WeaveMeta { name: n.clone(), pc: *p.labels.get(n).unwrap_or(&0), inputs: idxs, count: *count });
                }
                // Kahn topo: cycle check (execution order is the scheduler's job)
                let mut done = vec![false; metas.len()];
                for _ in 0..metas.len() {
                    let mut progressed = false;
                    for (i, m) in metas.iter().enumerate() {
                        if !done[i] && m.inputs.iter().all(|&j| done[j]) {
                            done[i] = true;
                            progressed = true;
                        }
                    }
                    if !progressed {
                        break;
                    }
                }
                if done.iter().any(|d| !d) {
                    panic!("WEAVE: cycle in task DAG");
                }
                // v13: `run <name>` names a terminal task. Tasks not reachable
                // from the terminal become orphans: they stay in scope (their
                // labels remain callable) but are dropped from the auto-run
                // DAG. The terminal's result is left on the stack after run.
                let mut terminal_idx: Option<usize> = None;
                if let Some(t) = &terminal {
                    match names.iter().position(|x| *x == t) {
                        Some(i) => terminal_idx = Some(i),
                        None => panic!("WEAVE: run names unknown task {}", t),
                    }
                }
                if let Some(ti) = terminal_idx {
                    // reverse BFS from the terminal over input edges
                    let mut reachable = vec![false; metas.len()];
                    reachable[ti] = true;
                    let mut changed = true;
                    while changed {
                        changed = false;
                        for (i, m) in metas.iter().enumerate() {
                            if !reachable[i] {
                                continue;
                            }
                            for &j in &m.inputs {
                                if !reachable[j] {
                                    reachable[j] = true;
                                    changed = true;
                                }
                            }
                        }
                    }
                    let mut kept: Vec<usize> = (0..metas.len()).filter(|&i| reachable[i]).collect();
                    // move the terminal last so the codegen pushes its result
                    kept.retain(|&i| i != ti);
                    kept.push(ti);
                    let mut remap = vec![0usize; metas.len()];
                    for (k, &i) in kept.iter().enumerate() {
                        remap[i] = k;
                    }
                    let mut metas2: Vec<WeaveMeta> = Vec::new();
                    for &i in &kept {
                        let m = &metas[i];
                        metas2.push(WeaveMeta {
                            name: m.name.clone(),
                            pc: m.pc.clone(),
                            inputs: m.inputs.iter().map(|&j| remap[j]).collect(),
                            count: m.count,
                        });
                    }
                    metas = metas2;
                }
                if !metas.is_empty() {
                    for m in &metas {
                        if !p.vars.contains(&m.name) {
                            p.vars.push(m.name.clone());
                        }
                    }
                    p.ins.push(Ins::Weave(metas));
                }
                cur_weave.clear();
            }
            Tok::LabelDef(n) => {
                p.labels.insert(n.clone(), p.ins.len());
                if pending_pub {
                    p.pubs.push(n.clone());
                    pending_pub = false;
                }
                if let Some(tname) = pending_method.take() {
                    let tk = method_typekey(&tname, structs)
                        .unwrap_or_else(|| panic!("METHOD: unknown type {}", tname));
                    p.methods.push((tk, fnv64(&n), n.clone()));
                }
                if let Some(x) = pending_export.take() {
                    p.exports.push((x, n));
                }
                // v13: consume a leading run of binding tokens as parameter
                // declarations (name! / ^name! / _!). Bindings are emitted as
                // instructions in REVERSE order so the first declared binding
                // pops the first cell the caller pushed (the caller's stack
                // holds the args in push order, last on top).
                let mut params: Vec<Param> = Vec::new();
                while let Some(t2) = q.front() {
                    match t2 {
                        Tok::LocalSet(name) => {
                            params.push(Param::Local(name.clone()));
                            q.pop_front();
                        }
                        Tok::SetV(name) => {
                            params.push(Param::Global(name.clone()));
                            q.pop_front();
                        }
                        Tok::Discard => {
                            params.push(Param::Discard);
                            q.pop_front();
                        }
                        _ => break,
                    }
                }
                if !params.is_empty() {
                    p.label_params.insert(p.ins.len(), params.clone());
                }
                for (k, par) in params.iter().rev().enumerate() {
                    let pc = p.ins.len();
                    match par {
                        Param::Local(name) => p.ins.push(Ins::LocalSet(name.clone())),
                        Param::Global(name) => {
                            if !p.vars.contains(name) {
                                p.vars.push(name.clone());
                            }
                            p.ins.push(Ins::SetV(name.clone()));
                        }
                        Param::Discard => p.ins.push(Ins::Simple("op_drop")),
                    }
                    // guard offset: the k-th pop (0-based, reversed emission)
                    // pops only when the caller left (arity - k) cells; see
                    // the guard in gen.rs
                    p.param_pcs.insert(pc, params.len() - 1 - k);
                }
            }
            Tok::Entry => {
                p.labels.insert("entry".to_string(), p.ins.len());
                p.entry_label = Some("entry".to_string());
            }
            Tok::Ident(n) => {
                if n == "@flush" {
                    p.ins.push(Ins::Flush);
                } else if n == "@liststart" {
                    p.ins.push(Ins::ListStart);
                } else if n == "@dictstart" {
                    p.ins.push(Ins::DictStart);
                } else if n == "@listlit" {
                    p.ins.push(Ins::ListLit);
                } else if n == "@dictlit" {
                    p.ins.push(Ins::DictLit);
                } else if let Some(rest) = n.strip_prefix("@sizeof:") {
                    if let Some(id) = type_id(rest) {
                        p.ins.push(Ins::PushI(if id == 3 { 1 } else { 8 }));
                    } else if let Some((_, tsz, _)) = structs.get(rest) {
                        p.ins.push(Ins::PushI(*tsz));
                    } else {
                        panic!("SIZEOF: unknown type {}", rest);
                    }
                } else if let Some(rest) = n.strip_prefix("@offset:") {
                    let parts: Vec<&str> = rest.splitn(2, '.').collect();
                    if parts.len() != 2 {
                        panic!("OFFSET needs Struct.field, got {}", rest);
                    }
                    let (fs, _, _) = structs
                        .get(parts[0])
                        .unwrap_or_else(|| panic!("OFFSET: unknown struct {}", parts[0]));
                    let off = fs
                        .iter()
                        .find(|(f, _)| f == parts[1])
                        .unwrap_or_else(|| panic!("OFFSET: no field {} in {}", parts[1], parts[0]));
                    p.ins.push(Ins::PushI(off.1));
                } else if let Some(rest) = n.strip_prefix("@objsize:") {
                    let (_, tsz, sid) = structs
                        .get(rest)
                        .unwrap_or_else(|| panic!("OBJ: unknown struct {}", rest));
                    // size in low 32 bits, struct id above (op_obj splits them)
                    p.ins.push(Ins::PushI(tsz | (sid << 32)));
                } else if let Some(rest) = n.strip_prefix("@cast:") {
                    let (_, _, sid) = structs
                        .get(rest)
                        .unwrap_or_else(|| panic!("CAST: unknown struct {}", rest));
                    p.ins.push(Ins::PushI(1000 + sid));
                } else if let Some(body) = macros.get(&n).cloned() {
                    expand_depth += 1;
                    if expand_depth > 1024 {
                        panic!("macro expansion too deep (recursive macro?)");
                    }
                    for t2 in body.into_iter().rev() {
                        q.push_front(t2);
                    }
                } else {
                    panic!("unknown identifier {} (not a macro)", n);
                }
            }
        }
    }
    let _ = imports;
    // v13: every label body must end with `ret` (a path that falls off the end
    // of a label is a compile error). Bodies consisting only of break/cont are
    // loop-exit continuations and are exempt; internal weave-skip regions end
    // with a Goto and are exempt (no user code may end a body with a jump).
    {
        let mut pcs: Vec<(String, usize)> = p.labels.iter().map(|(n, &pc)| (n.clone(), pc)).collect();
        pcs.sort_by_key(|(_, pc)| *pc);
        for (i, (name, pc)) in pcs.iter().enumerate() {
            // internal weave-skip labels are not user bodies
            if name.starts_with("__wskip") {
                continue;
            }
            let end = pcs.get(i + 1).map(|(_, e)| *e).unwrap_or(p.ins.len());
            if end <= *pc {
                continue; // empty label (alias)
            }
            let last = end - 1;
            if std::env::var("UF_DEBUG_PARSE").is_ok() {
                eprintln!("retcheck label={} pc={} end={} lastins={:?}", name, pc, end, p.ins.get(last));
            }
            // Terminating instructions never fall off the end: ret returns,
            // break/cont unwind a loop, Goto jumps, and if_else transfers to
            // one of two bodies. if/while/for fall through on some path, so a
            // body may not end with them.
            match &p.ins[last] {
                Ins::Ret | Ins::Break | Ins::Cont | Ins::Goto(_) | Ins::IfElse => {}
                _ => panic!(
                    "label '{}': body must end with `ret` — falling off the end of a label is a v13 compile error",
                    name
                ),
            }
        }
    }
    // v10: break/cont outside a loop are compile errors. A "function" here is
    // the instruction region starting at a label used as a code address
    // (CALL/ADDR/weave task entry); a loop is a FOR/WHILE whose body address
    // is a compile-time-known PushAddr. break/cont must textually appear in a
    // loop body (nested quotation bodies have their own labels, so nested
    // loops resolve to the innermost body as required).
    {
        let mut entries: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut loop_bodies: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let resolve = |l: &String| -> Option<usize> { p.labels.get(l).copied() };
        for (i, ins) in p.ins.iter().enumerate() {
            match ins {
                Ins::Call(l) => {
                    if let Some(t) = resolve(l) {
                        entries.insert(t);
                    }
                }
                Ins::PushAddr(l) => {
                    // A PushAddr that directly feeds a control-flow op
                    // (if/ifelse/while/for) is a continuation of the current
                    // body, not a new function entry: it runs in the caller's
                    // frame, so break/cont inside it bind to the caller's
                    // loop. Everything else (stored/computed addresses)
                    // starts a new entry.
                    let feeds_cf = match p.ins.get(i + 1) {
                        Some(Ins::If) | Some(Ins::IfElse) | Some(Ins::For) | Some(Ins::While) => true,
                        Some(Ins::PushAddr(_)) => {
                            matches!(p.ins.get(i + 2), Some(Ins::While) | Some(Ins::IfElse))
                        }
                        _ => false,
                    };
                    if !feeds_cf {
                        if let Some(t) = resolve(l) {
                            entries.insert(t);
                        }
                    }
                }
                Ins::Weave(ms) => {
                    for m in ms {
                        entries.insert(m.pc);
                    }
                }
                Ins::For => {
                    if i > 0 {
                        if let Ins::PushAddr(l) = &p.ins[i - 1] {
                            if let Some(t) = resolve(l) {
                                loop_bodies.insert(t);
                            }
                        }
                    }
                }
                Ins::While => {
                    if i > 0 {
                        if let Ins::PushAddr(l) = &p.ins[i - 1] {
                            if let Some(t) = resolve(l) {
                                loop_bodies.insert(t);
                            }
                        }
                    }
                    if i > 1 {
                        if let Ins::PushAddr(l) = &p.ins[i - 2] {
                            if let Some(t) = resolve(l) {
                                loop_bodies.insert(t);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let mut cur_fn: Option<usize> = None;
        for (i, ins) in p.ins.iter().enumerate() {
            if entries.contains(&i) || loop_bodies.contains(&i) {
                cur_fn = Some(i);
            }
            if matches!(ins, Ins::Break | Ins::Cont) {
                let ok = match cur_fn {
                    Some(f) => loop_bodies.contains(&f),
                    None => false,
                };
                if !ok {
                    panic!("{} outside a loop (compile error)",
                        if matches!(ins, Ins::Break) { "break" } else { "cont" });
                }
            }
        }
    }
    // Note: resolve_locals is deferred to merge_tus, where all TU offsets
    // are finalized. Calling it here would produce per-TU slot IDs and frame
    // sizes that are invalidated when instructions are offset during merge.
    p
}

// v11: resolve implicit local variables into per-call-body slot IDs.
//
// A "call body" is a group of labels that share a single local-variable frame.
// Each CALL target (or the entry label / pc 0) starts a new call body. Labels
// reached via PushAddr (if/while/for bodies) are continuations that share the
// frame of the call body that references them.
//
// To determine ownership, we use a worklist algorithm: starting from each
// call-entry label, we propagate the body ID to all PushAddr targets within
// the same body's instruction range. This correctly handles continuation
// labels that appear after other call entries in the instruction stream.
pub fn resolve_locals(p: &mut Parsed) {
    if p.ins.is_empty() {
        return;
    }
    // Determine which labels are "call entries" (get their own frame).
    let mut call_entries: std::collections::HashSet<usize> = std::collections::HashSet::new();
    call_entries.insert(0);
    if let Some(el) = &p.entry_label {
        if let Some(&pc) = p.labels.get(el) {
            call_entries.insert(pc);
        }
    }
    for ins in &p.ins {
        if let Ins::Call(l) = ins {
            if let Some(&pc) = p.labels.get(l) {
                call_entries.insert(pc);
            }
        }
    }
    for ins in &p.ins {
        if let Ins::Weave(tasks) = ins {
            for t in tasks {
                call_entries.insert(t.pc);
            }
        }
    }
    for (_, l) in &p.exports {
        if let Some(&pc) = p.labels.get(l) {
            call_entries.insert(pc);
        }
    }

    // Build a sorted list of label PCs to delimit label regions.
    // Always include pc 0 (the implicit entry point) even if there's no label.
    let mut all_label_pcs: Vec<usize> = p.labels.values().copied().collect();
    all_label_pcs.push(0);
    all_label_pcs.sort();
    all_label_pcs.dedup();

    // For each label PC, find the nearest preceding call-entry PC.
    // That call-entry is the "body owner" for that label's region.
    // But continuation labels (PushAddr targets) may belong to a body whose
    // entry is further back, past other call entries. We fix this with a
    // propagation pass: each PushAddr instruction inside body X that references
    // label L means L belongs to body X (regardless of position).
    //
    // Step 1: Initial assignment by position (nearest preceding call entry).
    let mut label_body: HashMap<usize, usize> = HashMap::new(); // label_pc -> body_entry_pc
    {
        let mut cur_body = 0usize; // pc 0 is always a call entry
        for &pc in &all_label_pcs {
            if call_entries.contains(&pc) {
                cur_body = pc;
            }
            label_body.insert(pc, cur_body);
        }
    }

    // Step 2: Propagate body ownership through PushAddr references.
    // A PushAddr target (continuation label) belongs to the same body as the
    // label whose range contains the reference. Continuation labels can be
    // mutually referenced (if/else webs), so this is a union-find merge over
    // labels — a naive "reassign until stable" loop oscillates forever on
    // such cycles. Each component takes the earliest positional body among
    // its members (a component spanning call entries shares the outermost
    // frame; locals resolution then treats those labels as one frame).
    {
        // union-find over label pcs
        let mut parent: HashMap<usize, usize> = HashMap::new();
        for &pc in &all_label_pcs {
            parent.insert(pc, pc);
        }
        fn find(parent: &mut HashMap<usize, usize>, x: usize) -> usize {
            let mut root = x;
            loop {
                let p = *parent.get(&root).unwrap_or(&root);
                if p == root { break; }
                root = p;
            }
            // path compression
            let mut cur = x;
            while cur != root {
                let next = *parent.get(&cur).unwrap_or(&root);
                parent.insert(cur, root);
                cur = next;
            }
            root
        }
        let ranges: Vec<(usize, usize)> = all_label_pcs.iter().copied().zip({
            let mut nexts = all_label_pcs.iter().copied().skip(1).collect::<Vec<_>>();
            nexts.push(p.ins.len());
            nexts.into_iter()
        }).collect();
        for &(lpc, end) in &ranges {
            let lroot = find(&mut parent, lpc);
            for i in lpc..end.min(p.ins.len()) {
                if let Ins::PushAddr(target) = &p.ins[i] {
                    if let Some(&target_pc) = p.labels.get(target) {
                        let troot = find(&mut parent, target_pc);
                        if troot != lroot {
                            parent.insert(troot, lroot);
                        }
                    }
                }
            }
        }
        // component body = min positional body among members
        let mut comp_body: HashMap<usize, usize> = HashMap::new();
        for &pc in &all_label_pcs {
            let root = find(&mut parent, pc);
            let pos = *label_body.get(&pc).unwrap_or(&0);
            let e = comp_body.entry(root).or_insert(pos);
            if pos < *e { *e = pos; }
        }
        for &pc in &all_label_pcs {
            let root = find(&mut parent, pc);
            if let Some(&b) = comp_body.get(&root) {
                label_body.insert(pc, b);
            }
        }
    }

    // Step 3: For each instruction, determine its body by finding the nearest
    // preceding label and looking up that label's body.
    let mut ins_body: Vec<usize> = vec![0usize; p.ins.len()];
    {
        let mut cur_body = 0usize;
        for i in 0..p.ins.len() {
            if all_label_pcs.contains(&i) {
                if let Some(&b) = label_body.get(&i) {
                    cur_body = b;
                }
            }
            ins_body[i] = cur_body;
        }
    }

    // Step 4: Collect local names per body, assigning slot IDs.
    let mut body_slots: HashMap<usize, HashMap<String, usize>> = HashMap::new();
    for (i, ins) in p.ins.iter().enumerate() {
        let body = ins_body[i];
        let slots = body_slots.entry(body).or_default();
        match ins {
            Ins::LocalSet(n) | Ins::LocalGet(n) => {
                if !slots.contains_key(n) {
                    let id = slots.len();
                    slots.insert(n.clone(), id);
                }
            }
            _ => {}
        }
    }

    // Step 5: Record frame sizes and resolve instructions.
    for (&body, slots) in &body_slots {
        p.local_counts.insert(body, slots.len());
        let mut names: Vec<(String, usize)> = slots.iter().map(|(n, &id)| (n.clone(), id)).collect();
        names.sort_by_key(|(_, id)| *id);
        p.local_names.insert(body, names.into_iter().map(|(n, _)| n).collect());
    }
    for (i, ins) in p.ins.iter_mut().enumerate() {
        let body = ins_body[i];
        match ins {
            Ins::LocalSet(n) => {
                if let Some(&id) = body_slots.get(&body).and_then(|s| s.get(n)) {
                    *ins = Ins::LocalSetI(id);
                }
            }
            Ins::LocalGet(n) => {
                if let Some(&id) = body_slots.get(&body).and_then(|s| s.get(n)) {
                    *ins = Ins::LocalGetI(id);
                }
            }
            _ => {}
        }
    }
}
pub fn prefix_task_labels(body: Vec<Tok>, prefix: &str) -> Vec<Tok> {
    body.into_iter()
        .map(|t| match t {
            Tok::LabelDef(n) => Tok::LabelDef(format!("{}{}", prefix, n)),
            Tok::Jump(op, l) => Tok::Jump(op, format!("{}{}", prefix, l)),
            Tok::MacroDef(n, mbody) => Tok::MacroDef(n, prefix_task_labels(mbody, prefix)),
            other => other,
        })
        .collect()
}

// Merge per-TU Parsed units into one image; tus[0] is the main TU (pc 0).
// Labels are mangled "<mod>:<name>"; jump/call operands defined in the same TU
// are rewritten to the mangled name, everything else stays a global (PUB) name.
// Variables are always file-local: "<mod>__<name>". IMPORT/EXTERN/USE dedupe.
pub fn merge_tus(tus: Vec<Parsed>, mods: Vec<String>, init_flags: &[bool]) -> Parsed {
    let mut m = Parsed {
        ins: Vec::new(),
        labels: HashMap::new(),
        imports: Vec::new(),
        exports: Vec::new(),
        externs: Vec::new(),
        strings: Vec::new(),
        vars: Vec::new(),
        uses: Vec::new(),
        modname: None,
        pubs: Vec::new(),
        methods: Vec::new(),
        init_pcs: Vec::new(),
        entry_label: None,
        local_counts: HashMap::new(),
        local_names: HashMap::new(),
        label_params: HashMap::new(),
        param_pcs: HashMap::new(),
    };
    for (tu_idx, (tu, defmod)) in tus.into_iter().zip(mods).enumerate() {
        let modname = tu.modname.clone().unwrap_or(defmod);
        let off = m.ins.len();
        if init_flags.get(tu_idx).copied().unwrap_or(false) {
            m.init_pcs.push(off);
        }
        let str_base = m.strings.len();
        let mut imp_map: Vec<usize> = Vec::new();
        for im in &tu.imports {
            let ix = match m.imports.iter().position(|x| x.name == im.name) {
                Some(i) => i,
                None => {
                    m.imports.push(im.clone());
                    m.imports.len() - 1
                }
            };
            imp_map.push(ix);
        }
        let local = |l: &String| -> String {
            if tu.labels.contains_key(l) {
                format!("{}:{}", modname, l)
            } else {
                l.clone()
            }
        };
        for ins in &tu.ins {
            m.ins.push(match ins {
                Ins::PushI(v) => Ins::PushI(*v),
                Ins::PushF(v) => Ins::PushF(*v),
                Ins::PushS(i) => Ins::PushS(i + str_base),
                Ins::PushAddr(l) => Ins::PushAddr(local(l)),
                Ins::Simple(h) => Ins::Simple(h),
                Ins::Goto(l) => Ins::Goto(local(l)),
                Ins::For => Ins::For,
                Ins::Call(l) => Ins::Call(local(l)),
                Ins::CallExt(ii) => Ins::CallExt(imp_map[*ii]),
                Ins::Ret => Ins::Ret,
                Ins::SetV(v) => Ins::SetV(format!("{}__{}", modname, v)),
                Ins::GetV(v) => Ins::GetV(format!("{}__{}", modname, v)),
                Ins::LocalSet(n) => Ins::LocalSet(n.clone()),
                Ins::LocalGet(n) => Ins::LocalGet(n.clone()),
                Ins::LocalSetI(id) => Ins::LocalSetI(*id),
                Ins::LocalGetI(id) => Ins::LocalGetI(*id),
                Ins::Extern(n) => Ins::Extern(n.clone()),
                Ins::Send => Ins::Send,
                Ins::Flush => Ins::Flush,
                Ins::ListStart => Ins::ListStart,
                Ins::DictStart => Ins::DictStart,
                Ins::ListLit => Ins::ListLit,
                Ins::DictLit => Ins::DictLit,
                Ins::Weave(metas) => Ins::Weave(
                    metas
                        .iter()
                        .map(|t| WeaveMeta {
                            name: format!("{}__{}", modname, t.name),
                            pc: t.pc + off,
                            inputs: t.inputs.clone(),
                            count: t.count,
                        })
                        .collect(),
                ),
                Ins::Sys(a) => Ins::Sys(*a),
                Ins::If => Ins::If,
                Ins::IfElse => Ins::IfElse,
                Ins::While => Ins::While,
                Ins::Break => Ins::Break,
                Ins::Cont => Ins::Cont,
            });
        }
        for (name, idx) in &tu.labels {
            m.labels.insert(format!("{}:{}", modname, name), idx + off);
        }
        // a TU's top-level flow must not fall through into the next TU
        m.ins.push(Ins::Ret);
        for name in &tu.pubs {
            if m.labels.contains_key(name) {
                panic!("duplicate PUB label {}", name);
            }
            m.labels.insert(name.clone(), tu.labels[name] + off);
            m.pubs.push(name.clone());
        }
        for (tk, mh, l) in &tu.methods {
            m.methods.push((*tk, *mh, local(l)));
        }
        m.strings.extend(tu.strings.iter().cloned());
        for e in &tu.externs {
            if !m.externs.contains(e) {
                m.externs.push(e.clone());
            }
        }
        for (c, l) in &tu.exports {
            m.exports.push((c.clone(), local(l)));
        }
        m.vars.extend(tu.vars.iter().map(|v| format!("{}__{}", modname, v)));
        for u in &tu.uses {
            if !m.uses.contains(u) {
                m.uses.push(u.clone());
            }
        }
        // carry entry_label from the first TU only
        if tu_idx == 0 {
            if let Some(el) = &tu.entry_label {
                m.entry_label = Some(format!("{}:{}", modname, el));
            }
        }
        // v13: label params and param-pop pcs shift with the TU offset
        for (&pc, ps) in &tu.label_params {
            m.label_params.insert(pc + off, ps.clone());
        }
        for (&pc, &k) in &tu.param_pcs {
            m.param_pcs.insert(pc + off, k);
        }
    }
    resolve_locals(&mut m);
    m
}

pub fn simple_ins(name: &'static str) -> Ins {
    // helper name in the C prelude
    let helper: &'static str = match name {
        "ADD" => "op_add",
        "SUB" => "op_sub",
        "MUL" => "op_mul",
        "AND" => "op_and",
        "POW" => "op_pow",
        "SQRT" => "op_sqrt",
        "LTE" => "op_lte",
        "GTE" => "op_gte",
        "SHUTDOWN" => "op_shutdown",
        "DROP" => "op_drop",
        "SHR" => "op_shr",
        "INC" => "op_inc",
        "DEC" => "op_dec",
        "OBJ" => "op_obj",
        "GET" => "op_get",
        "SET" => "op_set",
        "ARR" => "op_arr",
        "CLONE" => "op_clone",
        "CAST" => "op_cast",
        "TENSOR" => "op_tensor",
        "CAT" => "op_cat",
        "FMT" => "op_fmt",
        "BUF" => "op_buf",
        "BUFCOPY" => "op_bufcopy",
        "LOADX" => "op_loadx",
        "STOREX" => "op_storex",
        "SIZEOF" => "op_sizeof",
        "MALLOC" => "op_malloc",
        "FREE" => "op_free",
        "GC" => "op_gc",
        "PRINT" => "op_print",
        "SCAN" => "op_scan",
        "DICT" => "op_dict",
        "LIST" => "op_list",
        "PUSH" => "op_push",
        "POP" => "op_lpop",
        "CHAN" => "op_chan",
        "ENQ" => "op_enq",
        "DEQ" => "op_deq",
        "CLOSE" => "op_close",
        "ATOM" => "op_atom",
        "AGET" => "op_aget",
        "ASET" => "op_aset",
        "AADD" => "op_aadd",
        "CAS" => "op_cas",
        "TYPEOF" => "op_typeof",
        "LEN" => "op_len",
        "SH" => "op_sh",
        "SHP" => "op_shp",
        "EXEC" => "op_exec",
        "CAP" => "op_cap",
        "CAPS" => "op_caps",
        "MATCH" => "op_match",
        "REPLACE" => "op_replace",
        "RSPLIT" => "op_rsplit",
        "GLOB" => "op_glob",
        "SPLIT" => "op_split",
        "JOIN" => "op_join",
        "SLICE" => "op_slice",
        "FIND" => "op_find",
        "REPL" => "op_repl",
        "TRIM" => "op_trim",
        "UP" => "op_up",
        "DOWN" => "op_down",
        "STARTS" => "op_starts",
        "ENDS" => "op_ends",
        // v10: arithmetic & logic
        "DIV" => "op_div",
        "REM" => "op_rem",
        "EQ" => "op_eq",
        "SEQ" => "op_seq",
        "SNE" => "op_sne",
        "LT" => "op_lt",
        "GT" => "op_gt",
        "NOT" => "op_not",
        "OR" => "op_or",
        "XOR" => "op_xor",
        "SHL" => "op_shl",
        "BNOT" => "op_bnot",
        // v10: container protocol & sequences
        "DEL" => "op_del",
        "GETQ" => "op_getq",
        "HAS" => "op_has",
        "ORELSE" => "op_orelse",
        "KEYS" => "op_keys",
        "RANGE" => "op_range",
        "SORT" => "op_sort",
        "FILTER" => "op_filter",
        "SOME" => "op_some",
        "EVERY" => "op_every",
        // v10: vector ops
        "VADD" => "op_add",
        "VSUB" => "op_sub",
        "VMUL" => "op_mul",
        "VDIV" => "op_div",
        "VEADD" => "op_add",
        "VESUB" => "op_sub",
        "VEMUL" => "op_mul",
        "VEDIV" => "op_div",
        "VEMAX" => "op_vemax",
        "VEMIN" => "op_vemin",
        "VEQ" => "op_veq",
        "VLT" => "op_vlt",
        "VGT" => "op_vgt",
        "VGE" => "op_vge",
        "VLE" => "op_vle",
        "VAND" => "op_vand",
        "VOR" => "op_vor",
        "VNOT" => "op_vnot",
        "VCOUNT" => "op_vcount",
        "VGATHER" => "op_vgather",
        "VSUM" => "op_vsum",
        "VMEAN" => "op_vmean",
        "VMIN" => "op_vmin",
        "VMAX" => "op_vmax",
        "VMAP" => "op_vmap",
        "VFOLD" => "op_vfold",
        // v10: time, bloom, script I/O
        "NOW" => "op_now",
        "TIME" => "op_time",
        "TIMEF" => "op_timef",
        "BLOOM" => "op_bloom",
        "BADD" => "op_badd",
        "BTEST" => "op_btest",
        "SLURP" => "op_slurp",
        "SPIT" => "op_spit",
        "ARGV" => "op_argv",
        // v10: additional data ops
        "GROUP" => "op_group",
        "AGG" => "op_agg",
        "UNIQUE" => "op_unique",
        "FLAT" => "op_flat",
        "CHUNK" => "op_chunk",
        "VARGSORT" => "op_vargsort",
        "VSEARCHSORTED" => "op_vsearchsorted",
        "VWHERE" => "op_vwhere",
        // v10: large-data shortcuts
        "MMAP" => "op_mmap",
        "FEACH" => "op_feach",
        "FFOLD" => "op_ffold",
        "FSPLIT" => "op_fsplit",
        "FGET" => "op_fget",
        "FATOI" => "op_fatoi",
        "FATOF" => "op_fatof",
        "FSGET" => "op_fsget",
        "FBYTE" => "op_fbyte",
        "VGET" => "op_vget",
        "VSET" => "op_vset",
        "ADDTO" => "op_addto",
        "FADDTO" => "op_faddto",
        "FINC" => "op_finc",
        "INCR" => "op_count",
        "FMATCH" => "op_fmatch",
        "BFS" => "op_bfs",
        "DFS" => "op_dfs",
        "WFIND" => "op_wfind",
        // v10: JSON, iterators, sinks, containment, threads
        "JSON" => "op_json",
        "UNJSON" => "op_unjson",
        "ITER" => "op_iter",
        "NEXT" => "op_next",
        "COLLECT" => "op_collect",
        "IMAP" => "op_imap",
        "IFILTER" => "op_ifilter",
        "FEMIT" => "op_femit",
        "TRY" => "op_try",
        "RETRY" => "op_retry",
        "SPAWN" => "op_spawn",
        "ATOI" => "op_atoi",
        "ATOF" => "op_atof",
        "ITOA" => "op_itoa",
        "FTOA" => "op_ftoa",
        "HASARGS" => "op_hasargs",
        "ARGI" => "op_argi",
        "SORTKEYS" => "op_sortkeys",
        "TOPN" => "op_topn",
        "RANGEFOLD" => "op_rangefold",
        "TRANSPOSE" => "op_transpose",

        other => panic!("no helper for {}", other),
    };
    Ins::Simple(helper)
}

// ---------------- strict label-call arity (v13.1) ----------------
// Fixed-effect table for the ops the macro above can't express cleanly.
pub fn helper_effect(h: &str) -> Option<(i64, i64)> {
    Some(match h {
        // arithmetic / logic
        "op_add" | "op_sub" | "op_mul" | "op_and" | "op_pow" | "op_div" | "op_rem"
        | "op_eq" | "op_seq" | "op_sne" | "op_lt" | "op_gt" | "op_lte" | "op_gte"
        | "op_or" | "op_xor" | "op_shl" => (2, 1),
        "op_sqrt" | "op_shr" | "op_inc" | "op_dec" | "op_not" | "op_bnot" => (1, 1),
        // containers
        "op_get" | "op_getq" | "op_orelse" | "op_has" | "op_contains" => (2, 1),
        "op_set" => (3, 0),
        "op_append" | "op_push" => (2, 1),
        "op_lpop" => (1, 1),
        "op_del" => (2, 0),
        "op_len" | "op_typeof" | "op_clone" | "op_keys" | "op_sort" | "op_unique" | "op_flat" => (1, 1),
        "op_range" | "op_filter" | "op_some" | "op_every" | "op_chunk" => (2, 1),
        "op_dict" | "op_list" | "op_caps" => (0, 1),
        "op_arr" | "op_tensor" => (2, 1),
        "op_cast" => (2, 1),
        // vector
        "op_vadd" | "op_vsub" | "op_vmul" | "op_vdiv" => (2, 1),
        "op_veadd" | "op_vesub" | "op_vemul" | "op_vediv" | "op_vemax" | "op_vemin" => (2, 1),
        "op_veq" | "op_vlt" | "op_vgt" | "op_vge" | "op_vle" => (2, 1),
        "op_vand" | "op_vor" => (2, 1),
        "op_vnot" | "op_vcount" | "op_vsum" | "op_vmean" | "op_vmin" | "op_vmax" | "op_vargsort" => (1, 1),
        "op_vgather" | "op_vwhere" | "op_vsearchsorted" => (2, 1),
        "op_vmap" | "op_vfold" => (3, 1),
        "op_vget" => (2, 1),
        "op_vset" => (3, 0),
        // strings
        "op_cat" | "op_join" | "op_slice" | "op_starts" | "op_ends" | "op_find" | "op_rsplit" => (2, 1),
        "op_replace" | "op_repl" => (3, 1),
        "op_trim" | "op_up" | "op_down" => (1, 1),
        "op_glob" => (2, 1),
        // script I/O
        "op_slurp" | "op_mmap" => (1, 1),
        "op_spit" => (2, 0),
        "op_femit" => (2, 1),
        "op_feach" => (2, 0),
        "op_ffold" | "op_fmatch" => return Some((3, 1)).filter(|_| h == "op_ffold"),
        "op_fsplit" => (4, 1),
        "op_fget" | "op_fatoi" | "op_fatof" | "op_fbyte" => (1, 1),
        "op_fsget" => (2, 1),
        "op_addto" => (3, 0),
        "op_faddto" | "op_finc" => (2, 0),
        "op_count" => (1, 1),
        "op_argv" | "op_hasargs" | "op_now" => (0, 1),
        "op_argi" => (1, 1),
        // shell
        "op_sh" => (1, 3),
        "op_shp" | "op_exec" => (1, 1),
        // time / bloom
        "op_time" | "op_timef" => (2, 1),
        "op_bloom" => (1, 1),
        "op_badd" => (2, 0),
        "op_btest" => (2, 1),
        // JSON / iterators
        "op_json" | "op_unjson" | "op_iter" | "op_next" | "op_collect" => (1, 1),
        "op_imap" | "op_ifilter" => (2, 1),
        // conversion
        "op_atoi" | "op_atof" | "op_itoa" | "op_ftoa" => (1, 1),
        // threads / channels / atomics
        "op_chan" | "op_atom" | "op_aget" | "op_deq" | "op_spawn" => (1, 1),
        "op_enq" => (2, 0),
        "op_close" => (1, 0),
        "op_aset" | "op_aadd" => (2, 0),
        "op_cas" => (3, 1),
        // memory
        "op_buf" | "op_malloc" | "op_sizeof" => (1, 1),
        "op_free" => (1, 0),
        "op_bufcopy" | "op_storex" => (3, 0),
        "op_loadx" => (1, 1),
        // misc
        "op_print" => (1, 0),
        "op_scan" => (1, 1),
        "op_cap" | "op_transpose" => (1, 1),
        "op_shutdown" => (0, 0),
        "op_drop" => (1, 0),
        "op_sortkeys" | "op_topn" => return None,
        "op_wfind" | "op_bfs" | "op_dfs" => (2, 1),
        "op_rangefold" => (3, 1),
        _ => return None,
    })
}

/// Post-merge pass: at every `_call`, verify the statically-known stack depth
/// covers the callee's declared arity. Unknown (poisoned) depths defer to the
/// runtime backstop. Missing arguments are a compile error (null-fill removed).
pub fn check_label_arity(p: &Parsed) {
    let arity_at = |pc: usize| -> usize {
        // nearest label at or before pc that declares params
        let mut best: Option<(usize, usize)> = None;
        for (&lpc, params) in &p.label_params {
            if lpc <= pc && best.map_or(true, |(b, _)| lpc > b) {
                best = Some((lpc, params.len()));
            }
        }
        best.map(|(_, n)| n).unwrap_or(0)
    };
    let label_name_at = |pc: usize| -> String {
        let mut best: Option<(usize, String)> = None;
        for (name, &lpc) in &p.labels {
            if lpc <= pc && best.as_ref().map_or(true, |(b, _)| lpc > *b) {
                best = Some((lpc, name.clone()));
            }
        }
        best.map(|(_, n)| n).unwrap_or_else(|| "<top>".to_string())
    };
    let mut depth: Option<i64> = Some(0);
    let mut list_marks: Vec<i64> = Vec::new();
    for (i, ins) in p.ins.iter().enumerate() {
        // label boundary: a new body begins (params are the incoming cells)
        let is_label_entry = p.labels.values().any(|&lpc| lpc == i) || i == 0;
        if is_label_entry {
            depth = Some(arity_at(i) as i64);
            list_marks.clear();
        }
        match ins {
            Ins::PushI(_) | Ins::PushF(_) | Ins::PushS(_) | Ins::PushAddr(_) => {
                depth = depth.map(|d| d + 1);
            }
            Ins::Simple(h) => {
                if p.param_pcs.contains_key(&i) {
                    // guarded param pop at body start: consumes one arg cell
                    depth = depth.map(|d| d - 1);
                } else {
                    depth = match (depth, helper_effect(h)) {
                        (Some(d), Some((pops, pushes))) => Some(d - pops + pushes),
                        _ => None,
                    };
                }
            }
            Ins::Call(l) => {
                match p.labels.get(l.as_str()).copied() {
                    Some(tp) => {
                        let arity = p.label_params.get(&tp).map(|v| v.len()).unwrap_or(0);
                        if let Some(d) = depth {
                            if d < arity as i64 {
                                let params = p.label_params.get(&tp).cloned().unwrap_or_default();
                                let pnames: Vec<String> = params
                                    .iter()
                                    .map(|pr| match pr {
                                        Param::Local(n) | Param::Global(n) => n.clone(),
                                        Param::Discard => "_!".to_string(),
                                    })
                                    .collect();
                                let missing = arity as i64 - if d > 0 { d } else { 0 };
                                panic!(
                                    "label '{}' declares {} parameter(s) ({}) but the call site leaves only {} value(s) on the stack (missing {}) — missing arguments are no longer filled with null; provide all arguments or declare fewer bindings",
                                    l, arity, pnames.join(" "), d, missing
                                );
                            }
                            depth = Some(d - arity as i64 + 1);
                        } else {
                            depth = None;
                        }
                    }
                    None => depth = None,
                }
            }
            Ins::CallExt(ii) => {
                let im = &p.imports[*ii];
                if im.params.iter().any(|x| x == "...") || im.ret == "..." {
                    depth = None;
                } else {
                    let n = im.params.len() as i64;
                    depth = depth.map(|d| d - n + if im.ret == "void" { 0 } else { 1 });
                }
            }
            Ins::For | Ins::While => {
                depth = depth.map(|d| d - 2);
            }
            Ins::If => {
                depth = depth.map(|d| d - 2);
            }
            Ins::IfElse => {
                depth = depth.map(|d| d - 3);
            }
            Ins::Sys(n) => {
                depth = depth.map(|d| d - *n as i64 - 1 + 1);
            }
            Ins::Send => {
                depth = depth.map(|d| d - 2);
            }
            Ins::ListStart | Ins::DictStart => {
                if let Some(d) = depth {
                    list_marks.push(d);
                } else {
                    list_marks.push(-1);
                }
            }
            Ins::ListLit | Ins::DictLit => {
                depth = match (list_marks.pop(), depth) {
                    (Some(-1), _) => None,
                    (Some(m), _) => Some(m + 1),
                    (None, d) => d, // malformed; leave as-is
                };
            }
            Ins::Ret => {
                // body ends; the next label entry resets depth
                depth = None;
            }
            Ins::Break | Ins::Cont | Ins::Goto(_) | Ins::Weave(_) | Ins::Extern(_) | Ins::Flush => {
                depth = None;
            }
            Ins::SetV(_) | Ins::GetV(_) | Ins::LocalSet(_) | Ins::LocalGet(_)
            | Ins::LocalSetI(_) | Ins::LocalGetI(_) => {
                if p.param_pcs.contains_key(&i) {
                    depth = depth.map(|d| d - 1);
                } else {
                    let net = match ins {
                        Ins::SetV(_) | Ins::LocalSet(_) | Ins::LocalSetI(_) => 0, // pass-through
                        _ => 1,
                    };
                    depth = depth.map(|d| d + net);
                }
            }
        }
    }
}
