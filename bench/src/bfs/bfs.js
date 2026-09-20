"use strict";
// bfs.js — breadth-first search over packed CSR adjacency (bench/SPEC.md)
const n = process.argv.length > 2 ? parseInt(process.argv[2], 10) : 1000000;
const edgeCount = 2 * n + Math.floor((n + 2) / 3);
const offsets = new Int32Array(n + 1);
const edges = new Int32Array(edgeCount);
let edgePos = 0;
for (let u = 0; u < n; u++) {
  offsets[u] = edgePos;
  edges[edgePos++] = (7 * u + 3) % n;
  edges[edgePos++] = (13 * u + 11) % n;
  if (u % 3 === 0) edges[edgePos++] = (u + 1) % n;
}
offsets[n] = edgePos;
const dist = new Int32Array(n).fill(-1);
dist[0] = 0;
const q = new Int32Array(n + 1);
q[0] = 0;
let head = 0, tail = 1;
while (head < tail) {
  const u = q[head++];
  const du = dist[u];
  for (let k = offsets[u]; k < offsets[u + 1]; k++) {
    const v = edges[k];
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
