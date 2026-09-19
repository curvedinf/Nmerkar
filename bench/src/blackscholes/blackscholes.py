#!/usr/bin/env python3
# blackscholes.py — CPU variant, numpy (bench/SPEC.md)
import sys
import numpy as np
S_C = np.array([0.3989422525280872, -0.06648994026261523, 0.009972402563565597, -0.0011861254737728477, 0.00011477352256159136, -9.223601524488048e-06, 6.170929070124161e-07, -3.368381569529637e-08, 1.4305101232167135e-09, -4.2910324684542515e-11, 6.76393146459064e-13, 6.739509911670498e-15, -5.976961241995002e-16, 8.348882330925073e-18, 2.5294417305671556e-19, -1.3050099746435033e-20, 2.520013152005562e-22, -2.44092817642708e-24, 9.837418308177747e-27])
def ncdf(x):
    w = x*x
    p = np.full_like(w, S_C[18])
    for i in range(17, -1, -1):
        p = p*w + S_C[i]
    return 0.5 + x*p
n = int(sys.argv[1]) if len(sys.argv) > 1 else 2000000
k = np.arange(n, dtype=np.float64)
S = 100.0 + 0.000001*k
K = 95.0 + 0.0000005*k
st = 0.2 + 0.0000001*k
d1 = ((S/K) - 1.0 + 0.5*st*st)/st
d2 = d1 - st
D = 0.98511193960306265
pr = S*ncdf(d1) - K*D*ncdf(d2)
print(f"premium_sum: {pr.sum():.2f}")
print(f"sample[0]: {pr[0]:.6f}")
print(f"sample[N-1]: {pr[n-1]:.6f}")
