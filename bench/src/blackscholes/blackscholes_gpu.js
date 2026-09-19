// blackscholes_gpu.js — Vulkan variant via the shared gpucomp Node addon
const path = require('path');
const gpu = require(path.join(__dirname, '..', '_gpu', 'gpucomp_node.node'));
const n = parseInt(process.argv[2] || "2000000");
const inp = new Float64Array(n*3), out = new Float64Array(n);
for(let k=0;k<n;k++){ inp[3*k] = 100.0 + 0.000001*k; inp[3*k+1] = 95.0 + 0.0000005*k; inp[3*k+2] = 0.2 + 0.0000001*k; }
if(gpu.init(process.env.UF_DEVICE || "auto") !== 0){ console.error("gpu failed"); process.exit(1); }
if(gpu.run("bs", n, inp, n*3*8, null, 0, out, n*8, n, 0, 0, 0, 0.0) !== 0){ console.error("gpu failed"); process.exit(1); }
let sum = 0; for(let k=0;k<n;k++) sum += out[k];
console.log(`premium_sum: ${sum.toFixed(2)}`);
console.log(`sample[0]: ${out[0].toFixed(6)}`);
console.log(`sample[N-1]: ${out[n-1].toFixed(6)}`);
