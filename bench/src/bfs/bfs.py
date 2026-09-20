#!/usr/bin/env python3
# bfs.py — breadth-first search over a deterministic graph (bench/SPEC.md)
import sys
n = int(sys.argv[1]) if len(sys.argv) > 1 else 1000000
adj = {}
for u in range(n):
    lst = [(7 * u + 3) % n, (13 * u + 11) % n]
    if u % 3 == 0:
        lst.append((u + 1) % n)
    adj[u] = lst
dist = [-1] * n
dist[0] = 0
q = [0]
head = 0
while head < len(q):
    u = q[head]
    head += 1
    du = dist[u]
    for v in adj[u]:
        if dist[v] < 0:
            dist[v] = du + 1
            q.append(v)
r = sum(1 for d in dist if d >= 0)
s = sum(d for d in dist if d >= 0)
m = max(d for d in dist if d >= 0)
print(f"reached: {r}")
print(f"unreached: {n - r}")
print(f"sum_dist: {s}")
print(f"max_dist: {m}")
