"use strict";
// dynamicgraph.js — BFS over a Map of dynamic neighbor lists.
const n = process.argv.length > 2 ? parseInt(process.argv[2], 10) : 1000000;
const adj = new Map();
for (let u = 0; u < n; u++) {
  const neighbors = [(7 * u + 3) % n, (13 * u + 11) % n];
  if (u % 3 === 0) neighbors.push((u + 1) % n);
  adj.set(u, neighbors);
}
const dist = new Int32Array(n).fill(-1);
const q = new Int32Array(n);
dist[0] = 0;
q[0] = 0;
let head = 0, tail = 1;
while (head < tail) {
  const u = q[head++];
  const du = dist[u];
  const neighbors = adj.get(u);
  for (let k = 0; k < neighbors.length; k++) {
    const v = neighbors[k];
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
