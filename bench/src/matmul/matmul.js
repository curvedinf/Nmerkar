// matmul.js — CPU variant (bench/SPEC.md)
const n = parseInt(process.argv[2] || "512");
const a = new Float64Array(n*n), b = new Float64Array(n*n), c = new Float64Array(n*n);
for(let k=0;k<n*n;k++){ a[k] = 1.0 + 0.000001*k; b[k] = 1.0 - 0.000001*k; }
for(let i=0;i<n;i++) for(let k=0;k<n;k++){ const aik = a[i*n+k]; for(let j=0;j<n;j++) c[i*n+j] += aik*b[k*n+j]; }
let sum = 0; for(let k=0;k<n*n;k++) sum += c[k];
console.log(`c[0,0]: ${c[0].toFixed(6)}`);
console.log(`c[N-1,N-1]: ${c[n*n-1].toFixed(6)}`);
console.log(`checksum: ${sum.toFixed(2)}`);
