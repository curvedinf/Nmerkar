// matmul_gpu.rs — Vulkan variant via the shared gpucomp launcher
use std::os::raw::{c_char, c_int};
#[link(name = "gpucomp")]
extern "C" {
    fn gpu_init(device: *const c_char) -> c_int;
    fn gpu_run_sized(kernel: *const c_char, n: u64, a: *const f64, asz: usize, b: *const f64, bsz: usize, r: *mut f64, rsz: usize,
                     n0: i64, n1: i64, n2: i64, n3: i64, s: f64, rev: i64) -> c_int;
}
fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(512);
    let mut a = vec![0.0f64; n*n];
    let mut b = vec![0.0f64; n*n];
    let mut c = vec![0.0f64; n*n];
    for k in 0..n*n { let kf = k as f64; a[k] = 1.0 + 0.000001*kf; b[k] = 1.0 - 0.000001*kf; }
    let dev = std::env::var("UF_DEVICE").unwrap_or_else(|_| "auto".into());
    unsafe {
        let dk = std::ffi::CString::new(dev).unwrap();
        let kk = std::ffi::CString::new("matmul").unwrap();
        if gpu_init(dk.as_ptr()) != 0 || gpu_run_sized(kk.as_ptr(), (n*n) as u64, a.as_ptr(), n*n*8, b.as_ptr(), n*n*8, c.as_mut_ptr(), n*n*8, (n*n) as i64, n as i64, n as i64, n as i64, 0.0, 0) != 0 { eprintln!("gpu failed"); std::process::exit(1); }
    }
    let sum: f64 = c.iter().sum();
    println!("c[0,0]: {:.6}", c[0]);
    println!("c[N-1,N-1]: {:.6}", c[n*n-1]);
    println!("checksum: {:.2}", sum);
}
