#ifndef GPUCOMP_H
#define GPUCOMP_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
int gpu_init(const char* device);
int gpu_run(const char* kernel, uint64_t n, const double* A, const double* B, double* R,
            int64_t n0, int64_t n1, int64_t n2, int64_t n3, double s, int64_t rev);
int gpu_run_sized(const char* kernel, uint64_t n, const double* A, size_t asz, const double* B, size_t bsz, double* R, size_t rsz,
            int64_t n0, int64_t n1, int64_t n2, int64_t n3, double s, int64_t rev);
#ifdef __cplusplus
}
#endif
#endif
