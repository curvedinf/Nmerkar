// blackscholes.rs — CPU variant (bench/SPEC.md)
const S_C: [f64; 19] = [0.3989422525280872, -0.06648994026261523, 0.009972402563565597, -0.0011861254737728477, 0.00011477352256159136, -9.223601524488048e-06, 6.170929070124161e-07, -3.368381569529637e-08, 1.4305101232167135e-09, -4.2910324684542515e-11, 6.76393146459064e-13, 6.739509911670498e-15, -5.976961241995002e-16, 8.348882330925073e-18, 2.5294417305671556e-19, -1.3050099746435033e-20, 2.520013152005562e-22, -2.44092817642708e-24, 9.837418308177747e-27];
fn ncdf(x: f64) -> f64 { let w = x*x; let mut p = S_C[18]; let mut i = 17; while i >= 0 { p = p*w + S_C[i as usize]; i -= 1; } 0.5 + x*p }
fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(2000000);
    let d = 0.98511193960306265f64;
    let (mut sum, mut p0, mut pl) = (0.0, 0.0, 0.0);
    for k in 0..n {
        let kf = k as f64;
        let s = 100.0 + 0.000001*kf;
        let kk = 95.0 + 0.0000005*kf;
        let st = 0.2 + 0.0000001*kf;
        let d1 = ((s/kk) - 1.0 + 0.5*st*st)/st;
        let d2 = d1 - st;
        let pr = s*ncdf(d1) - kk*d*ncdf(d2);
        sum += pr; if k == 0 { p0 = pr; } pl = pr;
    }
    println!("premium_sum: {:.2}", sum);
    println!("sample[0]: {:.6}", p0);
    println!("sample[N-1]: {:.6}", pl);
}
