// bfs.rs — breadth-first search over packed CSR adjacency (bench/SPEC.md)
fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1000000);
    let edge_count = 2 * n + (n + 2) / 3;
    let mut offsets = vec![0usize; n + 1];
    let mut edges = vec![0usize; edge_count];
    let mut edge_pos = 0usize;
    for u in 0..n {
        offsets[u] = edge_pos;
        edges[edge_pos] = (7 * u + 3) % n;
        edge_pos += 1;
        edges[edge_pos] = (13 * u + 11) % n;
        edge_pos += 1;
        if u % 3 == 0 {
            edges[edge_pos] = (u + 1) % n;
            edge_pos += 1;
        }
    }
    offsets[n] = edge_pos;
    let mut dist = vec![-1i64; n];
    dist[0] = 0;
    let mut q = vec![0usize; n + 1];
    let mut head = 0usize;
    let mut tail = 1usize;
    while head < tail {
        let u = q[head];
        head += 1;
        let du = dist[u];
        for k in offsets[u]..offsets[u + 1] {
            let v = edges[k];
            if dist[v] < 0 {
                dist[v] = du + 1;
                q[tail] = v;
                tail += 1;
            }
        }
    }
    let mut r = 0i64;
    let mut s = 0i64;
    let mut m = 0i64;
    for d in dist.iter() {
        if *d >= 0 {
            r += 1;
            s += *d;
            if *d > m {
                m = *d;
            }
        }
    }
    println!("reached: {}", r);
    println!("unreached: {}", n as i64 - r);
    println!("sum_dist: {}", s);
    println!("max_dist: {}", m);
}
