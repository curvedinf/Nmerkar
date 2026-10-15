// nk — the Nmerkar compiler library. All logic lives in the modules below;
// the `nk` and `nks` binaries (src/main.rs, src/nks.rs) are thin wrappers.
// This crate is also built as a cdylib for wasm32-unknown-unknown, where
// wasm_api.rs exposes the JavaScript-target compiler as a library.

pub mod ast;
pub mod compute;
pub mod driver;
pub mod diag;
pub mod ed25519;
pub mod emit;
pub mod gen;
pub mod gen_js;
pub mod js_prelude;
pub mod lex;
pub mod parse;
pub mod pkg;
pub mod prelude;
pub mod sandbox;

// SPEC.md's opcode claims, enforced against the lex.rs tables (test-only).
#[cfg(test)]
mod spec_contract;

// Compile a single translation unit to JavaScript-target output: the
// self-contained ES5 script the web glue and node auto-run consume.
// `compile_js` panics on unsupported constructs (the same panic the native
// target uses for codegen errors).
#[cfg(target_arch = "wasm32")]
pub mod wasm_api;

/// Compile Nmerkar source to a standalone JavaScript program.
pub fn compile_js(src: &str) -> String {
    let caps = crate::sandbox::Caps::unrestricted();
    let mut structs: crate::ast::StructMap = std::collections::HashMap::new();
    let toks = crate::lex::lex_source(src);
    let parsed = crate::parse::parse(toks, &mut structs, &caps);
    if !parsed.pkg_uses.is_empty() {
        let (s, l) = &parsed.pkg_uses[0];
        crate::user_err!("parse", Some(*l), "package use \"{}\" is not supported in the wasm library build — resolve packages with `nks install` and compile via the CLI", s.source);
    }
    let mut parsed =
        crate::parse::merge_tus(vec![parsed], vec!["main".to_string()], &[false]);
    if !parsed.pkg_uses.is_empty() {
        let (s, l) = &parsed.pkg_uses[0];
        crate::user_err!("parse", Some(*l), "package use \"{}\" is not supported in the wasm library build — resolve packages with `nks install` and compile via the CLI", s.source);
    }
    crate::parse::check_label_arity(&parsed);
    crate::parse::check_strictness(&mut parsed);
    crate::parse::specialize_builtin_sends(&mut parsed);
    crate::gen_js::gen_js(&parsed, &structs)
}
