"use strict";
// bfs.js — breadth-first search over a deterministic graph (bench/SPEC.md)
const n = process.argv.length > 2 ? parseInt(process.argv[2], 10) : 1000000;
const adj = new Array(n);
for (let u = 0; u < n; u++) {
  const lst = [(7 * u + 3) % n, (13 * u + 11) % n];
  if (u % 3 === 0) lst.push((u + 1) % n);
  adj[u] = lst;
}
const dist = new Int32Array(n).fill(-1);
dist[0] = 0;
const q = new Int32Array(n + 1);
q[0] = 0;
let head = 0, tail = 1;
while (head < tail) {
  const u = q[head++];
  const du = dist[u];
  const nb = adj[u];
  for (let k = 0; k < nb.length; k++) {
    const v = nb[k];
    if (dist[v] < 0) {
      dist[v] = du + 1;
      q[tail++] = v;
    }
  }
}
let r = 0, s = 0, m = 0;
for (let i = 0; i < n; i++) {
  const d = dist[i];
  if (d >= 0) { r++; s += d; if (d > m) m = d; }
}
console.log(`reached: ${r}`);
console.log(`unreached: ${n - r}`);
console.log(`sum_dist: ${s}`);
console.log(`max_dist: ${m}`);
