// matmul.cpp — CPU variant (bench/SPEC.md)
#include <cstdio>
#include <cstdlib>
int main(int argc,char**argv){
  long n = argc>1?atol(argv[1]):512;
  double* a=(double*)malloc(n*n*8),*b=(double*)malloc(n*n*8),*c=(double*)malloc(n*n*8);
  for(long k=0;k<n*n;k++){ a[k]=1.0+0.000001*k; b[k]=1.0-0.000001*k; }
  for(long i=0;i<n;i++)
    for(long k=0;k<n;k++){ double aik=a[i*n+k]; for(long j=0;j<n;j++) c[i*n+j]+=aik*b[k*n+j]; }
  double sum=0; for(long k=0;k<n*n;k++) sum+=c[k];
  printf("c[0,0]: %.6lf\nc[N-1,N-1]: %.6lf\nchecksum: %.2lf\n",c[0],c[n*n-1],sum);
}
