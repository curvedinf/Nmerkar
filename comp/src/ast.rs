use std::collections::HashMap;

// ---------------- tokens ----------------
#[derive(Clone, Debug)]
pub struct Import {
    pub name: String,
    pub params: Vec<String>, // may contain "..."
    pub ret: String,
}

// v13 label parameter binding: the label's declared input arity.
#[derive(Clone, Debug)]
pub enum Param {
    Local(String),  // name!  — binds one caller cell to a body-local
    Global(String), // ^name! — binds one caller cell to a global
    Discard,        // _!     — bind and discard one caller cell
}

// v16 packages: `use [name] <string> [policy P] [allow caps] [deny caps]`
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UseSpec {
    pub alias: Option<String>,   // local alias (namespaced via `:` — phase 2)
    pub source: String,          // "curl", "gh:o/r", "gh:o/r@v1.2", "https://…", "path:dir"
    pub policy: Option<String>,  // named policy base for the grant (∅ when absent)
    pub allows: Vec<String>,     // capability patterns added to the base
    pub denies: Vec<String>,     // capability patterns subtracted (deny wins)
}

#[derive(Clone, Debug)]
pub enum Tok {
    Op(&'static str),          // simple opcode with no immediate
    PushI(i64),
    PushBool(bool),              // true/false literal (distinct from int and byte)
    PushF(f64),
    PushS(String),             // STR literal
    Jump(&'static str, String), // CALL/ADDR + label (JMP/JZ/JE removed in v10.1)
    SetV(String),                 // ^x! — global store (v11)
    GetV(String),                 // ^x@ — global fetch (v11)
    LocalSet(String),             // x!  — local store (v11)
    LocalGet(String),             // x@  — local fetch (v11)
    IncLocal(String),             // x++ — local increment by 1
    AddLocal(String),             // x+= — local accumulate from stack
    IncGlobal(String),            // shared x++ — atomic increment by 1
    AddGlobal(String),            // shared x+= — atomic accumulate from stack
    Discard,                      // _! — discard one param slot (label/task bindings only)
    Destr(String, Option<String>),// x# / x#key — destructure bind: positional slot or dict/obj key (v15.1)
    DestrDiscard,                 // _# — discard one slot in a destructure run (v15.1)
    Import(Import),
    ManifestImport(Import), // import from a loaded .ufm manifest (exempt from ffi.import gating)
    Export(String),
    Extern(String),
    Use(UseSpec),                // USE "name" — link -lname + load mods/<name>.ufm; schemed sources are packages
    Mod(String),                 // MOD"name" — translation-unit name
    Pub,                         // PUB — export the next label def globally
    Method(String),              // METHOD TypeName: — register the next label as a method
    Task { name: String, count: Option<i64>, body: Vec<(u32, Tok)> }, // inside WEAVE..RUN; leading body tokens are input bindings
    TaskEnd(String),             // internal: end of task body, holds skip label
    Wrun(Option<String>),        // RUN — schedule the collected tasks; optional terminal task
    List(Vec<(u32, Tok)>),      // v13: [ expr ... ] list-literal contents (closed); (line, tok)
    Dict(Vec<(u32, Tok)>),      // v13: { key val ... } dict-literal contents (closed); (line, tok)
    MacroDef(String, Vec<(u32, Tok)>),
    StructDef(String, Vec<(String, String)>), // field name, type name
    Sys(usize),                // arity immediate (default 0)
    Ident(String),             // macro invocation site
    LabelDef(String),
    AnonDef,                   // v16: `:` — anonymous label; body = following tokens until its depth-0 ret
    AnonCall,                  // v16: `':` — anonymous label + immediate apply (spread the list below)
    Entry,                     // ENTRY — marks the program entry point (replaces jmp main)
}

// ---------------- parser ----------------
#[derive(Clone, Debug)]
pub enum Ins {
    PushI(i64),
    PushF(f64),
    PushS(usize),
    PushAddr(String),             // v16: bare label name — pushes the label's fn cell (arity + name carried at runtime)
    Simple(&'static str), // maps to a C helper op_*
    For,
    Call(String),
    CallExt(usize),
    Ret,
    SetV(String),
    GetV(String),
    IncLocal(String),             // x++ — placeholder, expanded by the scope pass
    AddLocal(String),             // x+= — placeholder, expanded by the scope pass
    IncGlobal(String),            // shared x++ — placeholder, expanded by the scope pass
    AddGlobal(String),            // shared x+= — placeholder, expanded by the scope pass
    AtomicAdd(String, bool),      // shared x — atomic add; bool: implicit delta 1 (x++)
    Nop,                         // internal: erased by the scope pass rewrite
    Strict,                      // v14.1: compile-time strictness marker (v → v)
    Loose,                       // v14.1: compile-time strictness eraser (v → v)
    LocalSet(String),             // v11: unresolved local store (name)
    LocalGet(String),             // v11: unresolved local fetch (name)
    LocalSetI(usize),             // v11: resolved local store (slot)
    LocalGetI(usize),             // v11: resolved local fetch (slot)
    Extern(String),
    Send,                        // runtime dispatch through the method table
    Weave(Vec<WeaveMeta>),       // schedule a static task DAG, publish results
    Sys(usize),
    Flush,                       // v13: internal — flush the vstack (ret operand start)
    ListStart,                   // v13: internal — save the ds pointer at a list literal start
    DictStart,                   // v13: internal — save the ds pointer at a dict literal start
    ListLit,                     // v13: build a list from the cells above the literal start
    DictLit,                     // v13: build a dict from the cells above the literal start
    // v10 structured control flow (operands on the stack: code addresses)
    If,                          // cond body_addr ->
    IfElse,                      // cond then_addr else_addr ->
    While,                       // cond_addr body_addr ->
    Break,                       // -> (nearest enclosing loop)
    Cont,                        // -> (nearest enclosing loop)
    Goto(String),                // internal: unconditional goto label (weave task skip)
}

#[derive(Clone, Debug)]
pub struct WeaveMeta {
    pub name: String,      // task name (also the variable it publishes to)
    pub pc: usize,         // entry instruction pc (resolved at parse time, so
                           // duplicate task names across weaves cannot collide)
    pub inputs: Vec<usize>, // task indices in the same weave
    pub count: i64,        // fanout worker count (1 = ordinary task)
}

pub struct Parsed {
    pub ins: Vec<Ins>,
    // source line per instruction (parallel to `ins`; 0 = synthesized).
    // Filled by the parser, rewritten in lockstep by every pass that changes
    // `ins` length (the resolve_locals Nop compaction, merge_tus).
    pub ins_lines: Vec<u32>,
    // label pc -> source line of the label definition (0 = synthesized).
    pub label_lines: HashMap<usize, u32>,
    // label pc -> original (unmangled) label name: the error-site table
    // reports the name the user wrote, not the cross-TU mangled one.
    pub label_names: HashMap<usize, String>,
    // source file per instruction (index into `file_names`; 0 in a single-TU
    // program). Used by the error-site table for multi-TU messages.
    pub ins_files: Vec<usize>,
    // source file stems, one per translation unit (merge order).
    pub file_names: Vec<String>,
    pub labels: HashMap<String, usize>,
    pub imports: Vec<Import>,
    pub exports: Vec<(String, String)>, // C name, label
    pub externs: Vec<String>,
    pub strings: Vec<String>,
    pub vars: Vec<String>,
    pub uses: Vec<String>,
    pub pkg_uses: Vec<(UseSpec, u32)>, // schemed use sites, pre-resolution (driver resolves; embeddings reject)
    pub modname: Option<String>,
    pub pubs: Vec<String>, // label names marked PUB (exported cross-TU)
    pub methods: Vec<(i64, i64, String)>, // (type key, method name hash, label)
    pub init_pcs: Vec<usize>, // instruction offsets of init-TU entry points (auto-thread)
    pub entry_label: Option<String>, // label name of the ENTRY marker (jmp-from-pc0 target)
    pub local_counts: HashMap<usize, usize>, // v11: label pc -> frame size (local variable count)
    pub local_names: HashMap<usize, Vec<String>>, // debug: body pc -> slot names ordered by slot id
    pub label_params: HashMap<usize, Vec<Param>>, // v13: label pc -> declared input bindings
    pub param_pcs: HashMap<usize, usize>, // v13: instruction pc -> guarded-param-pop offset (arity-1-k)
}

// struct layouts: field name -> offset, total size, struct id. Shared across
// TUs and manifests (structs are global; labels/vars/macros are file-local).
pub type StructMap = HashMap<String, (Vec<(String, i64)>, i64, i64)>;
