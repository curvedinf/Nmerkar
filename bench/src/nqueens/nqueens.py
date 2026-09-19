#!/usr/bin/env python3
# nqueens.py — N-queens by iterative backtracking (bench/SPEC.md)
import sys
n = int(sys.argv[1]) if len(sys.argv) > 1 else 11
cols = [0] * n
nxt = [0] * (n + 1)
cnt = 0
d = 0
while d >= 0:
    c = nxt[d]
    found = False
    while not found and c < n:
        i = 0
        sf = True
        while i < d and sf:
            ci = cols[i]
            bad = ci == c or (ci - c) == (i - d) or (ci - c) == (d - i)
            sf = sf and not bad
            i += 1
        if sf:
            found = True
        else:
            c += 1
    if c < n:
        cols[d] = c
        nxt[d] = c + 1
        d += 1
        nxt[d] = 0
        if d == n:
            cnt += 1
            d -= 1
    else:
        d -= 1
print(f"solutions: {cnt}")
