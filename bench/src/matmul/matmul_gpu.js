// matmul_gpu.js — Vulkan variant via the shared gpucomp Node addon
const path = require('path');
const gpu = require(path.join(__dirname, '..', '_gpu', 'gpucomp_node.node'));
const n = parseInt(process.argv[2] || "512");
const a = new Float64Array(n*n), b = new Float64Array(n*n), c = new Float64Array(n*n);
for(let k=0;k<n*n;k++){ a[k] = 1.0 + 0.000001*k; b[k] = 1.0 - 0.000001*k; }
if(gpu.init(process.env.UF_DEVICE || "auto") !== 0){ console.error("gpu failed"); process.exit(1); }
if(gpu.run("matmul", n*n, a, n*n*8, b, n*n*8, c, n*n*8, n*n, n, n, n, 0.0) !== 0){ console.error("gpu failed"); process.exit(1); }
let sum = 0; for(let k=0;k<n*n;k++) sum += c[k];
console.log(`c[0,0]: ${c[0].toFixed(6)}`);
console.log(`c[N-1,N-1]: ${c[n*n-1].toFixed(6)}`);
console.log(`checksum: ${sum.toFixed(2)}`);
