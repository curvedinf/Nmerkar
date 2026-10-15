// pkg.rs — packages: sources, canonical content hashing (nkph1:), the
// content store, lockfile + approvals, use-site grant algebra, and the
// `nk install` / `nk pkg gc` driver subcommands.
//
// Phase 1 of the package system (SPEC.md "Packages"):
//   - sources: gh:owner/repo[@ref], https://…[@ref], path:<dir>
//   - "latest" exists only inside `install` (highest semver tag; @head for
//     the branch tip); the lockfile never records "latest"
//   - fetch is driver-level via subprocess (curl / tar / git ls-remote —
//     the cc precedent); compile never fetches
//   - no clause at a use site = zero privileges:
//       grant = ((policy ∪ allow) − deny) ∩ declared ∩ program policy
//   - phase-1 gating is compile-time: package TUs parse under a Caps built
//     from the granted set, so ops outside the grant are compile errors.

use crate::ast::Parsed;
use crate::ast::UseSpec;
use crate::sandbox::{self, resolve_cap, Caps, SandboxFile, Tri, CAP_NAMES};
use crate::user_err;
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------- SHA-256 (std-only) ----------------

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[4 * i..4 * i + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

fn hex(bs: &[u8]) -> String {
    let mut s = String::with_capacity(bs.len() * 2);
    for b in bs {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

// ---------------- sources ----------------

pub fn is_schemed(s: &str) -> bool {
    s.starts_with("gh:") || s.starts_with("https://") || s.starts_with("http://") || s.starts_with("path:")
}

/// Split `id@ref` into (id, Some(ref)); scheme-less or ref-less sources
/// return (s, None). The separator is the last '@' (https URLs may contain
/// earlier '@' characters).
pub fn split_ref(s: &str) -> (String, Option<String>) {
    match s.rfind('@') {
        Some(i) if i > 0 && !s[i + 1..].is_empty() && is_schemed(&s[..i]) => {
            (s[..i].to_string(), Some(s[i + 1..].to_string()))
        }
        _ => (s.to_string(), None),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Gh { owner: String, repo: String },
    Https { url: String },
    Path { dir: PathBuf },
}

pub fn parse_source(id: &str) -> Source {
    if let Some(rest) = id.strip_prefix("gh:") {
        let mut it = rest.splitn(2, '/');
        let owner = it.next().unwrap_or("").to_string();
        let repo = it.next().unwrap_or("").to_string();
        if owner.is_empty() || repo.is_empty() || repo.contains('/') {
            user_err!("pkg", None, "bad gh source `{}` — expected gh:owner/repo", id);
        }
        Source::Gh { owner, repo }
    } else if let Some(rest) = id.strip_prefix("path:") {
        if rest.is_empty() {
            user_err!("pkg", None, "bad path source — expected path:<dir>");
        }
        Source::Path { dir: PathBuf::from(rest) }
    } else if let Some(url) = id.strip_prefix("https://").or_else(|| id.strip_prefix("http://")) {
        if url.is_empty() {
            user_err!("pkg", None, "bad https source — expected https://host/path");
        }
        Source::Https { url: id.to_string() }
    } else {
        user_err!("pkg", None, "bad package source `{}` — expected gh:owner/repo, https://…, or path:<dir>", id);
    }
}

// ---------------- canonical hashing ----------------

/// LF-normalize (CRLF and lone CR → LF); non-UTF-8 package files are errors.
fn normalize(src: &[u8]) -> String {
    let s = String::from_utf8(src.to_vec())
        .unwrap_or_else(|e| user_err!("pkg", None, "package file is not valid UTF-8: {}", e.utf8_error()));
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next(); // CRLF collapses to the LF below
            }
            out.push('\n'); // lone CR also becomes LF
        } else {
            out.push(c);
        }
    }
    out
}

fn pkg_file_keeps(rel: &str) -> bool {
    let name = Path::new(rel).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if name.starts_with('.') {
        return false;
    }
    if name == "nk.pkg" {
        return true;
    }
    // the signature envelope is excluded from the hashed set: nk.sig can
    // never sign itself, and nk.pub must not change the root when a
    // publisher rotates keys (key continuity lives in the approvals)
    if name == "nk.sig" || name == "nk.pub" {
        return false;
    }
    matches!(Path::new(rel).extension().and_then(|e| e.to_str()), Some("n") | Some("nd") | Some("ufm"))
}

fn collect_files(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => user_err!("pkg", None, "cannot read package dir {}: {}", dir.display(), e),
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if name.starts_with('.') {
            continue; // .git, .nk, dotfiles
        }
        let rel = if prefix.is_empty() { name.clone() } else { format!("{}/{}", prefix, name) };
        if p.is_dir() {
            collect_files(&p, &rel, out);
        } else if pkg_file_keeps(&rel) {
            out.push((rel, p));
        }
    }
}

pub struct PackageContent {
    pub root: String,                   // "nkph1:<hex>"
    pub files: Vec<(String, String)>,   // sorted relpath -> blob sha (hex, no prefix)
}

/// Hash the canonical file set of a package directory: compiler-consumed
/// files only, LF-normalized, sorted paths, per-file SHA-256, Merkle root
/// over "path\0blob\n" lines.
pub fn hash_package(dir: &Path) -> PackageContent {
    let mut raw = Vec::new();
    collect_files(dir, "", &mut raw);
    raw.sort_by(|a, b| a.0.cmp(&b.0));
    let mut files = Vec::new();
    for (rel, p) in &raw {
        let bytes = fs::read(p).unwrap_or_else(|e| user_err!("pkg", None, "cannot read {}: {}", p.display(), e));
        let norm = normalize(&bytes);
        files.push((rel.clone(), hex(&sha256(norm.as_bytes()))));
    }
    let mut canon = String::new();
    for (rel, sha) in &files {
        canon.push_str(rel);
        canon.push('\0');
        canon.push_str(sha);
        canon.push('\n');
    }
    PackageContent { root: format!("nkph1:{}", hex(&sha256(canon.as_bytes()))), files }
}

// ---------------- content store ----------------

pub fn store_root() -> PathBuf {
    home_base().join("pkg")
}

/// The nk home: `$NKPKG_HOME` when set (tests, sandboxes), else `~/.nk`.
/// The content store lives at `<home>/pkg`, tool binaries at `<home>/tools`,
/// and PATH shims at `<home>/bin`.
pub fn home_base() -> PathBuf {
    if let Ok(h) = std::env::var("NKPKG_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".nk")
}

fn blob_path(sha: &str) -> PathBuf {
    store_root().join("blobs").join(sha)
}

fn root_dir(root: &str) -> PathBuf {
    let hexpart = root.strip_prefix("nkph1:").unwrap_or(root);
    store_root().join("roots").join(hexpart)
}

/// Write a package's normalized files into the store (blobs dedupe for
/// free); returns nothing (errors on io failure).
pub fn store_put(content: &PackageContent, read_from: &Path) {
    fs::create_dir_all(store_root().join("blobs")).unwrap_or_else(|e| user_err!("pkg", None, "store: {}", e));
    let rd = root_dir(&content.root);
    fs::create_dir_all(&rd).unwrap_or_else(|e| user_err!("pkg", None, "store: {}", e));
    for (rel, sha) in &content.files {
        let bp = blob_path(sha);
        if !bp.exists() {
            let bytes = fs::read(read_from.join(rel))
                .unwrap_or_else(|e| user_err!("pkg", None, "store: cannot read {}: {}", rel, e));
            let norm = normalize(&bytes);
            fs::write(&bp, norm.as_bytes()).unwrap_or_else(|e| user_err!("pkg", None, "store: {}", e));
        }
    }
    let mut tree = String::new();
    for (rel, sha) in &content.files {
        tree.push_str(&format!("{} {}\n", rel, sha));
    }
    fs::write(rd.join("tree"), tree).unwrap_or_else(|e| user_err!("pkg", None, "store: {}", e));
}

/// The stored tree of a root: relpath -> blob sha, source order (sorted).
pub fn store_tree(root: &str) -> Option<Vec<(String, String)>> {
    let raw = fs::read_to_string(root_dir(root).join("tree")).ok()?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\n');
        if line.is_empty() {
            continue;
        }
        let (rel, sha) = line.split_once(' ')?;
        out.push((rel.to_string(), sha.to_string()));
    }
    Some(out)
}

pub fn blob_contents(sha: &str) -> String {
    fs::read_to_string(blob_path(sha))
        .unwrap_or_else(|e| user_err!("pkg", None, "package store blob {} missing ({}); rerun `nk install`", sha, e))
}

// ---------------- lockfile + approvals ----------------

#[derive(Clone, Debug, PartialEq)]
pub struct Pin {
    pub source: String, // canonical id, no @ref
    pub ref_: String,   // resolved ref ("-" for ref-less https/path pins)
    pub sha: String,    // commit sha, "-" when none (https/path)
    pub root: String,   // nkph1:…
    pub signed: bool,   // ed25519 signature verified over the root
}

#[derive(Clone, Debug, Default)]
pub struct Lock {
    pub pins: Vec<Pin>,
}

fn parse_kv_fields(rest: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for part in rest.split_whitespace() {
        if let Some((k, v)) = part.split_once('=') {
            m.insert(k.to_string(), v.to_string());
        }
    }
    m
}

pub fn parse_lock(src: &str) -> Lock {
    let mut lock = Lock::default();
    for line in src.lines() {
        let line = line.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("pkg ") {
            let f = parse_kv_fields(rest);
            let (source, ref_, sha, root) = (
                f.get("source").cloned().unwrap_or_default(),
                f.get("ref").cloned().unwrap_or_else(|| "-".to_string()),
                f.get("sha").cloned().unwrap_or_else(|| "-".to_string()),
                f.get("root").cloned().unwrap_or_default(),
            );
            if source.is_empty() || root.is_empty() {
                user_err!("pkg", None, "lock: bad pin line `{}`", line);
            }
            lock.pins.push(Pin { source, ref_, sha, root, signed: f.get("sig").map(|s| s == "ok").unwrap_or(false) });
        }
    }
    lock
}

impl Lock {
    pub fn for_source(&self, id: &str) -> Vec<&Pin> {
        self.pins.iter().filter(|p| p.source == id).collect()
    }
    pub fn for_source_ref(&self, id: &str, ref_: &str) -> Option<&Pin> {
        self.pins.iter().find(|p| p.source == id && p.ref_ == ref_)
    }
    fn render(&self) -> String {
        let mut s = String::from("; nk package lock v1 — resolved pins; latest never appears here\n");
        for p in &self.pins {
            s.push_str(&format!("pkg source={} ref={} sha={} root={} sig={}\n", p.source, p.ref_, p.sha, p.root, if p.signed { "ok" } else { "-" }));
        }
        s
    }
    fn write_to(&self, repo: &Path) -> PathBuf {
        let dir = repo.join(".nk");
        fs::create_dir_all(&dir).unwrap_or_else(|e| user_err!("pkg", None, "cannot create {}: {}", dir.display(), e));
        let path = dir.join("lock");
        write_atomic(&path, &self.render());
        path
    }
}

fn write_atomic(path: &Path, content: &str) {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content).unwrap_or_else(|e| user_err!("pkg", None, "cannot write {}: {}", tmp.display(), e));
    fs::rename(&tmp, path).unwrap_or_else(|e| user_err!("pkg", None, "cannot replace {}: {}", path.display(), e));
}

#[derive(Clone, Debug)]
pub struct Approval {
    pub source: String,
    pub root: String,
    pub date: String,
    pub by: String,
    pub key: Option<String>, // publisher ed25519 key (signed packages)
}

pub fn parse_approvals(src: &str) -> Vec<Approval> {
    let mut out = Vec::new();
    for line in src.lines() {
        let line = line.split(';').next().unwrap_or("").trim();
        if let Some(rest) = line.strip_prefix("approve ") {
            let f = parse_kv_fields(rest);
            if let (Some(source), Some(root)) = (f.get("source").cloned(), f.get("root").cloned()) {
                out.push(Approval {
                    source,
                    root,
                    date: f.get("date").cloned().unwrap_or_default(),
                    by: f.get("by").cloned().unwrap_or_else(|| "?".to_string()),
                    key: f.get("key").cloned(),
                });
            }
        }
    }
    out
}

fn approvals_path(repo: &Path) -> PathBuf {
    repo.join(".nk").join("approvals")
}

fn approved(approvals: &[Approval], id: &str, root: &str) -> bool {
    approvals.iter().any(|a| a.source == id && a.root == root)
}

// civil date from unix days (Howard Hinnant's algorithm)
fn iso_date(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------------- package metadata (nk.pkg) ----------------

#[derive(Clone, Debug, Default)]
pub struct PkgMeta {
    pub name: Option<String>,
    pub caps: Vec<(String, bool)>, // cap, required
    pub bins: Vec<(String, String)>, // bin name -> TU rel path
}

pub fn parse_nkpkg(src: &str) -> PkgMeta {
    let mut m = PkgMeta::default();
    for line in src.lines() {
        let line = line.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let toks: Vec<&str> = line.split_whitespace().collect();
        match toks.as_slice() {
            ["name", n] => m.name = Some(n.to_string()),
            ["caps", c, r] => {
                let req = match *r {
                    "required" => true,
                    "optional" => false,
                    _ => user_err!("pkg", None, "nk.pkg: caps entry `{}` — expected required|optional", r),
                };
                m.caps.push((c.to_string(), req));
            }
            ["bin", n, "=", path] => m.bins.push((n.to_string(), path.to_string())),
            _ => user_err!("pkg", None, "nk.pkg: unknown line `{}` (expected name <x> | caps <cap> required|optional | bin <name> = <tu-file>)", line),
        }
    }
    m
}

fn meta_from_tree(tree: &[(String, String)]) -> PkgMeta {
    for (rel, sha) in tree {
        if rel == "nk.pkg" {
            return parse_nkpkg(&blob_contents(sha));
        }
    }
    PkgMeta::default()
}

// ---------------- semver ----------------

/// Semver ordering key: (major, minor, patch, release) where a pre-release
/// (v1.10.0-rc1) sorts below the plain release (v1.10.0). Non-semver tags
/// yield None (they never win "latest").
pub fn semver_key(tag: &str) -> Option<(u64, u64, u64, u8)> {
    let t = tag.strip_prefix('v').unwrap_or(tag);
    let mut it = t.split('.');
    let a: u64 = it.next()?.parse().ok()?;
    let b: u64 = it.next()?.parse().ok()?;
    let c_str = it.next()?;
    let c: u64 = c_str
        .split(|ch| ch == '-' || ch == '+')
        .next()
        .unwrap_or("")
        .parse()
        .ok()?;
    if it.next().is_some() {
        return None;
    }
    let release = if t.contains('-') { 0 } else { 1 };
    Some((a, b, c, release))
}

// ---------------- fetch ----------------

struct Fetched {
    dir: PathBuf,          // extracted/normalized package dir
    temp: bool,            // true when dir is ours to delete (gh/https); path: reads in place
    sha: String,           // commit sha or "-"
    resolved_ref: String,  // tag/sha/head/"-"
}

fn need_cmd(prog: &str, why: &str) {
    let probe = Command::new(prog).arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    match probe {
        Ok(_) => {}
        Err(_) => user_err!("pkg", None, "package fetch needs `{}` on PATH ({}), but it is not installed", prog, why),
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let d = std::env::temp_dir().join(format!("nkpkg-{}-{}-{}", tag, std::process::id(), nanos));
    fs::create_dir_all(&d).unwrap_or_else(|e| user_err!("pkg", None, "tmpdir: {}", e));
    d
}

fn strip_top(dir: &Path) {
    // GitHub tarballs (and most release tarballs) wrap content in one
    // top-level dir; move it up if present.
    let entries: Vec<PathBuf> = fs::read_dir(dir).ok().map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
    if entries.len() == 1 && entries[0].is_dir() {
        let top = entries[0].clone();
        let inner: Vec<PathBuf> = fs::read_dir(&top).ok().map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
        for p in inner {
            let name = p.file_name().unwrap_or_default().to_os_string();
            fs::rename(&p, dir.join(&name)).unwrap_or_else(|e| user_err!("pkg", None, "tar extract: {}", e));
        }
        let _ = fs::remove_dir_all(&top);
    }
}

fn fetch_tar_gz(url: &str, into: &Path) {
    need_cmd("curl", "downloading package tarballs");
    need_cmd("tar", "extracting package tarballs");
    let out = Command::new("curl").args(["-fsSL", url]).output()
        .unwrap_or_else(|e| user_err!("pkg", None, "curl spawn failed: {}", e));
    if !out.status.success() {
        user_err!("pkg", None, "fetch failed: curl {} (exit {})", url, out.status.code().unwrap_or(-1));
    }
    let tgz = into.join("pkg.tar.gz");
    fs::write(&tgz, &out.stdout).unwrap_or_else(|e| user_err!("pkg", None, "cannot write {}: {}", tgz.display(), e));
    let st = Command::new("tar").arg("-xzf").arg(&tgz).arg("-C").arg(into).status()
        .unwrap_or_else(|e| user_err!("pkg", None, "tar spawn failed: {}", e));
    if !st.success() {
        user_err!("pkg", None, "extract failed: tar -xzf {} (exit {})", tgz.display(), st.code().unwrap_or(-1));
    }
    let _ = fs::remove_file(&tgz);
    strip_top(into);
}

fn gh_ls_remote(url: &str, pattern: &str) -> Vec<(String, String)> {
    need_cmd("git", "resolving gh: refs (git ls-remote)");
    let out = Command::new("git").args(["ls-remote", url, pattern]).output()
        .unwrap_or_else(|e| user_err!("pkg", None, "git spawn failed: {}", e));
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        user_err!("pkg", None, "git ls-remote {} failed{}{}", url, if msg.is_empty() { "" } else { ": " }, msg);
    }
    let mut pairs = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some((sha, r)) = line.split_once('\t') {
            pairs.push((sha.to_string(), r.to_string()));
        }
    }
    pairs
}

fn resolve_gh_latest(owner: &str, repo: &str) -> (String, String) {
    let url = format!("https://github.com/{}/{}.git", owner, repo);
    let refs = gh_ls_remote(&url, "refs/tags/*");
    // annotated tags appear twice (tag object + ^{} commit deref); the ^{}
    // line carries the commit sha — prefer it
    let mut deref: HashMap<String, String> = HashMap::new();
    let mut plain: HashMap<String, String> = HashMap::new();
    for (sha, r) in &refs {
        if let Some(tag) = r.strip_prefix("refs/tags/") {
            if let Some(t) = tag.strip_suffix("^{}") {
                deref.insert(t.to_string(), sha.clone());
            } else {
                plain.insert(tag.to_string(), sha.clone());
            }
        }
    }
    let mut best: Option<((u64, u64, u64, u8), String, String)> = None;
    for tag in plain.keys() {
        if let Some(k) = semver_key(tag) {
            let sha = deref.get(tag).or_else(|| plain.get(tag)).unwrap().clone();
            if best.as_ref().map_or(true, |(bk, _, _)| k > *bk) {
                best = Some((k, tag.clone(), sha));
            }
        }
    }
    match best {
        Some((_, tag, sha)) => (tag.clone(), sha),
        None => user_err!("pkg", None, "gh:{}/{} has no semver tags (vMAJOR.MINOR.PATCH); pin an exact ref with @", owner, repo),
    }
}

fn resolve_gh_head(owner: &str, repo: &str) -> String {
    let url = format!("https://github.com/{}/{}.git", owner, repo);
    let refs = gh_ls_remote(&url, "HEAD");
    refs.first().map(|(sha, _)| sha.clone()).unwrap_or_else(|| {
        user_err!("pkg", None, "gh:{}/{}: no HEAD ref", owner, repo)
    })
}

fn fetch(id: &str, want_ref: Option<&str>) -> Fetched {
    match parse_source(id) {
        Source::Path { dir } => {
            if want_ref.is_some() {
                user_err!("pkg", None, "path: sources take no @ref (the directory is the identity)");
            }
            if !dir.is_dir() {
                user_err!("pkg", None, "path:{}: not a directory", dir.display());
            }
            Fetched { dir, temp: false, sha: "-".to_string(), resolved_ref: "-".to_string() }
        }
        Source::Gh { owner, repo } => {
            let (resolved_ref, sha) = match want_ref {
                Some(r) if r == "head" => ("head".to_string(), resolve_gh_head(&owner, &repo)),
                Some(r) if r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit()) => (r.to_string(), r.to_lowercase()),
                Some(tag) => {
                    let url = format!("https://github.com/{}/{}.git", owner, repo);
                    let refs = gh_ls_remote(&url, &format!("refs/tags/{}*", tag));
                    let sha = refs.iter().find(|(_, r)| r == &format!("refs/tags/{}", tag))
                        .or_else(|| refs.iter().find(|(_, r)| r == &format!("refs/tags/{}^{{}}", tag)))
                        .map(|(s, _)| s.clone());
                    match sha {
                        Some(s) => (tag.to_string(), s),
                        None => user_err!("pkg", None, "gh:{}/{}: no tag `{}`", owner, repo, tag),
                    }
                }
                None => resolve_gh_latest(&owner, &repo),
            };
            let url = format!("https://codeload.github.com/{}/{}/tar.gz/{}", owner, repo, sha);
            let dir = tmp_dir("gh");
            fetch_tar_gz(&url, &dir);
            Fetched { dir, temp: true, sha, resolved_ref }
        }
        Source::Https { url } => {
            let (resolved_ref, sha) = ("-".to_string(), "-".to_string());
            if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
                let dir = tmp_dir("https");
                fetch_tar_gz(url.as_str(), &dir);
                Fetched { dir, temp: true, sha, resolved_ref }
            } else {
                // single-file package: the bytes are the identity (TOFU)
                need_cmd("curl", "downloading https packages");
                let out = Command::new("curl").args(["-fsSL", url.as_str()]).output()
                    .unwrap_or_else(|e| user_err!("pkg", None, "curl spawn failed: {}", e));
                if !out.status.success() {
                    user_err!("pkg", None, "fetch failed: curl {} (exit {})", url, out.status.code().unwrap_or(-1));
                }
                let base = url.split('?').next().unwrap_or(url.as_str()).rsplit('/').next().unwrap_or("pkg.ufm");
                let base = if base.is_empty() { "pkg.ufm".to_string() } else { base.to_string() };
                let dir = tmp_dir("https1");
                fs::write(dir.join(&base), normalize(&out.stdout).as_bytes())
                    .unwrap_or_else(|e| user_err!("pkg", None, "cannot write single-file package: {}", e));
                Fetched { dir, temp: true, sha, resolved_ref }
            }
        }
    }
}

// ---------------- grant algebra ----------------

pub fn policy_allowed_caps(baked: Option<&SandboxFile>, name: &str) -> Option<Vec<String>> {
    if name == "all" {
        return Some(CAP_NAMES.iter().map(|s| s.to_string()).collect());
    }
    let sf = baked?;
    let rules = sf.policies.get(name)?;
    Some(
        CAP_NAMES
            .iter()
            .filter(|c| resolve_cap(rules, c) == Tri::Allowed)
            .map(|s| s.to_string())
            .collect(),
    )
}

fn patterns_cover(patterns: &[String], cap: &str) -> bool {
    patterns.iter().any(|p| sandbox::pattern_matches(p, cap).is_some())
}

/// A token is capability-shaped: `*`, a bare cap name, or a dotted
/// lowercase pattern. Identifiers legally cannot contain '.', so this
/// terminates grant lists without a closing delimiter.
pub fn is_cap_token(s: &str) -> bool {
    if s == "*" || CAP_NAMES.contains(&s) {
        return true;
    }
    if !s.contains('.') || s.starts_with('.') || s.ends_with('.') {
        return false;
    }
    s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '*')
}

/// Compute and validate the effective grant for one use site.
///
/// grant = ((policy ∪ allow) − deny) ∩ declared ∩ program policy, with a
/// missing clause meaning ∅ (zero privileges by default). Non-wildcard
/// allows must name a declared capability; required declared caps must
/// survive into the grant.
pub fn compute_grant(
    spec: &UseSpec,
    line: u32,
    id: &str,
    declared: &[(String, bool)],
    program_caps: &Caps,
    baked: Option<&SandboxFile>,
) -> Vec<String> {
    let declared_names: Vec<&str> = declared.iter().map(|(c, _)| c.as_str()).collect();
    for t in spec.allows.iter().chain(spec.denies.iter()) {
        if !is_cap_token(t) {
            user_err!("parse", Some(line), "use site at L{}: unknown capability `{}` — write dotted capability names (fs.read, proc, …) or wildcards (fs.*, *)", line, t);
        }
    }
    for t in &spec.allows {
        if *t != "*" && !t.contains('*') && !declared_names.contains(&t.as_str()) {
            let declared_note = if declared_names.is_empty() {
                " (declares none)".to_string()
            } else {
                format!(" (declared: {})", declared_names.join(", "))
            };
            user_err!(
                "parse", Some(line),
                "use site at L{} grants `{}` but {} declares no such capability{} — grant is wider than the declaration",
                line, t, id, declared_note
            );
        }
    }
    let base: Vec<String> = match &spec.policy {
        None => Vec::new(),
        Some(p) => policy_allowed_caps(baked, p).unwrap_or_else(|| {
            let mut names: Vec<String> = vec!["all".to_string()];
            if let Some(sf) = baked {
                names.extend(sf.policies.keys().cloned());
            }
            user_err!("parse", Some(line), "use site at L{} names unknown policy `{}` (known: {})", line, p, names.join(", "))
        }),
    };
    let mut grant: Vec<String> = CAP_NAMES
        .iter()
        .filter(|c| (patterns_cover(&base, c) || patterns_cover(&spec.allows, c)) && !patterns_cover(&spec.denies, c))
        .filter(|c| declared_names.contains(&c.to_string().as_str()))
        .filter(|c| program_caps.cap(c))
        .map(|s| s.to_string())
        .collect();
    grant.sort();
    for (cap, required) in declared {
        if *required && !grant.contains(cap) {
            user_err!(
                "parse", Some(line),
                "{}: use site at L{} starves required capability `{}` — grant it explicitly (allow {}), via a named policy, or policy all (discouraged); or use a package without {}",
                id, line, cap, cap, cap
            );
        }
    }
    grant
}

// ---------------- compile-side resolution (phase 2: aliases + `:`) ----------------

/// A package loaded and parsed under its use alias. The alias is the
/// package's namespace everywhere: labels are addressable as `alias:name`
/// (the standard per-TU mod mangling — `pub` marks the API contract),
/// shared vars mangle `alias__var` (isolated from every other TU), and
/// structs register as `alias:Name`.
pub struct LoadedPkg {
    pub alias: String,
    pub extra_aliases: Vec<String>, // same content bound under more names
    pub pin: Pin,
    pub meta: PkgMeta,
    pub grant: Vec<String>, // union over use sites (phase-2 semantics)
    pub tus: Vec<Parsed>,
    pub mods: Vec<String>, // defmod per TU (== alias: a package is one namespace)
}

/// Find `<dir>/.nk/lock` walking up from `dir` (like mods discovery).
pub fn find_lockfile(from: &Path) -> Option<PathBuf> {
    let mut cur = Some(from.to_path_buf());
    while let Some(d) = cur {
        let cand = d.join(".nk").join("lock");
        if cand.exists() {
            return Some(cand);
        }
        cur = d.parent().map(|p| p.to_path_buf());
    }
    None
}

fn sanitize_alias(a: &str) -> String {
    let mut out = String::new();
    for (i, c) in a.chars().enumerate() {
        if c.is_ascii_alphanumeric() || (c == '_' && i > 0) {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() || out.chars().next().map_or(true, |c| c.is_ascii_digit()) {
        out = format!("p{}", out);
    }
    out
}

/// A defmod-safe name for a package (mirrors driver file-stem cleanup).
pub fn pkg_modname(id: &str, meta: &PkgMeta) -> String {
    let raw = meta.name.clone().unwrap_or_else(|| {
        let stem = id.trim_start_matches("gh:").trim_start_matches("path:");
        let stem = stem.rsplit('/').next().unwrap_or(stem);
        let stem = stem.split('.').next().unwrap_or(stem);
        stem.to_string()
    });
    sanitize_alias(&raw)
}

/// Lex a package's TU files, collecting struct names (for qualification)
/// and schemed dep uses (deps of deps resolve transitively).
fn lex_tree(tree: &[(String, String)]) -> (Vec<(String, Vec<(u32, crate::ast::Tok)>)>, Vec<String>) {
    let mut out = Vec::new();
    let mut structs = Vec::new();
    for (rel, sha) in tree {
        if rel.ends_with(".ufm") || rel == "nk.pkg" {
            continue;
        }
        let src = blob_contents(sha);
        let toks = crate::lex::lex_source(&src);
        for (_, t) in &toks {
            if let crate::ast::Tok::StructDef(n, _) = t {
                if !structs.contains(n) {
                    structs.push(n.clone());
                }
            }
        }
        out.push((rel.clone(), toks));
    }
    (out, structs)
}

/// Qualify a package's struct names to `alias:Name` in its token stream:
/// struct definitions, their field types, and the `@sizeof:`/`@objsize:`/
/// `@cast:`/`@offset:` sentinels. Non-package (global) structs pass through.
fn qualify_struct_refs(toks: &mut Vec<(u32, crate::ast::Tok)>, alias: &str, local: &[String]) {
    let qual = |n: &str| -> String {
        if local.contains(&n.to_string()) {
            format!("{}:{}", alias, n)
        } else {
            n.to_string()
        }
    };
    for (_, t) in toks.iter_mut() {
        match t {
            crate::ast::Tok::StructDef(n, fields) => {
                *n = qual(n);
                for (_fname, fty) in fields.iter_mut() {
                    *fty = qual(fty);
                }
            }
            crate::ast::Tok::Ident(s) => {
                let mut changed = false;
                for p in ["@sizeof:", "@objsize:", "@cast:"] {
                    if let Some(rest) = s.strip_prefix(p) {
                        *s = format!("{}{}", p, qual(rest));
                        changed = true;
                        break;
                    }
                }
                if !changed {
                    if let Some(rest) = s.strip_prefix("@offset:") {
                        let (name, field) = match rest.split_once('.') {
                            Some((n, f)) => (n.to_string(), Some(f.to_string())),
                            None => (rest.to_string(), None),
                        };
                        if local.contains(&name) {
                            *s = match field {
                                Some(f) => format!("@offset:{}:{}.{}", alias, name, f),
                                None => format!("@offset:{}:{}", alias, name),
                            };
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

struct PkgSources {
    pin: Pin,
    meta: PkgMeta,
    grant_specs: Vec<(UseSpec, u32)>, // every use site of this root
    tu_files: Vec<(String, String)>,  // ordered TU rel -> blob sha
    manifests: Vec<(String, String)>,
    struct_names: Vec<String>,
}

/// Resolve every schemed use spec (transitively — packages may use
/// packages) against the lockfile + store, parse each package's TUs under
/// the union of its use-site grants, register its structs qualified by
/// alias, and return the loaded packages. Errors carry use-site lines.
pub fn resolve_and_load(
    specs: &[(UseSpec, u32)],
    base_dir: &Path,
    program_caps: &Caps,
    baked: Option<&SandboxFile>,
    structs: &mut crate::ast::StructMap,
) -> Vec<LoadedPkg> {
    if specs.is_empty() {
        return Vec::new();
    }
    let lock_path = find_lockfile(base_dir).unwrap_or_else(|| {
        let (s0, l0) = &specs[0];
        user_err!("parse", Some(*l0), "package use \"{}\" requires a .nk/lock — run `nk install .` from the project root first", s0.source)
    });
    let lock = parse_lock(&fs::read_to_string(&lock_path).unwrap_or_else(|e| user_err!("pkg", None, "cannot read {}: {}", lock_path.display(), e)));

    // Phase A: resolve pins + lex trees transitively (no parsing yet), so
    // grants can be unioned over ALL sites before any TU is compiled.
    let mut by_root: HashMap<String, PkgSources> = HashMap::new();
    let mut order: Vec<String> = Vec::new(); // first-seen root order
    let mut alias_of_root: HashMap<String, String> = HashMap::new();
    let mut root_of_alias: HashMap<String, String> = HashMap::new();
    let mut extra: HashMap<String, Vec<String>> = HashMap::new(); // root -> extra aliases
    let mut work: Vec<(UseSpec, u32)> = specs.to_vec();
    while let Some((spec, line)) = work.pop() {
        let (id, ref_) = split_ref(&spec.source);
        if !is_schemed(&id) {
            user_err!("parse", Some(line), "use site at L{}: bad package source `{}`", line, spec.source);
        }
        let pin = match &ref_ {
            Some(r) => lock.for_source_ref(&id, r).unwrap_or_else(|| {
                user_err!("parse", Some(line), "package {}@{} is not in .nk/lock — run `nk install {}`", id, r, spec.source)
            }).clone(),
            None => {
                let ps = lock.for_source(&id);
                match ps.len() {
                    0 => user_err!("parse", Some(line), "package {} is not in .nk/lock — run `nk install {}` (or pin a version: use \"{}@<ref>\")", id, id, id),
                    1 => ps[0].clone(),
                    _ => user_err!("parse", Some(line), "package {} has {} pins in .nk/lock — pin a version at the use site: use \"{}@<ref>\"", id, ps.len(), id),
                }
            }
        };
        // alias bookkeeping: first explicit alias wins; later distinct
        // aliases for the same content register as extra names
        if let Some(a) = &spec.alias {
            let a = sanitize_alias(a);
            match alias_of_root.get(&pin.root) {
                Some(prev) if *prev != a => {
                    if root_of_alias.get(&a).map(|r| *r != pin.root).unwrap_or(false) {
                        user_err!("parse", Some(line), "use alias `{}` names two different packages — aliases must be unique per program", a);
                    }
                    let e = extra.entry(pin.root.clone()).or_default();
                    if !e.contains(&a) {
                        e.push(a.clone());
                    }
                }
                _ => {
                    alias_of_root.insert(pin.root.clone(), a.clone());
                }
            }
            root_of_alias.entry(a.clone()).or_insert_with(|| pin.root.clone());
        }
        if by_root.contains_key(&pin.root) {
            by_root.get_mut(&pin.root).unwrap().grant_specs.push((spec.clone(), line));
            continue; // already lexed
        }
        let tree = store_tree(&pin.root).unwrap_or_else(|| {
            user_err!("parse", Some(line), "package {} root {} is not in the content store — rerun `nk install .`", id, pin.root)
        });
        let meta = meta_from_tree(&tree);
        let (tu_toks, struct_names) = lex_tree(&tree);
        if tu_toks.is_empty() {
            user_err!("parse", Some(line), "package {} root {} contains no .n/.nd translation units", id, pin.root);
        }
        let mut tu_files: Vec<(String, String)> = Vec::new();
        let mut manifests: Vec<(String, String)> = Vec::new();
        for (rel, sha) in &tree {
            if rel == "nk.pkg" {
                continue;
            }
            if rel.ends_with(".ufm") {
                manifests.push((rel.clone(), sha.clone()));
            } else {
                tu_files.push((rel.clone(), sha.clone()));
            }
        }
        tu_files.sort_by(|a, b| {
            let am = a.0 == "main.n" || a.0 == "main.nd";
            let bm = b.0 == "main.n" || b.0 == "main.nd";
            bm.cmp(&am).then(a.0.cmp(&b.0))
        });
        // deps of deps: schemed uses inside this package's sources
        for (_, toks) in &tu_toks {
            for (l, t) in toks {
                if let crate::ast::Tok::Use(s) = t {
                    let (did, _) = split_ref(&s.source);
                    if is_schemed(&did) {
                        work.push((s.clone(), *l));
                    }
                }
            }
        }
        by_root.insert(
            pin.root.clone(),
            PkgSources { pin: pin.clone(), meta, grant_specs: vec![(spec.clone(), line)], tu_files, manifests, struct_names },
        );
        order.push(pin.root.clone());
    }

    // Phase B: per-root grant union, struct qualification, TU parse.
    let mut out: Vec<LoadedPkg> = Vec::new();
    for root in &order {
        let src = by_root.get(root).unwrap();
        let id = &src.pin.source;
        let mut grant: Vec<String> = Vec::new();
        for (spec, line) in &src.grant_specs {
            for c in compute_grant(spec, *line, id, &src.meta.caps, program_caps, baked) {
                if !grant.contains(&c) {
                    grant.push(c);
                }
            }
        }
        grant.sort();
        let alias = alias_of_root.get(root).cloned().unwrap_or_else(|| pkg_modname(id, &src.meta));
        if let Some(prev) = root_of_alias.get(&alias) {
            if prev != root {
                user_err!("pkg", None, "package alias `{}` collides: {} and {} — give one an explicit use alias", alias, prev, root);
            }
        }
        root_of_alias.insert(alias.clone(), root.clone());
        let pkg_caps = crate::sandbox::caps_for_grants(&grant, base_dir);
        let mut tus: Vec<Parsed> = Vec::new();
        let mut seen_labels: HashMap<String, String> = HashMap::new(); // label -> file
        for (ti, (rel, sha)) in src.tu_files.iter().enumerate() {
            let mut toks = crate::lex::lex_source(&blob_contents(sha));
            qualify_struct_refs(&mut toks, &alias, &src.struct_names);
            if ti == 0 {
                // the package's .ufm binding manifests prepend to its first
                // TU, exactly like local mods manifests
                let mut mt_all = Vec::new();
                for (_mrel, msha) in &src.manifests {
                    let msrc = blob_contents(msha);
                    let mut mt = crate::lex::lex_source(&msrc);
                    for t in mt.iter_mut() {
                        if let (ln, crate::ast::Tok::Import(im)) = t {
                            *t = (*ln, crate::ast::Tok::ManifestImport(im.clone()));
                        }
                    }
                    mt_all.extend(mt);
                }
                toks.splice(0..0, mt_all);
            }
            let mut parsed = crate::parse::parse(toks, structs, &pkg_caps);
            // a package is one namespace: duplicate labels across its files error
            for (name, _) in &parsed.labels {
                if let Some(prev_file) = seen_labels.get(name) {
                    user_err!("pkg", None, "package {} (alias {}) defines label `{}` in both {} and {} — a package is one namespace; rename one", id, alias, name, prev_file, rel);
                }
                seen_labels.insert(name.clone(), rel.clone());
            }
            // labels are addressable via the alias (per-TU mod mangling
            // registers `alias:name`); plain-name PUB globals are
            // suppressed — two versions would collide there, and the
            // alias IS the public name.
            parsed.pubs.clear();
            tus.push(parsed);
        }
        // twin the qualified structs under any extra aliases NOW — caller
        // TUs parse next and their `_cast alias:Name` / `_offset` references
        // must resolve (label twins register post-merge, when labels exist)
        for e in extra.get(root).cloned().unwrap_or_default() {
            let prefix = format!("{}:", alias);
            let snames: Vec<String> =
                structs.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
            for k in snames {
                let nk = format!("{}:{}", e, &k[prefix.len()..]);
                if !structs.contains_key(&nk) {
                    if let Some(v) = structs.get(&k).cloned() {
                        structs.insert(nk, v);
                    }
                }
            }
        }
        out.push(LoadedPkg {
            alias: alias.clone(),
            extra_aliases: extra.get(root).cloned().unwrap_or_default(),
            pin: src.pin.clone(),
            meta: src.meta.clone(),
            grant,
            tus,
            mods: src.tu_files.iter().map(|_| alias.clone()).collect(),
        });
    }
    out
}

/// Post-merge registration of extra aliases (same content bound under a
/// second name): every mangled label and qualified struct gains a twin.
pub fn register_extra_aliases(
    parsed: &mut crate::ast::Parsed,
    structs: &mut crate::ast::StructMap,
    alias: &str,
    extras: &[String],
) {
    for e in extras {
        let prefix = format!("{}:", alias);
        let names: Vec<(String, usize)> = parsed
            .labels
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        for (k, v) in names {
            let nk = format!("{}:{}", e, &k[prefix.len()..]);
            parsed.labels.insert(nk, v);
        }
        let sprefix = format!("{}:", alias);
        let snames: Vec<String> = structs.keys().filter(|k| k.starts_with(&sprefix)).cloned().collect();
        for k in snames {
            let nk = format!("{}:{}", e, &k[sprefix.len()..]);
            if let Some(v) = structs.get(&k).cloned() {
                structs.insert(nk, v);
            }
        }
    }
}

pub fn split_tree(tree: &[(String, String)]) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let mut tus: Vec<(String, String)> = Vec::new();
    let mut manifests: Vec<(String, String)> = Vec::new();
    for (rel, sha) in tree {
        if rel == "nk.pkg" {
            continue; // package metadata, consumed by resolve (never compiled)
        }
        if rel.ends_with(".ufm") {
            manifests.push((rel.clone(), sha.clone()));
        } else {
            tus.push((rel.clone(), sha.clone()));
        }
    }
    tus.sort_by(|a, b| {
        let am = a.0 == "main.n" || a.0 == "main.nd";
        let bm = b.0 == "main.n" || b.0 == "main.nd";
        bm.cmp(&am).then(a.0.cmp(&b.0))
    });
    (tus, manifests)
}

// ---------------- install ----------------

struct InstallOpts {
    yes: bool,
    locked: bool,
    json: bool,
    refresh: Option<String>,
    grant: Option<Vec<String>>, // tool caps (single-source installs with bins)
    target: Option<String>,
}

fn parse_install_args(args: &[String]) -> InstallOpts {
    let mut o = InstallOpts { yes: false, locked: false, json: false, refresh: None, grant: None, target: None };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--yes" | "-y" => o.yes = true,
            "--locked" => o.locked = true,
            "--json" => o.json = true,
            "--grant" => {
                i += 1;
                let g = args.get(i).unwrap_or_else(|| user_err!("pkg", None, "--grant needs a comma-separated capability list (e.g. fs.read,proc)"));
                o.grant = Some(g.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());
            }
            "--refresh" => {
                i += 1;
                o.refresh = Some(args.get(i).unwrap_or_else(|| user_err!("pkg", None, "--refresh needs a package source (e.g. gh:owner/repo)")).clone());
            }
            "--" => break,
            s if s.starts_with('-') => user_err!("pkg", None, "unknown install option {}", s),
            s => {
                if o.target.is_some() {
                    user_err!("pkg", None, "install takes one target (source or directory), got `{}` and `{}`", o.target.as_deref().unwrap_or(""), s);
                }
                o.target = Some(s.to_string());
            }
        }
        i += 1;
    }
    o
}

fn report(o: &InstallOpts, line: String) {
    if o.json {
        // single-line machine mirror; callers pass pre-rendered json too
        println!("{}", line);
    } else {
        println!("{}", line);
    }
}

fn approval_gate(o: &InstallOpts, id: &str, root: &str, diff: &str, repo: &Path) -> bool {
    approval_gate_key(o, id, root, diff, repo, None)
}

fn approval_gate_key(o: &InstallOpts, id: &str, root: &str, diff: &str, repo: &Path, key: Option<&str>) -> bool {
    if o.locked {
        user_err!("pkg", None, "--locked: {} root {} is not approved (drift) — approve it outside CI, or refresh the pin", id, root);
    }
    let existing = fs::read_to_string(approvals_path(repo)).unwrap_or_default();
    let apps = parse_approvals(&existing);
    if approved(&apps, id, root) {
        return true;
    }
    if !o.yes {
        use std::io::IsTerminal as _;
        if !std::io::stdin().is_terminal() {
            user_err!("pkg", None, "approval required for {} root={}{}— rerun with --yes to record it non-interactively", id, root, diff);
        }
        print!("approve {} root={}{}? [y/N] ", id, root, diff);
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap_or(0);
        let a = line.trim().to_ascii_lowercase();
        if a != "y" && a != "yes" {
            println!("skipped {} (not approved)", id);
            return false;
        }
    }
    let by = if o.yes { "flag(--yes)".to_string() } else {
        std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "interactive".to_string())
    };
    let mut apps2 = apps;
    apps2.push(Approval { source: id.to_string(), root: root.to_string(), date: iso_date(now_secs()), by, key: key.map(|k| k.to_string()) });
    let mut s = String::from("; nk package approvals — content-roots a human approved\n");
    for a in &apps2 {
        match &a.key {
            Some(k) => s.push_str(&format!("approve source={} root={} date={} by={} key={}\n", a.source, a.root, a.date, a.by, k)),
            None => s.push_str(&format!("approve source={} root={} date={} by={}\n", a.source, a.root, a.date, a.by)),
        }
    }
    fs::create_dir_all(repo.join(".nk")).unwrap_or_else(|e| user_err!("pkg", None, "cannot create .nk: {}", e));
    write_atomic(&approvals_path(repo), &s);
    true
}

fn tree_diff(old: Option<&[(String, String)]>, new: &[(String, String)]) -> String {
    let (mut added, mut removed, mut changed) = (Vec::new(), Vec::new(), Vec::new());
    match old {
        None => added.extend(new.iter().map(|(r, _)| r.clone())),
        Some(old) => {
            let om: HashMap<&String, &String> = old.iter().map(|(r, s)| (r, s)).collect();
            let nm: HashMap<&String, &String> = new.iter().map(|(r, s)| (r, s)).collect();
            for (r, s) in new {
                match om.get(r) {
                    None => added.push(r.clone()),
                    Some(os) if os != &s => changed.push(r.clone()),
                    _ => {}
                }
            }
            for r in old {
                if !nm.contains_key(&r.0) {
                    removed.push(r.0.clone());
                }
            }
        }
    }
    if added.is_empty() && removed.is_empty() && changed.is_empty() {
        return String::new();
    }
    let mut s = format!(" ({} added, {} removed, {} changed:", added.len(), removed.len(), changed.len());
    for r in added.iter().take(5) { s.push_str(&format!(" +{}", r)); }
    for r in removed.iter().take(5) { s.push_str(&format!(" -{}", r)); }
    for r in changed.iter().take(5) { s.push_str(&format!(" ~{}", r)); }
    if added.len() + removed.len() + changed.len() > 15 { s.push_str(" …"); }
    s.push(')');
    s
}

/// Verify a package's detached signature envelope: `nk.pub` (hex ed25519
/// public key) + `nk.sig` (hex signature over the nkph1 root string). The
/// envelope files are read from the fetched dir (nk.sig is excluded from
/// the hashed set — a signature cannot sign itself). Returns Some(pubhex)
/// when signed, None when unsigned; malformed or failing envelopes are
/// hard errors.
fn verify_signature(dir: &Path, root: &str) -> Option<String> {
    let pub_path = dir.join("nk.pub");
    let sig_path = dir.join("nk.sig");
    match (pub_path.exists(), sig_path.exists()) {
        (false, false) => return None,
        (true, true) => {}
        _ => user_err!("pkg", None, "incomplete signature envelope in {}: nk.pub and nk.sig must ship together", dir.display()),
    }
    let pub_hex = fs::read_to_string(&pub_path).unwrap_or_default().trim().to_string();
    let sig_hex = fs::read_to_string(&sig_path).unwrap_or_default().trim().to_string();
    let pk = crate::ed25519::from_hex32(&pub_hex).unwrap_or_else(|| {
        user_err!("pkg", None, "nk.pub is not 64 hex characters (an ed25519 public key)")
    });
    let mut sig = [0u8; 64];
    if sig_hex.len() != 128 || !sig_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        user_err!("pkg", None, "nk.sig is not 128 hex characters (an ed25519 signature over the nkph1 root)")
    }
    for (i, b) in sig.iter_mut().enumerate() {
        *b = u8::from_str_radix(&sig_hex[2 * i..2 * i + 2], 16).unwrap_or(0);
    }
    if !crate::ed25519::verify(&pk, root.as_bytes(), &sig) {
        user_err!("pkg", None, "signature verification FAILED for root {} — the content does not match its signature; do not approve", root)
    }
    Some(pub_hex)
}

/// Install one source into `repo`'s lockfile: fetch, hash, verify the
/// signature envelope, approve, store, pin. Returns the new pin, or None
/// when the human declined.
fn install_one(o: &InstallOpts, id: &str, ref_: Option<&str>, repo: &Path) -> Option<Pin> {
    let f = fetch(id, ref_);
    let content = hash_package(&f.dir);
    let lock = parse_lock(&fs::read_to_string(repo.join(".nk").join("lock")).unwrap_or_default());
    let old_pin = match ref_ {
        Some(r) => lock.for_source_ref(id, r).cloned(),
        None => lock.for_source(id).first().map(|p| (*p).clone()),
    };
    if let Some(op) = &old_pin {
        if op.root == content.root {
            if store_tree(&content.root).is_none() {
                store_put(&content, &f.dir);
            }
            report(o, format!("verified\t{}\t{}\t{}", id, f.resolved_ref, content.root));
            return Some(op.clone());
        }
    }
    let old_tree = old_pin.as_ref().and_then(|p| store_tree(&p.root));
    let mut diff = tree_diff(old_tree.as_deref(), &content.files);
    // signature envelope: verified before any approval gate — a failing
    // signature is a hard error, never a prompt
    let sig_key = verify_signature(&f.dir, &content.root);
    let signed = sig_key.is_some();
    if let Some(prev_signed) = old_pin.as_ref().map(|p| p.signed) {
        if prev_signed && !signed {
            let note = " [UNSIGNED after a signed pin]";
            if o.locked {
                user_err!("pkg", None, "--locked: {} was signed (root {}) and the fetched content is unsigned — signature continuity is drift", id, old_pin.as_ref().unwrap().root);
            }
            diff.push_str(note);
        }
    }
    if let (Some(k), Some(apps_src)) = (&sig_key, fs::read_to_string(approvals_path(repo)).ok()) {
        let prev_key = parse_approvals(&apps_src).iter().rev().find(|a| a.source == id).and_then(|a| a.key.clone());
        if let Some(pk) = prev_key {
            if pk != *k {
                diff.push_str(" [PUBLISHER KEY ROTATED — verify out of band]");
            }
        }
    }
    if !approval_gate_key(o, id, &content.root, &diff, repo, sig_key.as_deref()) {
        return None;
    }
    store_put(&content, &f.dir);
    let pin = Pin { source: id.to_string(), ref_: f.resolved_ref, sha: f.sha, root: content.root, signed };
    if f.temp {
        let _ = fs::remove_dir_all(&f.dir);
    }
    let sig_note = if signed { " signed" } else { "" };
    report(o, format!("resolved\t{}\t{}\t{}{}", id, pin.ref_, pin.root, sig_note));
    Some(pin)
}

/// Recursively pin a freshly installed package's own package dependencies
/// (schemed uses inside its tree). Depth-first; a dependency cycle fails
/// naturally when a dep's own root pins nothing new.
fn pull_deps(o: &InstallOpts, pin: &Pin, repo: &Path, lock: &mut Lock) {
    let tree = match store_tree(&pin.root) {
        Some(t) => t,
        None => return,
    };
    for (rel, sha) in &tree {
        if rel.ends_with(".ufm") || rel == "nk.pkg" {
            continue;
        }
        for (spec, _line) in specs_of_src(&blob_contents(sha)) {
            let (id, ref_) = split_ref(&spec.source);
            let pinned = match &ref_ {
                Some(r) => lock.for_source_ref(&id, r).is_some(),
                None => !lock.for_source(&id).is_empty(),
            };
            if pinned {
                continue;
            }
            if o.locked {
                user_err!("pkg", None, "--locked: transitive dependency {} (of {}) is not pinned — pin it outside CI with `nk install {}`", id, pin.source, id);
            }
            if let Some(p) = install_one(o, &id, ref_.as_deref(), repo) {
                lock.pins.retain(|q| !(q.source == p.source && q.ref_ == p.ref_));
                lock.pins.push(p.clone());
                pull_deps(o, &p, repo, lock);
            }
        }
    }
}

/// Collect schemed use specs from a source file's tokens.
fn specs_of_src(src: &str) -> Vec<(UseSpec, u32)> {
    crate::lex::lex_source(src)
        .into_iter()
        .filter_map(|(l, t)| match t {
            crate::ast::Tok::Use(s) if is_schemed(&split_ref(&s.source).0) => Some((s, l)),
            _ => None,
        })
        .collect()
}

pub fn cmd_install(args: &[String]) {
    let o = parse_install_args(args);
    let target = o.target.clone().unwrap_or_else(|| ".".to_string());
    if is_schemed(&split_ref(&target).0) {
        // single-source install (gh:/https:/path:); lock lives in CWD's project root
        let (id, ref_) = split_ref(&target);
        if o.refresh.is_some() {
            user_err!("pkg", None, "--refresh applies to directory targets (`nk install . --refresh <source>`); a single-source install with no @ref already resolves latest");
        }
        let repo = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let pin = install_one(&o, &id, ref_.as_deref(), &repo);
        if let Some(p) = pin {
            if !o.locked {
                let mut lock = parse_lock(&fs::read_to_string(repo.join(".nk").join("lock")).unwrap_or_default());
                lock.pins.retain(|q| !(q.source == p.source && q.ref_ == p.ref_));
                lock.pins.push(p.clone());
                let path = lock.write_to(&repo);
                report(&o, format!("pin\t{}\t{}\t{}", p.source, p.ref_, path.display()));
                if p.ref_ != "-" {
                    report(&o, format!("use \"{}@{}\"", p.source, p.ref_));
                } else {
                    report(&o, format!("use \"{}\"", p.source));
                }
                // bins: compile-at-install tools (grants: --grant, else all
                // declared caps — the approval above covered the content)
                let tree = store_tree(&p.root);
                if let Some(tree) = tree {
                    let meta = meta_from_tree(&tree);
                    if !meta.bins.is_empty() {
                        let grants = match &o.grant {
                            Some(g) => g.clone(),
                            None => meta.caps.iter().map(|(c, _)| c.clone()).collect(),
                        };
                        for g in &grants {
                            if !meta.caps.iter().any(|(c, _)| c == g) {
                                user_err!("pkg", None, "--grant {} exceeds {}'s declared capabilities ({})", g, id, meta.caps.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>().join(", "));
                            }
                        }
                        install_tools(&id, &p, &meta, &grants);
                    }
                }
            }
        }
        return;
    }
    // directory reconcile
    let root = PathBuf::from(&target);
    if !root.is_dir() {
        user_err!("pkg", None, "install target `{}` is neither a package source (gh:/https:/path:) nor a directory", target);
    }
    let (files, _init) = crate::driver::discover_directory(&target);
    let mut specs: Vec<(UseSpec, u32)> = Vec::new();
    for f in &files {
        let src = fs::read_to_string(f).unwrap_or_else(|e| user_err!("pkg", None, "cannot read {}: {}", f, e));
        specs.extend(specs_of_src(&src));
    }
    // transitive closure: schemed uses inside pinned package trees are
    // dependencies too — walk store trees to a fixpoint so `install .`
    // pins deps-of-deps
    {
        let mut seen: Vec<String> = Vec::new();
        loop {
            let lock = parse_lock(&fs::read_to_string(root.join(".nk").join("lock")).unwrap_or_default());
            for pin in &lock.pins {
                if seen.contains(&pin.root) {
                    continue;
                }
                seen.push(pin.root.clone());
                if let Some(tree) = store_tree(&pin.root) {
                    for (rel, sha) in &tree {
                        if rel.ends_with(".ufm") || rel == "nk.pkg" {
                            continue;
                        }
                        specs.extend(specs_of_src(&blob_contents(sha)));
                    }
                }
            }
            let mut u2: Vec<(UseSpec, u32)> = Vec::new();
            for (s, l) in specs.clone() {
                if !u2.iter().any(|(u, _)| u == &s) {
                    u2.push((s, l));
                }
            }
            let grew = u2.len() > specs.len() || {
                // also continue when an unseen pin appears later (new installs)
                let lk = parse_lock(&fs::read_to_string(root.join(".nk").join("lock")).unwrap_or_default());
                lk.pins.iter().any(|p| !seen.contains(&p.root))
            };
            specs = u2;
            if !grew {
                break;
            }
        }
    }
    // dedupe by (source, ref) keeping first line
    let mut uniq: Vec<(UseSpec, u32)> = Vec::new();
    for (s, l) in specs {
        if !uniq.iter().any(|(u, _)| u == &s) {
            uniq.push((s, l));
        }
    }
    let lock_path = root.join(".nk").join("lock");
    let mut lock = parse_lock(&fs::read_to_string(&lock_path).unwrap_or_default());

    // decide actions
    struct Action { id: String, ref_: Option<String>, verify: Option<Pin> }
    let mut actions: Vec<Action> = Vec::new();
    for (spec, _line) in &uniq {
        let (id, ref_) = split_ref(&spec.source);
        let refresh_this = o.refresh.as_deref().map(|r| split_ref(r).0 == id).unwrap_or(false);
        if let Some(r) = &o.refresh {
            let (rid, _) = split_ref(r);
            if rid == id {
                actions.push(Action { id, ref_: None, verify: None });
                continue;
            }
        }
        let _ = refresh_this;
        match &ref_ {
            Some(r) => {
                let pin = lock.for_source_ref(&id, r);
                if pin.is_some() && !refresh_this {
                    actions.push(Action { id, ref_: Some(r.clone()), verify: pin.cloned() });
                } else {
                    actions.push(Action { id, ref_: Some(r.clone()), verify: None });
                }
            }
            None => {
                let ps = lock.for_source(&id);
                if ps.is_empty() {
                    actions.push(Action { id, ref_: None, verify: None });
                } else if ps.len() == 1 {
                    if refresh_this {
                        actions.push(Action { id, ref_: None, verify: None });
                    } else {
                        actions.push(Action { id, ref_: None, verify: Some(ps[0].clone()) });
                    }
                } else {
                    user_err!("pkg", None, "package {} has {} pins; a bare use \"{}\" is ambiguous — pin a version at the use site (use \"{}@<ref>\")", id, ps.len(), id, id);
                }
            }
        }
    }

    for a in &actions {
        if let Some(p) = &a.verify {
            // verify pin: re-fetch + re-hash; mismatch is drift (locked: fail)
            let f = fetch(&a.id, if p.ref_ == "-" { None } else { Some(p.ref_.as_str()) });
            let content = hash_package(&f.dir);
            if content.root != p.root {
                let diff = tree_diff(store_tree(&p.root).as_deref(), &content.files);
                if o.locked {
                    user_err!("pkg", None, "--locked: {} @{} content drifted: pinned {} != fetched {}{} — refresh the pin outside CI", a.id, p.ref_, p.root, content.root, diff);
                }
                if !approval_gate(&o, &a.id, &content.root, &diff, &root) {
                    continue;
                }
                store_put(&content, &f.dir);
                report(&o, format!("updated\t{}\t{}\t{}", a.id, f.resolved_ref, content.root));
                lock.pins.retain(|q| !(q.source == a.id && q.ref_ == p.ref_));
                lock.pins.push(Pin { source: a.id.clone(), ref_: f.resolved_ref, sha: f.sha, root: content.root, signed: false });
                continue;
            }
            if store_tree(&p.root).is_none() {
                if o.locked {
                    user_err!("pkg", None, "--locked: {} root {} missing from store — rerun `nk install .` outside CI", a.id, p.root);
                }
                store_put(&content, &f.dir);
            }
            report(&o, format!("verified\t{}\t{}\t{}", a.id, p.ref_, p.root));
            continue;
        }
        // new pin (explicit ref or latest)
        if o.locked {
            user_err!("pkg", None, "--locked: package {} is not pinned in .nk/lock — pin it outside CI with `nk install {}`", a.id, a.id);
        }
        if let Some(pin) = install_one(&o, &a.id, a.ref_.as_deref(), &root) {
            lock.pins.retain(|q| !(q.source == pin.source && q.ref_ == pin.ref_));
            lock.pins.push(pin.clone());
            // deps of the new pin: schemed uses inside its tree install
            // recursively (a dependency not yet pinned is part of this install)
            pull_deps(&o, &pin, &root, &mut lock);
        }
    }

    // prune unreachable pins: reachability starts at the project's direct
    // use ids and follows each pin's own tree (deps-of-deps stay pinned)
    let direct_ids: Vec<String> = uniq.iter().map(|(s, _)| split_ref(&s.source).0).collect();
    let mut reach: Vec<String> = direct_ids.clone();
    let mut queue = direct_ids;
    while let Some(id) = queue.pop() {
        for p in lock.for_source(&id) {
            if let Some(tree) = store_tree(&p.root) {
                for (rel, sha) in &tree {
                    if rel.ends_with(".ufm") || rel == "nk.pkg" {
                        continue;
                    }
                    for (spec, _l) in specs_of_src(&blob_contents(sha)) {
                        let did = split_ref(&spec.source).0;
                        if !reach.contains(&did) {
                            reach.push(did.clone());
                            queue.push(did);
                        }
                    }
                }
            }
        }
    }
    let before = lock.pins.len();
    lock.pins.retain(|p| reach.contains(&p.source));
    if lock.pins.len() != before {
        let removed = before - lock.pins.len();
        if o.locked {
            user_err!("pkg", None, "--locked: {} lockfile pin(s) no longer reachable from any use — prune them outside CI", removed);
        }
        report(&o, format!("pruned\t{}\tpins", removed));
    }
    if !o.locked {
        lock.write_to(&root);
        // .nk/tools: repo-pinned toolchain (mise/asdf semantics). Each line
        // `tool <name> = <source>[@ref] [grant <csv>]` installs/verifies the
        // pinned tool — the registry entry must match the pinned root.
        let tools_file = root.join(".nk").join("tools");
        if let Ok(src) = fs::read_to_string(&tools_file) {
            for line in src.lines() {
                let line = line.split(';').next().unwrap_or("").trim();
                if line.is_empty() {
                    continue;
                }
                let rest = match line.strip_prefix("tool ") {
                    Some(r) => r,
                    None => user_err!("pkg", None, ".nk/tools: bad line `{}` (expected tool <name> = <source>[@ref] [grant <caps>])", line),
                };
                let (name, spec_part) = rest.split_once('=')
                    .unwrap_or_else(|| user_err!("pkg", None, ".nk/tools: bad line `{}` (expected tool <name> = <source>[@ref] [grant <caps>])", line));
                let name = name.trim().to_string();
                let spec_part = spec_part.trim();
                let (spec, grants): (&str, Vec<String>) = match spec_part.split_once(" grant ") {
                    Some((s, g)) => (s.trim(), g.split(',').filter(|c| !c.is_empty()).map(|c| c.trim().to_string()).collect()),
                    None => (spec_part, Vec::new()),
                };
                if name.is_empty() || spec.is_empty() {
                    user_err!("pkg", None, ".nk/tools: bad line `{}` (expected tool <name> = <source>[@ref] [grant <caps>])", line);
                }
                let (tid, tref) = split_ref(&spec);
                // already installed at the pinned root?
                let lock2 = parse_lock(&fs::read_to_string(&lock_path).unwrap_or_default());
                let pin = tref
                    .as_ref()
                    .and_then(|r| lock2.for_source_ref(&tid, r).cloned())
                    .or_else(|| lock2.for_source(&tid).first().map(|p| (*p).clone()));
                let pin = match pin {
                    Some(p) => p,
                    None => {
                        // pin the tool source into this repo's lock, then re-read
                        if let Some(p) = install_one(&o, &tid, tref.as_deref(), &root) {
                            let mut l2 = parse_lock(&fs::read_to_string(&lock_path).unwrap_or_default());
                            l2.pins.retain(|q| !(q.source == p.source && q.ref_ == p.ref_));
                            l2.pins.push(p.clone());
                            write_atomic(&lock_path, &l2.render());
                            p
                        } else {
                            continue;
                        }
                    }
                };
                let reg = read_registry();
                if reg.iter().any(|t| t.name == name && t.root == pin.root) {
                    report(&o, format!("tool-ok\t{}\t{}", name, pin.root));
                    continue;
                }
                let tree = store_tree(&pin.root).unwrap_or_else(|| user_err!("pkg", None, "tool {} root {} missing from store", name, pin.root));
                let meta = meta_from_tree(&tree);
                if !meta.bins.iter().any(|(n, _)| n == &name) {
                    user_err!("pkg", None, ".nk/tools: package {} declares no bin `{}` (bins: {})", tid, name, meta.bins.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ").trim_end_matches(',').trim());
                }
                let gr = if grants.is_empty() { meta.caps.iter().map(|(c, _)| c.clone()).collect() } else { grants };
                install_tools(&tid, &pin, &meta, &gr);
            }
        }
    }
}

pub fn cmd_pkg(args: &[String]) {
    match args.get(0).map(|s| s.as_str()) {
        Some("gc") => cmd_gc(),
        // author key management: ed25519 (RFC 8032) detached signatures
        // over nkph1 roots; a package ships nk.pub + nk.sig at its root
        Some("keygen") => {
            // seed from /dev/urandom (std-only; no rand crate)
            let mut seed = [0u8; 32];
            {
                use std::io::Read as _;
                let mut f = fs::File::open("/dev/urandom")
                    .unwrap_or_else(|e| user_err!("pkg", None, "keygen: cannot open /dev/urandom: {}", e));
                f.read_exact(&mut seed).unwrap_or_else(|e| user_err!("pkg", None, "keygen: {}", e));
            }
            let pubhex = crate::ed25519::to_hex(&crate::ed25519::public_from_seed(&seed));
            println!("seed={}", crate::ed25519::to_hex(&seed));
            println!("pub={}", pubhex);
            eprintln!("; keep the seed private; commit nk.pub (and nk.sig per release) in the package");
        }
        Some("pub") => {
            let seed = seed_arg(args.get(1));
            println!("{}", crate::ed25519::to_hex(&crate::ed25519::public_from_seed(&seed)));
        }
        Some("sign") => {
            let seed = seed_arg(args.get(1));
            let mut msg = String::new();
            use std::io::Read as _;
            std::io::stdin().read_to_string(&mut msg)
                .unwrap_or_else(|e| user_err!("pkg", None, "sign: cannot read stdin: {}", e));
            let msg = msg.trim();
            println!("{}", crate::ed25519::to_hex(&crate::ed25519::sign(&seed, msg.as_bytes())));
        }
        _ => user_err!("pkg", None, "usage: nk pkg gc | keygen | pub <seed-hex> | sign <seed-hex>  (sign reads the nkph1 root on stdin)"),
    }
}

fn seed_arg(a: Option<&String>) -> [u8; 32] {
    let s = a.unwrap_or_else(|| user_err!("pkg", None, "missing <seed-hex> (64 hex characters from `nk pkg keygen`)"));
    crate::ed25519::from_hex32(s).unwrap_or_else(|| user_err!("pkg", None, "bad seed `{}` — expected 64 hex characters", s))
}

pub fn cmd_gc() {
    let roots_dir = store_root().join("roots");
    if !roots_dir.exists() {
        println!("store empty");
        return;
    }
    // keep roots referenced by any .nk/lock under the current directory
    let mut live: Vec<String> = Vec::new();
    let mut stack = vec![std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&d) {
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                if p.is_dir() {
                    if name == ".git" || name == "target" || name == "node_modules" {
                        continue;
                    }
                    stack.push(p);
                } else if name == "lock" && d.file_name().map(|n| n == ".nk").unwrap_or(false) {
                    if let Ok(s) = fs::read_to_string(&p) {
                        for pin in parse_lock(&s).pins {
                            let r = pin.root.strip_prefix("nkph1:").unwrap_or(&pin.root).to_string();
                            if !live.contains(&r) {
                                live.push(r);
                            }
                        }
                    }
                }
            }
        }
    }
    let mut removed = 0usize;
    for e in fs::read_dir(&roots_dir).unwrap_or_else(|e| user_err!("pkg", None, "gc: {}", e)).filter_map(|e| e.ok()) {
        let p = e.path();
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if !live.contains(&name) {
            if fs::remove_dir_all(&p).is_ok() {
                removed += 1;
            }
        }
    }
    println!("gc: {} root(s) kept, {} removed (store is a cache — nothing can go missing)", live.len(), removed);
}

// ---------------- tools (bins, phase 5) ----------------

#[derive(Clone, Debug)]
pub struct ToolEntry {
    pub name: String,
    pub source: String,
    pub root: String,
    pub grant: Vec<String>,
    pub bin: String,
}

fn tools_registry_path() -> PathBuf {
    home_base().join("tools").join("registry")
}

fn tools_bin_dir() -> PathBuf {
    home_base().join("tools").join("bin")
}

fn shim_dir() -> PathBuf {
    home_base().join("bin")
}

pub fn read_registry() -> Vec<ToolEntry> {
    let mut out = Vec::new();
    let src = fs::read_to_string(tools_registry_path()).unwrap_or_default();
    for line in src.lines() {
        let line = line.split(';').next().unwrap_or("").trim();
        if let Some(rest) = line.strip_prefix("tool ") {
            let f = parse_kv_fields(rest);
            if let (Some(n), Some(s), Some(r)) = (f.get("name"), f.get("source"), f.get("root")) {
                out.push(ToolEntry {
                    name: n.clone(),
                    source: s.clone(),
                    root: r.clone(),
                    grant: f.get("grant").map(|g| g.split(',').filter(|c| !c.is_empty()).map(|c| c.to_string()).collect()).unwrap_or_default(),
                    bin: f.get("bin").cloned().unwrap_or_default(),
                });
            }
        }
    }
    out
}

fn write_registry(entries: &[ToolEntry]) {
    let d = tools_registry_path().parent().unwrap().to_path_buf();
    fs::create_dir_all(&d).unwrap_or_else(|e| user_err!("pkg", None, "cannot create {}: {}", d.display(), e));
    let mut s = String::from("; nk tool registry — installed bins, their pins and grants\n");
    for t in entries {
        s.push_str(&format!(
            "tool name={} source={} root={} grant={} bin={}\n",
            t.name,
            t.source,
            t.root,
            t.grant.join(","),
            t.bin
        ));
    }
    write_atomic(&tools_registry_path(), &s);
}

/// Materialize a stored package root into a temp dir of real files (for
/// bin compilation via a compiler self-exec).
fn materialize(root: &str) -> PathBuf {
    let tree = store_tree(root).unwrap_or_else(|| user_err!("pkg", None, "tool root {} missing from store", root));
    let d = tmp_dir("tool");
    for (rel, sha) in &tree {
        let p = d.join(rel);
        if let Some(par) = p.parent() {
            let _ = fs::create_dir_all(par);
        }
        fs::write(&p, blob_contents(sha)).unwrap_or_else(|e| user_err!("pkg", None, "cannot materialize {}: {}", rel, e));
    }
    d
}

/// Compile one bin by re-executing this compiler: `nk -c <tu> -o <bin>
/// --sandbox <grant .ufs> --workspace .`. The sandbox file restricts the
/// compile to the granted caps; `--workspace .` bakes a relative root so
/// the tool's filesystem workspace resolves at invocation time (cwd),
/// never install time. Binaries are compiled from approved source bytes by
/// the user's own compiler — never shipped prebuilt.
fn compile_tool(dir: &Path, tu_rel: &str, name: &str, grants: &[String]) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|e| user_err!("pkg", None, "cannot locate compiler: {}", e));
    let tu = dir.join(tu_rel);
    if !tu.exists() {
        user_err!("pkg", None, "tool bin `{}`: TU {} not in package", name, tu_rel);
    }
    let mut ufs = String::from("; generated tool grant config\nname tool-grants\ndeny *\n");
    for g in grants {
        ufs.push_str(&format!("allow {}\n", g));
    }
    ufs.push_str("workspace .\n");
    let ufs_path = dir.join("nk-tool-grants.ufs");
    fs::write(&ufs_path, &ufs).unwrap_or_else(|e| user_err!("pkg", None, "cannot write grant config: {}", e));
    fs::create_dir_all(tools_bin_dir()).unwrap_or_else(|e| user_err!("pkg", None, "cannot create {}: {}", tools_bin_dir().display(), e));
    let out = tools_bin_dir().join(name);
    let st = Command::new(&exe)
        .arg("-c")
        .arg(&tu)
        .arg("-o")
        .arg(&out)
        .arg("--sandbox")
        .arg(&ufs_path)
        .arg("--workspace")
        .arg(".")
        .status()
        .unwrap_or_else(|e| user_err!("pkg", None, "compiler self-exec failed: {}", e));
    if !st.success() {
        user_err!("pkg", None, "tool `{}` failed to compile (exit {})", name, st.code().unwrap_or(-1));
    }
    out
}

fn install_tools(id: &str, pin: &Pin, meta: &PkgMeta, grants: &[String]) {
    let dir = materialize(&pin.root);
    let mut entries = read_registry();
    for (name, tu_rel) in &meta.bins {
        let bin = compile_tool(&dir, tu_rel, name, grants);
        entries.retain(|t| t.name != *name);
        entries.push(ToolEntry {
            name: name.clone(),
            source: id.to_string(),
            root: pin.root.clone(),
            grant: grants.to_vec(),
            bin: bin.to_string_lossy().to_string(),
        });
        // PATH shim (atomic swap on upgrade)
        fs::create_dir_all(shim_dir()).unwrap_or_else(|e| user_err!("pkg", None, "cannot create {}: {}", shim_dir().display(), e));
        let shim = shim_dir().join(name);
        write_atomic(&shim, &format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", bin.to_string_lossy()));
        // PATH shim (atomic swap on upgrade); exec bit where the platform
        // has one (the wasm library build has no unix perms — skip there)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut p = fs::metadata(&shim)
                .map(|m| m.permissions())
                .unwrap_or_else(|_| std::fs::Permissions::from_mode(0o644));
            p.set_mode(0o755);
            let _ = fs::set_permissions(&shim, p);
        }
        println!("tool\t{}\t{}\t{}", name, pin.root, grants.join(","));
        println!("shim\t{}\t(add {} to PATH if it is not already)", name, shim_dir().display());
    }
    write_registry(&entries);
    let _ = fs::remove_dir_all(&dir);
}

pub fn cmd_tools(args: &[String]) {
    match args.get(0).map(|s| s.as_str()) {
        Some("list") | None => {
            let entries = read_registry();
            if entries.is_empty() {
                println!("no tools installed");
                return;
            }
            for t in entries {
                println!("{}\t{}\t{}", t.name, t.source, t.grant.join(","));
            }
        }
        Some("rm") => {
            let name = args.get(1).unwrap_or_else(|| user_err!("pkg", None, "tools rm needs a name"));
            let mut entries = read_registry();
            let before = entries.len();
            let removed: Vec<ToolEntry> = entries.iter().filter(|t| t.name == *name).cloned().collect();
            entries.retain(|t| t.name != *name);
            if entries.len() == before {
                user_err!("pkg", None, "no tool named `{}` installed (see: tools list)", name);
            }
            for t in &removed {
                let _ = fs::remove_file(&t.bin);
                let _ = fs::remove_file(shim_dir().join(&t.name));
            }
            write_registry(&entries);
            println!("removed\t{}", name);
        }
        Some(other) => user_err!("pkg", None, "unknown tools subcommand `{}` (list | rm <name>)", other),
    }
}

/// `nk run <name> [--policy P] [args…]` — execute an installed tool with a
/// runtime ceiling. Without --policy the tool runs with exactly its
/// install-time grants; --policy may only restrict below them.
pub fn cmd_run(args: &[String], baked_sb: &str) {
    let mut policy: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--policy" => {
                i += 1;
                policy = Some(args.get(i).unwrap_or_else(|| user_err!("pkg", None, "--policy needs a name")).clone());
            }
            a => rest.push(a.to_string()),
        }
        i += 1;
    }
    let name = rest.first().cloned().unwrap_or_else(|| user_err!("pkg", None, "run needs a tool name (see: tools list)"));
    let entry = read_registry().into_iter().find(|t| t.name == name).unwrap_or_else(|| {
        user_err!("pkg", None, "no tool named `{}` installed (see: tools list)", name)
    });
    let mut cmd = Command::new(&entry.bin);
    cmd.args(&rest[1..]);
    if let Some(p) = &policy {
        // translate the named policy into the ceiling env (may only
        // restrict: the intersection with the tool's grants)
        let sf = crate::sandbox::parse_ufs(baked_sb, "<baked>");
        let allowed = policy_allowed_caps(Some(&sf), p)
            .unwrap_or_else(|| user_err!("pkg", None, "unknown policy `{}` for run (known: all{})", p, if sf.policies.is_empty() { String::new() } else { format!(", {}", sf.policies.keys().cloned().collect::<Vec<_>>().join(", ")) }));
        let eff: Vec<String> = allowed.into_iter().filter(|c| entry.grant.contains(c)).collect();
        cmd.env("NK_CAP_CEILING", eff.join(","));
    }
    let st = cmd.status().unwrap_or_else(|e| user_err!("pkg", None, "cannot execute {}: {}", entry.bin, e));
    std::process::exit(st.code().unwrap_or(1));
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_vectors() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn source_split() {
        assert_eq!(split_ref("gh:a/lib@v1.2"), ("gh:a/lib".into(), Some("v1.2".into())));
        assert_eq!(split_ref("gh:a/lib"), ("gh:a/lib".into(), None));
        assert_eq!(split_ref("https://x.io/p.tar.gz"), ("https://x.io/p.tar.gz".into(), None));
        assert_eq!(split_ref("https://x.io/p@v2"), ("https://x.io/p".into(), Some("v2".into())));
        assert!(is_schemed("gh:a/b"));
        assert!(!is_schemed("curl"));
    }

    #[test]
    fn semver_order() {
        let mut tags = vec!["v1.9.3", "v1.10.0", "v0.2.1", "v1.10.0-rc1", "nightly"];
        tags.sort_by_key(|t| (semver_key(t), t.to_string()));
        assert_eq!(tags, vec!["nightly", "v0.2.1", "v1.9.3", "v1.10.0-rc1", "v1.10.0"]);
        assert!(semver_key("v1.10.0").unwrap() > semver_key("v1.9.3").unwrap());
        assert_eq!(semver_key("not-semver"), None);
    }

    #[test]
    fn normalize_crlf() {
        assert_eq!(normalize(b"a\r\nb\rc"), "a\nb\nc");
    }

    #[test]
    fn lock_roundtrip() {
        let src = "; comment\npkg source=gh:a/lib ref=v1.0 sha=abc root=nkph1:ff\n";
        let l = parse_lock(src);
        assert_eq!(l.pins.len(), 1);
        assert_eq!(l.pins[0].source, "gh:a/lib");
        assert_eq!(l.pins[0].ref_, "v1.0");
        assert_eq!(l.for_source("gh:a/lib").len(), 1);
        assert!(l.for_source_ref("gh:a/lib", "v1.0").is_some());
    }

    #[test]
    fn approvals_parse() {
        let a = parse_approvals("approve source=gh:a/lib root=nkph1:ff date=2026-10-14 by=chase\n");
        assert_eq!(a.len(), 1);
        assert!(approved(&a, "gh:a/lib", "nkph1:ff"));
        assert!(!approved(&a, "gh:a/lib", "nkph1:ee"));
    }

    #[test]
    fn nkpkg_parse() {
        let m = parse_nkpkg("name demo\ncaps fs.read optional\ncaps proc required\n");
        assert_eq!(m.name.as_deref(), Some("demo"));
        assert_eq!(m.caps, vec![("fs.read".to_string(), false), ("proc".to_string(), true)]);
    }

    #[test]
    fn cap_tokens() {
        assert!(is_cap_token("fs.read"));
        assert!(is_cap_token("fs.*"));
        assert!(is_cap_token("*"));
        assert!(is_cap_token("proc"));
        assert!(is_cap_token("raw.mem"));
        assert!(!is_cap_token("proc_n"));
        assert!(!is_cap_token("entry:"));
        assert!(!is_cap_token("0"));
        assert!(!is_cap_token("fs."));
    }

    #[test]
    fn hash_package_deterministic() {
        let d = tmp_dir("test-hash");
        fs::write(d.join("main.n"), "ret 1\r\n").unwrap();
        fs::write(d.join("zz.n"), "ret 2\n").unwrap();
        fs::write(d.join("notes.md"), "ignore me").unwrap();
        fs::create_dir(d.join(".git")).unwrap();
        fs::write(d.join(".git").join("config"), "x").unwrap();
        let c1 = hash_package(&d);
        assert_eq!(c1.files.len(), 2); // notes.md and .git excluded
        assert_eq!(c1.files[0].0, "main.n"); // sorted
        // CRLF vs LF normalizes to the same root
        fs::write(d.join("main.n"), "ret 1\n").unwrap();
        let c2 = hash_package(&d);
        assert_eq!(c1.root, c2.root);
        // content change changes root
        fs::write(d.join("main.n"), "ret 3\n").unwrap();
        let c3 = hash_package(&d);
        assert_ne!(c1.root, c3.root);
        assert!(c1.root.starts_with("nkph1:"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn grant_algebra() {
        let spec = |policy: Option<&str>, allows: Vec<&str>, denies: Vec<&str>| UseSpec {
            alias: None,
            source: "gh:x/y".into(),
            policy: policy.map(|s| s.to_string()),
            allows: allows.into_iter().map(|s| s.to_string()).collect(),
            denies: denies.into_iter().map(|s| s.to_string()).collect(),
        };
        let caps = Caps::unrestricted();
        let declared = vec![("fs.read".to_string(), false), ("proc".to_string(), true)];
        // zero by default; required starved -> error (user_err panics as Diagnostic)
        let r = std::panic::catch_unwind(|| compute_grant(&spec(None, vec![], vec![]), 1, "gh:x/y", &declared, &caps, None));
        assert!(r.is_err());
        // allow proc -> granted
        let g = compute_grant(&spec(None, vec!["proc"], vec![]), 1, "gh:x/y", &declared, &caps, None);
        assert_eq!(g, vec!["proc"]);
        // fs.* wildcard covers fs.read only (declared-limited)
        let g = compute_grant(&spec(None, vec!["proc", "fs.*"], vec![]), 1, "gh:x/y", &declared, &caps, None);
        assert_eq!(g, vec!["fs.read", "proc"]);
        // deny subtracts
        let g = compute_grant(&spec(None, vec!["proc", "fs.*"], vec!["fs.read"]), 1, "gh:x/y", &declared, &caps, None);
        assert_eq!(g, vec!["proc"]);
        // policy all within declared
        let g = compute_grant(&spec(Some("all"), vec![], vec![]), 1, "gh:x/y", &declared, &caps, None);
        assert_eq!(g, vec!["fs.read", "proc"]);
        // deny * starves required
        let r = std::panic::catch_unwind(|| compute_grant(&spec(Some("all"), vec![], vec!["*"]), 1, "gh:x/y", &declared, &caps, None));
        assert!(r.is_err());
        // undeclared grant is an error
        let r = std::panic::catch_unwind(|| compute_grant(&spec(None, vec!["proc", "host.argv"], vec![]), 1, "gh:x/y", &declared, &caps, None));
        assert!(r.is_err());
        // unknown policy
        let r = std::panic::catch_unwind(|| compute_grant(&spec(Some("nope"), vec![], vec![]), 1, "gh:x/y", &declared, &caps, None));
        assert!(r.is_err());
    }

    #[test]
    fn tree_diff_summary() {
        let old = vec![("a.n".to_string(), "1".to_string()), ("b.n".to_string(), "2".to_string())];
        let new = vec![("a.n".to_string(), "9".to_string()), ("c.n".to_string(), "3".to_string())];
        let d = tree_diff(Some(&old), &new);
        assert!(d.contains("+c.n") && d.contains("-b.n") && d.contains("~a.n"));
        assert_eq!(tree_diff(Some(&old), &old), "");
    }
}
