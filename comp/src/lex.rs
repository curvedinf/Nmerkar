use crate::ast::*;

// ---------------- glyph tables ----------------
// Qwen3-BPE-optimized dense encoding: every codepoint below is a single
// Qwen3-0.6B token, verified via the tokenizer. Ops + v-space + l-space are
// mutually disjoint.
pub const DELIM_BASE: u32 = 0x13100;
pub const TYPE_BASE: u32 = 0x13110;

// Opcode glyphs (192 live ops). Index = OP_NAMES array position.
// All colored emoji (U+1F300+), each exactly one Qwen3 token.
pub const OP_GLYPHS: [u32; 215] = [
    0x1F300, 0x0, 0x0, 0x0, 0x0, 0x0, 0x1F682, 0x1F910,
    0x1F302, 0x1F602, 0x1F683, 0x1F911, 0x1F303, 0x1FA94, 0x1FA91, 0x1FA92,
    0x1F603, 0x1F684, 0x1F912, 0x1F304, 0x1F604, 0x1F685, 0x1FA90, 0x1F913,
    0x1FAAB, 0x0, 0x1F305, 0x1F605, 0x1F686, 0x1F914, 0x0, 0x0,
    0x0, 0x1F306, 0x1F606, 0x1F687, 0x1F915, 0x1F307, 0x1F607, 0x0,
    0x1F689, 0x1F916, 0x1F308, 0x1F608, 0x1F68A, 0x1F917, 0x1F309, 0x1F609,
    0x1F68C, 0x1F918, 0x1F30A, 0x1F60A, 0x1F68D, 0x1F919, 0x1F30B, 0x1F60B,
    0x1F690, 0x0, 0x0, 0x0, 0x0, 0x0, 0x1F91A, 0x1F30C,
    0x1F60C, 0x1F691, 0x1F91B, 0x1F30D, 0x1F60D, 0x1F692, 0x1F91C, 0x1F30E,
    0x1F60E, 0x1F693, 0x1F91D, 0x1F30F, 0x0, 0x0, 0x1F60F, 0x1F694,
    0x1F91E, 0x1F310, 0x1F610, 0x1F695, 0x1F91F, 0x1F311, 0x0, 0x0,
    0x1F611, 0x1F696, 0x1F920, 0x1F313, 0x1F612, 0x1F697, 0x1F921, 0x1F314,
    0x1F613, 0x1F698, 0x1F922, 0x1F315, 0x1F614, 0x1F699, 0x1F923, 0x1F318,
    0x1F615, 0x1F69A, 0x1F924, 0x1F319, 0x1F616, 0x1F69B, 0x1F925, 0x1F31A,
    0x1F617, 0x1F69C, 0x1F926, 0x1F31B, 0x1F618, 0x1F6A2, 0x1F927, 0x1F31C,
    0x1F619, 0x1F6A3, 0x1F928, 0x1F31D, 0x1F61A, 0x1F6A6, 0x1F929, 0x1F31E,
    0x1F61B, 0x1F6A7, 0x1F92A, 0x1F31F, 0x1F61C, 0x1F6A8, 0x1F92B, 0x1F320,
    0x1F61D, 0x1F6A9, 0x1F92C, 0x1F321, 0x1F61E, 0x1F6AA, 0x1F92D, 0x1F324,
    0x1F61F, 0x1F6AB, 0x1F92E, 0x1F325, 0x1F620, 0x1F6AC, 0x1F92F, 0x0,
    0x1F326, 0x1F621, 0x1F6B2, 0x1F930, 0x1F327, 0x1F622, 0x1F6B4, 0x1F931,
    0x1F328, 0x1F623, 0x1F6B5, 0x1F932, 0x1F329, 0x1F624, 0x1F6B6, 0x1F933,
    0x1F32A, 0x1F625, 0x1F6B9, 0x1F934, 0x1F32B, 0x1F626, 0x1F6BC, 0x1F935,
    0x1F32C, 0x1F627, 0x1F6BE, 0x1F936, 0x1F32E, 0x1F628, 0x1F6BF, 0x1F937,
    0x1F32F, 0x1F629, 0x1F6C0, 0x1F938, 0x1F330, 0x1F62A, 0x1F6C1, 0x1F939,
    0x1F331, 0x1F62B, 0x1F6CB, 0x1F93D, 0x1F332, 0x1F62C, 0x1F6CC, 0x1F93E,
    0x1F333, 0x1F62D, 0x1F6CD, 0x1F940, 0x1F334, 0x1F62E, 0x1F6CE,
    0x1F95B, 0x1F95C, 0x1F95D, 0x1F95E,
    0x1F958, 0x1F959, 0x1F95A,
    0x1F500,
];

// v-space: base-64 digits for variable/label name atoms (colored emoji).
pub const V_SPACE: [u32; 64] = [
    0x1F941, 0x1F335, 0x1F62F, 0x1F6CF, 0x1F942, 0x1F336, 0x1F630, 0x1F6D0,
    0x1F943, 0x1F337, 0x1F631, 0x1F6D1, 0x1F947, 0x1F338, 0x1F632, 0x1F6D2,
    0x1F948, 0x1F339, 0x1F633, 0x1F6E0, 0x1F949, 0x1F33A, 0x1F634, 0x1F6E1,
    0x1F94A, 0x1F33B, 0x1F635, 0x1F6E3, 0x1F94B, 0x1F33C, 0x1F636, 0x1F6E4,
    0x1F950, 0x1F33D, 0x1F637, 0x1F6E9, 0x1F951, 0x1F33E, 0x1F638, 0x1F6EB,
    0x1F952, 0x1F33F, 0x1F639, 0x1F6EC, 0x1F953, 0x1F340, 0x1F63A, 0x1F6F3,
    0x1F954, 0x1F341, 0x1F63B, 0x1F6F4, 0x1F955, 0x1F342, 0x1F63C, 0x1F6F5,
    0x1F956, 0x1F343, 0x1F63D, 0x1F6F6, 0x1F957, 0x1F344, 0x1F63F, 0x1F6F8,
];

// l-space: base-64 digits for number literals (Enclosed Alphanumeric Supp).
pub const L_SPACE: [u32; 64] = [
    0x1F130, 0x1F132, 0x1F136, 0x1F137, 0x1F138, 0x1F13D, 0x1F142, 0x1F145,
    0x1F150, 0x1F153, 0x1F154, 0x1F156, 0x1F158, 0x1F15A, 0x1F15B, 0x1F15D,
    0x1F162, 0x1F166, 0x1F170, 0x1F171, 0x1F174, 0x1F176, 0x1F17B, 0x1F17C,
    0x1F17D, 0x1F17E, 0x1F17F, 0x1F182, 0x1F183, 0x1F186, 0x1F18E, 0x1F192,
    0x1F193, 0x1F194, 0x1F195, 0x1F196, 0x1F197, 0x1F198, 0x1F199, 0x1F19A,
    0x1F1E6, 0x1F1E7, 0x1F1E8, 0x1F1E9, 0x1F1EA, 0x1F1EB, 0x1F1EC, 0x1F1ED,
    0x1F1EE, 0x1F1EF, 0x1F1F0, 0x1F1F1, 0x1F1F2, 0x1F1F3, 0x1F1F4, 0x1F1F5,
    0x1F1F6, 0x1F1F7, 0x1F1F8, 0x1F1F9, 0x1F1FA, 0x1F1FB, 0x1F1FC, 0x1F1FD,
];

// --- reverse lookups (cp -> index) ---
pub fn v_index(cp: u32) -> Option<u32> { V_SPACE.iter().position(|&c| c == cp).map(|i| i as u32) }
pub fn l_index(cp: u32) -> Option<u32> { L_SPACE.iter().position(|&c| c == cp).map(|i| i as u32) }
pub fn is_v(cp: u32) -> bool { v_index(cp).is_some() }
pub fn is_l(cp: u32) -> bool { l_index(cp).is_some() }
pub fn v_glyph(i: u32) -> char { char::from_u32(V_SPACE[i as usize]).unwrap() }
pub fn l_glyph(i: u32) -> char { char::from_u32(L_SPACE[i as usize]).unwrap() }

// glyph_of: OP_NAMES array index -> glyph char. Retired indices (~NN) are
// never looked up (compile error before reaching here).
pub fn glyph_of(idx: usize) -> char {
    char::from_u32(OP_GLYPHS[idx]).unwrap()
}
// opcode_index: glyph char -> OP_NAMES array index (reverse lookup).
pub fn opcode_index(c: char) -> Option<usize> {
    let cp = c as u32;
    OP_GLYPHS.iter().position(|&g| g == cp)
}
// Kept for emit.rs compatibility (dense glyph for an opcode by name).
pub fn op_glyph_of(name: &str) -> char {
    glyph_of(op_index(name).expect("op_index"))
}
// DEAD (v10.1): old CUSTOM_OPS table retained as empty placeholder so the
// const name still exists for any external references.
pub const CUSTOM_OPS: [(u32, usize); 0] = [];

// U+13110..U+13117 = int float ptr byte void handle str bool.
// Ids match type_id(): int 0, float 1, ptr 2, byte 3; handle/str are ptr
// aliases, bool a byte alias, void is 4 (SIZEOF void = 8, not useful).
pub fn glyph_type_id(c: char) -> Option<i64> {
    let cp = c as u32;
    if cp >= TYPE_BASE && cp <= TYPE_BASE + 7 {
        Some(match cp - TYPE_BASE {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 3,
            4 => 4,
            5 => 2,
            6 => 2,
            _ => 3,
        })
    } else {
        None
    }
}

// 196 opcode slots (0..=195). Retired indices use "~NN" placeholders: they
// have no glyph, no text mnemonic, and no runtime helper — any use is a
// compile error (unassigned glyph / unknown identifier).
pub const OP_NAMES: [&str; 215] = [
    "LIT", "~1", "~2", "~3", "~4", "~5", "ADD", "SUB", "MUL", "AND", "SHR", "INC", "DEC",
    "POW", "SQRT", "LTE", "FOR", "CALL", "RET", "OBJ", "GET", "SET", "GTE", "ARR", "SHUTDOWN", "~25",
    "CLONE", "CAST", "MACRO", "TENSOR", "~30", "~31", "~32", "SETV", "GETV", "STR", "CAT", "FMT",
    "BUF", "~39", "BUFCOPY", "ADDR", "LOADX", "STOREX", "SIZEOF", "OFFSET", "STRUCT", "MALLOC",
    "FREE", "SYS", "GC", "IMPORT", "EXPORT", "EXTERN", "PRINT", "SCAN",
    // v9 kept
    "DICT", "~57", "~58", "~59", "~60", "~61", "LIST", "PUSH", "POP", "CHAN",
    "ENQ", "DEQ", "CLOSE", "ATOM", "AGET", "ASET", "AADD", "CAS", "TYPEOF", "LEN", "~76",
    "~77", "USE", "MOD", "PUB", "WEAVE", "TASK", "ENDT", "WRUN",
    // shell + strings
    "SH", "CAP", "CAPS", "SHP", "EXEC",
    "MATCH", "REPLACE", "RSPLIT", "GLOB", "SPLIT", "JOIN", "SLICE", "FIND", "REPL",
    "TRIM", "UP", "DOWN", "STARTS", "ENDS",
    // v10: arithmetic & logic
    "DIV", "REM", "EQ", "LT", "GT", "NOT", "OR", "XOR", "SHL", "BNOT",
    // v10: structured control flow
    "IF", "IFELSE", "WHILE", "BREAK", "CONT",
    // v10: container protocol & sequences
    "GETQ", "HAS", "ORELSE", "KEYS", "RANGE", "SORT", "FILTER", "SOME", "EVERY",
    // v10: vector ops
    "VADD", "VSUB", "VMUL", "VDIV", "VEADD", "VESUB", "VEMUL", "VEDIV", "VEMAX",
    "VEQ", "VLT", "VGT", "VGE", "VLE", "VAND", "VOR", "VNOT", "VCOUNT", "VGATHER",
    "VSUM", "VMEAN", "VMIN", "VMAX", "~151", "DEL", "VMAP", "VFOLD",
    // v10: time, bloom, script I/O
    "NOW", "TIME", "TIMEF", "BLOOM", "BADD", "BTEST", "SLURP", "SPIT", "ARGV",
    // v10: additional data ops
    "GROUP", "AGG", "UNIQUE", "FLAT", "CHUNK", "VARGSORT", "VSEARCHSORTED", "VWHERE",
    // v10: large-data shortcuts
    "MMAP", "FEACH", "FFOLD", "FSPLIT", "FGET", "FATOI", "FATOF", "FSGET", "FBYTE", "FMATCH", "BFS", "DFS", "WFIND",
    "VGET", "VSET",
    "ADDTO", "FADDTO", "FINC",
    // v10: JSON, iterators, sinks, containment, threads
    "JSON", "UNJSON", "ITER", "NEXT", "COLLECT", "IMAP", "IFILTER", "FEMIT",
    "TRY", "RETRY", "SPAWN", "VEMIN",
    "ATOI", "ATOF", "ITOA", "FTOA",
    "ENTRY",
    "HASARGS", "ARGI", "SORTKEYS", "TOPN",
    "RANGEFOLD",
    "SEQ", "SNE",
    "TRANSPOSE",
];

pub fn op_index(name: &str) -> Option<usize> {
    OP_NAMES.iter().position(|n| *n == name)
}

/// Short usage description for each live opcode, keyed by name.
pub fn op_usage(name: &str) -> &'static str {
    match name {
        "LIT" => "→ v | immediate follows",
        "~1" | "~2" | "~3" | "~4" | "~5" => "retired in v12",
        "ADD" => "a b → a+b",
        "SUB" => "a b → a-b",
        "MUL" => "a b → a*b",
        "AND" => "a b → a&b",
        "SHR" => "a → a>>1",
        "INC" => "a → a+1",
        "DEC" => "a → a-1",
        "POW" => "a b → a^b | C/Python pow",
        "SQRT" => "a → √a",
        "LTE" => "a b → 0/1 | a<=b",
        "GTE" => "a b → 0/1 | a>=b",
        "SHUTDOWN" => "→ | graceful weave shutdown (drains and exits)",
        "FOR" => "count body_addr → | pushes index k per iter",
        "CALL" => "(label operand) | call subroutine",
        "RET" => "→ | return from call",
        "OBJ" => "→ h | type immediate (struct id)",
        "GET" => "h k → v | polymorphic container get",
        "SET" => "h k v → | polymorphic container set",
        "ARR" => "type len → h | type: int=0 float=1 ptr=2 byte=3",
        "CLONE" => "h → h' | deep copy",
        "CAST" => "h type → h | checked downcast",
        "MACRO" => "(directive) | macro name { body }",
        "TENSOR" => "type len → h",
        "SETV" => "value → | x!/^x! store local/global",
        "GETV" => "→ value | x@/^x@ fetch local/global",
        "STR" => "→ h | push string",
        "CAT" => "a b → h | concat (str/arr/list)",
        "FMT" => "args… fmt → h | format string",
        "BUF" => "size → ptr | raw untracked buffer",
        "BUFCOPY" => "dst src n → | memory copy",
        "ADDR" => "→ code address | 'label",
        "LOADX" => "addr → value | raw memory read",
        "STOREX" => "value addr → | raw memory write",
        "SIZEOF" => "type → n",
        "OFFSET" => "→ n | compile-time Struct.field",
        "STRUCT" => "(directive) | struct Name { field:type … }",
        "MALLOC" => "size → ptr | raw untracked",
        "FREE" => "ptr → | raw only, never GC handles",
        "SYS" => "args… num → ret | syscall by number",
        "GC" => "→ | force full mark-sweep",
        "IMPORT" => "(directive) | import c\"fn\"(types)->ret",
        "EXPORT" => "(directive) | export \"name\" before label",
        "EXTERN" => "→ address | extern \"symbol\"",
        "PRINT" => "args… fmt → n | printf; n = chars written",
        "SCAN" => "fmt → values… count | fscanf",
        "DICT" => "→ h | empty hash map",
        "LIST" => "→ h | empty growable list",
        "PUSH" => "h v → h' | append to list",
        "POP" => "h → v | pop from list",
        "CHAN" => "cap → h | bounded MPSC ring",
        "ENQ" => "h v → | enqueue (blocks if full)",
        "DEQ" => "h → v | dequeue (blocks if empty)",
        "CLOSE" => "h → | close channel",
        "ATOM" => "v → h | atomic i64 cell",
        "AGET" => "h → v | atomic load",
        "ASET" => "h v → | atomic store",
        "AADD" => "h n → old | atomic fetch-add",
        "CAS" => "h old new → 0/1 | compare-and-swap",
        "TYPEOF" => "h → tag | runtime type tag",
        "LEN" => "h → n | generalized length",
        "USE" => "(directive) | use \"name\" link + manifest",
        "MOD" => "(directive) | mod \"name\" TU name",
        "PUB" => "(directive) | export next label globally",
        "WEAVE" => "(directive) | begin task scope",
        "TASK" => "(directive) | begin task body",
        "ENDT" => "(directive) | end task body",
        "WRUN" => "(directive) | schedule DAG, wait, publish",
        "SH" => "cmd → stdout stderr status | /bin/sh -c",
        "SHP" => "cmd → chan | stream stdout line-by-line",
        "EXEC" => "list → status | no shell, argv list",
        "CAP" => "name → 0/1 | sandbox capability query (fs.read, proc, compute, ...)",
        "CAPS" => "→ dict | full capability report (policy, caps, workspace, modules, device)",
        "MATCH" => "str pat → list found | regex, group strings",
        "REPLACE" => "str pat repl → str' | regex replace all",
        "RSPLIT" => "str pat → list | regex split",
        "GLOB" => "str pat → 0/1 | fnmatch-style",
        "SPLIT" => "str sep → list | literal separator",
        "JOIN" => "list sep → str",
        "SLICE" => "seq a b → seq' | Python slice",
        "FIND" => "str sub → idx | -1 on miss",
        "REPL" => "str old new → str' | literal replace all",
        "TRIM" => "str → str' | strip whitespace",
        "UP" => "str → str' | ASCII uppercase",
        "DOWN" => "str → str' | ASCII lowercase",
        "STARTS" => "str affix → 0/1",
        "ENDS" => "str affix → 0/1",
        "DIV" => "a b → a/b | int truncates; b=0 dies",
        "REM" => "a b → a%b | C remainder; b=0 dies",
        "EQ" => "a b → 0/1 | numeric or string",
        "LT" => "a b → 0/1 | numeric or lexicographic",
        "GT" => "a b → 0/1",
        "NOT" => "a → 0/1 | 1 if a==0",
        "OR" => "a b → a|b | ints only",
        "XOR" => "a b → a^b | ints only",
        "SHL" => "a b → a<<b | ints only",
        "BNOT" => "a → ~a | ints only",
        "IF" => "cond body_addr → | call body if cond nonzero",
        "IFELSE" => "cond then_addr else_addr →",
        "WHILE" => "cond_addr body_addr → | loop until cond=0",
        "BREAK" => "→ | exit nearest loop",
        "CONT" => "→ | next iteration of nearest loop",
        "GETQ" => "h k → v_or_0 | never dies on absence",
        "HAS" => "h k → 0/1 | membership",
        "ORELSE" => "a b → c | a if truthy else b",
        "KEYS" => "h → list | dict keys or obj fields",
        "RANGE" => "start stop → list | ints [start, stop)",
        "SORT" => "seq → seq' | stable sort",
        "FILTER" => "list pred_addr → list' | keep where pred truthy",
        "SOME" => "list pred_addr → 0/1 | any element passes",
        "EVERY" => "list pred_addr → 0/1 | all elements pass",
        "VADD" => "arr scalar → arr'",
        "VSUB" => "arr scalar → arr'",
        "VMUL" => "arr scalar → arr'",
        "VDIV" => "arr scalar → arr' | scalar 0 dies",
        "VEADD" => "arr arr → arr' | elementwise add",
        "VESUB" => "arr arr → arr'",
        "VEMUL" => "arr arr → arr'",
        "VEDIV" => "arr arr → arr' | any 0 divisor dies",
        "VEMAX" => "arr arr → arr' | elementwise max",
        "VEQ" => "arr scalar → bitmap",
        "VLT" => "arr scalar → bitmap",
        "VGT" => "arr scalar → bitmap",
        "VGE" => "arr scalar → bitmap",
        "VLE" => "arr scalar → bitmap",
        "VAND" => "bm bm → bm' | bitmap and",
        "VOR" => "bm bm → bm' | bitmap or",
        "VNOT" => "bm → bm' | bitmap not",
        "VCOUNT" => "bm → n | popcount",
        "VGATHER" => "arr bm → arr' | keep set-bit elements",
        "VSUM" => "arr → scalar | empty → 0",
        "VMEAN" => "arr → f64 | empty dies",
        "VMIN" => "arr → scalar | empty dies",
        "VMAX" => "arr → scalar | empty dies",
        "DEL" => "h k → | remove (dict: tombstone)",
        "VMAP" => "arr fn_addr → arr' | elementwise fn",
        "VFOLD" => "arr init fn_addr → acc | reduction",
        "NOW" => "→ t | CLOCK_REALTIME nanos",
        "TIME" => "str fmt → t | \"unix\" or strptime",
        "TIMEF" => "t fmt → str | \"unix\" or strftime",
        "BLOOM" => "n → h | bloom filter",
        "BADD" => "h v → | add to bloom",
        "BTEST" => "h v → 0/1 | 1=maybe, 0=definitely not",
        "SLURP" => "path → str | whole file",
        "SPIT" => "path str → | create/truncate",
        "ARGV" => "→ list | program argv as strings",
        "GROUP" => "list fn_addr → dict | group by key fn",
        "AGG" => "dict fn_addr → dict' | aggregate groups",
        "UNIQUE" => "list → list' | dedup, first-occurrence order",
        "FLAT" => "list → list' | flatten one level",
        "CHUNK" => "seq size → list | split into pieces",
        "VARGSORT" => "arr → idx_arr | indices that sort",
        "VSEARCHSORTED" => "sorted_arr val → idx | binary search",
        "VWHERE" => "arr arr bm → arr' | blend by bitmap",
        "MMAP" => "path → str | read-only zero-copy",
        "FEACH" => "path fn_addr → | call fn per line, early stop",
        "FFOLD" => "path init fn_addr → acc | streaming reduce lines",
        "FSPLIT" => "path sep init fn_addr → acc | streaming split",
        "FGET" => "field_idx → str | zero-copy field view",
        "FATOI" => "field_idx → int | parse field, no alloc",
        "FATOF" => "field_idx → float | parse field, no alloc",
        "FSGET" => "field_idx off len → str | field substring",
        "FBYTE" => "field_idx off → int | single byte from field",
        "FMATCH" => "path pat → chan | stream regex-matching lines",
        "BFS" => "start fn_addr → list | breadth-first visit",
        "DFS" => "start fn_addr → list | depth-first pre-order",
        "WFIND" => "start fn_addr pred_addr → v_or_0 | BFS early exit",
        "VGET" => "h idx → v | direct typed array read",
        "VSET" => "h idx v → | direct typed array write",
        "ADDTO" => "dict key amount → | dict[key] += amount",
        "FADDTO" => "dict field_idx amount → | dict[field] += amount",
        "FINC" => "dict field_idx → | dict[field] += 1",
        "JSON" => "str → v | parse JSON",
        "UNJSON" => "v → str | serialize JSON",
        "ITER" => "h → it | create cursor",
        "NEXT" => "it → v more | more=0 exhausted",
        "COLLECT" => "it → list | drain iterator",
        "IMAP" => "it fn_addr → it' | lazy map",
        "IFILTER" => "it pred_addr → it' | lazy filter",
        "FEMIT" => "path it → n | stream iterable to file",
        "TRY" => "body_addr → result ok | catch die",
        "RETRY" => "n body_addr → result ok | try n+1 times",
        "SPAWN" => "body_addr → chan | detached thread",
        "VEMIN" => "arr arr → arr' | elementwise min",
        "ATOI" => "str → int | strtoll base 10",
        "ATOF" => "str → float | strtod",
        "ITOA" => "int → str",
        "FTOA" => "float → str",
        "ENTRY" => "→ | marks program entry point",
        "HASARGS" => "→ 0/1 | true if argv has >1 element",
        "ARGI" => "idx → int | argv[idx] parsed as int",
        "SORTKEYS" => "dict → key_list | keys + sort fused",
        "TOPN" => "dict n → list | top-n [key value] pairs",
        "SEQ" => "a b → 0/1 | strict equality (===)",
        "TRANSPOSE" => "mat → mat2 | transpose rows/cols (2-D tensor)",
        "SNE" => "a b → 0/1 | strict inequality (!==)",
        _ => "",
    }
}

pub fn type_id(kw: &str) -> Option<i64> {
    match kw {
        "int" => Some(0),
        "float" => Some(1),
        "ptr" | "handle" => Some(2),
        "byte" => Some(3),
        _ => None,
    }
}

// ---------------- lexer ----------------
pub struct Lexer {
    chars: Vec<char>,
    pos: usize,
    // v13: true when the previous token was RET — a number directly after
    // `ret` is the return operand, never a weave worker count
    prev_ret: bool,
}

impl Lexer {
    fn new(src: &str) -> Self {
        Lexer { chars: src.chars().collect(), pos: 0, prev_ret: false }
    }
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }
    fn skip_ws(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.pos += 1;
                }
                Some(';') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }
    fn err(&self, msg: &str) -> ! {
        panic!("lex error at char {}: {}", self.pos, msg)
    }
    fn lex_ident(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                s.push(c);
                self.pos += 1;
            } else {
                break;
            }
        }
        s
    }
    fn lex_string(&mut self) -> String {
        // assumes current char is '"'
        self.pos += 1;
        let mut s = String::new();
        loop {
            match self.next() {
                None => self.err("unterminated string"),
                Some('"') => break,
                Some('\\') => match self.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('r') => s.push('\r'),
                    Some('0') => s.push('\0'),
                    Some('\\') => s.push('\\'),
                    Some('"') => s.push('"'),
                    Some(c) => s.push(c),
                    None => self.err("unterminated escape"),
                },
                Some(c) => s.push(c),
            }
        }
        s
    }
    // fold a run of v-space slot glyphs into one name "v0_1_..."
    fn fold_slots(&mut self) -> String {
        let mut name = String::from("v");
        let mut first = true;
        while let Some(c) = self.peek() {
            let cp = c as u32;
            if let Some(i) = v_index(cp) {
                if !first {
                    name.push('_');
                }
                name.push_str(&i.to_string());
                first = false;
                self.pos += 1;
            } else {
                break;
            }
        }
        name
    }
    // read a run of l-space digit atoms, big-endian base-64.
    // Returns the digit values; empty if the next char is not an l-glyph.
    fn lrun(&mut self) -> Vec<u32> {
        let mut ds = Vec::new();
        while let Some(c) = self.peek() {
            let cp = c as u32;
            if let Some(d) = l_index(cp) {
                ds.push(d);
                self.pos += 1;
            } else {
                break;
            }
        }
        ds
    }
    fn lrun_u64(&mut self) -> u64 {
        let ds = self.lrun();
        let mut v: u64 = 0;
        for d in ds {
            v = v
                .checked_mul(64)
                .and_then(|x| x.checked_add(d as u64))
                .unwrap_or_else(|| self.err("base-64 literal overflow (> u64)"));
        }
        v
    }
    // full l-space number grammar (SPEC.md):
    //   number := ['-'] lrun ['.' lrun] ['e' ['-'] lrun]
    // bare lrun = big-endian base-64 unsigned int; '.' = base-64 fixed point
    // (d1/64 + d2/4096 + ...); 'e' = decimal scientific (x10^exp); '-' = sign
    // (only reachable under the lookback rule; neg=true comes from lex_into).
    fn lex_lnumber(&mut self, neg: bool) -> Tok {
        let v = self.lrun_u64();
        let mut is_float = false;
        let mut frac = 0.0f64;
        if self.peek() == Some('.') {
            self.pos += 1;
            let ds = self.lrun();
            if ds.is_empty() {
                self.err("'.' in a number must be followed by an l-run");
            }
            is_float = true;
            let mut scale = 64.0f64;
            for d in ds {
                frac += d as f64 / scale;
                scale *= 64.0;
                if !scale.is_finite() {
                    break; // further digits are below f64 resolution
                }
            }
        }
        let mut exp: i64 = 0;
        if self.peek() == Some('e') {
            self.pos += 1;
            let eneg = if self.peek() == Some('-') {
                self.pos += 1;
                true
            } else {
                false
            };
            let ds = self.lrun();
            if ds.is_empty() {
                self.err("'e' in a number must be followed by an l-run exponent");
            }
            let mut e: u64 = 0;
            for d in ds {
                e = e.saturating_mul(64).saturating_add(d as u64);
            }
            exp = if eneg { -(e.min(400) as i64) } else { e.min(400) as i64 };
            is_float = true;
        }
        if !is_float {
            let i = v as i64;
            return Tok::PushI(if neg { i.wrapping_neg() } else { i });
        }
        let mut f = (v as f64 + frac) * 10f64.powi(exp.clamp(-400, 400) as i32);
        if neg {
            f = -f;
        }
        Tok::PushF(f)
    }
    // lex the whole source
    fn lex(&mut self) -> Vec<Tok> {
        let mut out = Vec::new();
        self.lex_into(&mut out, 0);
        out
    }
    // lex until EOF (stop=0), until matching '}' (1), until ']' (3),
    // until the TASK or WRUN glyph (5, not consumed)
    fn lex_into(&mut self, out: &mut Vec<Tok>, stop: u8) {
        loop {
            self.skip_ws();
            let c = match self.peek() {
                None => {
                    if stop != 0 {
                        self.err("unterminated block");
                    }
                    return;
                }
                Some(c) => c,
            };
            let cp = c as u32;
            if cp >= DELIM_BASE && cp <= DELIM_BASE + 8 {
                self.pos += 1;
                continue; // delimiters are invisible
            }
            if stop == 1 && c == '}' {
                self.pos += 1;
                return;
            }
            if stop == 3 && c == ']' {
                self.pos += 1;
                return;
            }
            if stop == 5 && (opcode_index(c) == Some(82) || opcode_index(c) == Some(84)) {
                // TASK/WRUN glyphs bound a weave task body; leave them unconsumed
                return;
            }
            if stop == 5 && is_l(cp) && !self.prev_ret {
                // an l-run directly before the TASK glyph is the next task's
                // fanout worker count — leave it unconsumed
                let mut j = self.pos;
                while let Some(c2) = self.chars.get(j) {
                    let c2p = *c2 as u32;
                    if is_l(c2p) {
                        j += 1;
                    } else {
                        break;
                    }
                }
                while self.chars.get(j).map_or(false, |c2| c2.is_whitespace()) {
                    j += 1;
                }
                if self.chars.get(j).map_or(false, |c2| opcode_index(*c2) == Some(82)) {
                    return;
                }
            }
            // ---- l-space: self-evaluating base-64 number ----
            if is_l(cp) {
                self.prev_ret = false;
                out.push(self.lex_lnumber(false));
                continue;
            }
            // ---- ^ prefix: global variable (^x! / ^x@) ----
            if c == '^' {
                self.prev_ret = false;
                self.pos += 1;
                self.skip_ws();
                let c2 = match self.peek() {
                    Some(c) => c,
                    None => self.err("^ needs a variable name"),
                };
                let cp2 = c2 as u32;
                // v-run name after ^
                if is_v(cp2) {
                    let name = self.fold_slots();
                    self.skip_ws();
                    let g = self.peek();
                    if g == Some('!') || g.map_or(false, |c| opcode_index(c) == Some(33)) {
                        self.pos += 1;
                        out.push(Tok::SetV(name));
                    } else if g == Some('@') || g.map_or(false, |c| opcode_index(c) == Some(34)) {
                        self.pos += 1;
                        out.push(Tok::GetV(name));
                    } else {
                        self.err("^ needs ! or @ after the name");
                    }
                    continue;
                }
                // ASCII ident after ^ (dense mode with ASCII names)
                if c2.is_ascii_alphabetic() || c2 == '_' {
                    let name = self.lex_ident();
                    if is_bad_ident(&name) {
                        self.err(&format!("variable '{}' — identifiers may not start with '_'", name));
                    }
                    self.skip_ws();
                    let g = self.peek();
                    if g == Some('!') || g.map_or(false, |c| opcode_index(c) == Some(33)) {
                        self.pos += 1;
                        out.push(Tok::SetV(name));
                    } else if g == Some('@') || g.map_or(false, |c| opcode_index(c) == Some(34)) {
                        self.pos += 1;
                        out.push(Tok::GetV(name));
                    } else {
                        self.err("^ needs ! or @ after the name");
                    }
                    continue;
                }
                self.err("^ needs a variable name");
            }
            // ---- v-space: variable use or label definition ----
            if is_v(cp) {
                self.prev_ret = false;
                // fold a run of v-glyphs into one name
                let name = self.fold_slots();
                self.skip_ws();
                let g = self.peek();
                if g == Some('!') || g.map_or(false, |c| opcode_index(c) == Some(33)) {
                    self.pos += 1;
                    out.push(Tok::LocalSet(name));
                } else if g == Some('@') || g.map_or(false, |c| opcode_index(c) == Some(34)) {
                    self.pos += 1;
                    out.push(Tok::LocalGet(name));
                } else {
                    out.push(Tok::LabelDef(name));
                }
                continue;
            }
            // ---- retired JMP/JZ/JE glyphs (removed in v10.1) ----
            match cp {
                0x130BB => self.err("jmp removed — use entry/if/while/for"),
                0x130BC => self.err("jz removed — use if/ifelse/while"),
                0x130BD => self.err("je removed — use if/ifelse"),
                _ => {}
            }
            // ---- ASCII tokens ----
            match c {
                '[' => {
                    self.pos += 1;
                    let mut body = Vec::new();
                    self.lex_into(&mut body, 3);
                    out.push(Tok::List(body));
                    continue;
                }
                '{' => {
                    self.pos += 1;
                    let mut body = Vec::new();
                    self.lex_into(&mut body, 1);
                    out.push(Tok::Dict(body));
                    continue;
                }
                '\'' => {
                    // ADDR: '<label> (v-run or ASCII)
                    self.prev_ret = false;
                    self.pos += 1;
                    self.skip_ws();
                    let label = self.jump_label("ADDR");
                    out.push(Tok::Jump("ADDR", label));
                    continue;
                }
                '+' => self.err("'+' removed in v13 — use `add`"),
                '*' => self.err("'*' removed in v13 — use `mul`"),
                '&' => self.err("'&' removed in v13 — use `and`"),
                '=' => {
                    self.err("= (je) removed — use if/ifelse");
                }
                '"' => {
                    // bare string self-evaluates (PushS)
                    self.prev_ret = false;
                    out.push(Tok::PushS(self.lex_string()));
                    continue;
                }
                '-' => {
                    // v13: '-' is only part of a number literal — always a sign
                    self.pos += 1;
                    self.skip_ws();
                    match self.peek() {
                        Some(g) if is_l(g as u32) => {
                            out.push(self.lex_lnumber(true));
                        }
                        _ => self.err("stray '-' (v13 has no SUB symbol — use `sub`; a sign needs an l-run after it)"),
                    }
                    continue;
                }
                '@' => {
                    // dense-mode sentinel idents (@objsize:Name, @cast:Name,
                    // @sizeof:Name, @offset:Name.field) — the `_obj`-style
                    // immediate ops read a struct name and the emitter writes
                    // it back as a @-sentinel
                    self.pos += 1;
                    let mut word = String::new();
                    while let Some(c2) = self.peek() {
                        if c2.is_ascii_alphabetic() || c2 == ':' || c2 == '.' {
                            word.push(c2);
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    if word.starts_with("objsize:") || word.starts_with("cast:")
                        || word.starts_with("sizeof:") || word.starts_with("offset:")
                    {
                        out.push(Tok::Ident(format!("@{}", word)));
                        continue;
                    }
                    self.err("bare '@' must directly follow a v-run or start a sentinel");
                }
                '!' => self.err("bare '!' must directly follow a v-run"),
                _ => {}
            }
            // ---- bare type glyph: pushes the type id ----
            if let Some(id) = glyph_type_id(c) {
                self.prev_ret = false;
                self.pos += 1;
                out.push(Tok::PushI(id));
                continue;
            }
            // ---- opcode glyphs (custom table + legacy aliases) ----
            if let Some(idx) = opcode_index(c) {
                self.pos += 1;
                self.prev_ret = idx == 18; // RET glyph
                self.lex_op(idx, out);
                continue;
            }
            if c.is_ascii_alphabetic() || c == '_' {
                self.prev_ret = false;
                let name = self.lex_ident();
                if is_bad_ident(&name) {
                    self.err(&format!("identifier '{}' — identifiers may not start with '_'", name));
                }
                if name == "_" && (self.peek() == Some('!') || self.peek().map_or(false, |c| opcode_index(c) == Some(33))) {
                    self.pos += 1;
                    out.push(Tok::Discard);
                    continue;
                }
                if self.peek() == Some(':') {
                    self.pos += 1;
                    out.push(Tok::LabelDef(name));
                } else {
                    out.push(Tok::Ident(name));
                }
                continue;
            }
            if opcode_index(c).is_some() || is_v(cp) || is_l(cp) {
                self.err(&format!("internal: unhandled glyph U+{:04X}", cp));
            }
            self.err(&format!("unexpected character {:?}", c));
        }
    }

    // label operand for JMP/JZ/JE/CALL/ADDR/'='/'\'' sites: v-run or ASCII ident
    fn jump_label(&mut self, op: &str) -> String {
        if let Some(c) = self.peek() {
            let cp = c as u32;
            if is_v(cp) {
                return self.fold_slots();
            }
        }
        let label = self.lex_ident();
        if label.is_empty() {
            self.err(&format!("{} needs a label", op));
        }
        if is_bad_ident(&label) {
            self.err(&format!("{} label '{}' — identifiers may not start with '_'", op, label));
        }
        label
    }

    fn lex_op(&mut self, idx: usize, out: &mut Vec<Tok>) {
        self.lex_op_inner(idx, out);
    }

    fn lex_op_inner(&mut self, idx: usize, out: &mut Vec<Tok>) {
        let name = OP_NAMES[idx];
        match name {
            "LIT" => {
                self.skip_ws();
                let c = self.peek().unwrap_or_else(|| self.err("LIT needs a literal"));
                let cp = c as u32;
                if let Some(id) = glyph_type_id(c) {
                    // type glyph after LIT: push the type id (like `LIT int`)
                    self.pos += 1;
                    out.push(Tok::PushI(id));
                } else if is_l(cp) {
                    // l-run number after LIT (same grammar as a bare l-run)
                    out.push(self.lex_lnumber(false));
                } else if c.is_ascii_alphabetic() {
                    let kw = self.lex_ident();
                    match type_id(&kw) {
                        Some(id) => out.push(Tok::PushI(id)),
                        None => self.err(&format!("unknown type keyword {}", kw)),
                    }
                } else {
                    let mut s = String::new();
                    while let Some(c) = self.peek() {
                        if c.is_ascii_digit()
                            || matches!(c, 'x' | 'X' | 'a'..='f' | 'A'..='F' | '.' | '+' | '-' | '_')
                        {
                            s.push(c);
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    let s = s.replace('_', "");
                    if s.starts_with("0x") || s.starts_with("0X") {
                        out.push(Tok::PushI(
                            i64::from_str_radix(&s[2..], 16).unwrap_or_else(|_| self.err("bad hex literal")),
                        ));
                    } else if s.contains('.') || s.contains('e') || s.contains('E') {
                        out.push(Tok::PushF(s.parse().unwrap_or_else(|_| self.err("bad float literal"))));
                    } else {
                        out.push(Tok::PushI(s.parse().unwrap_or_else(|_| self.err("bad int literal"))));
                    }
                }
            }
            "STR" => {
                self.skip_ws();
                if self.peek() != Some('"') {
                    self.err("STR must be followed by a string literal");
                }
                out.push(Tok::PushS(self.lex_string()));
            }
            "JMP" | "JZ" | "JE" | "CALL" | "ADDR" => {
                self.skip_ws();
                let label = self.jump_label(name);
                if name == "JMP" { self.err("jmp removed — use entry/if/while/for"); }
                if name == "JZ" { self.err("jz removed — use if/ifelse/while"); }
                if name == "JE" { self.err("je removed — use if/ifelse"); }
                out.push(Tok::Jump(OP_NAMES[idx], label));
            }
            "ENTRY" => {
                out.push(Tok::Entry);
            }
            "IMPORT" => {
                self.skip_ws();
                if self.next() != Some('c') {
                    self.err("IMPORT must be followed by c\"name\"(sig)->ret");
                }
                self.skip_ws();
                if self.peek() != Some('"') {
                    self.err("IMPORT: expected string");
                }
                let fname = self.lex_string();
                self.skip_ws();
                if self.next() != Some('(') {
                    self.err("IMPORT: expected '('");
                }
                let mut params = Vec::new();
                let mut cur = String::new();
                loop {
                    match self.next() {
                        Some(')') => {
                            if !cur.trim().is_empty() {
                                params.push(cur.trim().to_string());
                            }
                            break;
                        }
                        Some(',') => {
                            params.push(cur.trim().to_string());
                            cur.clear();
                        }
                        Some(c) => cur.push(c),
                        None => self.err("IMPORT: unterminated signature"),
                    }
                }
                self.skip_ws();
                // accept "->" or bare ">" (paper typo tolerated)
                if self.peek() == Some('-') {
                    self.pos += 1;
                }
                if self.next() != Some('>') {
                    self.err("IMPORT: expected '->'");
                }
                self.skip_ws();
                let ret = self.lex_ident();
                out.push(Tok::Import(Import { name: fname, params, ret }));
            }
            "EXPORT" => {
                self.skip_ws();
                if self.peek() != Some('"') {
                    self.err("EXPORT must be followed by \"name\"");
                }
                out.push(Tok::Export(self.lex_string()));
            }
            "USE" => {
                self.skip_ws();
                if self.peek() != Some('"') {
                    self.err("USE must be followed by \"name\"");
                }
                out.push(Tok::Use(self.lex_string()));
            }
            "MOD" => {
                self.skip_ws();
                if self.peek() != Some('"') {
                    self.err("MOD must be followed by \"name\"");
                }
                out.push(Tok::Mod(self.lex_string()));
            }
            "PUB" => {
                out.push(Tok::Pub);
            }
            "WEAVE" => {
                // v13: [count l-run] TASK <name v-run> [':'] body, repeated until
                // the RUN glyph. The task body lexes until the next TASK or RUN
                // glyph; its leading param bindings (v-run + '!') are the task's
                // input bindings, extracted by the parser.
                loop {
                    self.skip_ws();
                    match self.peek() {
                        Some(g) if opcode_index(g) == Some(84) => {
                            self.pos += 1;
                            let terminal = match self.peek() {
                                Some(g2) if is_v(g2 as u32) => {
                                    let t = self.fold_slots();
                                    self.skip_ws();
                                    Some(t)
                                }
                                _ => None,
                            };
                            out.push(Tok::Wrun(terminal));
                            break;
                        }
                        Some(g) if is_l(g as u32) || opcode_index(g) == Some(82) => {
                            let mut count: Option<i64> = None;
                            if let Some(g2) = self.peek() {
                                if is_l(g2 as u32) {
                                    count = Some(self.lrun_u64() as i64);
                                    self.skip_ws();
                                }
                            }
                            match self.peek() {
                                Some(g2) if opcode_index(g2) == Some(82) => self.pos += 1,
                                _ => self.err("WEAVE: expected TASK glyph after worker count"),
                            }
                            self.skip_ws();
                            let name = match self.peek() {
                                Some(g2) if is_v(g2 as u32) => self.fold_slots(),
                                _ => self.err("WEAVE: task needs a name"),
                            };
                            self.skip_ws();
                            if self.peek() == Some(':') {
                                self.pos += 1;
                                self.skip_ws();
                            }
                            let mut body = Vec::new();
                            self.lex_into(&mut body, 5);
                            out.push(Tok::Task { name, count, body });
                        }
                        _ => self.err("WEAVE: expected task name or RUN"),
                    }
                }
            }
            "TASK" | "ENDT" | "WRUN" => self.err("TASK/ENDT/WRUN are only valid inside WEAVE..WRUN"),
            "EXTERN" => {
                self.skip_ws();
                if self.peek() != Some('"') {
                    self.err("EXTERN must be followed by \"symbol\"");
                }
                out.push(Tok::Extern(self.lex_string()));
            }
            "MACRO" => {
                self.skip_ws();
                let mname = self.lex_ident();
                if is_bad_ident(&mname) {
                    self.err(&format!("macro name '{}' — identifiers may not start with '_'", mname));
                }
                self.skip_ws();
                if self.next() != Some('{') {
                    self.err("MACRO: expected '{'");
                }
                let mut body = Vec::new();
                self.lex_into(&mut body, 1);
                out.push(Tok::MacroDef(mname, body));
            }
            "STRUCT" => {
                self.skip_ws();
                let sname = self.lex_ident();
                if is_bad_ident(&sname) {
                    self.err(&format!("struct name '{}' — identifiers may not start with '_'", sname));
                }
                self.skip_ws();
                if self.next() != Some('{') {
                    self.err("STRUCT: expected '{'");
                }
                let mut fields = Vec::new();
                let mut cur = String::new();
                loop {
                    match self.next() {
                        Some('}') => {
                            if !cur.trim().is_empty() {
                                let f: Vec<&str> = cur.trim().splitn(2, ':').collect();
                                if f.len() != 2 {
                                    self.err("STRUCT field must be name:type");
                                }
                                fields.push((f[0].trim().to_string(), f[1].trim().to_string()));
                            }
                            break;
                        }
                        Some(',') => {
                            let f: Vec<&str> = cur.trim().splitn(2, ':').collect();
                            if f.len() != 2 {
                                self.err("STRUCT field must be name:type");
                            }
                            fields.push((f[0].trim().to_string(), f[1].trim().to_string()));
                            cur.clear();
                        }
                        Some(c) => cur.push(c),
                        None => self.err("STRUCT: unterminated body"),
                    }
                }
                out.push(Tok::StructDef(sname, fields));
            }
            "SIZEOF" => {
                self.skip_ws();
                if let Some(c) = self.peek() {
                    if let Some(id) = glyph_type_id(c) {
                        self.pos += 1;
                        out.push(Tok::PushI(id));
                        out.push(Tok::Op("SIZEOF"));
                        return;
                    }
                    if c.is_ascii_alphabetic() {
                        let sym = self.lex_ident();
                        if is_bad_ident(&sym) {
                            self.err(&format!("sizeof: '{}' — identifiers may not start with '_'", sym));
                        }
                        out.push(Tok::Ident(format!("@sizeof:{}", sym)));
                        return;
                    }
                }
                out.push(Tok::Op("SIZEOF"));
            }
            "OFFSET" => {
                self.skip_ws();
                let sym = self.lex_ident();
                if is_bad_ident(&sym) {
                    self.err(&format!("offset: '{}' — identifiers may not start with '_'", sym));
                }
                if self.peek() == Some('.') {
                    self.pos += 1;
                    let field = self.lex_ident();
                    out.push(Tok::Ident(format!("@offset:{}.{}", sym, field)));
                } else if !sym.is_empty() {
                    out.push(Tok::Ident(format!("@offset:{}", sym)));
                } else {
                    self.err("OFFSET needs Struct.field");
                }
            }
            "OBJ" | "CAST" | "ARR" | "TENSOR" => {
                self.skip_ws();
                // v12: ARR/TENSOR take [len, type] with type on top. The type
                // immediate follows the op in source, so it lands on top of len
                // with no swap needed.
                if let Some(c) = self.peek() {
                    if let Some(id) = glyph_type_id(c) {
                        // type-glyph immediate: push the id, then the op
                        self.pos += 1;
                        out.push(Tok::PushI(id));
                        out.push(Tok::Op(OP_NAMES[idx]));
                        return;
                    }
                    if c.is_ascii_alphabetic() {
                        let kw = self.lex_ident();
                        if is_bad_ident(&kw) {
                            self.err(&format!("{}: type name '{}' — identifiers may not start with '_'", name, kw));
                        }
                        if let Some(id) = type_id(&kw) {
                            out.push(Tok::PushI(id));
                        } else if name == "OBJ" {
                            // struct name: resolved at parse time via @objsize
                            out.push(Tok::Ident(format!("@objsize:{}", kw)));
                        } else if name == "CAST" {
                            // struct name: checked downcast via @cast
                            out.push(Tok::Ident(format!("@cast:{}", kw)));
                        } else {
                            self.err(&format!("unknown type keyword {}", kw));
                        }
                        out.push(Tok::Op(OP_NAMES[idx]));
                        return;
                    }
                }
                out.push(Tok::Op(OP_NAMES[idx]));
            }
            "SYS" => {
                self.skip_ws();
                let mut n = 0usize;
                let mut got = false;
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() {
                        n = n * 10 + (c as usize - '0' as usize);
                        got = true;
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                out.push(Tok::Sys(if got { n } else { 0 }));
            }
            _ => {
                out.push(Tok::Op(OP_NAMES[idx]));
            }
        }
    }
}

// ---------------- text encoding (v9) ----------------
// Lowercase ASCII mnemonics, whitespace-delimited. Same Tok AST as dense.
// The dense lookback rule is a dense-only artifact: in text mode "-" alone is
// SUB and "-5" is a number, because tokens are space-delimited.

// text mnemonic for each opcode index (normative, SPEC_v13_proposal.md).
// Retired slots return "~retired" which never matches a source token.
pub fn text_mnemonic(idx: usize) -> &'static str {
    match OP_NAMES[idx] {
        "LIT" => "_lit", "~1" | "~2" | "~3" | "~4" | "~5" => "~retired",
        "ADD" => "add", "SUB" => "sub", "MUL" => "mul", "AND" => "and",
        "SHR" => "shr", "INC" => "inc", "DEC" => "dec",
        "POW" => "pow", "SQRT" => "sqrt", "LTE" => "lte", "GTE" => "gte",
        "SHUTDOWN" => "shutdown",
        "FOR" => "for", "CALL" => "_call", "RET" => "ret", "OBJ" => "_obj",
        "GET" => "get", "SET" => "set", "ARR" => "array", "CLONE" => "copy", "CAST" => "_cast",
        "MACRO" => "macro", "TENSOR" => "tensor", "SETV" => "setv", "GETV" => "getv",
        "STR" => "_str", "CAT" => "concat", "FMT" => "format", "BUF" => "buffer", "BUFCOPY" => "copy_memory",
        "ADDR" => "_addr", "LOADX" => "load", "STOREX" => "store", "SIZEOF" => "_size_of",
        "OFFSET" => "_offset", "STRUCT" => "struct", "MALLOC" => "malloc", "FREE" => "free",
        "SYS" => "_syscall", "GC" => "gc", "IMPORT" => "import", "EXPORT" => "export",
        "EXTERN" => "extern", "PRINT" => "print", "SCAN" => "scan",
        "DICT" => "dict", "LIST" => "list", "PUSH" => "append", "POP" => "pop", "CHAN" => "channel",
        "ENQ" => "enqueue", "DEQ" => "dequeue", "CLOSE" => "close", "ATOM" => "atomic", "AGET" => "atomic_get",
        "ASET" => "atomic_set", "AADD" => "atomic_add", "CAS" => "cas", "TYPEOF" => "type_of", "LEN" => "length",
        "USE" => "use", "MOD" => "mod", "PUB" => "pub", "WEAVE" => "weave", "TASK" => "task",
        "ENDT" => "~retired", "WRUN" => "run",
        "SH" => "shell", "SHP" => "shell_stream", "EXEC" => "execute",
        "CAP" => "cap", "CAPS" => "caps",
        "MATCH" => "regex_match", "REPLACE" => "regex_replace", "RSPLIT" => "regex_split", "GLOB" => "glob_match",
        "SPLIT" => "split", "JOIN" => "join", "SLICE" => "slice", "FIND" => "find",
        "REPL" => "replace_all", "TRIM" => "trim", "UP" => "uppercase", "DOWN" => "lowercase",
        "STARTS" => "starts_with", "ENDS" => "ends_with",
        // v10: arithmetic & logic
        "DIV" => "div", "REM" => "rem", "EQ" => "eq", "LT" => "lt", "GT" => "gt",
        "NOT" => "not", "OR" => "or", "XOR" => "xor", "SHL" => "shl", "BNOT" => "bnot",
        // v10: structured control flow
        "IF" => "if", "IFELSE" => "if_else", "WHILE" => "while", "BREAK" => "break", "CONT" => "continue",
        // v10: container protocol & sequences
        "GETQ" => "get_or_zero", "HAS" => "contains", "ORELSE" => "orelse", "KEYS" => "keys",
        "RANGE" => "range", "SORT" => "sort", "FILTER" => "filter", "SOME" => "any", "EVERY" => "all",
        // v10: vector ops
        "VADD" => "scalar_add", "VSUB" => "scalar_sub", "VMUL" => "scalar_mul", "VDIV" => "scalar_div",
        "VEADD" => "array_add", "VESUB" => "array_sub", "VEMUL" => "array_mul", "VEDIV" => "array_div",
        "VEMAX" => "array_max", "VEMIN" => "array_min",
        "VEQ" => "scalar_eq", "VLT" => "scalar_lt", "VGT" => "scalar_gt", "VGE" => "scalar_gte", "VLE" => "scalar_lte",
        "VAND" => "bitmap_and", "VOR" => "bitmap_or", "VNOT" => "bitmap_not", "VCOUNT" => "bitmap_count",
        "VGATHER" => "array_gather", "VSUM" => "sum", "VMEAN" => "mean", "VMIN" => "min",
        "VMAX" => "max", "DEL" => "remove", "VMAP" => "array_map", "VFOLD" => "array_reduce",
        // v10: time, bloom, script I/O
        "NOW" => "now", "TIME" => "parse_time", "TIMEF" => "format_time",
        "BLOOM" => "bloom", "BADD" => "bloom_add", "BTEST" => "bloom_test",
        "SLURP" => "read_file", "SPIT" => "write_file", "ARGV" => "argv",
        // v10: additional data ops
        "GROUP" => "group_by", "AGG" => "aggregate", "UNIQUE" => "unique", "FLAT" => "flatten",
        "CHUNK" => "chunk", "VARGSORT" => "array_argsort", "VSEARCHSORTED" => "array_search_sorted",
        "VWHERE" => "array_where",
        // v10: large-data shortcuts
        "MMAP" => "mmap", "FEACH" => "file_each_line", "FFOLD" => "file_fold_lines", "FSPLIT" => "file_split_lines", "FGET" => "field_get", "FATOI" => "field_int", "FATOF" => "field_float", "FSGET" => "field_slice", "FBYTE" => "field_byte", "VGET" => "array_get", "VSET" => "array_set", "ADDTO" => "add_to", "FADDTO" => "field_add_to", "FINC" => "field_inc", "FMATCH" => "file_match_lines",
        "BFS" => "bfs", "DFS" => "dfs", "WFIND" => "find_first",
        // v10: JSON, iterators, sinks, containment, threads
        "JSON" => "parse_json", "UNJSON" => "to_json", "ITER" => "iter", "NEXT" => "next",
        "COLLECT" => "collect", "IMAP" => "iter_map", "IFILTER" => "iter_filter", "FEMIT" => "file_emit",
        "TRY" => "try", "RETRY" => "retry", "SPAWN" => "spawn",
        "ATOI" => "parse_int", "ATOF" => "parse_float", "ITOA" => "format_int", "FTOA" => "format_float",
        "ENTRY" => "entry",
        "HASARGS" => "has_args", "ARGI" => "arg_index", "SORTKEYS" => "sort_keys", "TOPN" => "top_n",
        "RANGEFOLD" => "range_reduce",
        "TRANSPOSE" => "transpose",
        "TRANSPOSE" => "transpose",
        "SEQ" => "structural_equal", "SNE" => "structural_not_equal",
        other => other, // "~NN" retired placeholders: never a valid source token
    }
}

pub fn mnemonic_index(tok: &str) -> Option<usize> {
    (0..OP_NAMES.len()).find(|&i| text_mnemonic(i) == tok)
}

pub fn is_reserved(tok: &str) -> bool {
    mnemonic_index(tok).is_some()
}

/// Reject _-prefixed names — reserved for immediate-opcode mnemonics
pub fn is_bad_ident(name: &str) -> bool {
    name.starts_with('_')
}

/// Combined reserved-word and _-prefix check for identifiers
pub fn check_ident(name: &str, what: &str) {
    if is_bad_ident(name) {
        panic!("text lex error: {}: {} — identifiers may not start with '_'", what, name);
    }
    if is_reserved(name) {
        panic!("text lex error: {}: {} is a reserved word", what, name);
    }
}

// split text source into whitespace-delimited tokens; strings keep their
// quotes, '{' '}' are own tokens, ';' comments to end of line
pub fn text_tokens(src: &str) -> Vec<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut v = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == ';' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '"' {
            let mut s = String::from("\"");
            i += 1;
            while i < chars.len() {
                let c = chars[i];
                s.push(c);
                i += 1;
                if c == '\\' && i < chars.len() {
                    s.push(chars[i]);
                    i += 1;
                    continue;
                }
                if c == '"' {
                    break;
                }
            }
            v.push(s);
            continue;
        }
        if c == '{' || c == '}' || c == '[' || c == ']' {
            v.push(c.to_string());
            i += 1;
            continue;
        }
        let mut s = String::new();
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '{' && chars[i] != '}' && chars[i] != '[' && chars[i] != ']' && chars[i] != ';' {
            s.push(chars[i]);
            i += 1;
        }
        v.push(s);
    }
    v
}

// decode a quoted token (with quotes) into its string value
pub fn unquote(tok: &str) -> Option<String> {
    let chars: Vec<char> = tok.chars().collect();
    if chars.len() < 2 || chars[0] != '"' || chars[chars.len() - 1] != '"' {
        return None;
    }
    let mut s = String::new();
    let mut i = 1;
    while i < chars.len() - 1 {
        if chars[i] == '\\' && i + 1 < chars.len() - 1 {
            i += 1;
            match chars[i] {
                'n' => s.push('\n'),
                't' => s.push('\t'),
                'r' => s.push('\r'),
                '0' => s.push('\0'),
                '\\' => s.push('\\'),
                '"' => s.push('"'),
                c => s.push(c),
            }
            i += 1;
        } else {
            s.push(chars[i]);
            i += 1;
        }
    }
    Some(s)
}

// bare text number: optional '-', decimal/hex int or float; '_' separators ok
pub fn parse_text_num(tok: &str) -> Option<Tok> {
    if tok.is_empty() || tok == "-" {
        return None;
    }
    let s = tok.replace('_', "");
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b.to_string()),
        None => (false, s.clone()),
    };
    if body.is_empty() {
        return None;
    }
    if !body.chars().next().unwrap().is_ascii_digit() {
        return None;
    }
    if body.starts_with("0x") || body.starts_with("0X") {
        let v = i64::from_str_radix(&body[2..], 16).ok()?;
        return Some(Tok::PushI(if neg { -v } else { v }));
    }
    if body.contains('.') || body.contains('e') || body.contains('E') {
        let v: f64 = body.parse().ok()?;
        return Some(Tok::PushF(if neg { -v } else { v }));
    }
    let v: i64 = body.parse().ok()?;
    Some(Tok::PushI(if neg { -v } else { v }))
}

pub struct TextLexer {
    toks: Vec<String>,
    pos: usize,
    // v13: true when the previous token was a bare `ret`
    prev_ret: bool,
}

impl TextLexer {
    fn peek(&self) -> Option<&str> {
        self.toks.get(self.pos).map(|s| s.as_str())
    }
    fn next(&mut self) -> Option<String> {
        let t = self.peek().map(|s| s.to_string());
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn err(&self, msg: &str) -> ! {
        panic!("text lex error at token {}: {}", self.pos, msg)
    }
    fn name_token(&mut self, what: &str) -> String {
        let t = self.next().unwrap_or_else(|| self.err(&format!("{} needs a name", what)));
        check_ident(&t, what);
        t
    }
    // stop: 0 = eof, 1 = '}', 3 = ']', 5 = "task"/"run" (weave body end)
    fn run(&mut self, out: &mut Vec<Tok>, stop: u8) {
        loop {
            let tok = match self.peek() {
                None => {
                    if stop != 0 {
                        self.err("unterminated block");
                    }
                    return;
                }
                Some(t) => t.to_string(),
            };
            if tok == "}" && stop == 1 {
                self.pos += 1;
                return;
            }
            if tok == "]" && stop == 3 {
                self.pos += 1;
                return;
            }
            if stop == 5 && (tok == "task" || tok == "run") {
                // TASK/RUN bound a weave task body; leave them unconsumed
                return;
            }
            if stop == 5 && !self.prev_ret && parse_text_num(&tok).is_some()
                && self.toks.get(self.pos + 1).map(|t| t.as_str() == "task").unwrap_or(false) {
                // a numeric literal directly before `task` is the next task's
                // fanout worker count — leave it unconsumed
                return;
            }
            if tok == "[" {
                self.pos += 1;
                let mut body = Vec::new();
                self.run(&mut body, 3);
                out.push(Tok::List(body));
                continue;
            }
            if tok == "{" {
                self.pos += 1;
                let mut body = Vec::new();
                self.run(&mut body, 1);
                out.push(Tok::Dict(body));
                continue;
            }
            if let Some(s) = unquote(&tok) {
                self.pos += 1;
                out.push(Tok::PushS(s));
                continue;
            }
            if let Some(label) = tok.strip_prefix('\'') {
                self.pos += 1;
                if label.is_empty() {
                    let l = self.name_token("addr");
                    out.push(Tok::Jump("ADDR", l));
                } else {
                    check_ident(label, "addr");
                    out.push(Tok::Jump("ADDR", label.to_string()));
                }
                continue;
            }
            if let Some(name) = tok.strip_suffix(':') {
                if !name.is_empty() && name != "'" {
                    self.pos += 1;
                    if name == "entry" {
                        out.push(Tok::Entry);
                        continue;
                    }
                    check_ident(name, "label");
                    out.push(Tok::LabelDef(name.to_string()));
                    continue;
                }
            }
            if let Some(name) = tok.strip_suffix('!') {
                if !name.is_empty() && !tok.starts_with('!') {
                    self.pos += 1;
                    // v12: _! is the destructuring discard token
                    if name == "_" {
                        out.push(Tok::Discard);
                        continue;
                    }
                    // v11: ^x! = global, x! = local
                    if let Some(gname) = name.strip_prefix('^') {
                        if gname.is_empty() {
                            self.err("variable name is empty");
                        }
                        check_ident(gname, "variable");
                        out.push(Tok::SetV(gname.to_string()));
                    } else {
                        check_ident(name, "variable");
                        out.push(Tok::LocalSet(name.to_string()));
                    }
                    continue;
                }
            }
            if let Some(name) = tok.strip_suffix('@') {
                if !name.is_empty() && !tok.starts_with('@') {
                    self.pos += 1;
                    // v11: ^x@ = global, x@ = local
                    if let Some(gname) = name.strip_prefix('^') {
                        if gname.is_empty() {
                            self.err("variable name is empty");
                        }
                        check_ident(gname, "variable");
                        out.push(Tok::GetV(gname.to_string()));
                    } else {
                        check_ident(name, "variable");
                        out.push(Tok::LocalGet(name.to_string()));
                    }
                    continue;
                }
            }
            // name++ / ^name++ : increment variable by 1
            if let Some(name) = tok.strip_suffix("++") {
                if !name.is_empty() && !tok.starts_with('+') {
                    self.pos += 1;
                    if let Some(gname) = name.strip_prefix('^') {
                        if gname.is_empty() { self.err("variable name is empty"); }
                        check_ident(gname, "variable");
                        out.push(Tok::IncGlobal(gname.to_string()));
                    } else {
                        check_ident(name, "variable");
                        out.push(Tok::IncLocal(name.to_string()));
                    }
                    continue;
                }
            }
            // name+= / ^name+= : accumulate stack value into variable
            if let Some(name) = tok.strip_suffix("+=") {
                if !name.is_empty() && !tok.starts_with('+') {
                    self.pos += 1;
                    if let Some(gname) = name.strip_prefix('^') {
                        if gname.is_empty() { self.err("variable name is empty"); }
                        check_ident(gname, "variable");
                        out.push(Tok::AddGlobal(gname.to_string()));
                    } else {
                        check_ident(name, "variable");
                        out.push(Tok::AddLocal(name.to_string()));
                    }
                    continue;
                }
            }
            self.prev_ret = tok == "ret";
            self.pos += 1;
            match tok.as_str() {
                "{" | "}" => self.err("stray '{'/'}'"),
                "[" | "]" => self.err("stray '['/']'"),
                "+" => self.err("'+' removed in v13 — use `add`"),
                "-" => self.err("'-' removed in v13 — use `sub` (negative numbers still parse)"),
                "*" => self.err("'*' removed in v13 — use `mul`"),
                "&" => self.err("'&' removed in v13 — use `and`"),
                "=" => self.err("= (je) removed — use if/ifelse"),
                "pub" => out.push(Tok::Pub),
                "use" | "export" | "extern" | "mod" => {
                    let n = self.next().unwrap_or_else(|| self.err(&format!("{} needs \"name\"", tok)));
                    let n = unquote(&n).unwrap_or_else(|| self.err(&format!("{} needs \"name\"", tok)));
                    out.push(match tok.as_str() {
                        "use" => Tok::Use(n),
                        "export" => Tok::Export(n),
                        "mod" => Tok::Mod(n),
                        _ => Tok::Extern(n),
                    });
                }
                "import" => {
                    // one token: c"name"(params)->ret
                    let n = self.next().unwrap_or_else(|| self.err("import needs a binding"));
                    out.push(self.parse_import(&n));
                }
                "macro" => {
                    let n = self.name_token("macro");
                    if self.next().as_deref() != Some("{") {
                        self.err("macro: expected '{'");
                    }
                    let mut body = Vec::new();
                    self.run(&mut body, 1);
                    out.push(Tok::MacroDef(n, body));
                }
                "struct" => {
                    let n = self.name_token("struct");
                    if self.next().as_deref() != Some("{") {
                        self.err("struct: expected '{'");
                    }
                    let mut fields = Vec::new();
                    loop {
                        let f = self.next().unwrap_or_else(|| self.err("struct: unterminated body"));
                        if f == "}" {
                            break;
                        }
                        let f = f.trim_end_matches(',');
                        if f.is_empty() {
                            continue;
                        }
                        let parts: Vec<&str> = f.splitn(2, ':').collect();
                        if parts.len() != 2 {
                            self.err("struct field must be name:type");
                        }
                        fields.push((parts[0].to_string(), parts[1].to_string()));
                    }
                    out.push(Tok::StructDef(n, fields));
                }
                "weave" => {
                    loop {
                        match self.peek() {
                            Some("run") => {
                                self.pos += 1;
                                // optional terminal task name after `run`
                                let terminal = match self.peek() {
                                    Some(t) if !is_reserved(t) && !is_bad_ident(t)
                                        && !t.ends_with(':') && !t.ends_with('!') && !t.ends_with('@')
                                        && parse_text_num(t).is_none() && unquote(t).is_none()
                                        && !t.starts_with('\'') => {
                                        let t = self.next().unwrap();
                                        check_ident(&t, "run terminal");
                                        Some(t)
                                    }
                                    _ => None,
                                };
                                out.push(Tok::Wrun(terminal));
                                break;
                            }
                            Some(_) => {
                                let mut count: Option<i64> = None;
                                // optional worker count before 'task'
                                if let Some(t) = self.peek() {
                                    if let Some(Tok::PushI(v)) = parse_text_num(t) {
                                        if v > 0 {
                                            count = Some(v);
                                            self.pos += 1;
                                        }
                                    }
                                }
                                if self.next().as_deref() != Some("task") {
                                    self.err("weave: expected 'task' (or 'run')");
                                }
                                // task name, with optional trailing ':'
                                let mut name = self.name_token("task");
                                if name.ends_with(':') {
                                    name = name.trim_end_matches(':').to_string();
                                }
                                if self.peek() == Some(":") {
                                    self.pos += 1;
                                }
                                let mut body = Vec::new();
                                self.run(&mut body, 5);
                                out.push(Tok::Task { name, count, body });
                            }
                            None => self.err("weave: unterminated (missing run)"),
                        }
                    }
                }
                "task" | "run" => self.err("task/run only valid inside weave..run"),
                "endt" => self.err("endt removed in v13 — task bodies end with `ret`"),
                "_lit" => {
                    let n = self.next().unwrap_or_else(|| self.err("lit needs an operand"));
                    if let Some(id) = type_id(&n) {
                        out.push(Tok::PushI(id));
                    } else if let Some(t) = parse_text_num(&n) {
                        out.push(t);
                    } else {
                        self.err(&format!("lit: bad operand {}", n));
                    }
                }
                "jmp" => self.err("jmp removed — use entry/if/while/for"),
                "jz" => self.err("jz removed — use if/ifelse/while"),
                "je" => self.err("je removed — use if/ifelse"),
                "_call" => {
                    let l = self.name_token("call");
                    out.push(Tok::Jump("CALL", l));
                }
                "entry" => {
                    out.push(Tok::Entry);
                }
                "_addr" => {
                    let l = self.name_token("addr");
                    out.push(Tok::Jump("ADDR", l));
                }
                "_syscall" => {
                    let n = match self.peek() {
                        Some(t) => t.parse::<usize>().ok(),
                        None => None,
                    };
                    if let Some(n) = n {
                        self.pos += 1;
                        out.push(Tok::Sys(n));
                    } else {
                        out.push(Tok::Sys(0));
                    }
                }
                "_size_of" => {
                    match self.peek() {
                        Some(t) if type_id(t).is_some() => {
                            let id = type_id(self.next().unwrap().as_str()).unwrap();
                            out.push(Tok::PushI(id));
                            out.push(Tok::Op("SIZEOF"));
                        }
                        Some(_) => {
                            let sym = self.name_token("size_of");
                            out.push(Tok::Ident(format!("@sizeof:{}", sym)));
                        }
                        None => out.push(Tok::Op("SIZEOF")),
                    }
                }
                "_offset" => {
                    let sym = self.next().unwrap_or_else(|| self.err("offset needs Struct.field"));
                    let parts: Vec<&str> = sym.splitn(2, '.').collect();
                    if parts.len() == 2 {
                        out.push(Tok::Ident(format!("@offset:{}", sym)));
                    } else {
                        self.err("offset needs Struct.field");
                    }
                }
                "_obj" | "_cast" | "_array" | "_tensor" => {
                    let name = match tok.as_str() {
                        "_obj" => "OBJ",
                        "_cast" => "CAST",
                        "_array" => "ARR",
                        _ => "TENSOR",
                    };
                    // v12: ARR/TENSOR take [len, type]; type immediate lands on top
                    match self.peek() {
                        Some(t) if type_id(t).is_some() => {
                            let id = type_id(self.next().unwrap().as_str()).unwrap();
                            out.push(Tok::PushI(id));
                            out.push(Tok::Op(name));
                        }
                        Some(t) if !is_reserved(t) && !is_bad_ident(t) && !t.ends_with(':') && !t.ends_with('!') && !t.ends_with('@') && parse_text_num(t).is_none() && unquote(t).is_none() && !t.starts_with('\'') => {
                            let kw = self.next().unwrap();
                            if name == "OBJ" {
                                out.push(Tok::Ident(format!("@objsize:{}", kw)));
                            } else if name == "CAST" {
                                out.push(Tok::Ident(format!("@cast:{}", kw)));
                            } else {
                                self.err(&format!("{}: unknown type {}", name, kw));
                            }
                            out.push(Tok::Op(name));
                        }
                        _ => out.push(Tok::Op(name)),
                    }
                }
                "scan" | "print" => out.push(Tok::Op(if tok == "scan" { "SCAN" } else { "PRINT" })),
                "_str" => {
                    let n = self.next().unwrap_or_else(|| self.err("str needs a string"));
                    match unquote(&n) {
                        Some(s) => out.push(Tok::PushS(s)),
                        None => self.err("str needs a string"),
                    }
                }
                "setv" | "getv" => self.err("use name! / name@ for variables"),
                // backward-compat: old immediate-op names now _-prefixed
                "call" | "addr" | "sys" | "lit" | "str" | "sizeof" | "offset" | "obj" | "cast" | "arr" => {
                    let n: &str = if tok == "sys" { "syscall" } else if tok == "sizeof" { "size_of" } else if tok == "arr" { "array" } else { tok.as_str() };
                    self.err(&format!("'{}' is now '_{}' — immediate ops are _-prefixed", tok, n));
                }
                _ => {
                    if let Some(idx) = mnemonic_index(&tok) {
                        // plain opcode with no operand handling
                        out.push(Tok::Op(OP_NAMES[idx]));
                    } else if let Some(id) = type_id(&tok) {
                        // v13: bare type keywords push their type id
                        // (e.g. `[1 2] int array`, `x _cast int`)
                        out.push(Tok::PushI(id));
                    } else if let Some(t) = parse_text_num(&tok) {
                        out.push(t);
                    } else if is_bad_ident(&tok) {
                        self.err(&format!("identifier '{}' — identifiers may not start with '_'", tok));
                    } else {
                        out.push(Tok::Ident(tok));
                    }
                }
            }
        }
    }

    fn parse_import(&mut self, tok: &str) -> Tok {
        // c"name"(params)->ret
        let rest = tok.strip_prefix('c').unwrap_or_else(|| self.err("import: binding must start with c"));
        let q0 = rest.find('"').unwrap_or_else(|| self.err("import: expected c\"name\""));
        let q1 = rest[q0 + 1..].find('"').map(|i| i + q0 + 1).unwrap_or_else(|| self.err("import: unterminated name"));
        let fname = rest[q0 + 1..q1].to_string();
        let after = &rest[q1 + 1..];
        let p0 = after.find('(').unwrap_or_else(|| self.err("import: expected '('"));
        let p1 = after.rfind(')').unwrap_or_else(|| self.err("import: expected ')'"));
        let params: Vec<String> = after[p0 + 1..p1]
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let ret = after[p1 + 1..]
            .trim_start_matches('-')
            .trim_start_matches('>')
            .trim()
            .to_string();
        if ret.is_empty() {
            self.err("import: expected ->ret");
        }
        Tok::Import(Import { name: fname, params, ret })
    }
}

// lex one source, auto-detecting the encoding: any char >= U+13000 -> dense
pub fn lex_source(src: &str) -> Vec<Tok> {
    if src.chars().any(|c| c as u32 >= 0x13000) {
        Lexer::new(src).lex()
    } else {
        let toks = text_tokens(src);
        let mut lx = TextLexer { toks, pos: 0, prev_ret: false };
        let mut out = Vec::new();
        lx.run(&mut out, 0);
        out
    }
}
