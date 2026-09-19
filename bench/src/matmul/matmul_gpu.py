#!/usr/bin/env python3
# matmul_gpu.py — Vulkan variant via the shared gpucomp launcher (ctypes)
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
n = int(sys.argv[1]) if len(sys.argv) > 1 else 512
k = np.arange(n*n, dtype=np.float64)
a = 1.0 + 0.000001*k
b = 1.0 - 0.000001*k
c = np.empty(n*n)
dev = os.environ.get("UF_DEVICE", "auto").encode()
assert lib.gpu_init(dev) == 0, "gpu init failed"
assert lib.gpu_run_sized(b"matmul", n*n,
    a.ctypes.data_as(ctypes.POINTER(ctypes.c_double)), n*n*8,
    b.ctypes.data_as(ctypes.POINTER(ctypes.c_double)), n*n*8,
    c.ctypes.data_as(ctypes.POINTER(ctypes.c_double)), n*n*8,
    n*n, n, n, n, 0, 0.0, 0) == 0, "gpu run failed"
print(f"c[0,0]: {c[0]:.6f}")
print(f"c[N-1,N-1]: {c[n*n-1]:.6f}")
print(f"checksum: {c.sum():.2f}")
