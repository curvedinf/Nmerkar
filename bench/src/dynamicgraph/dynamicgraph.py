#!/usr/bin/env python3
# dynamicgraph.py — BFS over a dict of dynamic neighbor lists.
import sys

n = int(sys.argv[1]) if len(sys.argv) > 1 else 1000000
adj = {}
for u in range(n):
    neighbors = [(7 * u + 3) % n, (13 * u + 11) % n]
    if u % 3 == 0:
        neighbors.append((u + 1) % n)
    adj[u] = neighbors
dist = [-1] * n
q = [0] * n
dist[0] = 0
q[0] = 0
head, tail = 0, 1
while head < tail:
    u = q[head]
    head += 1
    du = dist[u]
    for v in adj[u]:
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
