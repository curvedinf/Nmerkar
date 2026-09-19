// matmul_gpu.cpp — Vulkan variant via the shared gpucomp launcher
#include <cstdio>
#include <cstdlib>
#include "gpucomp.h"
int main(int argc,char**argv){
  long n = argc>1?atol(argv[1]):512;
  double* a=(double*)malloc(n*n*8),*b=(double*)malloc(n*n*8),*c=(double*)malloc(n*n*8);
  for(long k=0;k<n*n;k++){ a[k]=1.0+0.000001*k; b[k]=1.0-0.000001*k; }
  if(gpu_init(getenv("UF_DEVICE")?getenv("UF_DEVICE"):"auto")||gpu_run_sized("matmul",n*n,a,n*n*8,b,n*n*8,c,n*n*8,n*n,n,n,n,0.0,0)){ fprintf(stderr,"gpu failed\n"); return 1; }
  double sum=0; for(long k=0;k<n*n;k++) sum+=c[k];
  printf("c[0,0]: %.6lf\nc[N-1,N-1]: %.6lf\nchecksum: %.2lf\n",c[0],c[n*n-1],sum);
}
