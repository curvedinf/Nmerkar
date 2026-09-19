// blackscholes_gpu.cpp — Vulkan variant via the shared gpucomp launcher
#include <cstdio>
#include <cstdlib>
#include "gpucomp.h"
int main(int argc,char**argv){
  long n = argc>1?atol(argv[1]):2000000;
  double* inp=(double*)malloc(n*3*sizeof(double)); double* out=(double*)malloc(n*sizeof(double));
  for(long k=0;k<n;k++){ inp[3*k]=100.0+0.000001*k; inp[3*k+1]=95.0+0.0000005*k; inp[3*k+2]=0.2+0.0000001*k; }
  if(gpu_init(getenv("UF_DEVICE")?getenv("UF_DEVICE"):"auto")||gpu_run_sized("bs",n,inp,n*3*8,NULL,0,out,n*8,n,0,0,0,0.0,0)){ fprintf(stderr,"gpu failed\n"); return 1; }
  double sum=0; for(long k=0;k<n;k++) sum+=out[k];
  printf("premium_sum: %.2lf\nsample[0]: %.6lf\nsample[N-1]: %.6lf\n",sum,out[0],out[n-1]);
}
