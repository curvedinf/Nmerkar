// nkr — the Enmerkar compiler (unrestricted build). All logic lives in driver.rs;
// this binary bakes a sandbox config only when NKR_SANDBOX_CONFIG was set at
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
    let baked = driver::NKR_BAKED_SB;
    driver::run(std::env::args().collect(), baked, false);
}
