// build.rs — bake a sandbox capability config into the compiled binaries.
//
// NK_SANDBOX_CONFIG=<path> cargo build   — the named .ufs is baked into BOTH
// `nk` and `nks` (this is "build the nk command with certain operations
// disabled"). When unset, `nk` bakes nothing (unrestricted) and `nks` falls
// back to the repo default comp/sandbox.ufs (see src/nks.rs).
use std::env;
use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=NK_SANDBOX_CONFIG");
    println!("cargo:rerun-if-changed=sandbox.ufs");
    let out = env::var("OUT_DIR").unwrap();
    let baked = match env::var("NK_SANDBOX_CONFIG") {
        Ok(p) if !p.is_empty() => {
            let src = fs::read_to_string(&p)
                .unwrap_or_else(|e| panic!("NK_SANDBOX_CONFIG={}: cannot read: {}", p, e));
            src
        }
        _ => String::new(),
    };
    let body = escape_raw(&baked);
    fs::write(
        Path::new(&out).join("baked_sb.rs"),
        format!("pub const NK_BAKED_SB: &str = {};\n", body),
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
