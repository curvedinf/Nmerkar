// ufsb — the sandboxed µFlux compiler. Identical to `uf` except a capability
// config is always baked: UF_SANDBOX_CONFIG when set at build time, else the
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
    let baked = if driver::UF_BAKED_SB.trim().is_empty() {
        include_str!("../sandbox.ufs")
    } else {
        driver::UF_BAKED_SB
    };
    driver::run(std::env::args().collect(), baked, true);
}
