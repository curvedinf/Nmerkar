// blackscholes_gpu.rs — Vulkan variant via the shared gpucomp launcher
use std::os::raw::{c_char, c_int};
#[link(name = "gpucomp")]
extern "C" {
    fn gpu_init(device: *const c_char) -> c_int;
    fn gpu_run_sized(kernel: *const c_char, n: u64, a: *const f64, asz: usize, b: *const f64, bsz: usize, r: *mut f64, rsz: usize,
                     n0: i64, n1: i64, n2: i64, n3: i64, s: f64, rev: i64) -> c_int;
}
fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(2000000);
    let mut inp = vec![0.0f64; n*3];
    let mut out = vec![0.0f64; n];
    for k in 0..n { let kf = k as f64; inp[3*k] = 100.0 + 0.000001*kf; inp[3*k+1] = 95.0 + 0.0000005*kf; inp[3*k+2] = 0.2 + 0.0000001*kf; }
    let dev = std::env::var("UF_DEVICE").unwrap_or_else(|_| "auto".into());
    unsafe {
        let dk = std::ffi::CString::new(dev).unwrap();
        let kk = std::ffi::CString::new("bs").unwrap();
        if gpu_init(dk.as_ptr()) != 0 || gpu_run_sized(kk.as_ptr(), n as u64, inp.as_ptr(), n*3*8, std::ptr::null(), 0, out.as_mut_ptr(), n*8, n as i64,0,0,0,0.0,0) != 0 { eprintln!("gpu failed"); std::process::exit(1); }
    }
    let sum: f64 = out.iter().sum();
    println!("premium_sum: {:.2}", sum);
    println!("sample[0]: {:.6}", out[0]);
    println!("sample[N-1]: {:.6}", out[n-1]);
}
