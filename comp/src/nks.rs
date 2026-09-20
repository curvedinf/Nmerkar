// nks — the sandboxed Nmerkar compiler. Identical to `nk` except a capability
// config is always baked: NK_SANDBOX_CONFIG when set at build time, else the
// repo default comp/sandbox.ufs (policy `data`, with pure/data/web/build
// selectable via --policy).

mod ast;
mod compute;
mod driver;
mod emit;
mod gen;
mod lex;
mod parse;
mod prelude;
mod sandbox;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    driver::configure_compiler_backtrace(&args);
    let baked = if driver::NK_BAKED_SB.trim().is_empty() {
        include_str!("../sandbox.ufs")
    } else {
        driver::NK_BAKED_SB
    };
    driver::run(args, baked, true);
}
