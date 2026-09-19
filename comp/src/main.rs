// uf — the µFlux compiler (unrestricted build). All logic lives in driver.rs;
// this binary bakes a sandbox config only when UF_SANDBOX_CONFIG was set at
// build time.

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
    let baked = driver::UF_BAKED_SB;
    driver::run(std::env::args().collect(), baked, false);
}
