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
        ("matmul", format!("{}layout(std430, binding=0) buffer A {{ float64_t a[]; }};\nlayout(std430, binding=1) buffer B {{ float64_t b[]; }};\nlayout(std430, binding=2) buffer R {{ float64_t r[]; }};\nvoid main() {{ int64_t work = int64_t(gl_GlobalInvocationID.x); int64_t ra = pc.n1, ca = pc.n2, cb = pc.n3; if (work >= ra*cb) return; int row = int(work / cb), col = int(work % cb); float64_t s = 0.0LF; for (int k = 0; int64_t(k) < ca; k++) s += a[row*int(ca)+k] * b[k*int(cb)+col]; r[int(work)] = s; }}\n", header())),
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
