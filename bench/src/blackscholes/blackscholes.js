// blackscholes.js — CPU variant (bench/SPEC.md)
const S_C = [0.3989422525280872, -0.06648994026261523, 0.009972402563565597, -0.0011861254737728477, 0.00011477352256159136, -9.223601524488048e-06, 6.170929070124161e-07, -3.368381569529637e-08, 1.4305101232167135e-09, -4.2910324684542515e-11, 6.76393146459064e-13, 6.739509911670498e-15, -5.976961241995002e-16, 8.348882330925073e-18, 2.5294417305671556e-19, -1.3050099746435033e-20, 2.520013152005562e-22, -2.44092817642708e-24, 9.837418308177747e-27];
function ncdf(x){ const w = x*x; let p = S_C[18]; for(let i=17;i>=0;i--) p = p*w + S_C[i]; return 0.5 + x*p; }
const n = parseInt(process.argv[2] || "2000000");
const D = 0.98511193960306265;
let sum = 0, p0 = 0, pl = 0;
for(let k=0;k<n;k++){
  const S = 100.0 + 0.000001*k, K = 95.0 + 0.0000005*k, st = 0.2 + 0.0000001*k;
  const d1 = ((S/K) - 1.0 + 0.5*st*st)/st, d2 = d1 - st;
  const pr = S*ncdf(d1) - K*D*ncdf(d2);
  sum += pr; if(k===0) p0 = pr; pl = pr;
}
console.log(`premium_sum: ${sum.toFixed(2)}`);
console.log(`sample[0]: ${p0.toFixed(6)}`);
console.log(`sample[N-1]: ${pl.toFixed(6)}`);
