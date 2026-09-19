"use strict";
// nqueens.js — N-queens by iterative backtracking (bench/SPEC.md)
const n = process.argv.length > 2 ? parseInt(process.argv[2], 10) : 11;
const cols = new Int32Array(n);
const nxt = new Int32Array(n + 1);
let cnt = 0;
let d = 0;
while (d >= 0) {
  let c = nxt[d];
  let found = false;
  while (!found && c < n) {
    let i = 0;
    let sf = true;
    while (i < d && sf) {
      const ci = cols[i];
      const bad = ci === c || (ci - c) === (i - d) || (ci - c) === (d - i);
      sf = sf && !bad;
      i++;
    }
    if (sf) found = true;
    else c++;
  }
  if (c < n) {
    cols[d] = c;
    nxt[d] = c + 1;
    d++;
    nxt[d] = 0;
    if (d === n) {
      cnt++;
      d--;
    }
  } else {
    d--;
  }
}
console.log(`solutions: ${cnt}`);
