#!/usr/bin/env python3
# matmul.py — CPU variant, numpy (bench/SPEC.md)
import sys
import numpy as np
n = int(sys.argv[1]) if len(sys.argv) > 1 else 512
k = np.arange(n*n, dtype=np.float64)
a = (1.0 + 0.000001*k).reshape(n, n)
b = (1.0 - 0.000001*k).reshape(n, n)
c = a @ b
print(f"c[0,0]: {c[0,0]:.6f}")
print(f"c[N-1,N-1]: {c[n-1,n-1]:.6f}")
print(f"checksum: {c.sum():.2f}")
