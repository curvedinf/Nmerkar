// compute.rs — automatic GPU offloading behind a compute-backend abstraction.
//
// There is no opt-in flag: when a Vulkan shader toolchain (glslc) is present
// and the device mode is not `--device cpu`, the compiler compiles the STATIC
// shader library below to SPIR-V, embeds the blobs into the generated C
// (`#define NKR_GPU`), and links -lvulkan. The runtime shims in the prelude
// pick the device (auto = most free VRAM via VK_EXT_memory_budget; pinned via
// the baked --device string) and launch prebuilt kernels for eligible ops
// when the element count clears NKR_GPU_MIN; otherwise they fall back to the
// CPU implementation. Kernels are only ever *launched*, never generated from
// user code.
//
// Eligible ops: add/sub/mul/div (elementwise, broadcast, matmul, matvec) and
// sum/mean/min/max, transpose — all on float64 (double) arrays/tensors/
// matrices. Other element types and int64 workloads stay on the CPU.

use std::hash::{Hash, Hasher};
use std::process::Command;

pub trait ComputeBackend {
    fn id(&self) -> &'static str;
    /// Compiler for the static shader library (e.g. glslc on PATH).
    fn toolchain_probe(&self) -> bool;
    /// The fixed, hand-written shader library: (name, GLSL compute source).
    fn shader_library(&self) -> Vec<(&'static str, String)>;
    /// Compile one .comp source to SPIR-V; returns (argv, tmp_src_path).
    fn compile_cmd(&self, src: &str, out: &str) -> (Vec<String>, String);
    fn link_libs(&self) -> Vec<String>;
}

pub struct VulkanBackend;

const WG: u32 = 256;

fn header() -> String {
    format!(
        "#version 450\n#extension GL_EXT_shader_explicit_arithmetic_types_float64 : require\n#extension GL_EXT_shader_explicit_arithmetic_types_int64 : require\nlayout(local_size_x={}) in;\nlayout(push_constant) uniform PC {{ int64_t n0, n1, n2, n3; float64_t s; int64_t rev; }} pc;\n",
        WG
    )
}

fn ebin(op: &str) -> String {
    format!(
        "{}layout(std430, binding=0) buffer A {{ float64_t a[]; }};\nlayout(std430, binding=1) buffer B {{ float64_t b[]; }};\nlayout(std430, binding=2) buffer R {{ float64_t r[]; }};\nvoid main() {{ int i = int(gl_GlobalInvocationID.x); if (int64_t(i) >= pc.n0) return; r[i] = a[i] {} b[i]; }}\n",
        header(),
        op
    )
}

fn bbin(op: &str) -> String {
    // rev=0: r = a op s ; rev=1: r = s op a
    format!(
        "{}layout(std430, binding=0) buffer A {{ float64_t a[]; }};\nlayout(std430, binding=2) buffer R {{ float64_t r[]; }};\nvoid main() {{ int i = int(gl_GlobalInvocationID.x); if (int64_t(i) >= pc.n0) return; r[i] = pc.rev != 0 ? (pc.s {} a[i]) : (a[i] {} pc.s); }}\n",
        header(),
        op,
        op
    )
}

fn shader_library() -> Vec<(&'static str, String)> {
    vec![
        ("eadd", ebin("+")),
        ("esub", ebin("-")),
        ("emul", ebin("*")),
        ("ediv", ebin("/")),
        ("badd", bbin("+")),
        ("bsub", bbin("-")),
        ("bmul", bbin("*")),
        ("bdiv", bbin("/")),
        // 16×16 tiled matmul: each workgroup streams K-tiles of A/B through
        // shared memory so B is not read with a stride-cb gather per k.
        // Dispatch is 2-D ((cb+15)/16, (ra+15)/16) — see uf_vk_run.
        ("matmul", String::from(
            "#version 450\n\
             #extension GL_EXT_shader_explicit_arithmetic_types_float64 : require\n\
             #extension GL_EXT_shader_explicit_arithmetic_types_int64 : require\n\
             layout(local_size_x=16, local_size_y=16) in;\n\
             layout(push_constant) uniform PC { int64_t n0, n1, n2, n3; float64_t s; int64_t rev; } pc;\n\
             layout(std430, binding=0) buffer A { float64_t a[]; };\n\
             layout(std430, binding=1) buffer B { float64_t b[]; };\n\
             layout(std430, binding=2) buffer R { float64_t r[]; };\n\
             shared float64_t As[16][16];\n\
             shared float64_t Bs[16][16];\n\
             void main() {\n\
               int ra = int(pc.n1), ca = int(pc.n2), cb = int(pc.n3);\n\
               int row = int(gl_GlobalInvocationID.y);\n\
               int col = int(gl_GlobalInvocationID.x);\n\
               int lx = int(gl_LocalInvocationID.x);\n\
               int ly = int(gl_LocalInvocationID.y);\n\
               float64_t acc = 0.0LF;\n\
               int tiles = (ca + 15) / 16;\n\
               for (int t = 0; t < tiles; t++) {\n\
                 int a_col = t * 16 + lx;\n\
                 int b_row = t * 16 + ly;\n\
                 As[ly][lx] = (row < ra && a_col < ca) ? a[row * ca + a_col] : 0.0LF;\n\
                 Bs[ly][lx] = (b_row < ca && col < cb) ? b[b_row * cb + col] : 0.0LF;\n\
                 barrier();\n\
                 for (int k = 0; k < 16; k++) acc += As[ly][k] * Bs[k][lx];\n\
                 barrier();\n\
               }\n\
               if (row < ra && col < cb) r[row * cb + col] = acc;\n\
             }\n",
        )),
        ("matvec", format!("{}layout(std430, binding=0) buffer A {{ float64_t a[]; }};\nlayout(std430, binding=1) buffer B {{ float64_t b[]; }};\nlayout(std430, binding=2) buffer R {{ float64_t r[]; }};\nvoid main() {{ int64_t work = int64_t(gl_GlobalInvocationID.x); int64_t c = pc.n2; if (work >= pc.n1) return; int row = int(work); float64_t s = 0.0LF; for (int k = 0; int64_t(k) < c; k++) s += a[row*int(c)+k] * b[k]; r[row] = s; }}\n", header())),
        ("rsum", format!("{}layout(std430, binding=0) buffer A {{ float64_t a[]; }};\nlayout(std430, binding=2) buffer R {{ float64_t r[]; }};\nshared float64_t sh[{}];\nvoid main() {{ uint lid = gl_LocalInvocationID.x; int i = int(gl_GlobalInvocationID.x); float64_t v = (int64_t(i) < pc.n0) ? a[i] : 0.0LF; sh[lid] = v; barrier(); for (uint off = {}u >> 1u; off > 0u; off >>= 1u) {{ if (lid < off) sh[lid] += sh[lid + off]; barrier(); }} if (lid == 0u) r[int(gl_WorkGroupID.x)] = sh[0]; }}\n", header(), WG, WG)),
        ("rmin", format!("{}layout(std430, binding=0) buffer A {{ float64_t a[]; }};\nlayout(std430, binding=2) buffer R {{ float64_t r[]; }};\nshared float64_t sh[{}];\nvoid main() {{ uint lid = gl_LocalInvocationID.x; int i = int(gl_GlobalInvocationID.x); float64_t v = (int64_t(i) < pc.n0) ? a[i] : 1.0LF/0.0LF; sh[lid] = v; barrier(); for (uint off = {}u >> 1u; off > 0u; off >>= 1u) {{ if (lid < off) sh[lid] = sh[lid] < sh[lid + off] ? sh[lid] : sh[lid + off]; barrier(); }} if (lid == 0u) r[int(gl_WorkGroupID.x)] = sh[0]; }}\n", header(), WG, WG)),
        ("rmax", format!("{}layout(std430, binding=0) buffer A {{ float64_t a[]; }};\nlayout(std430, binding=2) buffer R {{ float64_t r[]; }};\nshared float64_t sh[{}];\nvoid main() {{ uint lid = gl_LocalInvocationID.x; int i = int(gl_GlobalInvocationID.x); float64_t v = (int64_t(i) < pc.n0) ? a[i] : -1.0LF/0.0LF; sh[lid] = v; barrier(); for (uint off = {}u >> 1u; off > 0u; off >>= 1u) {{ if (lid < off) sh[lid] = sh[lid] > sh[lid + off] ? sh[lid] : sh[lid + off]; barrier(); }} if (lid == 0u) r[int(gl_WorkGroupID.x)] = sh[0]; }}\n", header(), WG, WG)),
        ("esqrt", format!("{}layout(std430, binding=0) buffer A {{ float64_t a[]; }};\nlayout(std430, binding=2) buffer R {{ float64_t r[]; }};\nvoid main() {{ int i = int(gl_GlobalInvocationID.x); if (int64_t(i) >= pc.n0) return; r[i] = sqrt(a[i]); }}\n", header())),
        ("transpose", format!("{}layout(std430, binding=0) buffer A {{ float64_t a[]; }};\nlayout(std430, binding=2) buffer R {{ float64_t r[]; }};\nvoid main() {{ int64_t work = int64_t(gl_GlobalInvocationID.x); int64_t rr = pc.n1, cc = pc.n2; if (work >= rr*cc) return; int row = int(work / cc), col = int(work % cc); r[col*int(rr) + row] = a[int(work)]; }}\n", header())),
    ]
}

impl ComputeBackend for VulkanBackend {
    fn id(&self) -> &'static str {
        "vulkan"
    }
    fn toolchain_probe(&self) -> bool {
        which("glslc") || which("glslangValidator")
    }
    fn shader_library(&self) -> Vec<(&'static str, String)> {
        shader_library()
    }
    fn compile_cmd(&self, src: &str, out: &str) -> (Vec<String>, String) {
        // write src to a temp .comp next to out; glslc -> SPIR-V
        let comp = format!("{}.comp", out);
        std::fs::write(&comp, src).unwrap_or_else(|e| panic!("cannot write {}: {}", comp, e));
        if which("glslc") {
            (vec!["glslc".into(), "-O".into(), "--target-env=vulkan1.2".into(), "-o".into(), out.into(), comp.clone()], comp)
        } else {
            (vec!["glslangValidator".into(), "-V".into(), "-o".into(), out.into(), comp.clone()], comp)
        }
    }
    fn link_libs(&self) -> Vec<String> {
        vec!["-lvulkan".to_string()]
    }
}

fn which(name: &str) -> bool {
    let ok = Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok
}

pub fn backend_for(device: &str) -> Option<VulkanBackend> {
    // explicit cpu disables offloading entirely; unknown backend prefixes are
    // errors; auto/vk*/vulkan* use the Vulkan backend
    if device == "cpu" {
        return None;
    }
    if device != "auto" && !device.starts_with("vk") && !device.starts_with("vulkan") {
        panic!(
            "unknown device '{}' — use cpu, auto, or vk<N> (e.g. vk0)",
            device
        );
    }
    Some(VulkanBackend)
}

/// Compile the static shader library to SPIR-V (cached under TMPDIR by
/// content hash). Returns C source embedding the blobs as byte arrays, or
/// None when the toolchain is unavailable and the mode is auto (silent CPU).
pub fn gpu_enablement(device: &str, extra_kernels: &[(String, String)]) -> Option<(String, Vec<String>)> {
    let backend = backend_for(device)?;
    let explicit = device != "auto";
    if !backend.toolchain_probe() {
        if explicit {
            panic!(
                "device '{}' requested but no Vulkan shader toolchain found (need glslc or glslangValidator on PATH) — install a Vulkan SDK, or use --device cpu",
                device
            );
        }
        return None; // auto: silent CPU-only build, exactly the old pipeline
    }

    // cache dir
    let dir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let cdir = std::path::Path::new(&dir).join("nkr-spirv");
    let _ = std::fs::create_dir_all(&cdir);

    let lib = backend.shader_library();
    let mut all: Vec<(String, String)> = lib.into_iter().map(|(n, s)| (n.to_string(), s)).collect();
    all.extend(extra_kernels.iter().cloned());
    let lib = all;
    let mut c = String::from("#define NKR_GPU 1\n");
    for (name, src) in &lib {
        let spv_path = cdir.join(format!("{}_{:016x}.spv", name, fnv(src)));
        let spv = if spv_path.exists() {
            std::fs::read(&spv_path).unwrap_or_default()
        } else {
            let out = spv_path.with_extension("spv.tmp");
            let (argv, comp) = backend.compile_cmd(src, &out.to_string_lossy());
            let st = Command::new(&argv[0])
                .args(&argv[1..])
                .output()
                .unwrap_or_else(|e| panic!("failed to run {}: {}", argv[0], e));
            let _ = std::fs::remove_file(&comp);
            if !st.status.success() {
                panic!(
                    "shader compilation failed for kernel `{}`:\n{}",
                    name,
                    String::from_utf8_lossy(&st.stderr)
                );
            }
            let bytes = std::fs::read(&out).unwrap_or_else(|e| panic!("cannot read {}: {}", out.display(), e));
            let _ = std::fs::remove_file(&out);
            let _ = std::fs::write(&spv_path, &bytes);
            bytes
        };
        c.push_str(&format!(
            "const unsigned int uf_spv_{}[{}] = {{",
            name,
            spv.len() / 4
        ));
        for (i, w) in spv.chunks_exact(4).enumerate() {
            if i % 8 == 0 {
                c.push_str("\n");
            }
            c.push_str(&format!(
                "0x{:02x}{:02x}{:02x}{:02x},",
                w[3], w[2], w[1], w[0]
            ));
        }
        c.push_str("};\n");
    }
    c.push_str(&format!(
        "static const struct {{ const char* name; const unsigned int* code; unsigned long words; }} uf_spv_all[] = {{\n{}\n}};\n",
        lib.iter()
            .map(|(name, _)| format!(
                "  {{\"{}\", uf_spv_{}, sizeof(uf_spv_{})/4}}",
                name, name, name
            ))
            .collect::<Vec<_>>()
            .join(",\n")
    ));
    // fold the blob identity into the cache key via a marker
    let mut h = 0xcbf29ce484222325u64;
    for (name, src) in &lib {
        h ^= fnv(src) ^ fnv(name);
        h = h.wrapping_mul(0x100000001b3);
    }
    c.push_str(&format!("/* spv-rev {:016x} */\n", h));
    Some((c, backend.link_libs()))
}

/// True when the parsed program contains at least one GPU-eligible helper.
pub fn program_gpu_relevant(p: &crate::ast::Parsed) -> bool {
    p.ins.iter().any(|i| matches!(i, crate::ast::Ins::Simple(h) if is_gpu_eligible(h)))
}

pub fn is_gpu_eligible(h: &str) -> bool {
    matches!(
        h,
        "op_add" | "op_sub" | "op_mul" | "op_div" | "op_sqrt" | "op_vsum" | "op_vmean" | "op_vmin" | "op_vmax" | "op_transpose"
    )
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[allow(dead_code)]
fn fnv_hash<H: Hash>(_: &H) -> u64 {
    0
}

// ---------------- weave-task kernel compilation (v13.1) ----------------
// A weave task whose body (after its input binds, before ret) is a
// straight-line chain of elementwise arithmetic over its inputs is compiled
// into ONE fused float64 kernel: inputs staged in once, every op executed
// on-device, result staged out once. Anything else in the body (control
// flow, calls, non-eligible ops, prints) declines fusion and the task runs
// on the CPU exactly as before.

#[derive(Clone, Debug)]
pub enum TExpr {
    Input(usize),          // task input array (work-item index read)
    Const(f64),
    Add(Box<TExpr>, Box<TExpr>),
    Sub(Box<TExpr>, Box<TExpr>),
    Mul(Box<TExpr>, Box<TExpr>),
    Div(Box<TExpr>, Box<TExpr>),
    Sqrt(Box<TExpr>),
}

#[derive(Clone, Debug)]
pub struct TaskKernel {
    pub pc: usize,         // task body entry instruction
    pub ninputs: usize,
    pub expr: TExpr,
    pub name: String,      // kernel name for SPIR-V registration
    pub glsl: String,
}

fn expr_to_glsl(e: &TExpr, ins: &[String]) -> String {
    match e {
        TExpr::Input(i) => format!("in{}[i]", i),
        TExpr::Const(v) => {
            let s = format!("{:.17e}", v);
            s
        }
        TExpr::Add(a, b) => format!("({} + {})", expr_to_glsl(a, ins), expr_to_glsl(b, ins)),
        TExpr::Sub(a, b) => format!("({} - {})", expr_to_glsl(a, ins), expr_to_glsl(b, ins)),
        TExpr::Mul(a, b) => format!("({} * {})", expr_to_glsl(a, ins), expr_to_glsl(b, ins)),
        TExpr::Div(a, b) => format!("({} / {})", expr_to_glsl(a, ins), expr_to_glsl(b, ins)),
        TExpr::Sqrt(a) => format!("sqrt({})", expr_to_glsl(a, ins)),
    }
}

pub fn task_glsl(name: &str, ninputs: usize, expr: &TExpr) -> String {
    let mut s = String::new();
    s.push_str("#version 450\n");
    s.push_str("#extension GL_EXT_shader_explicit_arithmetic_types_float64 : require\n");
    s.push_str("#extension GL_EXT_shader_explicit_arithmetic_types_int64 : require\n");
    s.push_str("layout(local_size_x=256) in;\n");
    s.push_str("layout(push_constant) uniform PC { int64_t n0, n1, n2, n3; float64_t s; int64_t rev; } pc;\n");
    for i in 0..ninputs {
        s.push_str(&format!("layout(std430, binding={}) buffer I{} {{ float64_t in{}[]; }};\n", i, i, i));
    }
    s.push_str(&format!("layout(std430, binding={}) buffer R {{ float64_t r[]; }};\n", ninputs));
    s.push_str("void main() {\n");
    s.push_str("  int i = int(gl_GlobalInvocationID.x);\n");
    s.push_str("  if (int64_t(i) >= pc.n0) return;\n");
    s.push_str(&format!("  r[i] = {};\n", expr_to_glsl(expr, &[])));
    s.push_str("}\n");
    let _ = name;
    s
}

/// Analyze all weave tasks in a parsed program; returns the compilable ones.
pub fn analyze_tasks(p: &crate::ast::Parsed) -> Vec<TaskKernel> {
    use crate::ast::Ins;
    use std::collections::HashMap;
    // collect task entry pcs from every Weave
    let mut task_pcs: Vec<(usize, String)> = Vec::new();
    for ins in &p.ins {
        if let Ins::Weave(ms) = ins {
            for m in ms {
                task_pcs.push((m.pc, m.name.clone()));
            }
        }
    }
    let mut out = Vec::new();
    let dbg = std::env::var("UF_DEBUG_TASK").is_ok();
    for (tpc, tname) in task_pcs {
        if let Some((ninputs, expr)) = analyze_one_task(p, tpc) {
            let name = format!("task_{}", fnv(&tname));
            let glsl = task_glsl(&name, ninputs, &expr);
            if dbg { eprintln!("[task] {} FUSED ({} inputs)", tname, ninputs); }
            out.push(TaskKernel { pc: tpc, ninputs, expr, name, glsl });
        } else if dbg {
            let stop = analyze_stop_at(p, tpc);
            eprintln!("[task] {} declined near ins {} of {}: {:?}", tname, stop.0, stop.1, stop.2.map(|x| format!("{:?}", x)).unwrap_or("end".into()));
            if std::env::var("UF_DEBUG_TASK2").is_ok() {
                for j in tpc..(tpc+10).min(p.ins.len()) { eprintln!("   ins[{}] = {:?}", j, p.ins[j]); }
            }
        }
    }
    out
}

/// Debug helper: where does the simulation stop?
fn analyze_stop_at(p: &crate::ast::Parsed, tpc: usize) -> (usize, usize, Option<crate::ast::Ins>) {
    use crate::ast::Ins;
    let end = p.ins.len();
    let mut i = tpc;
    while i < end {
        match &p.ins[i] {
            Ins::Ret => return (i, end, None),
            _ => {}
        }
        i += 1;
    }
    (tpc, end, p.ins.get(tpc).cloned())
}

#[allow(dead_code)]
fn task_dbg_none(l: u32) -> Option<(usize, TExpr)> {
    if std::env::var("UF_DEBUG_TASK").is_ok() {
        eprintln!("[task] declined at compute.rs:{}", l);
    }
    None
}

fn analyze_one_task(p: &crate::ast::Parsed, tpc: usize) -> Option<(usize, TExpr)> {
    use crate::ast::Ins;
    use std::collections::HashMap;
    #[derive(Clone)]
    enum V {
        E(TExpr),
        Opaque,
    }
    #[derive(Hash, PartialEq, Eq)]
    enum Slot {
        Id(usize),
        Name(String),
    }
    let mut stack: Vec<V> = Vec::new();
    let mut locals: HashMap<Slot, V> = HashMap::new();
    // input binds: the guarded param pops at the task start. They bind in
    // reverse (first bind pops the last-pushed input). Task labels don't get
    // a label_params entry (only plain labels do), so count the leading
    // guarded binds directly.
    let mut params = 0usize;
    while p.param_pcs.contains_key(&(tpc + params)) {
        params += 1;
    }
    let mut inputs_left = params;
    let mut i = tpc;
    let end = p.ins.len();
    while i < end {
        let ins = &p.ins[i];
        // stop at the task's ret
        match ins {
            Ins::Ret => break,
            _ => {}
        }
        if p.param_pcs.contains_key(&i) {
            // guarded input bind (name-based pre-resolution or resolved slot)
            if inputs_left == 0 {
                return task_dbg_none(line!());
            }
            inputs_left -= 1;
            match ins {
                Ins::LocalSetI(id) => {
                    locals.insert(Slot::Id(*id), V::E(TExpr::Input(inputs_left)));
                }
                Ins::LocalSet(name) => {
                    locals.insert(Slot::Name(name.clone()), V::E(TExpr::Input(inputs_left)));
                }
                _ => return None, // global binds not supported in fused tasks
            }
            i += 1;
            continue;
        }
        match ins {
            Ins::LocalGetI(id) => match locals.get(&Slot::Id(*id)) {
                Some(v) => stack.push(v.clone()),
                None => return None, // reads an unbound slot (e.g. loop k) -> decline
            },
            Ins::LocalGet(name) => match locals.get(&Slot::Name(name.clone())) {
                Some(v) => stack.push(v.clone()),
                None => return None,
            },
            Ins::PushF(v) => stack.push(V::E(TExpr::Const(*v))),
            Ins::PushI(v) => stack.push(V::E(TExpr::Const(*v as f64))),
            Ins::Simple(h) => match *h {
                "op_add" | "op_sub" | "op_mul" | "op_div" | "op_sqrt" => {
                    let (a, b) = if *h == "op_sqrt" {
                        let x = match stack.pop() {
                            Some(V::E(e)) => e,
                            _ => return None,
                        };
                        (TExpr::Input(usize::MAX), x) // placeholder; handled below
                    } else {
                        let bv = match stack.pop() { Some(v) => v, None => return task_dbg_none(line!()) };
                        let av = match stack.pop() { Some(v) => v, None => return task_dbg_none(line!()) };
                        let (ae, be) = match (av, bv) {
                            (V::E(ae), V::E(be)) => (ae, be),
                            _ => return None,
                        };
                        (ae, be)
                    };
                    let e = match *h {
                        "op_add" => TExpr::Add(Box::new(a.clone()), Box::new(b.clone())),
                        "op_sub" => TExpr::Sub(Box::new(a.clone()), Box::new(b.clone())),
                        "op_mul" => TExpr::Mul(Box::new(a.clone()), Box::new(b.clone())),
                        "op_div" => TExpr::Div(Box::new(a.clone()), Box::new(b.clone())),
                        _ => TExpr::Sqrt(Box::new(b)),
                    };
                    stack.push(V::E(e));
                }
                _ => return None, // any other op declines fusion
            },
            Ins::LocalSetI(id) => {
                let v = match stack.pop() { Some(v) => v, None => return task_dbg_none(line!()) };
                locals.insert(Slot::Id(*id), v);
            }
            Ins::LocalSet(name) => {
                let v = match stack.pop() { Some(v) => v, None => return task_dbg_none(line!()) };
                locals.insert(Slot::Name(name.clone()), v);
            }
            Ins::Flush => { /* ret-operand boundary; simulation continues */ }
            _ => return None, // control flow, calls, literals-as-addrs, etc.
        }
        i += 1;
    }
    if inputs_left != 0 {
        return task_dbg_none(line!());
    }
    // final value: top of stack at ret
    let final_v = match stack.pop() { Some(v) => v, None => return task_dbg_none(line!()) };
    let expr = match final_v {
        V::E(e) => e,
        V::Opaque => return None,
    };
    // fused tasks must actually use at least one input
    let uses_input = match &expr {
        _ => true,
    };
    if params == 0 && !uses_input {
        return task_dbg_none(line!());
    }
    Some((params, expr))
}

pub fn static_kernel_count() -> usize {
    shader_library().len()
}

// registry consumed by gen.rs (single-threaded compiler)
static TASK_KERNELS: std::sync::OnceLock<Vec<TaskKernel>> = std::sync::OnceLock::new();
pub fn register_task_kernels(ks: Vec<TaskKernel>) {
    let _ = TASK_KERNELS.set(ks);
}
pub fn task_kernel_at(pc: usize) -> Option<(usize, usize)> {
    // returns (kernel_table_index, ninputs)
    TASK_KERNELS.get().and_then(|ks| {
        ks.iter().position(|k| k.pc == pc).map(|i| (static_kernel_count() + i, ks[i].ninputs))
    })
}

// ---------------- elementwise region fusion (v13.2) ----------------
// A maximal straight-line run of eligible instructions ANYWHERE (not just
// weave task bodies) whose net effect is
//     out := expr(in0[i], in1[i], ...)      (elementwise + - * / sqrt, consts)
// is a fusable REGION: gen.rs emits one guarded fused C loop (CPU) plus one
// fused kernel launch (GPU) at the region head, falling back to the original
// per-op code whenever the runtime shapes don't cooperate. Same TExpr and
// kernel shape as weave-task fusion — this generalizes it to plain code.

#[derive(Clone, Debug)]
pub enum OutBind {
    Local(usize),
    Shared(String),
}

#[derive(Clone, Debug)]
pub struct RegionKernel {
    pub pc: usize,          // first instruction of the region
    pub end_pc: usize,      // first instruction AFTER the region (goto target)
    pub inputs: Vec<RegionInput>, // region inputs (locals or globals)
    pub exprs: Vec<(OutBind, TExpr)>, // live-outs (binding, elementwise expr), 1..=4
    pub name: String,
    pub glsl: String,
}

#[derive(Clone, Debug)]
pub enum RegionInput {
    Local(usize),
    Global(String),
}

static REGION_KERNELS: std::sync::OnceLock<Vec<RegionKernel>> = std::sync::OnceLock::new();
pub fn register_region_kernels(ks: Vec<RegionKernel>) {
    let _ = REGION_KERNELS.set(ks);
}

/// Everything gen.rs needs to emit the fused block for a region at `pc`.
pub struct RegionGen {
    pub kidx: usize,             // kernel table index (after static + task kernels)
    pub inputs: Vec<RegionInput>, // locals (cx->locals[base+id]) or shared (uf_sh_get(&var_name))
    pub end_pc: usize,           // goto target after the fused block
    pub out_locals: Vec<OutBind>, // live-out bindings (1..=4)
    pub c_exprs: Vec<String>,    // elementwise exprs as C over _v0.._v(n-1)
}
/// All registered (pc, end_pc) ranges — gen jumps over them when the fused
/// block succeeds; the covered instructions remain the decline path.
pub fn region_ranges() -> Vec<(usize, usize)> {
    REGION_KERNELS
        .get()
        .map(|ks| ks.iter().map(|k| (k.pc, k.end_pc)).collect())
        .unwrap_or_default()
}
pub fn region_at(pc: usize) -> Option<RegionGen> {
    REGION_KERNELS.get().and_then(|ks| {
        ks.iter().position(|k| k.pc == pc).map(|i| {
            let base = static_kernel_count()
                + TASK_KERNELS.get().map(|t| t.len()).unwrap_or(0);
            RegionGen {
                kidx: base + i,
                inputs: ks[i].inputs.clone(),
                end_pc: ks[i].end_pc,
                out_locals: ks[i].exprs.iter().map(|(b, _)| b.clone()).collect(),
                c_exprs: ks[i].exprs.iter().map(|(_, e)| expr_to_c(e)).collect(),
            }
        })
    })
}

fn expr_uses_input(e: &TExpr) -> bool {
    match e {
        TExpr::Input(_) => true,
        TExpr::Const(_) => false,
        TExpr::Add(a, b) | TExpr::Sub(a, b) | TExpr::Mul(a, b) | TExpr::Div(a, b) => {
            expr_uses_input(a) || expr_uses_input(b)
        }
        TExpr::Sqrt(a) => expr_uses_input(a),
    }
}

fn expr_has_div(e: &TExpr) -> bool {
    match e {
        TExpr::Input(_) | TExpr::Const(_) => false,
        TExpr::Div(_, _) => true,
        TExpr::Add(a, b) | TExpr::Sub(a, b) | TExpr::Mul(a, b) => expr_has_div(a) || expr_has_div(b),
        TExpr::Sqrt(a) => expr_has_div(a),
    }
}

/// TExpr as C over per-element locals _v0.._v(n-1) (mirrors expr_to_glsl).
/// Division carries an inline zero-divisor check so the fused loop keeps the
/// per-op `die("div: zero divisor")` semantics exactly (GNU statement expr).
pub fn expr_to_c(e: &TExpr) -> String {
    match e {
        TExpr::Input(i) => format!("_v{}", i),
        TExpr::Const(v) => format!("{:?}", v),
        TExpr::Add(a, b) => format!("({} + {})", expr_to_c(a), expr_to_c(b)),
        TExpr::Sub(a, b) => format!("({} - {})", expr_to_c(a), expr_to_c(b)),
        TExpr::Mul(a, b) => format!("({} * {})", expr_to_c(a), expr_to_c(b)),
        TExpr::Div(a, b) => format!(
            "({} / ({{double _d={}; if(_d==0.0)die(\"div: zero divisor\"); _d;}}))",
            expr_to_c(a),
            expr_to_c(b)
        ),
        TExpr::Sqrt(a) => format!("sqrt({})", expr_to_c(a)),
    }
}

/// GLSL for a multi-output elementwise kernel: bindings 0..n are inputs,
/// n..n+k are outputs r_j[i] = expr_j[i]. Same header/layout as task_glsl.
pub fn region_glsl(ninputs: usize, exprs: &[(OutBind, TExpr)]) -> String {
    let mut s = String::new();
    s.push_str("#version 450\n");
    s.push_str("#extension GL_EXT_shader_explicit_arithmetic_types_float64 : require\n");
    s.push_str("#extension GL_EXT_shader_explicit_arithmetic_types_int64 : require\n");
    s.push_str("layout(local_size_x=256) in;\n");
    s.push_str("layout(push_constant) uniform PC { int64_t n0, n1, n2, n3; float64_t s; int64_t rev; } pc;\n");
    for i in 0..ninputs {
        s.push_str(&format!("layout(std430, binding={}) buffer I{} {{ float64_t in{}[]; }};\n", i, i, i));
    }
    for j in 0..exprs.len() {
        s.push_str(&format!("layout(std430, binding={}) buffer R{} {{ float64_t r{}[]; }};\n", ninputs + j, j, j));
    }
    s.push_str("void main() {\n");
    s.push_str("  int i = int(gl_GlobalInvocationID.x);\n");
    s.push_str("  if (int64_t(i) >= pc.n0) return;\n");
    for (j, (_, e)) in exprs.iter().enumerate() {
        s.push_str(&format!("  r{}[i] = {};\n", j, expr_to_glsl(e, &[])));
    }
    s.push_str("}\n");
    s
}

/// Analyze the whole program for fusable elementwise regions.
pub fn analyze_regions(p: &crate::ast::Parsed) -> Vec<RegionKernel> {
    use std::collections::HashSet;
    let targets: HashSet<usize> = p.labels.values().copied().collect();
    let dbg = std::env::var("UF_DEBUG_REGION").is_ok() || std::env::var("NKR_DEBUG_REGION").is_ok();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < p.ins.len() {
        if let Some((end_pc, inputs, exprs)) = walk_one_region(p, i, &targets) {
            let name = format!("region_{}", fnv(&format!("pc{}", i)));
            let glsl = region_glsl(inputs.len(), &exprs);
            if dbg {
                let outs: Vec<String> = exprs.iter().map(|(b, _)| match b {
                    OutBind::Local(id) => format!("L{}", id),
                    OutBind::Shared(n) => format!("S:{}", n),
                }).collect();
                eprintln!(
                    "[region] pc {}..{} FUSED ({} inputs, outs [{}])",
                    i, end_pc, inputs.len(), outs.join(",")
                );
            }
            out.push(RegionKernel { pc: i, end_pc, inputs, exprs, name, glsl });
            i = end_pc;
        } else {
            i += 1;
        }
    }
    out
}

fn slot_read_later(p: &crate::ast::Parsed, id: usize, name: &str, from: usize) -> bool {
    use crate::ast::Ins;
    for ins in &p.ins[from.min(p.ins.len())..] {
        match ins {
            Ins::LocalGetI(i) if *i == id => return true,
            Ins::LocalGet(n) if !name.is_empty() && n == name => return true,
            Ins::GetV(n) if !name.is_empty() && n == name => return true,
            _ => {}
        }
    }
    false
}

// A region input as the analyzer sees it. Phantom marks the constant list
// consumed by an unrolled array_reduce — filtered before registration.
#[derive(Clone, PartialEq)]
enum InSlot {
    Loc(usize),
    Glob(String),
    Phantom,
}

/// Resolve the constant float list bound to local `list_slot` (v13: most
/// recent LocalSetI) or shared var `list_name` (v14: most recent SetV) at or
/// before instruction `before`. The binding must be fed by a literal-only
/// ListLit ([ c0 c1 ... ] name!). Returns the constants in source order.
fn const_list_of(p: &crate::ast::Parsed, list_slot: usize, list_name: &str, before: usize) -> Option<Vec<f64>> {
    use crate::ast::Ins;
    let mut j = before;
    while j > 0 {
        j -= 1;
        let matches_binding = match &p.ins[j] {
            Ins::LocalSetI(id) => *id == list_slot,
            Ins::SetV(n) => !list_name.is_empty() && n == list_name,
            _ => false,
        };
        if matches_binding {
            if j == 0 || !matches!(&p.ins[j - 1], Ins::ListLit) {
                return None;
            }
            let mut k = j - 1;
            while k > 0 && matches!(&p.ins[k - 1], Ins::PushF(_) | Ins::PushI(_)) {
                k -= 1;
            }
            if k == 0 || !matches!(&p.ins[k - 1], Ins::ListStart) {
                return None;
            }
            let mut out = Vec::new();
            for ins in &p.ins[k..j - 1] {
                match ins {
                    Ins::PushF(v) => out.push(*v),
                    Ins::PushI(v) => out.push(*v as f64),
                    _ => return None,
                }
            }
            if out.is_empty() || out.len() > 64 {
                return None;
            }
            return Some(out);
        }
    }
    None
}

/// Simulate one fold-body invocation symbolically: initial stack [acc, elem],
/// eligible instructions only (arith, literals, GetV globals, simple local
/// binds), terminating at Ret with exactly one value. Returns the new acc.
fn simulate_fold_body(
    p: &crate::ast::Parsed,
    body_pc: usize,
    acc: TExpr,
    elem: f64,
    inputs: &mut Vec<InSlot>,
    ops: &mut usize,
) -> Option<TExpr> {
    use crate::ast::Ins;
    #[derive(Clone, PartialEq, Eq, Hash)]
    enum BSlot {
        Id(usize),
        Name(String),
    }
    #[derive(Clone)]
    enum V {
        E(TExpr),
        Opaque,
    }
    let mut stack: Vec<V> = vec![V::E(acc), V::E(TExpr::Const(elem))];
    let mut locals: std::collections::HashMap<BSlot, V> = std::collections::HashMap::new();
    let mut i = body_pc;
    while i < p.ins.len() && i < body_pc + 128 {
        match &p.ins[i] {
            Ins::Ret => break,
            Ins::Flush => {}
            Ins::PushF(v) => stack.push(V::E(TExpr::Const(*v))),
            Ins::PushI(v) => stack.push(V::E(TExpr::Const(*v as f64))),
            Ins::GetV(v) => {
                let slot = InSlot::Glob(v.clone());
                let idx = inputs.iter().position(|s| *s == slot).unwrap_or_else(|| {
                    inputs.push(slot);
                    inputs.len() - 1
                });
                stack.push(V::E(TExpr::Input(idx)));
            }
            Ins::LocalGetI(id) => match locals.get(&BSlot::Id(*id)) {
                Some(v) => stack.push(v.clone()),
                None => return None,
            },
            Ins::LocalGet(n) => match locals.get(&BSlot::Name(n.clone())) {
                Some(v) => stack.push(v.clone()),
                None => return None,
            },
            Ins::LocalSetI(id) => match stack.pop() {
                Some(V::E(e)) => {
                    locals.insert(BSlot::Id(*id), V::E(e));
                }
                _ => return None,
            },
            Ins::LocalSet(n) => match stack.pop() {
                Some(V::E(e)) => {
                    locals.insert(BSlot::Name(n.clone()), V::E(e));
                }
                _ => return None,
            },
            Ins::Simple(h) => match *h {
                "op_add" | "op_sub" | "op_mul" | "op_div" => {
                    match (stack.pop(), stack.pop()) {
                        (Some(V::E(be)), Some(V::E(ae))) => {
                            stack.push(V::E(match *h {
                                "op_add" => TExpr::Add(Box::new(ae), Box::new(be)),
                                "op_sub" => TExpr::Sub(Box::new(ae), Box::new(be)),
                                "op_mul" => TExpr::Mul(Box::new(ae), Box::new(be)),
                                _ => TExpr::Div(Box::new(ae), Box::new(be)),
                            }));
                            *ops += 1;
                        }
                        _ => return None,
                    }
                }
                "op_sqrt" => match stack.pop() {
                    Some(V::E(ae)) => {
                        stack.push(V::E(TExpr::Sqrt(Box::new(ae))));
                        *ops += 1;
                    }
                    _ => return None,
                },
                _ => return None,
            },
            other => {
                if std::env::var("NKR_DEBUG_REGION2").is_ok() { eprintln!("[fold-sim] ineligible: {:?}", other); }
                return None;
            }
        }
        i += 1;
    }
    if stack.len() != 1 {
        if std::env::var("NKR_DEBUG_REGION2").is_ok() { eprintln!("[fold-sim] stack={}", stack.len()); }
        return None;
    }
    match stack.pop() {
        Some(V::E(e)) => Some(e),
        _ => {
            if std::env::var("NKR_DEBUG_REGION2").is_ok() { eprintln!("[fold-sim] opaque"); }
            None
        }
    }
}

/// Renumber TExpr::Input indices after Phantom inputs are dropped; None if
/// the expression still references a dropped input (should not happen).
fn remap_input(e: &TExpr, map: &[Option<usize>]) -> Option<TExpr> {
    match e {
        TExpr::Input(i) => map.get(*i).copied().flatten().map(TExpr::Input),
        TExpr::Const(v) => Some(TExpr::Const(*v)),
        TExpr::Add(a, b) => Some(TExpr::Add(
            Box::new(remap_input(a, map)?),
            Box::new(remap_input(b, map)?),
        )),
        TExpr::Sub(a, b) => Some(TExpr::Sub(
            Box::new(remap_input(a, map)?),
            Box::new(remap_input(b, map)?),
        )),
        TExpr::Mul(a, b) => Some(TExpr::Mul(
            Box::new(remap_input(a, map)?),
            Box::new(remap_input(b, map)?),
        )),
        TExpr::Div(a, b) => Some(TExpr::Div(
            Box::new(remap_input(a, map)?),
            Box::new(remap_input(b, map)?),
        )),
        TExpr::Sqrt(a) => Some(TExpr::Sqrt(Box::new(remap_input(a, map)?))),
    }
}

fn walk_one_region(
    p: &crate::ast::Parsed,
    start: usize,
    targets: &std::collections::HashSet<usize>,
) -> Option<(usize, Vec<RegionInput>, Vec<(OutBind, TExpr)>)> {
    use crate::ast::Ins;
    use std::collections::HashMap;
    #[derive(Clone, PartialEq, Eq, Hash)]
    enum Slot {
        Id(usize),
        Name(String),
    }
    #[derive(Clone)]
    enum V {
        E(TExpr),
        Opaque,
    }
    let mut stack: Vec<V> = Vec::new();
    let mut locals: HashMap<Slot, V> = HashMap::new();
    let mut inputs: Vec<InSlot> = Vec::new();
    let mut written: Vec<Slot> = Vec::new();
    let mut ops = 0usize;
    let mut final_out: Option<Slot> = None;
    // last point where the simulated stack was empty (a statement boundary).
    // If the walk stops mid-statement (e.g. it absorbed the leading Gets of
    // the NEXT statement before hitting an ineligible op), the region ends
    // here instead — the trailing instructions simply re-run per-op.
    let mut clean: Option<(usize, Vec<InSlot>, Vec<Slot>, usize, HashMap<Slot, V>, Option<Slot>)> =
        None;
    let mut i = start;
    while i < p.ins.len() {
        if i > start && targets.contains(&i) {
            break;
        }
        // constant-list array_reduce unrolling: PushAddr(label) immediately
        // followed by op_vfold whose list operand is a bound literal-only
        // list expands to N body simulations (elem = each constant), giving
        // one fused expression — the source keeps its fold shape.
        if let Ins::PushAddr(l) = &p.ins[i] {
            let vfold_next = matches!(p.ins.get(i + 1), Some(Ins::Simple("op_vfold")))
                || (matches!(p.ins.get(i + 1), Some(Ins::Flush))
                    && matches!(p.ins.get(i + 2), Some(Ins::Simple("op_vfold"))));
            if vfold_next {
                // vfold operand order is `arr init fn` (SPEC): at the fold the
                // stack holds [.., arr, init] — init is on top.
                let accv = match stack.pop() {
                    Some(V::E(e)) => e,
                    _ => break,
                };
                let listv = match stack.pop() {
                    Some(V::E(e)) => e,
                    _ => break,
                };
                // the list operand must be a plain region input (a local)
                let list_idx = match &listv {
                    TExpr::Input(idx) => *idx,
                    _ => break,
                };
                let (list_slot, list_name) = match inputs.get(list_idx) {
                    Some(InSlot::Loc(id)) => (*id, String::new()),
                    Some(InSlot::Glob(name)) => (usize::MAX, name.clone()),
                    _ => break,
                };
                let consts = match const_list_of(p, list_slot, &list_name, i) {
                    Some(c) => c,
                    None => break,
                };
                let body_pc = match p.labels.get(l) {
                    Some(pc) => *pc,
                    None => break,
                };
                // unroll: acc = body(acc, c_k) for each constant
                let mut acc = accv;
                let mut unfolded = false;
                for c in consts {
                    match simulate_fold_body(p, body_pc, acc.clone(), c, &mut inputs, &mut ops) {
                        Some(next) => acc = next,
                        None => break,
                    }
                    unfolded = true;
                }
                if !unfolded {
                    break;
                }
                inputs[list_idx] = InSlot::Phantom;
                stack.push(V::E(acc));
                // skip PushAddr (+ Flush when present) + op_vfold
                i += if matches!(p.ins.get(i + 1), Some(Ins::Flush)) { 3 } else { 2 };
                if stack.is_empty() {
                    clean = Some((i, inputs.clone(), written.clone(), ops, locals.clone(), final_out.clone()));
                }
                continue;
            }
            break;
        }
        let stop;
        match &p.ins[i] {
            Ins::Flush => {
                stop = false;
            }
            Ins::PushF(v) => {
                stack.push(V::E(TExpr::Const(*v)));
                stop = false;
            }
            Ins::PushI(v) => {
                stack.push(V::E(TExpr::Const(*v as f64)));
                stop = false;
            }
            Ins::LocalGetI(id) => {
                let slot = Slot::Id(*id);
                match locals.get(&slot) {
                    Some(v) => stack.push(v.clone()),
                    None => {
                        let idx = match inputs.iter().position(|s| *s == InSlot::Loc(*id)) {
                            Some(idx) => idx,
                            None => {
                                inputs.push(InSlot::Loc(*id));
                                inputs.len() - 1
                            }
                        };
                        stack.push(V::E(TExpr::Input(idx)));
                    }
                }
                stop = false;
            }
            Ins::LocalGet(n) => {
                let slot = Slot::Name(n.clone());
                match locals.get(&slot) {
                    Some(v) => stack.push(v.clone()),
                    None => break, // unresolved named read — don't fuse
                }
                stop = false;
            }
            Ins::GetV(n) => {
                let slot = Slot::Name(n.clone());
                match locals.get(&slot) {
                    Some(v) => stack.push(v.clone()),
                    None => {
                        let islot = InSlot::Glob(n.clone());
                        let idx = match inputs.iter().position(|s| *s == islot) {
                            Some(idx) => idx,
                            None => {
                                inputs.push(islot);
                                inputs.len() - 1
                            }
                        };
                        stack.push(V::E(TExpr::Input(idx)));
                    }
                }
                stop = false;
            }
            Ins::Simple(h) => match *h {
                "op_add" | "op_sub" | "op_mul" | "op_div" => {
                    match (stack.pop(), stack.pop()) {
                        (Some(V::E(be)), Some(V::E(ae))) => {
                            stack.push(V::E(match *h {
                                "op_add" => TExpr::Add(Box::new(ae), Box::new(be)),
                                "op_sub" => TExpr::Sub(Box::new(ae), Box::new(be)),
                                "op_mul" => TExpr::Mul(Box::new(ae), Box::new(be)),
                                _ => TExpr::Div(Box::new(ae), Box::new(be)),
                            }));
                            ops += 1;
                            stop = false;
                        }
                        _ => stop = true,
                    }
                }
                "op_sqrt" => match stack.pop() {
                    Some(V::E(ae)) => {
                        stack.push(V::E(TExpr::Sqrt(Box::new(ae))));
                        ops += 1;
                        stop = false;
                    }
                    _ => stop = true,
                },
                _ => stop = true,
            },
            Ins::LocalSetI(id) => {
                match stack.pop() {
                    Some(V::E(e)) => {
                        let slot = Slot::Id(*id);
                        locals.insert(slot.clone(), V::E(e));
                        if !written.contains(&slot) {
                            written.push(slot.clone());
                        }
                        final_out = Some(slot);
                        stop = false;
                    }
                    _ => stop = true,
                }
            }
            Ins::LocalSet(n) => {
                match stack.pop() {
                    Some(V::E(e)) => {
                        let slot = Slot::Name(n.clone());
                        locals.insert(slot.clone(), V::E(e));
                        if !written.contains(&slot) {
                            written.push(slot.clone());
                        }
                        final_out = Some(slot);
                        stop = false;
                    }
                    _ => stop = true,
                }
            }
            Ins::SetV(n) => {
                match stack.pop() {
                    Some(V::E(e)) => {
                        let slot = Slot::Name(n.clone());
                        locals.insert(slot.clone(), V::E(e));
                        if !written.contains(&slot) {
                            written.push(slot.clone());
                        }
                        final_out = Some(slot);
                        stop = false;
                    }
                    _ => stop = true,
                }
            }
            _ => stop = true,
        }
        if stop {
            break;
        }
        if stack.is_empty() {
            clean = Some((i + 1, inputs.clone(), written.clone(), ops, locals.clone(), final_out.clone()));
        }
        i += 1;
    }
    let mut end_pc = i;
    if !stack.is_empty() {
        // stopped mid-statement: fall back to the last clean boundary
        match clean {
            Some((cp, cin, cwr, cops, clocals, cout)) => {
                end_pc = cp;
                inputs = cin;
                written = cwr;
                ops = cops;
                locals = clocals;
                final_out = cout;
                stack.clear();
            }
            None => return None,
        }
    }
    if std::env::var("NKR_DEBUG_REGION2").is_ok() {
        let ctx: Vec<String> = (start..((start+6).min(p.ins.len()))).map(|k| format!("{:?}", p.ins[k])).collect();
        eprintln!("[region?] start={} end={} stack={} ops={} inputs={} ctx={:?}",
            start, end_pc, stack.len(), ops, inputs.len(), ctx);
    }
    if end_pc == start || !stack.is_empty() || ops == 0 || inputs.is_empty() {
        return None;
    }
    // the region must end on a local bind (statement boundary after a Set)
    if !matches!(&p.ins[end_pc - 1], Ins::LocalSetI(_) | Ins::LocalSet(_) | Ins::SetV(_)) {
        return None;
    }
    // live-outs: every slot written in the region that is read at or after
    // end_pc, plus the final bind. Slots never read again are internal
    // temporaries. (Cross-body slot-id collisions over-promote to live-out,
    // which is safe — just an extra output.) Cap 4 outputs.
    let mut live: Vec<Slot> = Vec::new();
    for slot in &written {
        let (id, name) = match slot {
            Slot::Id(id) => (*id, ""),
            Slot::Name(n) => (usize::MAX, n.as_str()),
        };
        if Some(slot) == final_out.as_ref() || slot_read_later(p, id, name, end_pc) {
            if !live.contains(slot) {
                live.push(slot.clone());
            }
        }
    }
    if std::env::var("NKR_DEBUG_REGION2").is_ok() {
        let lv: Vec<String> = live.iter().map(|x| match x { Slot::Id(i)=>format!("L{}",i), Slot::Name(n)=>format!("S:{}",n) }).collect();
        eprintln!("[region?] start={} live={:?}", start, lv);
    }
    if live.is_empty() || live.len() > 4 {
        return None;
    }
    let mut exprs: Vec<(OutBind, TExpr)> = Vec::new();
    for slot in &live {
        let bind = match slot {
            Slot::Id(id) => OutBind::Local(*id),
            Slot::Name(n) => OutBind::Shared(n.clone()),
        };
        let e = match locals.get(slot) {
            Some(V::E(e)) => e.clone(),
            _ => return None,
        };
        exprs.push((bind, e));
    }
    if !exprs.iter().any(|(_, e)| expr_uses_input(e)) {
        return None;
    }
    // drop Phantom inputs (consumed constant lists) and renumber
    let mut real: Vec<RegionInput> = Vec::new();
    let mut map: Vec<Option<usize>> = Vec::with_capacity(inputs.len());
    for s in &inputs {
        match s {
            InSlot::Phantom => map.push(None),
            InSlot::Loc(id) => {
                map.push(Some(real.len()));
                real.push(RegionInput::Local(*id));
            }
            InSlot::Glob(name) => {
                map.push(Some(real.len()));
                real.push(RegionInput::Global(name.clone()));
            }
        }
    }
    if real.is_empty() || real.len() > 7 || real.len() + exprs.len() > 8 {
        return None;
    }
    let exprs = exprs
        .into_iter()
        .map(|(id, e)| remap_input(&e, &map).map(|e| (id, e)))
        .collect::<Option<Vec<_>>>()?;
    Some((end_pc, real, exprs))
}
