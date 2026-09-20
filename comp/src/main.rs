// nk — the Nmerkar compiler (unrestricted build). All logic lives in driver.rs;
// this binary bakes a sandbox config only when NK_SANDBOX_CONFIG was set at
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
    let args: Vec<String> = std::env::args().collect();
    driver::configure_compiler_backtrace(&args);
    let baked = driver::NK_BAKED_SB;
    driver::run(args, baked, false);
}
