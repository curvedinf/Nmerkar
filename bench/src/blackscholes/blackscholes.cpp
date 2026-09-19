// blackscholes.cpp — CPU variant (bench/SPEC.md)
#include <cstdio>
#include <cstdlib>
#include <cmath>
static double S_C[19] = {0.39894225252808718, -0.066489940262615232, 0.009972402563565597, -0.0011861254737728477, 0.00011477352256159136, -9.2236015244880478e-06, 6.1709290701241611e-07, -3.3683815695296368e-08, 1.4305101232167135e-09, -4.2910324684542515e-11, 6.7639314645906397e-13, 6.7395099116704979e-15, -5.9769612419950018e-16, 8.3488823309250727e-18, 2.5294417305671556e-19, -1.3050099746435033e-20, 2.5200131520055621e-22, -2.4409281764270801e-24, 9.8374183081777466e-27};
static double ncdf(double x){ double w=x*x, p=S_C[18]; for(int i=17;i>=0;i--) p=p*w+S_C[i]; return 0.5+x*p; }
int main(int argc,char**argv){
  long n = argc>1?atol(argv[1]):2000000;
  double D=0.98511193960306265, sum=0;
  double p0=0,pl=0;
  for(long k=0;k<n;k++){
    double S=100.0+0.000001*k, K=95.0+0.0000005*k, st=0.2+0.0000001*k;
    double d1=((S/K)-1.0+0.5*st*st)/st, d2=d1-st;
    double pr=S*ncdf(d1)-K*D*ncdf(d2);
    sum+=pr; if(k==0)p0=pr; pl=pr;
  }
  printf("premium_sum: %.2lf\nsample[0]: %.6lf\nsample[N-1]: %.6lf\n",sum,p0,pl);
}
