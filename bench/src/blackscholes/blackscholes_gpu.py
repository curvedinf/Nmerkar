#!/usr/bin/env python3
# blackscholes_gpu.py — Vulkan variant via the shared gpucomp launcher (ctypes)
import ctypes, os, sys
lib = ctypes.CDLL(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "_gpu", "libgpucomp.so"))
lib.gpu_init.argtypes = [ctypes.c_char_p]
lib.gpu_init.restype = ctypes.c_int
lib.gpu_run_sized.restype = ctypes.c_int
lib.gpu_run_sized.argtypes = [ctypes.c_char_p, ctypes.c_uint64,
    ctypes.POINTER(ctypes.c_double), ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_double), ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_double), ctypes.c_size_t,
    ctypes.c_int64, ctypes.c_int64, ctypes.c_int64, ctypes.c_int64,
    ctypes.c_double, ctypes.c_int64]
import numpy as np
n = int(sys.argv[1]) if len(sys.argv) > 1 else 2000000
k = np.arange(n, dtype=np.float64)
inp = np.empty((n, 3))
inp[:,0] = 100.0 + 0.000001*k
inp[:,1] = 95.0 + 0.0000005*k
inp[:,2] = 0.2 + 0.0000001*k
out = np.empty(n)
dev = os.environ.get("UF_DEVICE", "auto").encode()
assert lib.gpu_init(dev) == 0, "gpu init failed"
A = inp.astype(np.float64).ctypes.data_as(ctypes.POINTER(ctypes.c_double))
R = out.ctypes.data_as(ctypes.POINTER(ctypes.c_double))
assert lib.gpu_run_sized(b"bs", n, A, n*3*8, None, 0, R, n*8, n, 0, 0, 0, 0, 0.0, 0) == 0, "gpu run failed"
print(f"premium_sum: {out.sum():.2f}")
print(f"sample[0]: {out[0]:.6f}")
print(f"sample[N-1]: {out[n-1]:.6f}")
