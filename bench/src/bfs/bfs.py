#!/usr/bin/env python3
# bfs.py — breadth-first search over packed CSR adjacency (bench/SPEC.md)
import sys
n = int(sys.argv[1]) if len(sys.argv) > 1 else 1000000
edge_count = 2 * n + (n + 2) // 3
offsets = [0] * (n + 1)
edges = [0] * edge_count
edge_pos = 0
for u in range(n):
    offsets[u] = edge_pos
    edges[edge_pos] = (7 * u + 3) % n
    edge_pos += 1
    edges[edge_pos] = (13 * u + 11) % n
    edge_pos += 1
    if u % 3 == 0:
        edges[edge_pos] = (u + 1) % n
        edge_pos += 1
offsets[n] = edge_pos
dist = [-1] * n
dist[0] = 0
q = [0] * n
head, tail = 0, 1
while head < tail:
    u = q[head]
    head += 1
    du = dist[u]
    for k in range(offsets[u], offsets[u + 1]):
        v = edges[k]
        if dist[v] < 0:
            dist[v] = du + 1
            q[tail] = v
            tail += 1
r = sum(1 for d in dist if d >= 0)
s = sum(d for d in dist if d >= 0)
m = max(d for d in dist if d >= 0)
print(f"reached: {r}")
print(f"unreached: {n - r}")
print(f"sum_dist: {s}")
print(f"max_dist: {m}")
