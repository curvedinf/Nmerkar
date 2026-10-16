// Parity gate for the JavaScript target.
//
// Every corpus program must produce the same rc/stdout/stderr under the
// native `nk` binary as under the web nk.js (wasm compile + Program.run).
// neg_*.n must be rejected at compile time; compileOnly programs must
// compile but are never run (they loop forever).
//
// Corpus:
//   web/tests/*.n        JS-target micro-gates (bare names in this dir)
//   comp/tests/*.n       compiler feature gates
//   examples/**/*.n      repo examples (mtu/ and dirtest/ excluded: they are
//                       multi-TU/sandbox fixtures, not single programs;
//                       text/chat.n is a REPL: compile-only)
//   bench/src/*/*.n      the benchmark programs
//
// Per-file spec fields:
//   flags       nk compiler flags placed before the file (e.g. --device cpu)
//   argv        program arguments (oracle: after `--`; JS: env.argv[1..])
//   stdin       bytes fed to the oracle's stdin (JS: env.stdin)
//   fs          { "path": text } files the program opens (written to a temp
//                 dir for the oracle, passed as env.fs for JS)
//   fsPreload   { "path": text } existing repo files opened by name
//   ts          normalize bare recent-epoch lines to "TS" (wall clock)
//   compileOnly expect nk.compile to resolve; never run
"use strict";

const { spawnSync } = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

const ROOT = path.join(__dirname, "..", "..");
const NK = path.join(ROOT, "comp", "target", "release", "nk");
const nk = require("../nk.js");
const FIX = fs.mkdtempSync(path.join(os.tmpdir(), "nkjs-"));

function rep(f) { return path.join(ROOT, f); }
function repo(f) { return fs.readFileSync(rep(f), "utf8"); }
function fix(name, content) {
  const p = path.join(FIX, name);
  fs.writeFileSync(p, content);
  return p;
}

// --- fixtures -----------------------------------------------------------

const analyticsCsv = [
  "date,channel,region,country,product,sku,qty,units,discount,note,total",
  "2026-01-02,web,r1,c1,p1,s1,1,1,0.05,o1,10.5",
  "2026-01-03,web,r2,c2,p2,s2,2,2,0.05,o2,20.5",
  "2026-01-04,retail,r3,c1,p3,s3,3,3,0.1,o3,30.5",
  "2026-01-05,web,r4,c3,p4,s4,4,4,0.1,o4,40.5",
  "2026-01-06,retail,r5,c1,p5,s5,5,5,0.15,o5,50.5",
  "2026-01-07,web,r6,c2,p1,s6,6,6,0.15,o6,60.5",
  "2026-01-08,web,r1,c3,p2,s7,7,7,0.0,o7,70.5",
  "2026-01-09,retail,r2,c1,p3,s8,8,8,0.0,o8,80.5",
  "2026-01-10,web,r3,c2,p4,s9,9,9,0.2,o9,90.5",
  "2026-01-11,retail,r4,c1,p5,s10,10,10,0.2,o10,100.5",
  "2026-01-12,web,r5,c3,p1,s11,11,11,0.25,o11,110.5",
  "2026-01-13,retail,r6,c2,p2,s12,12,12,0.25,o12,120.5",
].join("\n") + "\n";
// 6 regions / 5 products / 3 countries, all distinct aggregates, so
// top_n orderings are deterministic (no tie depends on dict iteration order).

function accessLog() {
  // 10 distinct IPs with distinct request counts (10..1), >= 10 space
  // tokens each; [3] = "[dd/Mon/yyyy:HH:MM:SS (hour at bytes 13-14),
  // [8] status, [9] bytes.
  const status = [200, 301, 404, 500];
  const out = [];
  for (let ip = 1; ip <= 10; ip++) {
    for (let k = 0; k < 11 - ip; k++) {
      const hh = String((ip + k) % 24).padStart(2, "0");
      out.push("10.0.0." + ip + " - - [01/Oct/2026:" + hh + ":15:30 +0000] " +
        '"GET /p' + ip + " HTTP/1.1" + " " +
        status[(ip + k) % 4] + " " + (ip * 100 + k));
    }
  }
  return out.join("\n") + "\n";
}

// --- corpus --------------------------------------------------------------

const catF1 = fix("cat1.txt", "line one\nline two\n");
const catF2 = fix("cat2.txt", "trailing without newline");
const headF1 = fix("head1.txt", "h1.1\nh1.2\nh1.3\nh1.4\nh1.5\nh1.6\nh1.7\nh1.8\nh1.9\nh1.10\nh1.11\nh1.12\n");
const headF2 = fix("head2.txt", "h2.1\nh2.2\nh2.3\n");
const nlF = fix("nl.txt", "alpha\n\nbeta\n\ngamma\n");
const ckF = fix("ck.txt", "The quick brown fox jumps over the lazy dog\n".repeat(3));
const b64F = fix("b64.txt", "abcdefghijklmnopqrstuvwxyz012345678901234567890123456789012345678901234567890123");
const teeOut = path.join(FIX, "tee_out.txt");
const anCsv = fix("analytics.csv", analyticsCsv);
const logFile = fix("access.log", accessLog());

const SPECS = [
  // comp/tests: compiler feature gates
  ["comp/tests/t01_basic.n", {}],
  ["comp/tests/t02_containers.n", {}],
  ["comp/tests/t03_ctl.n", {}],
  ["comp/tests/t04_str.n", {}],
  ["comp/tests/t05_seq.n", {}],
  ["comp/tests/t06_vec.n", {}],
  ["comp/tests/t07_iter.n", {}],
  ["comp/tests/t08_io.n", {
    ts: true,
    fsPreload: { "tests/log.txt": repo("tests/log.txt") },
  }],
  ["comp/tests/t09_weave.n", {}],
  ["comp/tests/t12_matrix.n", {}],
  ["comp/tests/t13_compute.n", { flags: ["--device", "cpu", "--gc-threshold", "200000000"] }],
  ["comp/tests/t14_task_gpu.n", { flags: ["--device", "cpu"], argv: ["100000"] }],
  ["comp/tests/t15_shared.n", {}],
  ["comp/tests/t16_auto_device.n", { flags: ["--device", "cpu"], argv: ["512"] }],
  ["comp/tests/t17_strict.n", {}],
  ["comp/tests/t18_debug.n", {}],
  ["comp/tests/t19_checkfail.n", {}],
  ["comp/tests/t20_veccoerce.n", {}],
  ["comp/tests/t21_strarith.n", {}],
  ["comp/tests/t22_deepbranch.n", {}],
  ["comp/tests/t23_sharedpoly.n", {}],
  ["comp/tests/t24_refsem.n", {}],
  ["comp/tests/t25_methods.n", {}],
  ["comp/tests/t26_migrated.n", {}],
  ["comp/tests/t27_callable.n", {}],
  ["comp/tests/t28_anonfn.n", {}],
  ["comp/tests/t29_fncall.n", {}],
  ["comp/tests/t30_edweave.n", {}],
  ["comp/tests/t31_nestweave.n", {}],
  ["comp/tests/t32_selfmethod.n", {}],
  // examples: repo example programs
  ["examples/hello.n", {}],
  ["examples/fib.n", {}],
  ["examples/dot.n", {}],
  ["examples/maptest.n", {}],
  ["examples/matmul.n", {}],
  ["examples/memtest.n", {}],
  ["examples/ring.n", {}],
  ["examples/strtest.n", {}],
  ["examples/weave.n", {}],
  ["examples/basics_stores.n", {}],
  ["examples/basics_containers.n", {}],
  ["examples/shelltest.n", {}],
  ["examples/text/matmul.n", {}],
  ["examples/text/chat.n", { compileOnly: true }],
  // examples/gnu: the single-file transpiler targets (libc emulation in the
  // JS prelude; files come from the in-memory fs model)
  ["examples/gnu/cat.n", { argv: [catF1, catF2] }],
  ["examples/gnu/head.n", { argv: [headF1, headF2] }],
  ["examples/gnu/nl.n", { argv: [nlF] }],
  ["examples/gnu/cksum.n", { argv: [ckF] }],
  ["examples/gnu/tee.n", { argv: [teeOut], stdin: "tee line 1\ntee line 2" }],
  ["examples/gnu/base64.n", { argv: [b64F] }],
  // bench/src: the benchmark programs
  ["bench/src/analytics/analytics.n", { argv: [anCsv] }],
  ["bench/src/logextract/logextract.n", { argv: [logFile] }],
  ["bench/src/bfs/bfs.n", { argv: ["100000"] }],
  ["bench/src/blackscholes/blackscholes.n", { argv: ["100000"] }],
  ["bench/src/dynamicgraph/dynamicgraph.n", { argv: ["100000"] }],
  ["bench/src/mandelbrot/mandelbrot.n", {}],
  ["bench/src/matmul/matmul.n", { flags: ["--device", "cpu"], argv: ["256"] }],
  ["bench/src/nqueens/nqueens.n", { argv: ["11"] }],
  ["bench/src/spectralnorm/spectralnorm.n", { argv: ["100"] }],
];

function testFiles() {
  const out = [];
  for (const f of fs.readdirSync(__dirname).sort()) {
    if (/\.n$/.test(f)) out.push([f, {}]);
  }
  return out;
}

// --- runners -------------------------------------------------------------

function fileOf(f) {
  return f.includes("/") ? rep(f) : path.join(__dirname, f);
}

function oracle(f, spec) {
  const args = [];
  if (spec.flags) args.push(...spec.flags);
  args.push(fileOf(f));
  if (spec.argv && spec.argv.length) {
    args.push("--");
    args.push(...spec.argv);
  }
  // spawnSync (not execFileSync): the latter drops the child's stderr on a
  // successful run, and gates like t18_debug print to stderr and succeed.
  const r = spawnSync(NK, args, { cwd: ROOT, encoding: "utf8", maxBuffer: 1 << 26, timeout: 300000, input: spec.stdin });
  return {
    rc: r.status === null ? 1 : r.status,
    stdout: r.stdout == null ? "" : r.stdout,
    stderr: r.stderr == null ? "" : r.stderr,
  };
}

async function jsRun(src, spec) {
  const prog = await nk.compile(src);
  const env = {};
  env.argv = ["prog"].concat(spec.argv || []);
  if (spec.stdin !== undefined) env.stdin = spec.stdin;
  const fsEnv = {};
  if (spec.fs) Object.assign(fsEnv, spec.fs);
  if (spec.fsPreload) Object.assign(fsEnv, spec.fsPreload);
  // argv fixture files exist on disk; mirror them into the in-memory table
  // (fopen returns NULL for an unknown name in any mode, including "w").
  if (spec.argv) for (const a of spec.argv)
    if (path.isAbsolute(a) && a.startsWith(FIX) && fs.existsSync(a))
      fsEnv[a] = fs.readFileSync(a, "utf8");
  if (Object.keys(fsEnv).length) env.fs = fsEnv;
  const r = await prog.run(env);
  // Native die writes "nk: <msg>\n" to stderr and exits 1; the JS run
  // reports the raw message in r.error, so reconstruct the same stream.
  const stderr = (r.err || "") + (r.error ? "nk: " + r.error + "\n" : "");
  return { rc: r.rc, stdout: r.output || "", stderr };
}

function normalize(text, spec) {
  if (!spec || !spec.ts) return text;
  return text
    .split("\n")
    .map((l) => {
      const m = /^(\d+)\.\d+$/.exec(l);
      return m && +m[1] > 1710000000 ? "TS" : l;
    })
    .join("\n");
}

function diff(nat, js, spec) {
  if (nat.rc !== js.rc) return "rc: native=" + nat.rc + " js=" + js.rc;
  const ns = normalize(nat.stdout, spec);
  const jsOut = normalize(js.stdout, spec);
  if (ns !== jsOut) return "stdout:\n--native--\n" + ns + "--js--\n" + jsOut;
  const ne = normalize(nat.stderr, spec).replace(/\n+$/, "");
  const je = normalize(js.stderr, spec).replace(/\n+$/, "");
  if (ne !== je) return "stderr:\n--native--\n" + ne + "--js--\n" + je;
  return null;
}

async function main() {
  const all = SPECS.concat(testFiles());
  let failed = 0;
  let skipped = 0;
  for (const [f, spec] of all) {
    const src = fs.readFileSync(fileOf(f), "utf8");
    if (f.startsWith("neg_")) {
      let err = null;
      try {
        const prog = await nk.compile(src);
        await prog.run();
      } catch (e) {
        err = e.message;
      }
      if (!err) {
        failed++;
        console.log("FAIL " + f + ": expected a compile rejection, got a result");
        continue;
      }
      console.log("OK   " + f + "  (rejected: " + err + ")");
      continue;
    }
    if (spec.compileOnly) {
      try {
        await nk.compile(src);
      } catch (e) {
        failed++;
        console.log("FAIL " + f + ": compile-only gate rejected: " + e.message);
        continue;
      }
      console.log("OK   " + f + "  (compile-only)");
      skipped++;
      continue;
    }
    const nat = oracle(f, spec);
    let js;
    try {
      js = await jsRun(src, spec);
    } catch (e) {
      js = { rc: 1, stdout: "", stderr: e.message + "\n" };
    }
    const d = diff(nat, js, spec);
    if (d) {
      failed++;
      console.log("FAIL " + f + ":\n" + d);
      continue;
    }
    const lines = nat.stdout === "" ? 0 : nat.stdout.trim().split("\n").length;
    console.log("OK   " + f + "  (rc=" + nat.rc + ", " + lines + " line(s) out)");
  }
  if (fs.rmSync(FIX, { recursive: true, force: true })) {}
  if (failed) {
    console.log(failed + " of " + all.length + " gates failed");
    process.exit(1);
  }
  console.log("all " + all.length + " gates passed (" + skipped + " compile-only)");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
