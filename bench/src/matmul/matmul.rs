// matmul.rs — CPU variant (bench/SPEC.md)
fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(512);
    let mut a = vec![0.0f64; n*n];
    let mut b = vec![0.0f64; n*n];
    let mut c = vec![0.0f64; n*n];
    for k in 0..n*n { let kf = k as f64; a[k] = 1.0 + 0.000001*kf; b[k] = 1.0 - 0.000001*kf; }
    for i in 0..n {
        for k in 0..n {
            let aik = a[i*n+k];
            for j in 0..n { c[i*n+j] += aik*b[k*n+j]; }
        }
    }
    let sum: f64 = c.iter().sum();
    println!("c[0,0]: {:.6}", c[0]);
    println!("c[N-1,N-1]: {:.6}", c[n*n-1]);
    println!("checksum: {:.2}", sum);
}
