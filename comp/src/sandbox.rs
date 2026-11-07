// sandbox.rs — capability model, .ufs config parsing, effective-policy resolution.
//
// A capability config (.ufs) is line-based:
//   ; comment
//   deny  <cap...>        — deny capabilities (`*`, `fs.*` wildcards)
//   allow <cap...>        — exceptions to denies (specificity wins: exact > prefix.* > *)
//   workspace <path...>   — filesystem roots for fs.* (repeatable)
//   allow-module <name..> — modules USE may load (repeatable; default all)
//   deny-module <name..>  — modules USE may never load
//   [policy NAME]         — start a named policy section (selectable via --policy)
//
// Runtime configs (`--sandbox FILE`) may only tighten the baked policy: a
// capability denied anywhere stays denied; workspace roots intersect; module
// allowlists intersect. Runtime files cannot define selectable policies.

use crate::user_err;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const CAP_NAMES: [&str; 8] = [
    "fs.read", "fs.write", "proc", "ffi.use", "ffi.import", "raw.syscall", "raw.mem", "host.argv",
];

// helper name (op_*) -> capability. None = ungated op.
pub fn op_capability(helper: &str) -> Option<&'static str> {
    Some(match helper {
        "op_slurp" | "op_feach" | "op_ffold" | "op_fsplit" | "op_fmatch" | "op_walk" => "fs.read",
        // mmap maps a file read/write: needs both halves of fs
        "op_mmap" => "fs.mmap",
        "op_spit" | "op_femit" => "fs.write",
        "op_sh" | "op_shp" | "op_exec" => "proc",
        "op_malloc" | "op_free" | "op_buf" | "op_bufcopy" | "op_loadx" | "op_storex" => "raw.mem",
        "op_argv" | "op_hasargs" | "op_argi" => "host.argv",
        _ => return None,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tri {
    Allowed,
    Denied,
}

#[derive(Clone, Debug, Default)]
pub struct Rules {
    allow: Vec<String>,
    deny: Vec<String>,
    workspace: Vec<String>,
    allow_module: Vec<String>,
    deny_module: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SandboxFile {
    base: Rules,
    pub policies: HashMap<String, Rules>,
    pub name: Option<String>,
}

// ---- pattern matching ----
// specificity: 2 = exact name, 1 = prefix wildcard (fs.*), 0 = bare *
pub fn pattern_matches(pat: &str, cap: &str) -> Option<u8> {
    if pat == "*" {
        Some(0)
    } else if let Some(prefix) = pat.strip_suffix(".*") {
        if cap.starts_with(prefix) && cap[prefix.len()..].starts_with('.') {
            Some(1)
        } else {
            None
        }
    } else if pat == cap {
        Some(2)
    } else {
        None
    }
}

pub fn resolve_cap(rules: &Rules, cap: &str) -> Tri {
    let mut best_allow: Option<u8> = None;
    for p in &rules.allow {
        if let Some(s) = pattern_matches(p, cap) {
            if best_allow.map_or(true, |b| s > b) {
                best_allow = Some(s);
            }
        }
    }
    let mut best_deny: Option<u8> = None;
    for p in &rules.deny {
        if let Some(s) = pattern_matches(p, cap) {
            if best_deny.map_or(true, |b| s > b) {
                best_deny = Some(s);
            }
        }
    }
    match (best_deny, best_allow) {
        (Some(d), Some(a)) if a > d => Tri::Allowed,
        (Some(_), _) => Tri::Denied,
        _ => Tri::Allowed,
    }
}

// ---- .ufs parsing ----
fn parse_rules(lines: &[(usize, String)], file: &str) -> (Rules, Option<String>) {
    let mut r = Rules::default();
    let mut name = None;
    for (lineno, line) in lines {
        let line = line.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (key, rest) = match line.split_once(char::is_whitespace) {
            Some((k, v)) => (k, v.trim()),
            None => (line, ""),
        };
        let vals: Vec<String> = rest
            .split_whitespace()
            .map(|v| v.trim_matches('"').to_string())
            .collect();
        match key {
            "deny" => r.deny.extend(vals),
            "allow" => r.allow.extend(vals),
            "workspace" => r.workspace.extend(vals),
            "allow-module" => r.allow_module.extend(vals),
            "deny-module" => r.deny_module.extend(vals),
            "name" if vals.len() == 1 => name = Some(vals[0].clone()),
            _ => user_err!("sandbox", Some(*lineno as u32), "{}:{}: unknown config key `{}` (expected deny/allow/workspace/allow-module/deny-module/name)", file, lineno, key),
        }
    }
    (r, name)
}

pub fn parse_ufs(src: &str, file: &str) -> SandboxFile {
    let mut sf = SandboxFile::default();
    let mut section: Vec<(usize, String)> = Vec::new();
    let mut cur: Option<String> = None;
    let flush = |section: Vec<(usize, String)>, sf: &mut SandboxFile, cur: &mut Option<String>| {
        let flushed = section;
        if let Some(prev) = cur.take() {
            sf.policies.insert(prev, parse_rules(&flushed, file).0);
        } else {
            let (rules, name) = parse_rules(&flushed, file);
            sf.base = rules;
            if name.is_some() {
                sf.name = name;
            }
        }
    };
    for (i, raw) in src.lines().enumerate() {
        let line = raw.trim();
        if let Some(name) = line.strip_prefix("[policy ") {
            let name = name.trim_end_matches(']').trim().to_string();
            if name.is_empty() {
                user_err!("sandbox", Some((i + 1) as u32), "{}:{}: empty policy name", file, i + 1);
            }
            let flushed = std::mem::take(&mut section);
            flush(flushed, &mut sf, &mut cur);
            if sf.policies.contains_key(&name) {
                user_err!("sandbox", Some((i + 1) as u32), "{}:{}: duplicate policy `{}`", file, i + 1, name);
            }
            cur = Some(name);
        } else {
            section.push((i + 1, raw.to_string()));
        }
    }
    flush(std::mem::take(&mut section), &mut sf, &mut cur);
    sf
}

// ---- effective policy ----
#[derive(Clone, Debug)]
pub struct Caps {
    caps: HashMap<String, Tri>,
    // None = all modules allowed (subject to caps["ffi.use"]); Some = allowlist
    pub module_allow: Option<Vec<String>>,
    module_deny: Vec<String>,
    // None = no workspace restriction; Some(roots) = fs.* paths must resolve under a root
    pub workspace: Option<Vec<PathBuf>>,
    pub policy_name: String,
    pub origin: String,
    pub is_sandboxed: bool,
    // capabilities restored by an explicit --allow (reporting only)
    pub grants: Vec<String>,
}

impl Caps {
    pub fn unrestricted() -> Caps {
        Caps {
            caps: HashMap::new(),
            module_allow: None,
            module_deny: Vec::new(),
            workspace: None,
            policy_name: "none".to_string(),
            origin: "unrestricted build".to_string(),
            is_sandboxed: false,
            grants: Vec::new(),
        }
    }

    pub fn cap(&self, cap: &str) -> bool {
        // fs.mmap is the special op needing both fs.read and fs.write
        if cap == "fs.mmap" {
            return self.cap("fs.read") && self.cap("fs.write");
        }
        matches!(self.caps.get(cap), None | Some(Tri::Allowed))
    }

    /// 1/0 for the `cap` opcode (pseudo-caps included).
    pub fn cap_i64(&self, cap: &str) -> i64 {
        match cap {
            "fs.workspace" => self.workspace.is_some() as i64,
            "compute" => 1,
            c => self.cap(c) as i64,
        }
    }

    pub fn module_allowed(&self, name: &str) -> bool {
        self.cap("ffi.use")
            && !self.module_deny.iter().any(|m| m == name)
            && match &self.module_allow {
                None => true,
                Some(list) => list.iter().any(|m| m == name),
            }
    }

    pub fn describe(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("policy={}\n", self.policy_name));
        s.push_str(&format!("origin={}\n", self.origin));
        for c in CAP_NAMES {
            s.push_str(&format!("{}={}\n", c, self.cap(c) as i64));
        }
        s.push_str(&format!("fs.workspace={}\n", self.workspace.is_some() as i64));
        match &self.workspace {
            Some(roots) => {
                for r in roots {
                    s.push_str(&format!("workspace={}\n", r.display()));
                }
            }
            None => {}
        }
        match &self.module_allow {
            Some(list) => {
                for m in list {
                    s.push_str(&format!("module_allow={}\n", m));
                }
            }
            None => s.push_str("module_allow=*\n"),
        }
        for m in &self.module_deny {
            s.push_str(&format!("module_deny={}\n", m));
        }
        s
    }

    /// Human-readable report for --caps.
    pub fn report(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("policy: {} ({})\n", self.policy_name, self.origin));
        if !self.grants.is_empty() {
            s.push_str(&format!("--allow: {}\n", self.grants.join(", ")));
        }
        s.push_str("capabilities:\n");
        for c in CAP_NAMES {
            let v = if self.cap(c) { "allowed" } else { "DENIED" };
            s.push_str(&format!("  {:12} {}\n", c, v));
        }
        s.push_str(&format!("  {:12} {}\n", "fs.workspace", if self.workspace.is_some() { "roots below" } else { "unrestricted" }));
        if let Some(roots) = &self.workspace {
            for r in roots {
                s.push_str(&format!("    {}\n", r.display()));
            }
        }
        s.push_str("modules (USE): ");
        match &self.module_allow {
            None => s.push_str("all allowed"),
            Some(list) => {
                if list.is_empty() {
                    s.push_str("none");
                } else {
                    s.push_str(&list.join(", "));
                }
            }
        }
        if !self.module_deny.is_empty() {
            s.push_str(&format!(" (denied: {})", self.module_deny.join(", ")));
        }
        s.push('\n');
        s
    }

    fn apply_rules(&mut self, rules: &Rules, tighten_only: bool) {
        for c in CAP_NAMES {
            if resolve_cap(rules, c) == Tri::Denied {
                self.caps.insert(c.to_string(), Tri::Denied);
            } else if !tighten_only {
                // only the baked base/policy may (re)allow; runtime layers never do
                self.caps.remove(c);
            }
        }
        // modules: intersection across layers
        if !rules.allow_module.is_empty() {
            self.module_allow = Some(match self.module_allow.take() {
                None => rules.allow_module.clone(),
                Some(prev) => prev
                    .into_iter()
                    .filter(|m| rules.allow_module.contains(m))
                    .collect(),
            });
        }
        for m in &rules.deny_module {
            if !self.module_deny.contains(m) {
                self.module_deny.push(m.clone());
            }
        }
        // workspace: intersection. A layer with no workspace key is "anywhere".
        if !rules.workspace.is_empty() {
            let new_roots: Vec<PathBuf> = rules.workspace.iter().map(|p| expand_home(PathBuf::from(p))).collect();
            self.workspace = Some(match self.workspace.take() {
                None => new_roots,
                Some(prev) => prev
                    .into_iter()
                    .filter(|p| new_roots.iter().any(|n| path_within(p, n)))
                    .collect(),
            });
        }
    }
}

pub fn path_within(path: &Path, root: &Path) -> bool {
    let p = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    match (p.canonicalize(), root.canonicalize()) {
        (Ok(cp), Ok(cr)) => cp.starts_with(&cr),
        // not-yet-existing write target: compare the resolved parent chain lexically
        (Err(_), Ok(cr)) => {
            let mut abs = p.clone();
            while !abs.exists() {
                match abs.parent() {
                    Some(par) => abs = par.to_path_buf(),
                    None => return false,
                }
            }
            abs.canonicalize().map(|cp| cp.starts_with(&cr)).unwrap_or(false)
        }
        _ => false,
    }
}

/// A Caps allowing exactly `grants` (the always-on pure compute is never
/// gated): the compile-time gate for package translation units
/// (SPEC.md "Packages"). Structurally tighten-only — grants are always a
/// subset of the program policy by construction.
pub fn caps_for_grants(grants: &[String], base_dir: &Path) -> Caps {
    let mut rules = Rules::default();
    rules.deny.push("*".to_string());
    for g in grants {
        rules.allow.push(g.clone());
    }
    let sf = SandboxFile { base: rules, policies: HashMap::new(), name: Some("package-grants".to_string()) };
    effective(Some((&sf, "package use grants")), None, &[], &[], &[], base_dir)
}

/// Expand a leading `~` / `~/…` to `$HOME`. A workspace root of `~` means
/// the home directory, not a literal `~` directory under the program —
/// without this the resolved root blocks every fs path.
pub fn expand_home(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy().to_string();
    if s == "~" {
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h);
        }
    } else if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h).join(rest);
        }
    }
    p
}

/// Build the effective capability set.
///
/// - `baked`: the build-time .ufs (None for an unrestricted `uf`).
/// - `selected_policy`: --policy NAME, must exist in `baked`.
/// - `runtime_files`: --sandbox FILE configs, applied in order (tighten-only).
/// - `grants`: --allow capabilities — an explicit human grant that may restore
///   what the selected policy denied. Runtime tightening configs still win:
///   they are applied below and re-insert their denies.
/// - `base_dir`: directory used to resolve relative workspace roots.
pub fn effective(
    baked: Option<(&SandboxFile, &str)>,
    selected_policy: Option<&str>,
    runtime_files: &[SandboxFile],
    extra_roots: &[String],
    grants: &[String],
    base_dir: &Path,
) -> Caps {
    let mut caps = Caps::unrestricted();
    let mut policy_name = String::from("none");
    let mut origin = String::from("unrestricted build");

    if let Some((sf, origin_label)) = baked {
        let rules = match selected_policy {
            Some(p) => Some(
                sf.policies
                    .get(p)
                    .unwrap_or_else(|| user_err!("sandbox", None, "sandbox: no baked policy `{}` (available: {})", p, policy_list(sf))),
            ),
            None => None,
        };
        policy_name = match selected_policy {
            Some(p) => p.to_string(),
            None => sf.name.clone().unwrap_or_else(|| "default".to_string()),
        };
        origin = origin_label.to_string();
        // the selected policy replaces the base rules; with no --policy the
        // file's top-level rules are the policy
        match rules {
            Some(r) => caps.apply_rules(r, false),
            None => caps.apply_rules(&sf.base, false),
        }
        caps.is_sandboxed = true;
    } else if let Some(p) = selected_policy {
        user_err!("sandbox", None, "sandbox: --policy `{}` given but this build has no baked policies", p);
    }

    // --allow: a human-typed one-shot grant. Applied after the policy so it
    // can restore a denied capability, but before the tighten-only runtime
    // configs below — their denies therefore still win over any grant.
    for g in grants {
        if !CAP_NAMES.contains(&g.as_str()) {
            user_err!("sandbox", None, "sandbox: unknown capability `{}` for --allow (valid: {})", g, CAP_NAMES.join(", "));
        }
        if caps.caps.remove(g).is_some() && !caps.grants.contains(g) {
            caps.grants.push(g.clone());
        }
    }

    for rf in runtime_files {
        caps.apply_rules(&rf.base, true);
    }

    // --workspace roots intersect whatever is present
    if !extra_roots.is_empty() {
        let new_roots: Vec<PathBuf> = extra_roots.iter().map(|p| expand_home(PathBuf::from(p))).collect();
        caps.workspace = Some(match caps.workspace.take() {
            None => new_roots,
            Some(prev) => prev
                .into_iter()
                .filter(|p| new_roots.iter().any(|n| path_within(p, n)))
                .collect(),
        });
    }

    // default workspace when fs is allowed but no roots were configured:
    // the program's directory plus $TMPDIR/nk
    if caps.workspace.is_none() && (caps.cap("fs.read") || caps.cap("fs.write")) && caps.is_sandboxed {
        let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
        let mut roots = vec![base_dir.to_path_buf()];
        let t = PathBuf::from(tmp).join("nk");
        let _ = std::fs::create_dir_all(&t);
        roots.push(t);
        caps.workspace = Some(roots);
    }

    // resolve relative roots against base_dir, then canonicalize (the runtime
    // gate compares against realpath results)
    if let Some(roots) = &mut caps.workspace {
        for r in roots.iter_mut() {
            if !r.is_absolute() {
                *r = base_dir.join(&*r);
            }
            if let Ok(c) = r.canonicalize() {
                *r = c;
            }
        }
    }

    caps.policy_name = policy_name;
    caps.origin = origin;
    caps
}

fn policy_list(sf: &SandboxFile) -> String {
    let mut names: Vec<&String> = sf.policies.keys().collect();
    names.sort();
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.into_iter().map(|n| n.as_str()).collect::<Vec<_>>().join(", ")
    }
}

/// C statements baking the capability table + workspace roots into a compiled
/// program's main() (consumed by op_cap/op_caps and uf_fs_gate; the globals
/// themselves live in the prelude with unrestricted defaults).
pub fn c_bake(caps: &Caps, device: &str, required: &[(String, Vec<String>)]) -> String {
    let mut s = String::new();
    s.push_str(&format!("uf_sb_on={};\n", caps.is_sandboxed as i64));
    s.push_str(&format!("uf_sb_policy=\"{}\";\n", c_escape(&caps.policy_name)));
    s.push_str(&format!("uf_device=\"{}\";\n", c_escape(device)));
    let cap_vals: Vec<i64> = CAP_NAMES.iter().map(|c| caps.cap_i64(c)).collect();
    s.push_str(&format!(
        "{{int _i;for(_i=0;_i<{};_i++)uf_sb_caps[_i]=1;}}\n",
        CAP_NAMES.len()
    ));
    for (i, v) in cap_vals.iter().enumerate() {
        s.push_str(&format!("uf_sb_caps[{}]={};\n", i, v));
    }
    // phase 3: runtime ceiling (NK_CAP_CEILING) + per-package required-cap
    // validation — dies before main's body runs when a required capability
    // is denied for that run
    s.push_str("uf_rt_ceiling();\n");
    for (alias, reqs) in required {
        for r in reqs {
            if let Some(i) = CAP_NAMES.iter().position(|c| c == r) {
                s.push_str(&format!("uf_req_one(\"{}\",{});\n", c_escape(alias), i));
            }
        }
    }
    let roots = caps.workspace.clone().unwrap_or_default();
    s.push_str("uf_ws_nroots=0;\n");
    for r in roots.iter().take(16) {
        s.push_str(&format!("uf_ws_roots[uf_ws_nroots++]=\"{}\";\n", c_escape(&r.to_string_lossy())));
    }
    match &caps.module_allow {
        Some(list) => {
            s.push_str("uf_mod_allow_n=0;\n");
            for m in list.iter().take(32) {
                s.push_str(&format!("uf_mod_allow[uf_mod_allow_n++]=\"{}\";\n", c_escape(m)));
            }
        }
        None => s.push_str("uf_mod_allow_n=-1;\n"),
    }
    s.push_str("uf_mod_deny_n=0;\n");
    for m in caps.module_deny.iter().take(32) {
        s.push_str(&format!("uf_mod_deny[uf_mod_deny_n++]=\"{}\";\n", c_escape(m)));
    }
    s
}

fn c_escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            c => o.push(c),
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_tilde_expands_to_home() {
        // a `workspace ~` root must resolve under $HOME, not a literal `~`
        // directory under the program's base dir
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let probe = std::path::Path::new(&home).join(format!("nk-tilde-test-{}", std::process::id()));
        std::fs::create_dir_all(&probe).unwrap();
        std::env::set_var("HOME", &probe);
        let rules = "name t\ndeny proc\nworkspace ~\n";
        let sf = parse_ufs(rules, "<test>");
        let caps = effective(Some((&sf, "test")), None, &[], &[], &[], Path::new("/some/base"));
        std::env::set_var("HOME", &home);
        let _ = std::fs::remove_dir_all(&probe);
        let roots = caps.workspace.expect("workspace roots");
        assert!(
            roots.iter().all(|r| r.starts_with(&probe)),
            "roots {:?} should resolve under {}",
            roots,
            probe.display()
        );
    }

    #[test]
    fn allow_grant_restores_policy_deny() {
        let rules = "name t\ndeny proc fs.read\n";
        let sf = parse_ufs(rules, "<test>");
        let base = Path::new("/tmp");
        let denied = effective(Some((&sf, "test")), None, &[], &[], &[], base);
        assert!(!denied.cap("proc"));
        assert!(!denied.cap("fs.read"));
        let granted = effective(Some((&sf, "test")), None, &[], &[], &["proc".to_string()], base);
        assert!(granted.cap("proc"), "--allow proc must restore the denied cap");
        assert!(!granted.cap("fs.read"), "un-granted caps stay denied");
        assert_eq!(granted.grants, vec!["proc".to_string()]);
    }

    #[test]
    fn allow_grant_cannot_override_sandbox_tightening() {
        let baked = parse_ufs("name t\ndeny proc\n", "<baked>");
        let tighten = parse_ufs("deny proc\n", "<tighten>");
        let base = Path::new("/tmp");
        let caps = effective(
            Some((&baked, "test")),
            None,
            &[tighten],
            &[],
            &["proc".to_string()],
            base,
        );
        assert!(!caps.cap("proc"), "a --sandbox tighten config still wins over --allow");
    }
}
