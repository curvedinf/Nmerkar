// nk — the Nmerkar compiler (unrestricted build). All logic lives in the
// `nk` library; this binary bakes a sandbox config only when NK_SANDBOX_CONFIG
// was set at build time.

use nk::driver;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    driver::configure_compiler_backtrace(&args);
    let baked = driver::NK_BAKED_SB;
    driver::run(args, baked, false);
}
