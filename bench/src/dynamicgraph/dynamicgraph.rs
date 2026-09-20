// dynamicgraph.rs — BFS over a hash map of dynamic neighbor lists.
use std::collections::HashMap;

fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1000000);
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    for u in 0..n {
        let mut neighbors = Vec::with_capacity(3);
        neighbors.push((7 * u + 3) % n);
        neighbors.push((13 * u + 11) % n);
        if u % 3 == 0 {
            neighbors.push((u + 1) % n);
        }
        adj.insert(u, neighbors);
    }
    let mut dist = vec![-1i64; n];
    let mut q = vec![0usize; n];
    dist[0] = 0;
    q[0] = 0;
    let mut head = 0usize;
    let mut tail = 1usize;
    while head < tail {
        let u = q[head];
        head += 1;
        let du = dist[u];
        for &v in adj.get(&u).unwrap() {
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
    for &d in &dist {
        if d >= 0 {
            r += 1;
            s += d;
            if d > m { m = d; }
        }
    }
    println!("reached: {}", r);
    println!("unreached: {}", n as i64 - r);
    println!("sum_dist: {}", s);
    println!("max_dist: {}", m);
}
