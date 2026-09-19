// build.rs — bake a sandbox capability config into the compiled binaries.
//
// NKR_SANDBOX_CONFIG=<path> cargo build   — the named .ufs is baked into BOTH
// `nkr` and `nkrsb` (this is "build the nkr command with certain operations
// disabled"). When unset, `nkr` bakes nothing (unrestricted) and `nkrsb` falls
// back to the repo default comp/sandbox.ufs (see src/nkrsb.rs).
use std::env;
use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=NKR_SANDBOX_CONFIG");
    println!("cargo:rerun-if-changed=sandbox.ufs");
    let out = env::var("OUT_DIR").unwrap();
    let baked = match env::var("NKR_SANDBOX_CONFIG") {
        Ok(p) if !p.is_empty() => {
            let src = fs::read_to_string(&p)
                .unwrap_or_else(|e| panic!("NKR_SANDBOX_CONFIG={}: cannot read: {}", p, e));
            src
        }
        _ => String::new(),
    };
    let body = escape_raw(&baked);
    fs::write(
        Path::new(&out).join("baked_sb.rs"),
        format!("pub const NKR_BAKED_SB: &str = {};\n", body),
    )
    .unwrap();
}

// wrap config text in a raw string literal with enough #s to avoid collisions
fn escape_raw(s: &str) -> String {
    let mut hashes = 1;
    while s.contains(&format!("\"{}", "#".repeat(hashes))) {
        hashes += 1;
    }
    let h = "#".repeat(hashes);
    format!("r{}\"{}\"{}", h, s, h)
}
