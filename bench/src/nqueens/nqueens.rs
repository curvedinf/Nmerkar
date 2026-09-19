// nqueens.rs — N-queens by iterative backtracking (bench/SPEC.md)
fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(11);
    let mut cols = vec![0i64; n];
    let mut nxt = vec![0i64; n + 1];
    let mut cnt: i64 = 0;
    let mut d: i64 = 0;
    while d >= 0 {
        let mut c = nxt[d as usize];
        let mut found = false;
        while !found && c < n as i64 {
            let mut i: i64 = 0;
            let mut sf = true;
            while i < d && sf {
                let ci = cols[i as usize];
                let bad = ci == c || (ci - c) == (i - d) || (ci - c) == (d - i);
                sf = sf && !bad;
                i += 1;
            }
            if sf {
                found = true;
            } else {
                c += 1;
            }
        }
        if c < n as i64 {
            cols[d as usize] = c;
            nxt[d as usize] = c + 1;
            d += 1;
            nxt[d as usize] = 0;
            if d == n as i64 {
                cnt += 1;
                d -= 1;
            }
        } else {
            d -= 1;
        }
    }
    println!("solutions: {}", cnt);
}
