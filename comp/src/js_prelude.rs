// js_prelude.rs — the JavaScript runtime that `--emit-js` and the wasm
// `nk_js_compile` library emit at the head of every compiled program.
//
// It is a self-contained port of the C prelude's *scalar* semantics: cells,
// type coercion, i64/f64 arithmetic (wrapped), the hand-rolled open-addressing
// map (slot-ordered, mirroring the C layout so keys/print/sortkeys match),
// iterators, containers, strings, glob, regex, JSON, fmt/print, and the op
// dispatch table. Only the op subset allowed by `gen_js` is reachable; the
// prelude implements exactly those. No JS RegExp (the C matcher is ported 1:1),
// no toFixed/toPrecision (the glibc float engine is ported by hand).
//
// The emitted file is JS_PRELUDE + one `function F_<pc>(rt, fr)` per region +
// a `__NK_API` export + (for node) an auto-run tail. The presentation layer is
// separate and never lives here.

pub const JS_PRELUDE: &str = r##"
var RT = (function () {
  "use strict";

  // ============================ i64 / f64 bits ============================
  var M64 = 1n << 64n;
  var H64 = 1n << 63n;
  var IMAX = (1n << 63n) - 1n;
  var IMIN = -(1n << 63n);
  function wrap64(x) { x = x & (M64 - 1n); if (x >= H64) x -= M64; return x; }
  function toU(x) { return x & (M64 - 1n); }
  var AB = new ArrayBuffer(8);
  var DV = new DataView(AB, 0, 8);
  function fBits(f) { DV.setFloat64(0, f, true); return DV.getBigInt64(0, true); }
  function bitsF(b) { DV.setBigInt64(0, b, true); return DV.getFloat64(0, true); }
  function le8(x) { // x: number or bigint -> 8 little-endian bytes (as a Uint8Array)
    var a = new Uint8Array(8);
    if (typeof x === "number") { DV.setFloat64(0, x, true); for (var i = 0; i < 8; i++) a[i] = a8read(i); }
    else { DV.setBigInt64(0, x, true); for (var j = 0; j < 8; j++) a[j] = a8read(j); }
    return a;
  }
  function a8read(i) { return new Uint8Array(AB, i, 1)[0]; }

  // ================================ cells =================================
  var _uid = 0;
  function I(x) { return { t: "i", v: (typeof x === "bigint") ? x : BigInt(Math.trunc(x)) }; }
  function F(x) { return { t: "f", v: x }; }
  function Y(x) { return { t: "y", v: x & 0xff }; }
  function B(x) { return { t: "b", v: x ? 1 : 0 }; }
  function S(s) { return { t: "s", v: s }; }
  function N() { return { t: "n", v: null }; }
  function L(a) { return { t: "l", v: a, u: ++_uid }; }
  function D(m) { return { t: "d", v: m, u: ++_uid }; }
  function A(fn) { return { t: "a", v: fn, u: ++_uid }; }
  function T(o) { return { t: "t", v: o, u: ++_uid }; }
  // Numeric array (arr/tensor/mat) — see the vector/tensor section below.
  function V(kind, ety, len, data, rows) { return { t: "v", v: { kind: kind, ety: ety, len: len, data: data, rows: rows || 0 }, u: ++_uid }; }
  // T_TIME cell: v is the nanosecond count as a bigint (C T_TIME payload).
  function TM(ns) { return { t: "tm", v: (typeof ns === "bigint") ? ns : BigInt(ns) }; }
  // Managed struct object: v = { sid, esz, data }, data is esz/8 zero slots.
  function OB(o) { return { t: "ob", v: o, u: ++_uid }; }
  // Bloom filter: v = { len (bits, number), k, words (bigint[]), bits64 }
  function BL(o) { return { t: "bl", v: o, u: ++_uid }; }

  // =========================== runtime handle ============================
  // opName mirrors the C uf_cur_op (initial "<startup>", never cleared):
  // runtime messages report the op in flight when a pop underflows.
  // rt is one task's state; reg is shared by every task of the run: the
  // task list, the chan registry (deadlock accounting), a progress counter
  // (bumped on every segment entry and every wake), and the run-level
  // settle hook installed by the scheduler.
  function makeRt() {
    // env: the run environment (argv, stdin text, in-memory file table) —
    // the node tail and the web glue set rt.reg.env before the entry runs
    var reg = { tasks: [], chans: [], timers: 0, gen: 0, settle: null, G: {}, out: [], err: [], env: { argv: [], stdin: "", fs: {} } };
    var rt = { st: [], iv: [], G: reg.G, L: [], lb: 0, lsp: 0, out: reg.out, opName: "<startup>", curN: 0, site: -1, reg: reg };
    reg.tasks.push({ rt: rt, exit: 0 });
    return rt;
  }
  // A spawned task: fresh stack/frame, the same shared vars and registry.
  function taskRt(reg) {
    var rt = { st: [], iv: [], G: reg.G, L: [], lb: 0, lsp: 0, out: reg.out, opName: "<startup>", curN: 0, site: -1, reg: reg };
    reg.tasks.push({ rt: rt, exit: 0 });
    return rt;
  }
  var BREAK = { nkBreak: 1 };
  var CONT = { nkCont: 1 };
  // Site head for die/dbg: "L<line> [label]", or "<stem>:L<line> [label]"
  // in a multi-TU program; "" when the site is unknown. Reads the __SITES
  // table the codegen emits (null-safe: the prelude must also stand alone).
  // Mirrors the C uf_site helper.
  function siteHead(rt) {
    var S = (typeof __SITES === "undefined") ? null : __SITES;
    if (!S || !rt || !(rt.site >= 0) || rt.site >= S.length) return "";
    var e = S[rt.site];
    if (!e[2]) return "";
    var lab = e[0] ? " [" + e[0] + "]" : "";
    var F = (typeof __SF === "undefined") ? null : __SF;
    if (F && F.length > 1) return F[e[1]] + ":L" + e[2] + lab;
    return "L" + e[2] + lab;
  }
  function die(rt, m) { var h = siteHead(rt); var fm = h ? h + " " + m : m; var e = new Error(fm); e.nkErr = 1; e.nkMsg = fm; throw e; }
  function dieE(m) { var e = new Error(m); e.nkErr = 1; e.nkMsg = m; return e; }
  // Normalize a task-level failure (task error, break/cont escaping a
  // spawned body, foreign throw) to a nk-die error.
  function normErr(e) {
    if (e === BREAK) return dieE("break outside loop");
    if (e === CONT) return dieE("cont outside loop");
    if (e && e.nkErr) return e;
    return dieE(e && e.message ? e.message : String(e));
  }
  function pop(rt, op) {
    var st = rt.st;
    if (st.length === 0) die(rt, "stack underflow in " + (rt.opName || op) + " (sp=0)");
    return st.pop();
  }
  function pop2(rt, op) { var b = pop(rt, op); var a = pop(rt, op); return [a, b]; }

  // ========================= string parse helpers ========================
  function isWS(c) { return c === 0x20 || c === 0x09 || c === 0x0a || c === 0x0b || c === 0x0c || c === 0x0d; }
  function hexVal(c) { c |= 0x20; var d = c - 97; if (d >= 0 && d <= 5) return 10 + d; d = c - 48; if (d >= 0 && d <= 9) return d; return -1; }
  function clampI64(x) { return x > IMAX ? IMAX : (x < IMIN ? IMIN : x); }
  // strtoll base-10 prefix (leading WS ok; stops at first non-digit) -> i64
  function parseInt10(s) {
    var i = 0, n = s.length;
    while (i < n && isWS(s.charCodeAt(i))) i++;
    var neg = false;
    if (i < n && (s.charCodeAt(i) === 43 || s.charCodeAt(i) === 45)) { if (s.charCodeAt(i) === 45) neg = true; i++; }
    var acc = 0n, any = false;
    while (i < n) { var c = s.charCodeAt(i); if (c < 48 || c > 57) break; acc = acc * 10n + BigInt(c - 48); any = true; i++; }
    if (!any) return 0n;
    acc = neg ? -acc : acc;
    return clampI64(acc);
  }
  function matchCase(s, i, seq) {
    if (i + seq.length > s.length) return false;
    for (var k = 0; k < seq.length; k++) {
      var c = (s.charCodeAt(i + k) | 0x20);
      if (String.fromCharCode(c) !== seq[k]) return false;
    }
    return true;
  }
  // glibc-strtod prefix scanner from s[i..] -> {v, e} (e = index just past
  // the consumed prefix). Leading WS is the caller's job. After the sign:
  // "inf" (+ optional "inity"), "nan" (+ optional balanced "(...)"),
  // 0x[.hexdigits][pP±digits], or decimal int [.frac] [eE ±digits].
  // An incomplete exponent is NOT consumed (glibc behavior). "0x" with no
  // hex digits at all consumes only the leading "0" (value 0).
  function scanNum(s, i) {
    var n = s.length, sign = 1;
    if (i < n && (s.charCodeAt(i) === 43 || s.charCodeAt(i) === 45)) { if (s.charCodeAt(i) === 45) sign = -1; i++; }
    if (matchCase(s, i, "inf")) {
      var ei = i + 3 + (matchCase(s, i + 3, "inity") ? 5 : 0);
      return { v: sign * Infinity, e: ei };
    }
    if (matchCase(s, i, "nan")) {
      var en = i + 3;
      if (en < n && s.charCodeAt(en) === 40) {
        var dp = 1, p2 = en + 1;
        while (p2 < n && dp) { var pc = s.charCodeAt(p2); if (pc === 40) dp++; else if (pc === 41) dp--; p2++; }
        if (dp === 0) en = p2;
      }
      return { v: NaN, e: en };
    }
    if (i + 1 < n && s.charCodeAt(i) === 48 && (s.charCodeAt(i + 1) === 120 || s.charCodeAt(i + 1) === 88)) {
      var j = i + 2, mant = 0, fb = 0, got = 0;
      while (j < n && s.charCodeAt(j) !== 46) { var hv = hexVal(s.charCodeAt(j)); if (hv < 0) break; mant = mant * 16 + hv; got = 1; j++; }
      if (j < n && s.charCodeAt(j) === 46) {
        j++;
        while (j < n) { var h2 = hexVal(s.charCodeAt(j)); if (h2 < 0) break; mant = mant * 16 + h2; fb++; j++; }
      }
      if (j < n && (s.charCodeAt(j) === 112 || s.charCodeAt(j) === 110)) {
        j++; var es = 1;
        if (j < n && (s.charCodeAt(j) === 43 || s.charCodeAt(j) === 45)) { if (s.charCodeAt(j) === 45) es = -1; j++; }
        var ee = 0;
        while (j < n) { var c3 = s.charCodeAt(j); if (c3 < 48 || c3 > 57) break; ee = ee * 10 + (c3 - 48); j++; }
        return { v: sign * mant * Math.pow(16, -fb) * Math.pow(2, es * ee), e: j };
      }
      if (got === 0 && fb === 0) return { v: 0, e: i + 1 };
      return { v: sign * mant * Math.pow(16, -fb), e: j };
    }
    var ip = 0, fp = 0, jd = i;
    while (jd < n) { var c5 = s.charCodeAt(jd); if (c5 < 48 || c5 > 57) break; ip++; jd++; }
    if (jd < n && s.charCodeAt(jd) === 46) {
      jd++;
      while (jd < n) { var c6 = s.charCodeAt(jd); if (c6 < 48 || c6 > 57) break; fp++; jd++; }
    }
    if (jd < n && (s.charCodeAt(jd) === 101 || s.charCodeAt(jd) === 69)) {
      var je = jd + 1;
      if (je < n && (s.charCodeAt(je) === 43 || s.charCodeAt(je) === 45)) je++;
      var ed = je;
      while (je < n) { var c7 = s.charCodeAt(je); if (c7 < 48 || c7 > 57) break; je++; }
      if (je > ed) jd = je;
    }
    if (ip === 0 && fp === 0) return { v: 0, e: i };
    return { v: sign * Number(s.slice(i, jd)), e: jd };
  }
  // strtod prefix (leading WS, sign, inf/nan, hex, decimal) -> number
  function parseDouble(s) {
    var i = 0, n = s.length;
    while (i < n && isWS(s.charCodeAt(i))) i++;
    return scanNum(s, i).v;
  }
  // strict full-match strtod (trailing WS ok) else NaN
  function strictDouble(s) {
    var t = s.replace(/[ \t\n\v\f\r]+$/, "");
    if (t.length === 0) return NaN;
    if (!/^[+-]?([0-9]*\.?[0-9]+([eE][+-]?[0-9]+)?|0x[0-9a-fA-F]*\.?[0-9a-fA-F]+([pP][+-]?[0-9]+)?|inf(inity)?|nan)$/.test(t)) return NaN;
    if (/^(inf|nan)/i.test(t) || /^-inf/i.test(t)) return parseDouble(t);
    return parseDouble(t);
  }

  // ============================ type coercion ===========================
  // uf_i: raw i64 view of a cell
  function iOf(x) {
    switch (x.t) {
      case "i": case "y": case "b": return typeof x.v === "number" ? wrap64(BigInt(x.v)) : x.v;
      case "f": return fBits(x.v);
      case "s": return parseInt10(x.v);
      case "tm": return x.v;
      case "n": return 0n;
      case "l": case "d": case "v": return 0n; // documented: C uses pointer bits
      case "a": case "t": return toU(BigInt(x.u));
    }
    return 0n;
  }
  function asInt(x) { // i/y/b -> v; f -> fbits; else 0
    switch (x.t) {
      case "i": case "y": case "b": return typeof x.v === "number" ? wrap64(BigInt(x.v)) : x.v;
      case "f": return fBits(x.v);
      default: return 0n;
    }
  }
  // uf_f: f64 view
  function fOf(x) {
    switch (x.t) {
      case "i": return Number(x.v);
      case "f": return x.v;
      case "y": return x.v;
      case "b": return x.v;
      case "s": return parseDouble(x.v);
      case "tm": return Number(x.v);
      case "n": return 0;
      case "l": case "d": case "v": return 0; // documented
      case "a": case "t": return 0;
    }
    return 0;
  }
  function truthy(x) {
    switch (x.t) {
      case "f": return x.v !== 0 && !isNaN(x.v);
      case "i": case "y": case "b": return Number(x.v) !== 0; // Number() is exact for the zero question on both number and bigint
      case "s": case "l": return x.v.length > 0;
      case "d": return x.v.len > 0;
      case "v": return x.v.len > 0;
      case "m": return x.v.len > 0;
      case "tm": return false; // C: T_TIME is never truthy
      case "n": return false;
      case "a": case "t": return true;
      case "p": return x.v !== null && x.v.k !== "n"; // C: pointer bits nonzero
    }
    return false;
  }
  function zero(x) {
    switch (x.t) {
      case "f": return x.v === 0; // NaN !== 0 -> false
      case "i": case "y": case "b": return Number(x.v) === 0; // 0n === 0 is false in JS; Number() compares across both
      case "tm": return true; // C: T_TIME is in the numeric family, zero-test on its ns int
      case "n": return true;
      case "p": return !x.v || x.v.k === "n";
      default: return false;
    }
  }
  // to_number
  function toNumber(x) {
    switch (x.t) {
      case "i": return Number(x.v);
      case "f": return x.v;
      case "y": return x.v;
      case "b": return x.v;
      case "s": return strictDouble(x.v);
      case "l": {
        if (x.v.length === 0) return 0;
        if (x.v.length === 1) return toNumber(x.v[0]);
        return NaN;
      }
      case "d": return NaN;
      case "v": return 0; // documented: C uses pointer bits
      case "a": return 0;
      case "n": return 0;
      case "t": return 0;
      case "tm": return Number(x.v);
    }
    return 0;
  }
  // uf_to_string (delegate float printing to the glibc engine below)
  function toStr(x) {
    switch (x.t) {
      case "f": if (isNaN(x.v)) return "NaN"; return printFloatG(x.v, 17, "g");
      case "b": return x.v ? "true" : "false";
      case "i": return x.v.toString();
      case "y": return x.v.toString();
      case "n": return "null";
      case "s": return x.v;
      case "l": {
        var out = [];
        for (var i = 0; i < x.v.length; i++) out.push(toStr(x.v[i]));
        return out.join(",");
      }
      case "d": return "[object Object]";
      case "v": return iOf(x).toString(); // documented: C prints pointer bits
      case "a": case "t": return x.u.toString(); // documented: uid, not ptr bits
      case "tm": return x.v.toString(); // C: decimal nanosecond count
      case "ob": return "[object Object]";
      case "bl": return "<bloom>";
    }
    return "";
  }

  // ============================ open map =================================
  // Layout mirrors the C map: open addressing, slot order is scan order.
  // st: 0 empty, 1 occupied, 2 tombstone. cap starts at 64.
  function mapNew() {
    return { cap: 64, st: new Uint8Array(64), k: new Array(64), v: new Array(64), len: 0 };
  }
  function fnvBytes(b) {
    var h = 1469598103934665603n, p = 1099511628211n;
    for (var i = 0; i < b.length; i++) { h ^= BigInt(b[i]); h = (h * p) & (M64 - 1n); }
    return h;
  }
  function mapHash(k) {
    switch (k.t) {
      case "i": return toU(k.v);
      case "s": { var a = new Uint8Array(k.v.length); for (var i = 0; i < k.v.length; i++) a[i] = k.v.charCodeAt(i); return fnvBytes(a); }
      case "f": return fnvBytes(le8(k.v));
      case "y": case "b": return fnvBytes(le8(BigInt(k.v)));
      case "n": return fnvBytes(new Uint8Array(8));
      default: return fnvBytes(le8(BigInt(k.u))); // l/d/a/t -> uid bits (documented)
    }
  }
  function keyeq(a, b) {
    if (a.t !== b.t) return false;
    if (a.t === "i" || a.t === "y" || a.t === "b") return iOf(a) === iOf(b);
    if (a.t === "f") return a.v === b.v;
    if (a.t === "s") {
      if (a.v.length !== b.v.length) return false;
      for (var i = 0; i < a.v.length; i++) if (a.v.charCodeAt(i) !== b.v.charCodeAt(i)) return false;
      return true;
    }
    if (a.t === "n") return true;
    if (a.t === "a" || a.t === "t" || a.t === "l" || a.t === "d") return a.u === b.u; // identity
    return false;
  }
  function mapPutRaw(m, k, v) {
    var i = Number(mapHash(k) % BigInt(m.cap));
    while (true) {
      if (m.st[i] === 0) { m.st[i] = 1; m.k[i] = k; m.v[i] = v; m.len++; return; }
      if (m.st[i] === 1 && keyeq(m.k[i], k)) { m.v[i] = v; return; }
      i = (i + 1) % m.cap;
    }
  }
  function mapGrow(m) {
    var oc = m.cap, os = m.st, okk = m.k, ov = m.v;
    m.cap = m.cap * 4;
    m.st = new Uint8Array(m.cap); m.k = new Array(m.cap); m.v = new Array(m.cap); m.len = 0;
    for (var i = 0; i < oc; i++) if (os[i] === 1) mapPutRaw(m, okk[i], ov[i]);
  }
  function mapPut(m, k, v) { if ((m.len + 1) * 10 >= m.cap * 7) mapGrow(m); mapPutRaw(m, k, v); }
  function mapGet(m, k) {
    var i = Number(mapHash(k) % BigInt(m.cap));
    while (true) {
      if (m.st[i] === 0) return null;
      if (m.st[i] === 1 && keyeq(m.k[i], k)) return m.v[i];
      i = (i + 1) % m.cap;
    }
  }
  function mapDel(m, k) {
    var i = Number(mapHash(k) % BigInt(m.cap));
    while (true) {
      if (m.st[i] === 0) return;
      if (m.st[i] === 1 && keyeq(m.k[i], k)) { m.st[i] = 2; m.k[i] = null; m.v[i] = null; m.len--; return; }
      i = (i + 1) % m.cap;
    }
  }
  // slot-ordered key list (scan 0..cap, st===1)
  function mapKeys(m) {
    var out = new Array(m.len); var n = 0;
    for (var i = 0; i < m.cap; i++) if (m.st[i] === 1) out[n++] = m.k[i];
    return L(out);
  }

  // ============================= iterators ==============================
  // kinds: list, arr, dict, str, map, filter
  function itList(a) { return T({ kind: "list", src: a, idx: 0 }); }
  function itArr(a) { return T({ kind: "arr", src: a, idx: 0 }); }
  function itBm(b) { return T({ kind: "bm", src: b, idx: 0 }); }
  function itDict(m) { return T({ kind: "dict", src: m, idx: 0 }); }
  function itStr(s) { return T({ kind: "str", src: s, idx: 0 }); }
  function itMap(src, f) { return T({ kind: "map", src: src, idx: 0, f: f }); }
  function itFilter(src, f) { return T({ kind: "filter", src: src, idx: 0, f: f }); }
  async function itStep(it, rt) { // advance + return next cell or null (exhausted)
    var o = it.v;
    if (o.kind === "list") { if (o.idx >= o.src.length) return null; return o.src[o.idx++]; }
    if (o.kind === "arr") { if (o.idx >= o.src.v.len) return null; return vread(o.src, o.idx++); }
    if (o.kind === "bm") {
      // C IT_BITMAP: yield the index of each set bit, in order
      var b2 = o.src.v, n2 = b2.len;
      while (o.idx < n2) { var i2 = o.idx++; if (bmBit(b2, i2)) return I(BigInt(i2)); }
      return null;
    }
    if (o.kind === "str") { if (o.idx >= o.src.length) return null; return I(o.src.charCodeAt(o.idx++)); }
    if (o.kind === "dict") {
      var m = o.src;
      while (o.idx < m.cap) { var i = o.idx++; if (m.st[i] === 1) return m.k[i]; }
      return null;
    }
    if (o.kind === "map") {
      var c = await itStep(o.src, rt);
      if (c === null) return null;
      var base = rt.st.length;
      rt.st.push(c);
      await preludeCall(rt, o.f, 1);
      var r = rt.st.pop();
      rt.st.length = base;
      return r;
    }
    if (o.kind === "filter") {
      for (;;) {
        var c2 = await itStep(o.src, rt);
        if (c2 === null) return null;
        var base2 = rt.st.length;
        rt.st.push(c2);
        await preludeCall(rt, o.f, 1);
        var r2 = rt.st.pop();
        rt.st.length = base2;
        if (truthy(r2)) return c2;
      }
    }
    return null;
  }
  // materialize any iterable into a list; lists pass through the SAME array
  // (C: HT_DYN returned in place, mutated by callers like SORT)
  async function materialize(x, rt) {
    if (x.t === "l") return x.v;
    if (x.t === "v") { var vo = new Array(x.v.len); for (var w = 0; w < x.v.len; w++) vo[w] = vread(x, w); return vo; }
    if (x.t === "s") { var a = new Array(x.v.length); for (var i = 0; i < x.v.length; i++) a[i] = I(x.v.charCodeAt(i)); return a; }
    if (x.t === "d") return mapKeys(x.v).v;
    if (x.t === "m") { var outm = []; for (;;) { var cm = await itStep(itBm(x), rt); if (cm === null) break; outm.push(cm); } return outm; }
    if (x.t === "t") { var out = []; for (;;) { var c = await itStep(x, rt); if (c === null) break; out.push(c); } return out; }
    die(rt, "not a sequence");
  }
  // Invoke a user callback with `nargs` args on the stack using the
  // generated frame contract: bump rt.lb by the current region's local
  // count so fr.base is the callee's local base; drain all but the args.
  // The body is async (every generated op call awaits), so the call must be
  // awaited too; otherwise a body that suspends mid-flight leaves the stack
  // in a torn state for the caller's post-call pop.
  async function preludeCall(rt, fn, nargs) {
    if (typeof fn !== "function") die(rt, "JavaScript target: cannot call a non-function cell");
    var _lb = rt.lb;
    rt.lb += (rt.curN || 0);
    await fn(rt, { drain: rt.st.length > nargs ? rt.st.length - nargs : 0, base: rt.lb });
    rt.lb = _lb;
  }

  // ============================== comparison =============================
  // uf_cmp: numeric family is T_INT/T_FLOAT only (null = T_INT; bytes and
  // bools are *not* comparable, matching C). NaN compares as "equal" (both
  // < and > are false), same as C's double comparison.
  function cmp(a, b) {
    var na = (a.t === "i" || a.t === "f" || a.t === "n" || a.t === "tm");
    var nb = (b.t === "i" || b.t === "f" || b.t === "n" || b.t === "tm");
    if (na && nb) { var x = fOf(a), y = fOf(b); return { c: x < y ? -1 : (x > y ? 1 : 0), ok: true }; }
    if (a.t === "s" && b.t === "s") { var m = Math.min(a.v.length, b.v.length); for (var i = 0; i < m; i++) { var d = a.v.charCodeAt(i) - b.v.charCodeAt(i); if (d) return { c: d < 0 ? -1 : 1, ok: true }; } if (a.v.length !== b.v.length) return { c: a.v.length < b.v.length ? -1 : 1, ok: true }; return { c: 0, ok: true }; }
    return { c: 0, ok: false };
  }

  // ========================== arithmetic / logic =========================
  // C tag view: i and n are both T_INT (null = int 0); y, b, f, ptr distinct.
  function ctag(x) {
    switch (x.t) {
      case "i": case "n": return "i";
      case "f": return "f";
      case "y": return "y";
      case "b": return "b";
      case "tm": return "tm"; // T_TIME has its own C tag
      default: return "p"; // s, l, d, a, t, ob, bl are all pointer-valued in C
    }
  }
  function isTInt(x) { return x.t === "i" || x.t === "n"; }
  function intRaw(x) { return x.t === "n" ? 0n : x.v; }
  function isBitok(x) { return x.t === "i" || x.t === "y" || x.t === "b" || x.t === "n"; }
  // managed-handle identity (C: pointer bits compared as numbers; two distinct
  // builds of the same contents compare unequal, the same handle equals itself)
  function isHandle(x) { return x.t === "v" || x.t === "m" || x.t === "ob" || x.t === "bl"; }
  // C pointer bits: NULL equals NULL, a pointer equals itself, two distinct
  // allocations (even of equal contents) are unequal
  function ptrEq(a, b) {
    if (a.t !== "p" || b.t !== "p" || !a.v || !b.v) return false;
    if (a.v.k === "n" && b.v.k === "n") return true;
    return a === b;
  }
  function looseEq(a, b) {
    if (a.t === "p" || b.t === "p") return ptrEq(a, b);
    if (isHandle(a) || isHandle(b)) return isHandle(a) && a.t === b.t && a.u === b.u;
    if (a.t === "s" && b.t === "s") return a.v === b.v;
    return toNumber(a) === toNumber(b);
  }
  function strictEq(a, b) {
    if (a.t === "p" || b.t === "p") return ptrEq(a, b);
    if (isHandle(a) || isHandle(b)) return isHandle(a) && a.t === b.t && a.u === b.u;
    if (ctag(a) !== ctag(b)) return false;
    if (a.t === "f") return fBits(a.v) === fBits(b.v);
    if (a.t === "s" && b.t === "s") return a.v === b.v;
    if (a.t === "l" || a.t === "d" || a.t === "a" || a.t === "t") return a.u === b.u;
    return iOf(a) === iOf(b);
  }
  function arith2(rt, op) {
    var p = pop2(rt, op); var a = p[0], b = p[1];
    var add = op === "add";
    if (vecCand(a) || vecCand(b)) { polyArith(rt, a, b, add ? 0 : 1, add ? "add" : "sub"); return; }
    // C (uf_cadd/uf_csub): pointer ± int is raw byte-offset arithmetic; a
    // string plus an integer shifts the string pointer (the C flip path is
    // add-only). string - int and string + string fall through to the
    // numeric path below, as in C.
    if (a.t === "p" || b.t === "p") {
      var pv = a.t === "p" ? a : b, iv = a.t === "p" ? b : a;
      if (!isTInt(iv)) die(rt, "pointer arithmetic: non-integer operand");
      ptrShift(rt, pv, iv, op);
      return;
    }
    if (a.t === "s" && isTInt(b)) { ptrShift(rt, a, b, op); return; }
    if (add && b.t === "s" && isTInt(a)) { ptrShift(rt, b, a, "add"); return; }
    if (add) { var r = cadd2(a, b); rt.st.push(r); }
    else {
      var x = toNumber(a), y = toNumber(b);
      if (isNaN(x) || isNaN(y)) { rt.st.push(F(NaN)); return; }
      if (isTInt(a) && isTInt(b)) rt.st.push(I(wrap64(intRaw(a) - intRaw(b))));
      else rt.st.push(F(x - y));
    }
  }
  // C uf_cadd: the content-add shared by + and the counter ops (addto,
  // faddto, finc). Both ints -> wrapped int; any NaN -> float NaN; else float.
  function cadd2(a, b) {
    var x = toNumber(a), y = toNumber(b);
    if (isNaN(x) || isNaN(y)) return F(NaN);
    if (isTInt(a) && isTInt(b)) return I(wrap64(intRaw(a) + intRaw(b)));
    return F(x + y);
  }
  function op_add(rt) { arith2(rt, "add"); }
  function op_sub(rt) { arith2(rt, "sub"); }
  function op_mul(rt) {
    var p = pop2(rt, "mul"); var a = p[0], b = p[1];
    if (vecCand(a) || vecCand(b)) { polyArith(rt, a, b, 2, "mul"); return; }
    if (a.t === "i" && a.v === 1n && b.t !== "f") { rt.st.push(b); return; }
    if (b.t === "i" && b.v === 1n && a.t !== "f") { rt.st.push(a); return; }
    var x = toNumber(a), y = toNumber(b);
    if (isNaN(x) || isNaN(y)) { rt.st.push(F(NaN)); return; }
    if (isTInt(a) && isTInt(b)) rt.st.push(I(wrap64(intRaw(a) * intRaw(b))));
    else rt.st.push(F(x * y));
  }
  function op_and(rt) { var p = pop2(rt, "and"); rt.st.push(I(iOf(p[0]) & iOf(p[1]))); }
  function op_or(rt) {
    var p = pop2(rt, "or");
    if (!isBitok(p[0]) || !isBitok(p[1])) die(rt, "OR: ints only");
    rt.st.push(I(iOf(p[0]) | iOf(p[1])));
  }
  function op_xor(rt) {
    var p = pop2(rt, "xor");
    if (!isBitok(p[0]) || !isBitok(p[1])) die(rt, "XOR: ints only");
    rt.st.push(I(iOf(p[0]) ^ iOf(p[1])));
  }
  function op_shl(rt) {
    var p = pop2(rt, "shl");
    if (!isBitok(p[0]) || !isBitok(p[1])) die(rt, "SHL: ints only");
    var sb = iOf(p[1]);
    if (sb < 0n || sb >= 64n) die(rt, "SHL: shift out of range");
    rt.st.push(I(wrap64(iOf(p[0]) << sb)));
  }
  function op_shr(rt) {
    var p = pop2(rt, "shr");
    if (!isBitok(p[0]) || !isBitok(p[1])) die(rt, "SHR: ints only");
    var sb = iOf(p[1]);
    if (sb < 0n || sb >= 64n) die(rt, "SHR: shift out of range");
    rt.st.push(I(wrap64(toU(iOf(p[0])) >> sb)));
  }
  function op_bnot(rt) {
    var a = pop(rt, "bnot");
    if (!isBitok(a)) die(rt, "BNOT: ints only");
    rt.st.push(I(~iOf(a)));
  }
  function op_inc(rt) {
    var a = pop(rt, "inc");
    var x = toNumber(a);
    if (isNaN(x)) { rt.st.push(F(NaN)); return; }
    if (a.t === "i" || a.t === "n") rt.st.push(I(wrap64(intRaw(a) + 1n)));
    else rt.st.push(F(x + 1));
  }
  function op_dec(rt) {
    var a = pop(rt, "dec");
    var x = toNumber(a);
    if (isNaN(x)) { rt.st.push(F(NaN)); return; }
    if (a.t === "i" || a.t === "n") rt.st.push(I(wrap64(intRaw(a) - 1n)));
    else rt.st.push(F(x - 1));
  }
  function op_pow(rt) { var p = pop2(rt, "pow"); rt.st.push(F(Math.pow(toNumber(p[0]), toNumber(p[1])))); }
  function op_sqrt(rt) {
    var a = pop(rt, "sqrt");
    if (vecCand(a)) {
      var h = vecNormV(rt, a, "sqrt");
      var n = h.v.len, r = vArrLike(h, n);
      for (var i = 0; i < n; i++) putEl(r, i, Math.sqrt(elD(h, i)));
      rt.st.push(r);
      return;
    }
    rt.st.push(F(Math.sqrt(toNumber(a))));
  }
  function op_div(rt) {
    var p = pop2(rt, "div"); var a = p[0], b = p[1];
    if (vecCand(a) || vecCand(b)) { polyArith(rt, a, b, 3, "div"); return; }
    var x = toNumber(a), y = toNumber(b);
    if (isNaN(x) || isNaN(y)) { rt.st.push(F(NaN)); return; }
    if (y === 0) die(rt, "DIV: division by zero");
    if (isTInt(a) && isTInt(b)) rt.st.push(I(wrap64(intRaw(a) / intRaw(b))));
    else rt.st.push(F(x / y));
  }
  function op_rem(rt) {
    var p = pop2(rt, "rem"); var a = p[0], b = p[1];
    var x = toNumber(a), y = toNumber(b);
    if (isNaN(x) || isNaN(y)) { rt.st.push(F(NaN)); return; }
    if (y === 0) die(rt, "REM: division by zero");
    if (isTInt(a) && isTInt(b)) rt.st.push(I(wrap64(intRaw(a) % intRaw(b))));
    else rt.st.push(F(x % y));
  }
  function op_lt(rt) { var p = pop2(rt, "lt"); var x = toNumber(p[0]), y = toNumber(p[1]); rt.st.push(I((!isNaN(x) && !isNaN(y) && x < y) ? 1 : 0)); }
  function op_gt(rt) { var p = pop2(rt, "gt"); var x = toNumber(p[0]), y = toNumber(p[1]); rt.st.push(I((!isNaN(x) && !isNaN(y) && x > y) ? 1 : 0)); }
  function op_lte(rt) { var p = pop2(rt, "lte"); var x = toNumber(p[0]), y = toNumber(p[1]); rt.st.push(I((!isNaN(x) && !isNaN(y) && x <= y) ? 1 : 0)); }
  function op_gte(rt) { var p = pop2(rt, "gte"); var x = toNumber(p[0]), y = toNumber(p[1]); rt.st.push(I((!isNaN(x) && !isNaN(y) && x >= y) ? 1 : 0)); }
  function op_eq(rt) { var p = pop2(rt, "eq"); rt.st.push(I(looseEq(p[0], p[1]) ? 1 : 0)); }
  function op_seq(rt) { var p = pop2(rt, "seq"); rt.st.push(I(strictEq(p[0], p[1]) ? 1 : 0)); }
  function op_sne(rt) { var p = pop2(rt, "sne"); rt.st.push(I(strictEq(p[0], p[1]) ? 0 : 1)); }
  function op_not(rt) { var a = pop(rt, "not"); rt.st.push(I(truthy(a) ? 0 : 1)); }
  function op_orelse(rt) { var p = pop2(rt, "orelse"); rt.st.push(truthy(p[0]) ? p[0] : p[1]); }
  function op_drop(rt) { pop(rt, "drop"); }

  // =============================== cast =================================
  // _cast (static): raw-payload reinterpret; id operand's ty is its i64 payload.
  function op_cast(rt) {
    var id = pop(rt, "cast");
    var h = pop(rt, "cast");
    var ty = asInt(id);
    if (ty >= 1000n) {
      if (h.t === "n") die(rt, "CAST: null handle");
      if (h.t === "ob" && BigInt(h.v.sid) === ty - 1000n) { rt.st.push(h); return; }
      die(rt, "CAST: type mismatch");
    }
    switch (Number(ty)) {
      case 0:
        if (h.t === "f") rt.st.push(I(fBits(h.v)));
        else if (h.t === "i" || h.t === "y" || h.t === "b") rt.st.push(I(h.v));
        else if (h.t === "n") rt.st.push(I(0));
        else die(rt, "JavaScript target: pointer cast not supported");
        return;
      case 1:
        if (h.t === "f") rt.st.push(h);
        else if (h.t === "i" || h.t === "y" || h.t === "b") rt.st.push(F(Number(h.v)));
        else if (h.t === "n") rt.st.push(F(0));
        else die(rt, "CAST float: not a scalar");
        return;
      case 2:
        if (h.t === "i" || h.t === "y" || h.t === "b" || h.t === "f") die(rt, "JavaScript target: pointer cast not supported");
        rt.st.push(h);
        return;
      case 3:
        rt.st.push(Y(Number(asInt(h) & 0xffn)));
        return;
      case 21:
        if (h.t === "s" || h.t === "l" || h.t === "d" || h.t === "a" || h.t === "t") rt.st.push(B(1));
        else if (h.t === "n") rt.st.push(B(0));
        else rt.st.push(B(truthy(h)));
        return;
      default:
        die(rt, "CAST: unsupported type id " + ty.toString());
    }
  }
  // cast (dynamic): content-aware conversion.
  function op_dcast(rt) {
    var id = pop(rt, "dcast");
    var v = pop(rt, "dcast");
    var ty = asInt(id);
    if (ty >= 1000n) {
      if (v.t === "n") die(rt, "cast: null handle");
      if (v.t === "ob" && BigInt(v.v.sid) === ty - 1000n) { rt.st.push(v); return; }
      die(rt, "cast: type mismatch");
    }
    switch (Number(ty)) {
      case 0: {
        var d = toNumber(v);
        if (isNaN(d)) die(rt, "cast int: value has no numeric form");
        // x86 cvttsd2si saturates to INT64_MIN for |d| >= 2^63
        if (d >= 9223372036854775808 || d <= -9223372036854775808) rt.st.push(I(IMIN));
        else rt.st.push(I(BigInt(Math.trunc(d))));
        return;
      }
      case 1:
        rt.st.push(F(toNumber(v)));
        return;
      case 2:
        if (v.t === "i" || v.t === "y" || v.t === "b" || v.t === "f") die(rt, "JavaScript target: pointer cast not supported");
        rt.st.push(v);
        return;
      case 3:
        rt.st.push(Y(Number(asInt(v) & 0xffn)));
        return;
      case 21:
        if (v.t === "s") {
          if (v.v === "true") rt.st.push(B(1));
          else if (v.v === "false") rt.st.push(B(0));
          else die(rt, "cast bool: expected true or false string");
          return;
        }
        rt.st.push(B(truthy(v)));
        return;
      case 9:
        rt.st.push(S(toStr(v)));
        return;
      default:
        die(rt, "cast: unsupported type id " + ty.toString());
    }
  }
  // typeof: h -> tag id (i:0 f:1 null/addr:2 byte:3 arr:5 tensor:6 list:7
  //          dict:8 str:9 bitmap:14 iter:18 mat:20 bool:21)
  function op_typeof(rt) {
    var h = pop(rt, "typeof");
    var tag;
    switch (h.t) {
      case "i": tag = 0; break;
      case "f": tag = 1; break;
      case "n": tag = 2; break;
      case "a": tag = 2; break;
      case "y": tag = 3; break;
      case "l": tag = 7; break;
      case "d": tag = 8; break;
      case "s": tag = 9; break;
      case "t": tag = 18; break;
      case "b": tag = 21; break;
      case "m": tag = 14; break;
      case "tm": tag = 15; break;
      case "ob": tag = 13; break;
      case "bl": tag = 17; break;
      case "v": tag = h.v.kind === "mat" ? 20 : (h.v.kind === "tensor" ? 6 : 5); break;
    }
    rt.st.push(I(tag));
  }
  function op_len(rt) {
    var h = pop(rt, "len");
    switch (h.t) {
      case "l": rt.st.push(I(h.v.length)); return;
      case "s": rt.st.push(I(h.v.length)); return;
      case "d": rt.st.push(I(h.v.len)); return;
      case "v": rt.st.push(I(h.v.len)); return;
      case "m": rt.st.push(I(h.v.len)); return;
      case "c": rt.st.push(I(h.v.q.length - h.v.head)); return;
      case "o": rt.st.push(I(1)); return;
      case "bl": rt.st.push(I(BigInt(h.v.len))); return; // bit count
      case "n": die(rt, "handle is null");
      default: die(rt, "LEN: handle has no length"); // a (code), t (iter)
    }
  }

  // ============================== containers =============================
  function op_list(rt) { rt.st.push(L([])); }
  function op_dict(rt) { rt.st.push(D(mapNew())); }
  function op_get(rt) {
    var k = pop(rt, "get");
    var h = pop(rt, "get");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t === "d") { var v = mapGet(h.v, k); if (v === null) die(rt, "GET: missing key"); rt.st.push(v); return; }
    if (h.t === "l") { var ix = iOf(k); if (ix < 0n || ix >= BigInt(h.v.length)) die(rt, "GET: index out of bounds"); rt.st.push(h.v[Number(ix)]); return; }
    if (h.t === "s") { var j = iOf(k); if (j < 0n || j >= BigInt(h.v.length)) die(rt, "GET: index out of bounds"); rt.st.push(I(h.v.charCodeAt(Number(j)))); return; }
    if (h.t === "v") { var ixv = iOf(k); if (ixv < 0n || ixv >= BigInt(h.v.len)) die(rt, "index out of bounds"); rt.st.push(vread(h, Number(ixv))); return; }
    if (h.t === "ob") { var og = objOff(h.v, k); if (og < 0 || og >= h.v.esz / 8) die(rt, "GET: no such field"); rt.st.push(h.v.data[og >> 3]); return; }
    die(rt, "GET: unsupported handle"); // a, t
  }
  function op_getq(rt) {
    var k = pop(rt, "getq");
    var h = pop(rt, "getq");
    // mirrors the C op: non-pointer handles are not managed memory -> 0
    if (h.t === "n" || h.t === "a" || h.t === "i" || h.t === "f" || h.t === "y" || h.t === "b") { rt.st.push(I(0)); return; }
    if (h.t === "d") { var v = mapGet(h.v, k); rt.st.push(v === null ? I(0) : v); return; }
    if (h.t === "l" || h.t === "s") {
      var ix = iOf(k);
      if (ix < 0n || ix >= BigInt(h.v.length)) { rt.st.push(I(0)); return; }
      rt.st.push(h.t === "l" ? h.v[Number(ix)] : I(h.v.charCodeAt(Number(ix))));
      return;
    }
    if (h.t === "v") { var ixv = iOf(k); if (ixv < 0n || ixv >= BigInt(h.v.len)) { rt.st.push(I(0)); return; } rt.st.push(vread(h, Number(ixv))); return; }
    if (h.t === "ob") { var oq = objOff(h.v, k); rt.st.push(oq < 0 || oq >= h.v.esz / 8 ? I(0) : h.v.data[oq >> 3]); return; }
    die(rt, "GETQ: unsupported handle"); // t (iter), matching the C default
  }
  // SET: h k v -> v (pass-through)
  function op_set(rt) {
    var v = pop(rt, "set");
    var k = pop(rt, "set");
    var h = pop(rt, "set");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t === "d") {
      mapPut(h.v, k, v);
    } else if (h.t === "l") {
      var ix = iOf(k);
      if (ix < 0n || ix >= BigInt(h.v.length)) die(rt, "index out of bounds");
      h.v[Number(ix)] = v;
    } else if (h.t === "s") {
      var j = iOf(k);
      if (j < 0n || j >= BigInt(h.v.length)) die(rt, "SET: index out of bounds");
      h.v = h.v.slice(0, Number(j)) + String.fromCharCode(Number(asInt(v)) & 0xff) + h.v.slice(Number(j) + 1);
    } else if (h.t === "v") {
      var ixv = iOf(k);
      if (ixv < 0n || ixv >= BigInt(h.v.len)) die(rt, "index out of bounds");
      vwrite(h, Number(ixv), v);
    } else if (h.t === "ob") {
      var os = objOff(h.v, k);
      if (os < 0 || os >= h.v.esz / 8) die(rt, "SET: no such field");
      h.v.data[os >> 3] = v;
    } else {
      die(rt, "SET: unsupported handle");
    }
    rt.st.push(v);
  }
  // DEL: h k -> (dict only; pushes nothing)
  function op_del(rt) {
    var k = pop(rt, "del");
    var h = pop(rt, "del");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t !== "d") die(rt, "DEL: only dict supports del");
    mapDel(h.v, k);
  }
  // HAS: h k -> 0/1 (null handle -> 0)
  function op_has(rt) {
    var k = pop(rt, "has");
    var h = pop(rt, "has");
    if (h.t === "n") { rt.st.push(I(0)); return; }
    if (h.t === "d") { var v = mapGet(h.v, k); rt.st.push(I(v === null ? 0 : 1)); return; }
    if (h.t === "l") { var ix = iOf(k); rt.st.push(I(ix >= 0n && ix < BigInt(h.v.length) ? 1 : 0)); return; }
    if (h.t === "s") {
      if (k.t !== "s") die(rt, "expected string, got non-pointer cell");
      rt.st.push(I(k.v.length === 0 || h.v.indexOf(k.v) >= 0 ? 1 : 0));
      return;
    }
    if (h.t === "v") { var ixv = iOf(k); rt.st.push(I(ixv >= 0n && ixv < BigInt(h.v.len) ? 1 : 0)); return; }
    if (h.t === "ob") { var oh = objOff(h.v, k); rt.st.push(I(oh >= 0 && oh < h.v.esz / 8 ? 1 : 0)); return; }
    die(rt, "HAS: unsupported handle"); // a, t
  }
  // KEYS: dict -> slot-ordered key list; struct -> field-name list
  function op_keys(rt) {
    var h = pop(rt, "keys");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t === "ob") {
      var ent = nksBySid(h.v.sid);
      if (!ent) die(rt, "KEYS: unknown struct id");
      var names = [];
      for (var i = 0; i < ent.f.length; i++) names.push(S(ent.f[i][0]));
      rt.st.push(L(names));
      return;
    }
    if (h.t !== "d") die(rt, "KEYS: unsupported handle");
    rt.st.push(mapKeys(h.v));
  }
  // __NKS reflection table entry for a struct id (null if absent). The table
  // is emitted by gen_js at script scope; the bare identifier resolves through
  // the IIFE's scope chain at call time (browser and node module both work).
  function nksBySid(sid) {
    if (typeof __NKS === "undefined") return null;
    for (var i = 0; i < __NKS.length; i++) if (__NKS[i].sid === sid) return __NKS[i];
    return null;
  }
  // Field offset of k in struct object o: int key -> raw offset; string key
  // -> reflection-table lookup (sid must match o's sid, name must appear);
  // anything else, or a miss, is -1 (C uf_obj_off).
  function objOff(o, k) {
    if (k.t === "i" || k.t === "n") return Number(iOf(k));
    if (k.t !== "s") return -1;
    var ent = nksBySid(o.sid);
    if (!ent) return -1;
    for (var i = 0; i < ent.f.length; i++) if (ent.f[i][0] === k.v) return ent.f[i][1];
    return -1;
  }

  // =========================== vector / tensor ==========================
  // Numeric arrays (C HT_ARR=5/HT_TENSOR=6/HT_MAT=20) are { t:"v", v:{ kind:"arr"|"tensor"|"mat",
  // ety, len, data, rows } } cells. data is a JS Array: numbers for ety 1/3/21, bigints otherwise.
  // `rows` (C esz) is set only for "mat". ety is the RAW source type id (CAT compares it raw).
  function vZero(ety, n) { var d = new Array(n); var z = (ety === 1 || ety === 3 || ety === 21) ? 0 : 0n; for (var i = 0; i < n; i++) d[i] = z; return d; }
  function vFill(c, i, val, ety) { if (ety === 21) c.data[i] = truthy(val) ? 1 : 0; else if (ety === 3) c.data[i] = Number(iOf(val) & 0xffn); else if (ety === 1) c.data[i] = fOf(val); else c.data[i] = iOf(val); }
  function i64OfDouble(dv) { if (isNaN(dv) || dv >= 9223372036854775808 || dv <= -9223372036854775808) return IMIN; return BigInt(Math.trunc(dv)); }
  function elD(c, i) { var e = c.v.data[i]; return typeof e === "number" ? e : Number(e); }
  function elB(c, i) { var e = c.v.data[i]; return typeof e === "bigint" ? e : BigInt(e); }
  function putEl(c, i, dv) { var ety = c.v.ety; if (ety === 1) c.v.data[i] = dv; else if (ety === 21) c.v.data[i] = (dv !== 0 && !isNaN(dv)) ? 1 : 0; else if (ety === 3) c.v.data[i] = Number(i64OfDouble(dv) & 0xffn); else c.v.data[i] = i64OfDouble(dv); }
  function vread(c, ix) { var ety = c.v.ety, d = c.v.data[ix]; if (ety === 21) return B(d); if (ety === 1) return F(d); return I(d); }
  function vwrite(c, ix, val) { var ety = c.v.ety; if (ety === 1) c.v.data[ix] = fOf(val); else if (ety === 21) c.v.data[ix] = truthy(val) ? 1 : 0; else if (ety === 3) c.v.data[ix] = Number(iOf(val) & 0xffn); else c.v.data[ix] = iOf(val); }
  function vdims(c) { return c.v.rows + "x" + Math.floor(c.v.len / c.v.rows); }
  function vArrLike(proto, n) { return V(proto.v.kind, proto.v.ety, n, vZero(proto.v.ety, n), proto.v.kind === "mat" ? proto.v.rows : 0); }
  function opArrnLike(rt, kind) {
    var ty = asInt(pop(rt, kind));
    var top = pop(rt, kind);
    if (top.t === "l") {
      var d = top.v, r = V(kind, Number(ty), d.length, vZero(Number(ty), d.length), 0);
      for (var i = 0; i < d.length; i++) vFill(r.v, i, d[i], Number(ty));
      rt.st.push(r);
      return;
    }
    var len = iOf(top);
    if (len < 0n) die(rt, "negative length");
    rt.st.push(V(kind, Number(ty), Number(len), vZero(Number(ty), Number(len)), 0));
  }
  function op_arr(rt) { opArrnLike(rt, "arr"); }
  function op_tensor(rt) {
    var st = rt.st;
    if (st.length >= 2) {
      var shp = st[st.length - 2];
      if (shp.t === "l" && shp.v.length === 2) {
        var ty = asInt(st.pop()); st.pop();
        var rows = iOf(shp.v[0]), cols = iOf(shp.v[1]);
        if (rows < 0n || cols < 0n) die(rt, "tensor: negative shape");
        var rr = Number(rows), cc = Number(cols);
        if (rr === 0 || cc === 0) die(rt, "matrix: zero dimension");
        rt.st.push(V("mat", Number(ty), rr * cc, vZero(Number(ty), rr * cc), rr));
        return;
      }
      if (shp.t === "l" && shp.v.length >= 3) {
        var rows2 = iOf(shp.v[0]), cols2 = iOf(shp.v[1]);
        if (rows2 < 0n || cols2 < 0n) die(rt, "tensor: negative shape");
        if (shp.v.length === Number(rows2) * Number(cols2) + 2) {
          var ty2 = asInt(st.pop()); st.pop();
          var r2 = Number(rows2), c2 = Number(cols2);
          if (r2 === 0 || c2 === 0) die(rt, "matrix: zero dimension");
          var m = V("mat", Number(ty2), r2 * c2, vZero(Number(ty2), r2 * c2), r2);
          for (var q = 0; q < r2 * c2; q++) vFill(m.v, q, shp.v[q + 2], Number(ty2));
          rt.st.push(m);
          return;
        }
      }
    }
    opArrnLike(rt, "tensor"); // 1-D form; mismatched list shapes fall through to the value-copy quirk
  }
  function op_vget(rt) { var ix = iOf(pop(rt, "vget")); var h = vcheckV(rt, pop(rt, "vget")); if (ix < 0n || ix >= BigInt(h.v.len)) die(rt, "VGET: index out of bounds"); rt.st.push(vread(h, Number(ix))); }
  function op_vset(rt) { var val = pop(rt, "vset"); var ix = iOf(pop(rt, "vset")); var h = vcheckV(rt, pop(rt, "vset")); if (ix < 0n || ix >= BigInt(h.v.len)) die(rt, "VSET: index out of bounds"); vwrite(h, Number(ix), val); }
  function op_vsum(rt) { var h = vecNormV(rt, pop(rt, "vsum"), "vsum"); var s = 0; for (var i = 0; i < h.v.len; i++) s += elD(h, i); if (h.v.ety === 1) rt.st.push(F(s)); else rt.st.push(I(i64OfDouble(s))); }
  async function op_vfold(rt) {
    var f = pop(rt, "vfold");
    var acc = pop(rt, "vfold");
    var h = vecNormV(rt, pop(rt, "vfold"), "vfold");
    var fr = rt.lsp++;
    if (rt.lsp >= 64) die(rt, "loops nested too deep");
    for (var q = 0; q < h.v.len; q++) {
      rt.st.push(acc);
      if (h.v.ety === 1) rt.st.push(F(elD(h, q)));
      else rt.st.push(I(i64OfDouble(elD(h, q))));
      await preludeCall(rt, f.t === "a" ? f.v : f, 2);
      acc = rt.st.pop();
    }
    rt.lsp = fr;
    rt.st.push(acc);
  }
  // C handle checks: non-pointer cells (ints, floats, bytes, bools, null,
  // code) die "handle is null"; managed pointers of the wrong tag die with
  // the op-specific message.
  function vcheckV(rt, h) {
    if (h.t === "i" || h.t === "f" || h.t === "y" || h.t === "b" || h.t === "n" || h.t === "a") die(rt, "handle is null");
    if (h.t !== "v") die(rt, "vector op: not an arr");
    return h;
  }
  // ---------- bitmap (C HT_BITMAP=14): { t:"m", v:{ len, words } } ----------
  // words are 64-bit BigInts; len is the bit count.
  var BMASK64 = (1n << 64n) - 1n;
  function bmNew(n) {
    var w = (n + 63) / 64 | 0, ws = new Array(w);
    for (var i = 0; i < w; i++) ws[i] = 0n;
    return { t: "m", v: { len: n, words: ws } };
  }
  function bmBit(b, i) { return (b.words[i >> 6] >> BigInt(i & 63)) & 1n; }
  // ============ v15.1: uniform vector-op operand normalization ============
  // (C uf_vec_norm): a list becomes a typed array — int elements stay int
  // unless one forces float — a bitmap becomes the int array of its set-bit
  // indices, and a scalar/string operand becomes a one-element array by the
  // scalar numeric-coercion table. Elements with no numeric form die.
  function strNumClass(s) {
    var t = s.replace(/^\s+|\s+$/g, "");
    if (t === "") return { fl: 0, d: 0 };
    if (/^[+-]?[0-9]+$/.test(t)) {
      var b = BigInt(t);
      if (b <= 9223372036854775807n && b >= -9223372036854775808n) return { fl: 0, d: Number(b) };
    }
    return { fl: 1, d: strictDouble(t) };
  }
  function vecClassV(rt, x, idx) {
    for (var k = 0; k < 8; k++) {
      if (x.t === "f") return { fl: 1, d: x.v };
      if (x.t === "i" || x.t === "y" || x.t === "b" || x.t === "tm") return { fl: 0, d: Number(x.v) };
      if (x.t === "n") return { fl: 0, d: 0 };
      if (x.t === "s") return strNumClass(x.v);
      if (x.t === "l") {
        if (x.v.length === 0) return { fl: 0, d: 0 };
        if (x.v.length === 1) { x = x.v[0]; continue; }
      }
      break;
    }
    die(rt, "vector op: element " + idx + " of the list has no numeric form (convert explicitly, e.g. `list type array`)");
  }
  function vecNormV(rt, h, name) {
    if (h.t === "v") return h;
    if (h.t === "l") {
      var n = h.v.length, fl = 0, cl = new Array(n), i;
      for (i = 0; i < n; i++) { cl[i] = vecClassV(rt, h.v[i], i); if (cl[i].fl) fl = 1; }
      var d = new Array(n);
      if (fl) for (i = 0; i < n; i++) d[i] = cl[i].d;
      else for (i = 0; i < n; i++) d[i] = BigInt(Math.trunc(cl[i].d));
      return V("arr", fl ? 1 : 0, n, d, 0);
    }
    if (h.t === "m") {
      var ix = [];
      for (var q = 0; q < h.v.len; q++) if (bmBit(h.v, q)) ix.push(BigInt(q));
      return V("arr", 0, ix.length, ix, 0);
    }
    if (h.t === "f") return V("arr", 1, 1, [h.v], 0);
    if (h.t === "i" || h.t === "y" || h.t === "b" || h.t === "tm") return V("arr", 0, 1, [iOf(h)], 0);
    if (h.t === "n") return V("arr", 0, 1, [0n], 0);
    if (h.t === "s") {
      var c = strNumClass(h.v);
      return V("arr", c.fl, 1, [c.fl ? c.d : BigInt(Math.trunc(c.d))], 0);
    }
    if (h.t === "p" || h.t === "a") die(rt, "vector op: not an arr");
    die(rt, "vector op: not an arr (operand has no numeric form; convert explicitly, e.g. `x float array`)");
  }
  function vecCand(x) { return x.t === "v" || x.t === "l" || x.t === "m"; }
  // (C uf_bm_norm): bitmap ops coerce list/arr/tensor operands to bitmaps
  // via element truthiness
  function bmNormV(rt, h, name) {
    if (h.t === "m") return h.v;
    if (h.t === "l") {
      var b = bmNew(h.v.length);
      for (var i = 0; i < h.v.length; i++) if (truthy(h.v[i])) b.v.words[i >> 6] |= 1n << BigInt(i & 63);
      return b.v;
    }
    if (h.t === "v") {
      var b2 = bmNew(h.v.len);
      for (var j = 0; j < h.v.len; j++) { var e = elD(h, j); if (e !== 0 && !isNaN(e)) b2.v.words[j >> 6] |= 1n << BigInt(j & 63); }
      return b2.v;
    }
    die(rt, "bitmap op: not a bitmap");
  }
  // elementwise max/min (C UF_VEOP): both arrs, length mismatch dies
  function veMinMax(rt, isMax, name) {
    var h2 = pop(rt, name), h1 = pop(rt, name);
    var a = vecNormV(rt, h1, name), b = vecNormV(rt, h2, name);
    if (a.v.len !== b.v.len) die(rt, name + ": length mismatch");
    var n = a.v.len, r = vArrLike(a, n);
    if (a.v.ety === 1 && b.v.ety === 1 && a.v.kind !== "mat" && b.v.kind !== "mat") {
      var A = a.v.data, B = b.v.data, R = r.v.data;
      for (var i = 0; i < n; i++) R[i] = isMax ? (A[i] > B[i] ? A[i] : B[i]) : (A[i] < B[i] ? A[i] : B[i]);
    } else {
      for (var m = 0; m < n; m++)
        putEl(r, m, isMax
          ? (elD(a, m) > elD(b, m) ? elD(a, m) : elD(b, m))
          : (elD(a, m) < elD(b, m) ? elD(a, m) : elD(b, m)));
    }
    rt.st.push(r);
  }
  function op_vemax(rt) { veMinMax(rt, 1, "op_vemax"); }
  function op_vemin(rt) { veMinMax(rt, 0, "op_vemin"); }
  // comparisons (C UF_VCOP): arr scalar -> bitmap, scalar coerced to double
  function vcCmp(rt, fn, name) {
    var s = pop(rt, name), h = pop(rt, name);
    var a = vecNormV(rt, h, name);
    var n = a.v.len, r = bmNew(n), d = fOf(s);
    for (var i = 0; i < n; i++) if (fn(elD(a, i), d)) r.v.words[i >> 6] |= 1n << BigInt(i & 63);
    rt.st.push(r);
  }
  function op_veq(rt) { vcCmp(rt, function (x, d) { return x === d; }, "op_veq"); }
  function op_vlt(rt) { vcCmp(rt, function (x, d) { return x < d; }, "op_vlt"); }
  function op_vgt(rt) { vcCmp(rt, function (x, d) { return x > d; }, "op_vgt"); }
  function op_vge(rt) { vcCmp(rt, function (x, d) { return x >= d; }, "op_vge"); }
  function op_vle(rt) { vcCmp(rt, function (x, d) { return x <= d; }, "op_vle"); }
  function bmBinOp(rt, isAnd, op, tag) {
    var h2 = pop(rt, op), h1 = pop(rt, op);
    var a = bmNormV(rt, h1, op), b = bmNormV(rt, h2, op);
    if (a.len !== b.len) die(rt, tag + ": length mismatch");
    var w = (a.len + 63) / 64 | 0, r = bmNew(a.len);
    for (var i = 0; i < w; i++) r.v.words[i] = isAnd ? (a.words[i] & b.words[i]) : (a.words[i] | b.words[i]);
    rt.st.push(r);
  }
  function op_vand(rt) { bmBinOp(rt, 1, "op_vand", "VAND"); }
  function op_vor(rt) { bmBinOp(rt, 0, "op_vor", "VOR"); }
  function op_vnot(rt) {
    var h = pop(rt, "op_vnot");
    var a = bmNormV(rt, h, "op_vnot");
    var w = (a.len + 63) / 64 | 0, r = bmNew(a.len);
    for (var i = 0; i < w; i++) r.v.words[i] = ~a.words[i] & BMASK64;
    if (a.len % 64) r.v.words[w - 1] &= (1n << BigInt(a.len % 64)) - 1n;
    rt.st.push(r);
  }
  function op_vcount(rt) {
    var h = pop(rt, "op_vcount");
    var a = bmNormV(rt, h, "op_vcount");
    var n = 0n;
    for (var i = 0; i < a.words.length; i++) {
      var w = a.words[i];
      while (w > 0n) { w &= w - 1n; n++; }
    }
    rt.st.push(I(n));
  }
  // VGATHER: arr bitmap -> arr' (keep the set-bit elements)
  function op_vgather(rt) {
    var h2 = pop(rt, "VGATHER"), h1 = pop(rt, "VGATHER");
    var a = vecNormV(rt, h1, "VGATHER"), b = bmNormV(rt, h2, "VGATHER");
    if (a.v.len !== b.len) die(rt, "VGATHER: length mismatch");
    var n = 0n;
    for (var i = 0; i < a.v.len; i++) if (bmBit(b, i)) n++;
    var r = vArrLike(a, Number(n)), k = 0;
    for (i = 0; i < a.v.len; i++) if (bmBit(b, i)) putEl(r, k++, elD(a, i));
    rt.st.push(r);
  }
  // reductions (C op_vmean / op_vmin / op_vmax)
  function op_vmean(rt) {
    var h = pop(rt, "VMEAN");
    var a = vecNormV(rt, h, "VMEAN");
    if (!a.v.len) die(rt, "VMEAN: empty arr");
    var s = 0;
    for (var i = 0; i < a.v.len; i++) s += elD(a, i);
    rt.st.push(F(s / a.v.len));
  }
  function vMinMaxRed(rt, isMax, name) {
    var h = pop(rt, name);
    var a = vecNormV(rt, h, name);
    if (!a.v.len) die(rt, name + ": empty arr");
    var s = elD(a, 0);
    for (var i = 1; i < a.v.len; i++) { var d = elD(a, i); if (isMax ? d > s : d < s) s = d; }
    if (a.v.ety === 1) rt.st.push(F(s)); else rt.st.push(I(i64OfDouble(s)));
  }
  function op_vmin(rt) { vMinMaxRed(rt, 0, "VMIN"); }
  function op_vmax(rt) { vMinMaxRed(rt, 1, "VMAX"); }
  // VMAP: arr fn_addr -> arr' (element as the single argument; the result
  // element is the double of the return, stored through put_el)
  async function op_vmap(rt) {
    var f = pop(rt, "VMAP"), h = pop(rt, "VMAP");
    var a = vecNormV(rt, h, "VMAP");
    var r = vArrLike(a, a.v.len);
    var fr = rt.lsp++;
    if (rt.lsp >= 64) die(rt, "loops nested too deep");
    for (var i = 0; i < a.v.len; i++) {
      rt.st.push(a.v.ety === 1 ? F(elD(a, i)) : I(i64OfDouble(elD(a, i))));
      await preludeCall(rt, f.t === "a" ? f.v : f, 1);
      putEl(r, i, fOf(rt.st.pop()));
    }
    rt.lsp = fr;
    rt.st.push(r);
  }
  // VARGSORT: arr -> arr of indices, stable-mergesorted by element value (C
  // keeps the left index on ties; NaN compares as neither <= nor >, so a
  // NaN on the right pulls the right index forward — same as C)
  function op_vargsort(rt) {
    var h = pop(rt, "VARGSORT");
    var a = vecNormV(rt, h, "VARGSORT");
    var n = a.v.len, idx = new Array(n), tmp = new Array(n);
    for (var i = 0; i < n; i++) idx[i] = i;
    for (var w = 1; w < n; w <<= 1) {
      for (var lo = 0; lo < n; lo += 2 * w) {
        var mid = lo + w < n ? lo + w : n, hi = lo + 2 * w < n ? lo + 2 * w : n;
        var p = lo, q = mid, k = lo;
        while (p < mid && q < hi) {
          if (elD(a, idx[p]) <= elD(a, idx[q])) tmp[k++] = idx[p++];
          else tmp[k++] = idx[q++];
        }
        while (p < mid) tmp[k++] = idx[p++];
        while (q < hi) tmp[k++] = idx[q++];
      }
      var t = idx; idx = tmp; tmp = t;
    }
    var r = V("arr", 0, n, new Array(n), 0);
    for (i = 0; i < n; i++) r.v.data[i] = BigInt(idx[i]);
    rt.st.push(r);
  }
  // VSEARCHSORTED: sorted_arr val -> insertion index (first pos with el >= val)
  function op_vsearchsorted(rt) {
    var v = pop(rt, "VSEARCHSORTED"), h = pop(rt, "VSEARCHSORTED");
    var a = vecNormV(rt, h, "VSEARCHSORTED");
    var d = fOf(v), lo = 0, hi = a.v.len;
    while (lo < hi) { var mid = (lo + hi) >> 1; if (elD(a, mid) < d) lo = mid + 1; else hi = mid; }
    rt.st.push(I(BigInt(lo)));
  }
  // VWHERE: arr arr bitmap -> arr' (bit set -> first arr, else the second)
  function op_vwhere(rt) {
    var h3 = pop(rt, "VWHERE"), h2 = pop(rt, "VWHERE"), h1 = pop(rt, "VWHERE");
    var a = vecNormV(rt, h1, "VWHERE"), b = vecNormV(rt, h2, "VWHERE"), m = bmNormV(rt, h3, "VWHERE");
    if (a.v.len !== b.v.len || a.v.len !== m.len) die(rt, "VWHERE: length mismatch");
    var r = vArrLike(a, a.v.len);
    for (var i = 0; i < a.v.len; i++) putEl(r, i, bmBit(m, i) ? elD(a, i) : elD(b, i));
    rt.st.push(r);
  }
  // TRANSPOSE: mat -> mat' (rows/cols swapped)
  function op_transpose(rt) {
    var h = pop(rt, "transpose");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t !== "v" || h.v.kind !== "mat") die(rt, "transpose: not a matrix (build one with [rows cols] type tensor)");
    var r = h.v.rows, c = Math.floor(h.v.len / r);
    var t = V("mat", h.v.ety, r * c, vZero(h.v.ety, r * c), c);
    for (var i = 0; i < r; i++) for (var j = 0; j < c; j++) putEl(t, j * r + i, elD(h, i * c + j));
    rt.st.push(t);
  }
  function polyMatmul(rt, ha, hb) {
    var ra = ha.v.rows, ca = Math.floor(ha.v.len / ra), rb = hb.v.rows, cb = Math.floor(hb.v.len / rb);
    if (ca !== rb) die(rt, "mul: matmul inner dims mismatch (" + vdims(ha) + " \u00B7 " + vdims(hb) + ")");
    var ety = (ha.v.ety === 1 || hb.v.ety === 1) ? 1 : ((ha.v.ety === 3 && hb.v.ety === 3) ? 3 : 0);
    var r = V("mat", ety, ra * cb, vZero(ety, ra * cb), ra);
    var R = r.v.data;
    if (ety === 1) {
      for (var i = 0; i < ra; i++) for (var k = 0; k < ca; k++) {
        var aik = elD(ha, i * ca + k);
        if (aik === 0) continue;
        for (var j = 0; j < cb; j++) R[i * cb + j] += aik * elD(hb, k * cb + j);
      }
    } else if (ety === 3) {
      // C over-reads bytes as int64 here (latent C bug); intended per-byte semantics, documented divergence
      for (var i2 = 0; i2 < ra; i2++) for (var k2 = 0; k2 < ca; k2++) {
        var aik2 = elD(ha, i2 * ca + k2);
        if (aik2 === 0) continue;
        for (var j2 = 0; j2 < cb; j2++) R[i2 * cb + j2] = (R[i2 * cb + j2] + aik2 * elD(hb, k2 * cb + j2)) % 256;
      }
    } else {
      for (var i3 = 0; i3 < ra; i3++) for (var k3 = 0; k3 < ca; k3++) {
        var aik3 = elB(ha, i3 * ca + k3);
        if (aik3 === 0n) continue;
        for (var j3 = 0; j3 < cb; j3++) R[i3 * cb + j3] = wrap64(R[i3 * cb + j3] + aik3 * elB(hb, k3 * cb + j3));
      }
    }
    return r;
  }
  function polyMatvec(rt, m, v) {
    var r = m.v.rows, c = Math.floor(m.v.len / r);
    if (v.v.len !== c) die(rt, "mul: matvec dims mismatch (" + vdims(m) + " \u00B7 len " + v.v.len + ")");
    var ety = (m.v.ety === 1 || v.v.ety === 1) ? 1 : 0;
    var proto = ety === 1 ? (m.v.ety === 1 ? m : v) : (m.v.ety === 0 ? m : v);
    var res = V("arr", proto.v.ety, r, vZero(proto.v.ety, r), 0);
    for (var i = 0; i < r; i++) { var s = 0; for (var k = 0; k < c; k++) s += elD(m, i * c + k) * elD(v, k); putEl(res, i, s); }
    return res;
  }
  function polyVecmat(rt, v, m) {
    var r = m.v.rows, c = Math.floor(m.v.len / r);
    if (v.v.len !== r) die(rt, "mul: vecmat dims mismatch (len " + v.v.len + " \u00B7 " + vdims(m) + ")");
    var ety = (m.v.ety === 1 || v.v.ety === 1) ? 1 : 0;
    var proto = ety === 1 ? (m.v.ety === 1 ? m : v) : (m.v.ety === 0 ? m : v);
    var res = V("arr", proto.v.ety, c, vZero(proto.v.ety, c), 0);
    for (var j = 0; j < c; j++) { var s = 0; for (var i = 0; i < r; i++) s += elD(v, i) * elD(m, i * c + j); putEl(res, j, s); }
    return res;
  }
  function polyArith(rt, a, b, op, opn) {
    // v15.1: list/bitmap operands normalize to typed arrays at the boundary
    if (vecCand(a) && a.t !== "v") a = vecNormV(rt, a, opn);
    if (vecCand(b) && b.t !== "v") b = vecNormV(rt, b, opn);
    var ha = a.t === "v" ? a : null, hb = b.t === "v" ? b : null;
    if ((ha && ha.v.ety === 21) || (hb && hb.v.ety === 21)) die(rt, "arithmetic on bool arrays requires numeric elements");
    if (ha && hb) {
      if (op === 2 && ha.v.kind === "mat" && hb.v.kind === "mat") { rt.st.push(polyMatmul(rt, ha, hb)); return; }
      if (op === 2 && ha.v.kind === "mat") { rt.st.push(polyMatvec(rt, ha, hb)); return; }
      if (op === 2 && hb.v.kind === "mat") { rt.st.push(polyVecmat(rt, ha, hb)); return; }
      if (ha.v.len !== hb.v.len) die(rt, opn + ": length mismatch (" + ha.v.len + " vs " + hb.v.len + ")");
      if (ha.v.kind === "mat" && hb.v.kind === "mat" && ha.v.rows !== hb.v.rows) die(rt, opn + ": matrix shape mismatch (" + vdims(ha) + " vs " + vdims(hb) + ")");
      var r = vArrLike(ha, ha.v.len);
      if (ha.v.ety === 1 && hb.v.ety === 1) {
        var Ad = ha.v.data, Bd = hb.v.data, Rd = r.v.data, n = ha.v.len;
        if (op === 0) for (var i = 0; i < n; i++) Rd[i] = Ad[i] + Bd[i];
        else if (op === 1) for (var j = 0; j < n; j++) Rd[j] = Ad[j] - Bd[j];
        else if (op === 2) for (var k = 0; k < n; k++) Rd[k] = Ad[k] * Bd[k];
        else for (var q = 0; q < n; q++) { var yq = Bd[q]; if (yq === 0) die(rt, opn + ": zero divisor"); Rd[q] = Ad[q] / yq; }
      } else {
        if (op === 3) for (var p = 0; p < ha.v.len; p++) if (elD(hb, p) === 0) die(rt, opn + ": zero divisor");
        for (var m = 0; m < ha.v.len; m++) {
          var xm = elD(ha, m), ym = elD(hb, m);
          var vm = op === 0 ? xm + ym : op === 1 ? xm - ym : op === 2 ? xm * ym : xm / ym;
          putEl(r, m, vm);
        }
      }
      rt.st.push(r);
      return;
    }
    if (ha && !hb) {
      var n1 = ha.v.len, r1 = vArrLike(ha, n1);
      if (ha.v.ety === 1 && ha.v.kind !== "mat") {
        var a1d = ha.v.data, r1d = r1.v.data, d1 = fOf(b);
        if (op === 3 && d1 === 0) die(rt, "div: zero divisor");
        if (op === 0) for (var i1 = 0; i1 < n1; i1++) r1d[i1] = a1d[i1] + d1;
        else if (op === 1) for (var j1 = 0; j1 < n1; j1++) r1d[j1] = a1d[j1] - d1;
        else if (op === 2) for (var k1 = 0; k1 < n1; k1++) r1d[k1] = a1d[k1] * d1;
        else for (var q1 = 0; q1 < n1; q1++) r1d[q1] = a1d[q1] / d1;
        rt.st.push(r1);
        return;
      }
      if (ha.v.ety === 1 || b.t === "f") {
        var d2 = fOf(b);
        if (op === 3 && d2 === 0) die(rt, "div: zero divisor");
        for (var m1 = 0; m1 < n1; m1++) { var x1 = elD(ha, m1); putEl(r1, m1, op === 0 ? x1 + d2 : op === 1 ? x1 - d2 : op === 2 ? x1 * d2 : x1 / d2); }
      } else {
        var d3 = iOf(b);
        if (op === 3 && d3 === 0n) die(rt, "div: zero divisor");
        var nd3 = Number(d3);
        for (var p1 = 0; p1 < n1; p1++) { var x2 = elD(ha, p1); putEl(r1, p1, op === 0 ? x2 + nd3 : op === 1 ? x2 - nd3 : op === 2 ? x2 * nd3 : x2 / nd3); }
      }
      rt.st.push(r1);
      return;
    }
    if (hb && !ha) {
      var n2 = hb.v.len, r2 = vArrLike(hb, n2);
      if (hb.v.ety === 1 && hb.v.kind !== "mat") {
        var b2d = hb.v.data, r2d = r2.v.data, db = fOf(a);
        if (op === 3 && db === 0) die(rt, "div: zero divisor");
        if (op === 0) for (var i2 = 0; i2 < n2; i2++) r2d[i2] = db + b2d[i2];
        else if (op === 1) for (var j2 = 0; j2 < n2; j2++) r2d[j2] = db - b2d[j2];
        else if (op === 2) for (var k2 = 0; k2 < n2; k2++) r2d[k2] = db * b2d[k2];
        else for (var q2 = 0; q2 < n2; q2++) r2d[q2] = db / b2d[q2];
        rt.st.push(r2);
        return;
      }
      if (hb.v.ety === 1 || a.t === "f") {
        var d4 = fOf(a);
        if (op === 3 && d4 === 0) die(rt, "div: zero divisor");
        for (var m2 = 0; m2 < n2; m2++) { var y2b = elD(hb, m2); putEl(r2, m2, op === 0 ? d4 + y2b : op === 1 ? d4 - y2b : op === 2 ? d4 * y2b : d4 / y2b); }
      } else {
        var d5 = iOf(a);
        if (op === 3 && d5 === 0n) die(rt, "div: zero divisor");
        var nd5 = Number(d5);
        for (var p2 = 0; p2 < n2; p2++) { var y3b = elD(hb, p2); putEl(r2, p2, op === 0 ? nd5 + y3b : op === 1 ? nd5 - y3b : op === 2 ? nd5 * y3b : nd5 / y3b); }
      }
      rt.st.push(r2);
      return;
    }
    die(rt, "poly arith: no array operand");
  }
  // CAT: a b -> a' (list concat / str concat)
  function op_cat(rt) {
    var p = pop2(rt, "cat"); var a = p[0], b = p[1];
    if (a.t === "l" || b.t === "l") {
      if (a.t !== "l" || b.t !== "l") die(rt, "CAT: list/str mismatch");
      rt.st.push(L(a.v.concat(b.v)));
      return;
    }
    if (a.t === "v" || b.t === "v") {
      if (a.t !== "v" || b.t !== "v" || (a.v.kind !== "arr" && a.v.kind !== "tensor") || (b.v.kind !== "arr" && b.v.kind !== "tensor")) die(rt, "CAT: arr/str mismatch");
      if (a.v.ety !== b.v.ety) die(rt, "CAT: arr element-type mismatch");
      rt.st.push(V("arr", a.v.ety, a.v.len + b.v.len, a.v.data.concat(b.v.data), 0));
      return;
    }
    if (a.t === "s" && b.t === "s") { rt.st.push(S(a.v + b.v)); return; }
    die(rt, "expected string, got non-pointer cell");
  }
  // SLICE: seq a b -> seq' (Python clamp semantics, Python slice bounds)
  function op_slice(rt) {
    var b = pop(rt, "slice"); // stop
    var a = pop(rt, "slice"); // start
    var st = pop(rt, "slice");
    if (st.t === "n") die(rt, "handle is null");
    if (st.t === "v") {
      if (st.v.kind === "mat") die(rt, "SLICE: unsupported handle");
      var nv = BigInt(st.v.len);
      var iv = iOf(a), jv = iOf(b);
      if (iv < 0n) iv += nv; if (jv < 0n) jv += nv;
      if (iv < 0n) iv = 0n; if (jv < 0n) jv = 0n;
      if (iv > nv) iv = nv; if (jv > nv) jv = nv;
      if (jv < iv) jv = iv;
      var dd = st.v.data.slice(Number(iv), Number(jv));
      rt.st.push(V(st.v.kind, st.v.ety, dd.length, dd, 0));
      return;
    }
    if (st.t !== "s" && st.t !== "l") die(rt, "SLICE: unsupported handle");
    var n = BigInt(st.v.length);
    var i = iOf(a), j = iOf(b);
    if (i < 0n) i += n; if (j < 0n) j += n;
    if (i < 0n) i = 0n; if (j < 0n) j = 0n;
    if (i > n) i = n; if (j > n) j = n;
    if (j < i) j = i;
    if (st.t === "s") rt.st.push(S(st.v.slice(Number(i), Number(j))));
    else rt.st.push(L(st.v.slice(Number(i), Number(j))));
  }
  // PUSH: h v -> h' (C returns a new grown handle; JS mutates and re-pushes
  // the same object — documented divergence)
  function op_push(rt) {
    var v = pop(rt, "push");
    var h = pop(rt, "push");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t !== "l") die(rt, "PUSH: not a list");
    h.v.push(v);
    rt.st.push(h);
  }
  // LPOP: h -> last element
  function op_lpop(rt) {
    var h = pop(rt, "lpop");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t !== "l") die(rt, "POP: not a list");
    if (h.v.length === 0) die(rt, "POP: empty list");
    rt.st.push(h.v.pop());
  }
  // RANGE: start stop -> list of ints [start, stop)
  function op_range(rt) {
    var stop = iOf(pop(rt, "range"));
    var start = iOf(pop(rt, "range"));
    var out = [];
    for (var k = start; k < stop; k++) out.push(I(k));
    rt.st.push(L(out));
  }

  // ============================= strings / glob ===========================
  // C's uf_sptr die for a non-str operand
  function strOf(rt, x) {
    if (x.t !== "s") die(rt, "expected string, got non-pointer cell");
    return x.v;
  }
  // Embedded matcher (C's WIN32 uf_glob_match, ported 1:1). Documented
  // divergence: Linux native uses fnmatch(3).
  function globMatch(pat, s) {
    return globRec(pat, s, 0, 0);
  }
  function globRec(pat, s, pi, si) {
    var pl = pat.length, sl = s.length;
    while (pi < pl) {
      var c = pat.charCodeAt(pi);
      if (c === 42) {
        while (pi < pl && pat.charCodeAt(pi) === 42) pi++;
        if (pi >= pl) return true;
        for (var t = si; ; t++) { if (globRec(pat, s, pi, t)) return true; if (t >= sl) break; }
        return false;
      }
      if (c === 63) { if (si >= sl) return false; pi++; si++; continue; }
      if (c === 91) {
        var cl = rxClsFind(pat, pi + 1);
        if (cl >= 0) {
          if (si >= sl) return false;
          var neg = 0, p = pi + 1, ok = 0, cc = s.charCodeAt(si);
          if (p < cl && pat.charCodeAt(p) === 33) { neg = 1; p++; } // '!' not '^'
          while (p < cl) {
            var lo = pat.charCodeAt(p++);
            if (p < cl && pat.charCodeAt(p) === 45 && p + 1 < cl) { p++; var hi = pat.charCodeAt(p++); if (cc >= lo && cc <= hi) ok = 1; }
            else if (cc === lo) ok = 1;
          }
          if (neg) ok = !ok;
          if (!ok) return false;
          pi = cl + 1; si++; continue;
        }
      }
      if (c === 92 && pi + 1 < pl) pi++;
      if (pat.charCodeAt(pi) !== s.charCodeAt(si)) return false;
      if (si >= sl) return false;
      pi++; si++;
    }
    return si >= sl;
  }
  function op_glob(rt) {
    var pat = strOf(rt, pop(rt, "glob"));
    var st = strOf(rt, pop(rt, "glob"));
    rt.st.push(I(globMatch(pat, st) ? 1 : 0));
  }
  // SPLIT: str sep -> list (fields; last = remainder)
  function op_split(rt) {
    var sep = strOf(rt, pop(rt, "split"));
    var st = strOf(rt, pop(rt, "split"));
    if (sep.length === 0) die(rt, "SPLIT: empty separator");
    var out = [], cur = 0;
    for (;;) {
      var m = st.indexOf(sep, cur);
      if (m < 0) break;
      out.push(S(st.slice(cur, m)));
      cur = m + sep.length;
    }
    out.push(S(st.slice(cur)));
    rt.st.push(L(out));
  }
  // JOIN: list sep -> str
  function op_join(rt) {
    var sep = strOf(rt, pop(rt, "join"));
    var h = pop(rt, "join");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t !== "l") die(rt, "JOIN: not a list");
    var out = "";
    for (var i = 0; i < h.v.length; i++) {
      if (i) out += sep;
      var e = h.v[i];
      if (e.t !== "s") die(rt, "expected string, got non-pointer cell");
      out += e.v;
    }
    rt.st.push(S(out));
  }
  // FIND: str sub -> byte index or -1
  function op_find(rt) {
    var sub = strOf(rt, pop(rt, "find"));
    var st = strOf(rt, pop(rt, "find"));
    rt.st.push(I(BigInt(st.indexOf(sub))));
  }
  // REPL: str old new -> str' (literal, replace all, left to right)
  function op_repl(rt) {
    var nw = strOf(rt, pop(rt, "repl"));
    var old = strOf(rt, pop(rt, "repl"));
    var st = strOf(rt, pop(rt, "repl"));
    if (old.length === 0) die(rt, "REPL: empty pattern");
    var out = "", cur = 0;
    for (;;) {
      var m = st.indexOf(old, cur);
      if (m < 0) break;
      out += st.slice(cur, m) + nw;
      cur = m + old.length;
    }
    out += st.slice(cur);
    rt.st.push(S(out));
  }
  function op_trim(rt) {
    var st = strOf(rt, pop(rt, "trim"));
    var i = 0, n = st.length;
    while (n > i && isWS(st.charCodeAt(i))) i++;
    while (n > i && isWS(st.charCodeAt(n - 1))) n--;
    rt.st.push(S(st.slice(i, n)));
  }
  function op_up(rt) {
    var st = strOf(rt, pop(rt, "up"));
    var o = "";
    for (var i = 0; i < st.length; i++) { var c = st.charCodeAt(i); o += (c >= 97 && c <= 122) ? String.fromCharCode(c - 32) : st[i]; }
    rt.st.push(S(o));
  }
  function op_down(rt) {
    var st = strOf(rt, pop(rt, "down"));
    var o = "";
    for (var i = 0; i < st.length; i++) { var c = st.charCodeAt(i); o += (c >= 65 && c <= 90) ? String.fromCharCode(c + 32) : st[i]; }
    rt.st.push(S(o));
  }
  function op_starts(rt) {
    var af = strOf(rt, pop(rt, "starts"));
    var st = strOf(rt, pop(rt, "starts"));
    rt.st.push(I(st.indexOf(af) === 0 ? 1 : 0));
  }
  function op_ends(rt) {
    var af = strOf(rt, pop(rt, "ends"));
    var st = strOf(rt, pop(rt, "ends"));
    rt.st.push(I(af.length <= st.length && st.slice(st.length - af.length) === af ? 1 : 0));
  }
  // ATOI/ATOF/ITOA/FTOA
  function op_atoi(rt) { rt.st.push(I(parseInt10(strOf(rt, pop(rt, "atoi"))))); }
  function op_atof(rt) { rt.st.push(F(parseDouble(strOf(rt, pop(rt, "atof"))))); }
  function op_itoa(rt) { rt.st.push(S(iOf(pop(rt, "itoa")).toString())); }
  function op_ftoa(rt) { rt.st.push(S(printFloatG(fOf(pop(rt, "ftoa")), 6, "g"))); }

  // ================================ regex =================================
  // 1:1 port of the C embedded matcher. No JS RegExp. Strings are byte
  // strings; all positions are indices. caps: 10 [start, end] pairs,
  // -1 = not captured.
  function rxClsFind(pat, p) {
    if (p < pat.length && pat.charCodeAt(p) === 94) p++;
    if (p < pat.length && pat.charCodeAt(p) === 93) p++;
    while (p < pat.length) {
      var c = pat.charCodeAt(p);
      if (c === 92 && p + 1 < pat.length) { p += 2; continue; }
      if (c === 93) return p;
      p++;
    }
    return -1;
  }
  function rxClsIn(pat, cs, ce, c) {
    var neg = false, p = cs, ok = false, first = 1;
    if (p < ce && pat.charCodeAt(p) === 94) { neg = true; p++; }
    while (p < ce) {
      var lo;
      if (pat.charCodeAt(p) === 92 && p + 1 < ce) { lo = pat.charCodeAt(p + 1); p += 2; }
      else lo = pat.charCodeAt(p++);
      if (first && lo === 93) { if (c === 93) ok = true; first = 0; continue; }
      first = 0;
      if (p < ce && pat.charCodeAt(p) === 45 && p + 1 < ce) {
        p++;
        var hi;
        if (pat.charCodeAt(p) === 92 && p + 1 < ce) { hi = pat.charCodeAt(p + 1); p += 2; }
        else hi = pat.charCodeAt(p++);
        if (c >= lo && c <= hi) ok = true;
      } else if (c === lo) ok = true;
    }
    return neg ? !ok : ok;
  }
  function rxGroupIndex(pat, p0, p1) {
    var n = 0, q = p0;
    while (q < p1) {
      if (pat.charCodeAt(q) === 92 && q + 1 < pat.length) { q += 2; continue; }
      if (pat.charCodeAt(q) === 91) { var cl = rxClsFind(pat, q + 1); if (cl >= 0) { q = cl + 1; continue; } }
      if (pat.charCodeAt(q) === 40) n++;
      q++;
    }
    return n;
  }
  function rxParseAtom(pat, p, rt) {
    var c = pat.charCodeAt(p);
    if (c === 92) {
      if (p + 1 >= pat.length) die(rt, "MATCH: trailing backslash");
      return { a: { type: 0, ch: pat.charCodeAt(p + 1) }, q: p + 2 };
    }
    if (c === 46) return { a: { type: 1 }, q: p + 1 };
    if (c === 91) {
      var cl = rxClsFind(pat, p + 1);
      if (cl < 0) die(rt, "MATCH: unbalanced [");
      return { a: { type: 2, cs: p + 1, ce: cl }, q: cl + 1 };
    }
    if (c === 40) {
      var depth = 1, q = p + 1;
      while (q < pat.length && depth) {
        var cq = pat.charCodeAt(q);
        if (cq === 92 && q + 1 < pat.length) { q += 2; continue; }
        if (cq === 91) { var cl2 = rxClsFind(pat, q + 1); if (cl2 >= 0) { q = cl2 + 1; continue; } q++; continue; }
        if (cq === 40) depth++;
        else if (cq === 41) depth--;
        q++;
      }
      if (depth) die(rt, "MATCH: unbalanced (");
      var cap = rxGroupIndex(pat, 0, p) + 1;
      if (cap > 9) die(rt, "MATCH: more than 9 groups");
      return { a: { type: 3, gs: p + 1, ge: q - 1, cap: cap }, q: q };
    }
    return { a: { type: 0, ch: c }, q: p + 1 };
  }
  function rxAtom1(pat, subj, a, si, caps, rt) {
    switch (a.type) {
      case 0: return (si < subj.length && subj.charCodeAt(si) === a.ch) ? si + 1 : -1;
      case 1: return si < subj.length ? si + 1 : -1;
      case 2: return (si < subj.length && rxClsIn(pat, a.cs, a.ce, subj.charCodeAt(si))) ? si + 1 : -1;
      case 3: {
        var alt = a.gs;
        for (;;) {
          var ae = alt, depth = 0;
          while (ae < a.ge) {
            var cae = pat.charCodeAt(ae);
            if (cae === 92 && ae + 1 < a.ge) { ae += 2; continue; }
            if (cae === 91) { var cl = rxClsFind(pat, ae + 1); if (cl >= 0 && cl < a.ge) { ae = cl + 1; continue; } }
            if (cae === 40) depth++;
            else if (cae === 41) depth--;
            else if (cae === 124 && depth === 0) break;
            ae++;
          }
          var r = rxSeq(pat, subj, alt, ae, si, caps, rt);
          if (r >= 0) { caps[a.cap][0] = si; caps[a.cap][1] = r; return r; }
          if (ae >= a.ge) return -1;
          alt = ae + 1;
        }
      }
    }
    return -1;
  }
  function rxSeq(pat, subj, p, pend, si, caps, rt) {
    if (p >= pend) return si;
    if (pat.charCodeAt(p) === 36 && p + 1 === pend) return si === subj.length ? si : -1;
    var pr = rxParseAtom(pat, p, rt);
    var a = pr.a, q = pr.q;
    var min = 1, max = 1;
    if (q < pend) {
      var tq = pat.charCodeAt(q);
      if (tq === 42 || tq === 43 || tq === 63) {
        q++;
        if (tq === 42) { min = 0; max = 1 << 30; }
        else if (tq === 43) { min = 1; max = 1 << 30; }
        else { min = 0; max = 1; }
      }
    }
    if (max === 1) {
      var r1 = rxAtom1(pat, subj, a, si, caps, rt);
      if (r1 >= 0) { var e = rxSeq(pat, subj, q, pend, r1, caps, rt); if (e >= 0) return e; }
      if (min === 0) return rxSeq(pat, subj, q, pend, si, caps, rt);
      return -1;
    }
    var v = [si];
    while (v.length - 1 < max) {
      var r = rxAtom1(pat, subj, a, v[v.length - 1], caps, rt);
      if (r < 0 || r === v[v.length - 1]) break;
      v.push(r);
    }
    for (var k = v.length - 1; k >= min; k--) {
      var e2 = rxSeq(pat, subj, q, pend, v[k], caps, rt);
      if (e2 >= 0) return e2;
    }
    return -1;
  }
  function newCaps() { var c = new Array(10); for (var i = 0; i < 10; i++) c[i] = [-1, -1]; return c; }
  function rxExec(pat, subj, start, caps, rt) {
    var alt = 0;
    for (;;) {
      var ae = alt, depth = 0;
      while (ae < pat.length) {
        var ca = pat.charCodeAt(ae);
        if (ca === 92 && ae + 1 < pat.length) { ae += 2; continue; }
        if (ca === 91) { var cl = rxClsFind(pat, ae + 1); if (cl >= 0) { ae = cl + 1; continue; } }
        if (ca === 40) depth++;
        else if (ca === 41) depth--;
        else if (ca === 124 && depth === 0) break;
        ae++;
      }
      var p0 = alt, anch = false;
      if (p0 < ae && pat.charCodeAt(p0) === 94) { anch = true; p0++; }
      var pos = start;
      for (;;) {
        for (var i = 0; i < 10; i++) { caps[i][0] = -1; caps[i][1] = -1; }
        var r = rxSeq(pat, subj, p0, ae, pos, caps, rt);
        if (r >= 0) { caps[0][0] = pos; caps[0][1] = r; return 1; }
        if (anch || pos >= subj.length) break;
        pos++;
      }
      if (ae >= pat.length) return 0;
      alt = ae + 1;
    }
  }
  // MATCH: str pat -> [ [whole, g1..gn], 1|0 ]
  function op_match(rt) {
    var pat = strOf(rt, pop(rt, "match"));
    var st = strOf(rt, pop(rt, "match"));
    var ntotal = rxGroupIndex(pat, 0, pat.length);
    var caps = newCaps();
    if (rxExec(pat, st, 0, caps, rt)) {
      var g = new Array(ntotal + 1);
      for (var j = 0; j <= ntotal; j++) g[j] = caps[j][0] < 0 ? S("") : S(st.slice(caps[j][0], caps[j][1]));
      rt.st.push(L([L(g), I(1)]));
    } else {
      rt.st.push(L([L([]), I(0)]));
    }
  }
  // REPLACE: str pat repl -> str' (all matches; \1..\9, \\)
  function op_replace(rt) {
    var repl = strOf(rt, pop(rt, "replace"));
    var pat = strOf(rt, pop(rt, "replace"));
    var st = strOf(rt, pop(rt, "replace"));
    var caps = newCaps();
    var out = "", cur = 0;
    while (rxExec(pat, st, cur, caps, rt)) {
      var s0 = caps[0][0], e0 = caps[0][1];
      out += st.slice(cur, s0);
      for (var r = 0; r < repl.length; r++) {
        if (repl.charCodeAt(r) === 92 && r + 1 < repl.length) {
          var nx = repl.charCodeAt(r + 1);
          if (nx >= 49 && nx <= 57) { var g = nx - 48; if (caps[g][0] >= 0) out += st.slice(caps[g][0], caps[g][1]); }
          else out += repl[r + 1];
          r++;
        } else out += repl[r];
      }
      if (e0 === s0) { if (cur >= st.length) break; out += st[cur]; cur++; }
      else cur = e0;
    }
    out += st.slice(cur);
    rt.st.push(S(out));
  }
  // RSPLIT: str pat -> list (zero-width match consumes one char)
  function op_rsplit(rt) {
    var pat = strOf(rt, pop(rt, "rsplit"));
    var st = strOf(rt, pop(rt, "rsplit"));
    var caps = newCaps();
    var out = [], cur = 0;
    while (rxExec(pat, st, cur, caps, rt)) {
      var s0 = caps[0][0], e0 = caps[0][1];
      if (e0 === s0) { if (cur >= st.length) break; cur++; continue; }
      out.push(S(st.slice(cur, s0)));
      cur = e0;
    }
    out.push(S(st.slice(cur)));
    rt.st.push(L(out));
  }

  // ============================ iterators (ops) ===========================
  // ITER: list/str/dict/arr/bitmap -> iter; anything else dies (C: ITER: not iterable)
  function op_iter(rt) {
    var h = pop(rt, "iter");
    if (h.t === "l") { rt.st.push(itList(h.v)); return; }
    if (h.t === "v") { rt.st.push(itArr(h)); return; }
    if (h.t === "m") { rt.st.push(itBm(h)); return; }
    if (h.t === "s") { rt.st.push(itStr(h.v)); return; }
    if (h.t === "d") { rt.st.push(itDict(h.v)); return; }
    die(rt, "ITER: not iterable");
  }
  // NEXT: iter -> [value 1] or [0 0]
  async function op_next(rt) {
    var h = pop(rt, "next");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t !== "t") die(rt, "NEXT: not an iter");
    var c = await itStep(h, rt);
    rt.st.push(c === null ? L([I(0), I(0)]) : L([c, I(1)]));
  }
  // COLLECT: iter -> list
  async function op_collect(rt) {
    var h = pop(rt, "collect");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t !== "t") die(rt, "COLLECT: not an iter");
    var out = [];
    for (;;) { var c = await itStep(h, rt); if (c === null) break; out.push(c); }
    rt.st.push(L(out));
  }
  // IMAP/IFILTER: fn iter -> iter (C checks the iter, not the function)
  function op_imap(rt) {
    var f = pop(rt, "imap"), h = pop(rt, "imap");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t !== "t") die(rt, "IMAP: not an iter");
    rt.st.push(itMap(h, f.t === "a" ? f.v : f));
  }
  function op_ifilter(rt) {
    var f = pop(rt, "ifilter"), h = pop(rt, "ifilter");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t !== "t") die(rt, "IFILTER: not an iter");
    rt.st.push(itFilter(h, f.t === "a" ? f.v : f));
  }

  // ===================== sorting (verbatim uf_msort port) ==================
  function msortR(rt, v, tmp, lo, hi) {
    if (hi - lo < 2) return;
    var mid = (lo + hi) >> 1;
    msortR(rt, v, tmp, lo, mid);
    msortR(rt, v, tmp, mid, hi);
    var i = lo, j = mid, k = lo;
    while (i < mid && j < hi) {
      var r = cmp(v[i], v[j]);
      if (!r.ok) die(rt, "SORT: incomparable element types");
      if (r.c <= 0) tmp[k++] = v[i++]; else tmp[k++] = v[j++];
    }
    while (i < mid) tmp[k++] = v[i++];
    while (j < hi) tmp[k++] = v[j++];
    for (i = lo; i < hi; i++) v[i] = tmp[i];
  }
  function msortTop(rt, arr) {
    if (arr.length > 1) { var tmp = new Array(arr.length); msortR(rt, arr, tmp, 0, arr.length); }
  }
  // SORT: seq -> fresh sorted list (stable; mutates the materialized array)
  async function op_sort(rt) {
    var h = pop(rt, "sort");
    var arr = await materialize(h, rt);
    msortTop(rt, arr);
    var out = new Array(arr.length);
    for (var i = 0; i < arr.length; i++) out[i] = arr[i];
    rt.st.push(L(out));
  }
  // SORTKEYS: dict -> sorted key list
  function op_sortkeys(rt) {
    var h = pop(rt, "sortkeys");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t !== "d") die(rt, "SORTKEYS: not a dict");
    var m = h.v, arr = new Array(m.len), n2 = 0;
    for (var i = 0; i < m.cap; i++) if (m.st[i] === 1) arr[n2++] = m.k[i];
    msortTop(rt, arr);
    rt.st.push(L(arr));
  }
  // TOPN: dict n -> list of [key value] pairs, value desc, ties by key asc
  function op_topn(rt) {
    var n = iOf(pop(rt, "topn"));
    var h = pop(rt, "topn");
    if (h.t === "n") die(rt, "handle is null");
    if (h.t !== "d") die(rt, "TOPN: not a dict");
    if (n < 0n) n = 0n;
    var m = h.v, pairs = [];
    for (var i = 0; i < m.cap; i++) if (m.st[i] === 1) pairs.push(L([m.k[i], m.v[i]]));
    var plen = pairs.length, limit = Number(n < BigInt(plen) ? n : BigInt(plen)), result = [];
    for (var rank = 0; rank < limit && plen > 0; rank++) {
      var best = 0, bestv = fOf(pairs[0].v[1]);
      for (var j = 1; j < plen; j++) {
        var v = fOf(pairs[j].v[1]);
        var r = cmp(pairs[j].v[0], pairs[best].v[0]);
        if (v > bestv || (v === bestv && r.ok && r.c < 0)) { bestv = v; best = j; }
      }
      result.push(pairs[best]);
      pairs[best] = pairs[plen - 1];
      plen--;
    }
    rt.st.push(L(result));
  }
  // RANGEFOLD: count init fn -> scalar (fold over range 0..count)
  async function op_rangefold(rt) {
    var f = pop(rt, "rangefold");
    var acc = pop(rt, "rangefold");
    var cnt = iOf(pop(rt, "rangefold"));
    var fr = rt.lsp++;
    if (rt.lsp >= 64) die(rt, "loops nested too deep");
    for (var k = 0n; k < cnt; k++) {
      rt.st.push(acc);
      rt.st.push(I(k));
      await preludeCall(rt, f.t === "a" ? f.v : f, 2);
      acc = rt.st.pop();
    }
    rt.lsp = fr;
    rt.st.push(acc);
  }
  // FILTER/SOME/EVERY: fn seq
  async function op_filter(rt) {
    var f = pop(rt, "filter"), h = pop(rt, "filter");
    var s = await materialize(h, rt), r = [];
    for (var i = 0; i < s.length; i++) {
      rt.st.push(s[i]);
      await preludeCall(rt, f.t === "a" ? f.v : f, 1);
      if (truthy(rt.st.pop())) r.push(s[i]);
    }
    rt.st.push(L(r));
  }
  async function op_some(rt) {
    var f = pop(rt, "some"), h = pop(rt, "some");
    var s = await materialize(h, rt), r = 0;
    for (var i = 0; i < s.length; i++) {
      rt.st.push(s[i]);
      await preludeCall(rt, f.t === "a" ? f.v : f, 1);
      if (truthy(rt.st.pop())) { r = 1; break; }
    }
    rt.st.push(I(r));
  }
  async function op_every(rt) {
    var f = pop(rt, "every"), h = pop(rt, "every");
    var s = await materialize(h, rt), r = 1;
    for (var i = 0; i < s.length; i++) {
      rt.st.push(s[i]);
      await preludeCall(rt, f.t === "a" ? f.v : f, 1);
      if (!truthy(rt.st.pop())) { r = 0; break; }
    }
    rt.st.push(I(r));
  }
  // UNIQUE: seq -> list of distinct elements, first-occurrence order
  async function op_unique(rt) {
    var h = pop(rt, "unique");
    var s = await materialize(h, rt), seen = mapNew(), out = [];
    for (var i = 0; i < s.length; i++) {
      if (mapGet(seen, s[i]) === null) { mapPut(seen, s[i], I(1)); out.push(s[i]); }
    }
    rt.st.push(L(out));
  }
  // FLAT: seq -> list (list elements spliced in, others pass through)
  async function op_flat(rt) {
    var h = pop(rt, "flat");
    var s = await materialize(h, rt), out = [];
    for (var i = 0; i < s.length; i++) {
      var e = s[i];
      if (e.t === "l") { for (var j = 0; j < e.v.length; j++) out.push(e.v[j]); }
      else out.push(e);
    }
    rt.st.push(L(out));
  }

  // ============================== JSON ==================================
  // 1:1 port of the C embedded JSON. Byte/charCode oriented. Quirks kept:
  // true/false/null are case-sensitive PREFIX literals ("trueXYZ" parses
  // true, then dies "trailing garbage"); \u00XX keeps the low ASCII byte,
  // any other \uXXXX becomes '?'; numbers that strtoll can view as int
  // (hex/inf/nan included) become INT cells via the base-10 prefix view.
  function jWs(j) { var s = j.s; while (j.i < s.length && isWS(s.charCodeAt(j.i))) j.i++; }
  function jStr(j, rt) {
    var s = j.s;
    j.i++; // opening quote
    var out = "";
    for (;;) {
      var c = j.i < s.length ? s.charCodeAt(j.i++) : 0;
      if (c === 0) die(rt, "JSON: unterminated string");
      if (c === 34) break;
      if (c === 92) {
        var e = j.i < s.length ? s.charCodeAt(j.i++) : 0;
        if (e === 110) out += "\n";
        else if (e === 116) out += "\t";
        else if (e === 114) out += "\r";
        else if (e === 98) out += "\b";
        else if (e === 102) out += "\f";
        else if (e === 47) out += "/";
        else if (e === 92) out += "\\";
        else if (e === 34) out += "\"";
        else if (e === 117) {
          if (s.charCodeAt(j.i) === 48 && s.charCodeAt(j.i + 1) === 48) {
            var ch = parseInt(s.slice(j.i + 2, j.i + 4), 16);
            if (isNaN(ch)) ch = 0;
            out += String.fromCharCode(ch & 0xff);
          } else out += "?";
          j.i += 4;
        }
        else die(rt, "JSON: bad escape");
      } else out += String.fromCharCode(c);
    }
    return out;
  }
  function jParse(j, rt) {
    jWs(j);
    var s = j.s, c = j.i < s.length ? s.charCodeAt(j.i) : 0;
    if (c === 123) {
      j.i++;
      var m = mapNew();
      jWs(j);
      if (j.i < s.length && s.charCodeAt(j.i) === 125) { j.i++; rt.st.push(D(m)); return; }
      for (;;) {
        jWs(j);
        if (j.i >= s.length || s.charCodeAt(j.i) !== 34) die(rt, "JSON: object key must be a string");
        var k = S(jStr(j, rt));
        jWs(j);
        if (j.i >= s.length || s.charCodeAt(j.i) !== 58) die(rt, "JSON: expected ':'");
        j.i++;
        jParse(j, rt);
        mapPut(m, k, rt.st.pop());
        jWs(j);
        var cc = j.i < s.length ? s.charCodeAt(j.i) : 0;
        if (cc === 44) { j.i++; continue; }
        if (cc === 125) { j.i++; break; }
        die(rt, "JSON: expected ',' or '}'");
      }
      rt.st.push(D(m));
      return;
    }
    if (c === 91) {
      j.i++;
      var a = [];
      jWs(j);
      if (j.i < s.length && s.charCodeAt(j.i) === 93) { j.i++; rt.st.push(L(a)); return; }
      for (;;) {
        jParse(j, rt);
        a.push(rt.st.pop());
        jWs(j);
        var cc2 = j.i < s.length ? s.charCodeAt(j.i) : 0;
        if (cc2 === 44) { j.i++; continue; }
        if (cc2 === 93) { j.i++; break; }
        die(rt, "JSON: expected ',' or ']'");
      }
      rt.st.push(L(a));
      return;
    }
    if (c === 34) { rt.st.push(S(jStr(j, rt))); return; }
    if (s.startsWith("true", j.i)) { j.i += 4; rt.st.push(B(1)); return; }
    if (s.startsWith("false", j.i)) { j.i += 5; rt.st.push(B(0)); return; }
    if (s.startsWith("null", j.i)) { j.i += 4; rt.st.push(N()); return; }
    if (c === 45 || (c >= 48 && c <= 57)) {
      var si = j.i;
      var r = scanNum(s, si);
      var consumed = r.e - si;
      // C: strtod endptr == start => "JSON: bad number" (covers lone '-')
      if (consumed === 0 || (c === 45 && consumed === 1)) die(rt, "JSON: bad number");
      var tok = s.slice(si, r.e);
      j.i = r.e;
      var isint = tok.indexOf(".") < 0 && tok.indexOf("e") < 0 && tok.indexOf("E") < 0;
      if (isint) rt.st.push(I(parseInt10(tok)));
      else rt.st.push(F(r.v));
      return;
    }
    die(rt, "JSON: malformed input");
  }
  function op_json(rt) {
    var s = strOf(rt, pop(rt, "json"));
    var j = { s: s, i: 0 };
    jParse(j, rt);
    jWs(j);
    if (j.i < s.length) die(rt, "JSON: trailing garbage");
  }
  // UNJSON: cell -> JSON text (C: dict keys must be strings; managed
  // non-list/dict/str handles die)
  function unjsonW(rt, x) {
    switch (x.t) {
      case "s": {
        var o = "\"";
        for (var i = 0; i < x.v.length; i++) {
          var cc = x.v.charCodeAt(i);
          if (cc === 34 || cc === 92) o += "\\" + x.v[i];
          else if (cc === 10) o += "\\n";
          else if (cc === 9) o += "\\t";
          else if (cc === 13) o += "\\r";
          else if (cc < 0x20) {
            var h4 = cc.toString(16);
            while (h4.length < 4) h4 = "0" + h4;
            o += "\\u" + h4;
          }
          else o += x.v[i];
        }
        return o + "\"";
      }
      case "l": {
        var ob = "[";
        for (var j = 0; j < x.v.length; j++) { if (j) ob += ","; ob += unjsonW(rt, x.v[j]); }
        return ob + "]";
      }
      case "d": {
        var oc = "{", m = x.v, first = 1;
        for (var k = 0; k < m.cap; k++) if (m.st[k] === 1) {
          if (m.k[k].t !== "s") die(rt, "UNJSON: dict keys must be strings");
          if (!first) oc += ",";
          first = 0;
          oc += unjsonW(rt, m.k[k]) + ":" + unjsonW(rt, m.v[k]);
        }
        return oc + "}";
      }
      case "b": return x.v ? "true" : "false";
      case "n": return "null";
      case "f": return printFloatG(x.v, 17, "g");
      case "i": case "y": return x.v.toString();
      default: die(rt, "UNJSON: unsupported handle (atom/chan/iter/bitmap/bloom)");
    }
    return "";
  }
  function op_unjson(rt) { rt.st.push(S(unjsonW(rt, pop(rt, "unjson")))); }

  // ============================ raw memory / pointers =====================
  // Pointer cells {t:"p", v:{k,...}} with kinds:
  //   "b"    user buffer (BUF)     {buf, off, n, free}   byte offsets
  //   "m"    malloc block          {buf, off, n, free}
  //   "s"    string pointer        {s, b (utf8 bytes), off}  byte offset into b
  //   "n"    NULL
  //   "cell" cell pointer (extern values)   {ref}
  //   "argv" argv vector (extern)  {arr, off}  8-byte slots, slot argc is NUL
  //   "file" stream (extern/fopen) {f}
  // Mirrors the C prelude (uf_cadd/uf_rawptr/op_loadx/...); unlike C,
  // out-of-bounds access is a clean die instead of undefined behavior.
  // "s" offsets may go negative or past the end like raw C char* math;
  // reads at the ends yield the C NUL terminator (0).
  function utf8Enc(s) {
    var out = [];
    for (var i = 0; i < s.length; i++) {
      var c = s.charCodeAt(i);
      if (c < 0x80) out.push(c);
      else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f));
      else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
      else { c -= 0x10000; out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 0x3f), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f)); }
    }
    return out;
  }
  function utf8Dec(bytes) {
    var s = "", i = 0, n = bytes.length, c;
    while (i < n) {
      var c0 = bytes[i];
      if (c0 < 0x80) { c = c0; i += 1; }
      else if (c0 >= 0xc0 && c0 < 0xe0) {
        if (i + 1 >= n || (bytes[i + 1] & 0xc0) !== 0x80) { s += "\uFFFD"; i += 1; continue; }
        c = ((c0 & 0x1f) << 6) | (bytes[i + 1] & 0x3f);
        i += 2;
      } else if (c0 >= 0xe0 && c0 < 0xf0) {
        if (i + 2 >= n || (bytes[i + 1] & 0xc0) !== 0x80 || (bytes[i + 2] & 0xc0) !== 0x80) { s += "\uFFFD"; i += 1; continue; }
        c = ((c0 & 0x0f) << 12) | ((bytes[i + 1] & 0x3f) << 6) | (bytes[i + 2] & 0x3f);
        i += 3;
      } else if (c0 >= 0xf0 && c0 <= 0xf4) {
        if (i + 3 >= n || (bytes[i + 1] & 0xc0) !== 0x80 || (bytes[i + 2] & 0xc0) !== 0x80 || (bytes[i + 3] & 0xc0) !== 0x80) { s += "\uFFFD"; i += 1; continue; }
        c = ((c0 & 0x07) << 18) | ((bytes[i + 1] & 0x3f) << 12) | ((bytes[i + 2] & 0x3f) << 6) | (bytes[i + 3] & 0x3f);
        i += 4;
      } else { s += "\uFFFD"; i += 1; continue; }
      if (c >= 0xd800 && c <= 0xdfff) s += "\uFFFD";
      else if (c <= 0xffff) s += String.fromCharCode(c);
      else { c -= 0x10000; s += String.fromCharCode(0xd800 + (c >> 10), 0xdc00 + (c & 0x3ff)); }
    }
    return s;
  }
  function P(buf, off, k) { return { t: "p", v: { k: k, buf: buf, off: off, n: buf.byteLength - off, free: 0 } }; }
  function PS(s, off) { return { t: "p", v: { k: "s", s: s, b: utf8Enc(s), off: (off || 0) } }; }
  function PNUL() { return { t: "p", v: { k: "n" } }; }
  function PCELL(ref) { return { t: "p", v: { k: "cell", ref: ref } }; }
  function PARGV(arr, off) { return { t: "p", v: { k: "argv", arr: arr, off: (off || 0) } }; }
  function PFILE(f) { return { t: "p", v: { k: "file", f: f } }; }
  function ptrCheck(rt, x, op) {
    if (x.t !== "p") die(rt, op + ": expected pointer");
    var v = x.v;
    if (v.k !== "b" && v.k !== "m") die(rt, op + ": not a raw memory pointer");
    if (v.free) die(rt, op + ": use after free");
    if (v.off < 0) die(rt, op + ": negative pointer offset");
    return v;
  }
  // Pointer arithmetic (C uf_cadd/uf_csub raw-ptr + str-ptr paths). Pushes
  // the result. d is the integer operand (already checked by the caller).
  function ptrShift(rt, pv, iv, op) {
    var d = Number(intRaw(iv));
    var s2 = op === "add" ? 1 : -1;
    if (pv.t === "s") { rt.st.push(PS(pv.v, s2 * d)); return; }
    if (pv.t !== "p") die(rt, "pointer arithmetic: expected pointer");
    var v = pv.v;
    if (v.k === "b" || v.k === "m") {
      if (v.free) die(rt, "pointer arithmetic: use after free");
      rt.st.push(P(v.buf, v.off + s2 * d, v.k));
      return;
    }
    if (v.k === "s") { rt.st.push(PS(v.s, v.off + s2 * d)); return; }
    if (v.k === "n") die(rt, "pointer arithmetic on NULL");
    if (v.k === "argv") {
      var aoff = v.off + s2 * d;
      if (aoff % 8 !== 0 || aoff < 0 || aoff > v.arr.length * 8) die(rt, "argv pointer out of bounds");
      rt.st.push(PARGV(v.arr, aoff));
      return;
    }
    die(rt, "pointer arithmetic on unsupported pointer kind '" + v.k + "'");
  }
  function pushMalloc(rt, sz) {
    if (sz < 0n) die(rt, "negative MALLOC size");
    var len = sz === 0n ? 1 : Number(sz);
    var buf;
    try { buf = new ArrayBuffer(len); } catch (e) { die(rt, "out of memory"); }
    rt.st.push(P(buf, 0, "m"));
  }
  function op_malloc(rt) { pushMalloc(rt, asInt(pop(rt, "malloc"))); }
  function op_free(rt) {
    var p = pop(rt, "free");
    if (p && p.t === "p" && p.v && (p.v.k === "b" || p.v.k === "m")) p.v.free = 1;
  }
  function op_buf(rt) {
    var sz = asInt(pop(rt, "buffer"));
    if (sz < 0n) die(rt, "negative BUF size");
    var buf;
    try { buf = new ArrayBuffer(Number(sz)); } catch (e) { die(rt, "out of memory"); }
    rt.st.push(P(buf, 0, "b"));
  }
  function op_bufcopy(rt) {
    var n = asInt(pop(rt, "copy_memory"));
    var s = pop(rt, "copy_memory"), d = pop(rt, "copy_memory");
    if (n <= 0n) return;
    var sv = ptrCheck(rt, s, "copy_memory"), dv = ptrCheck(rt, d, "copy_memory");
    if (n > sv.n || n > dv.n) die(rt, "copy_memory: out of bounds");
    new Uint8Array(dv.buf, dv.off, Number(n)).set(new Uint8Array(sv.buf, sv.off, Number(n)));
  }
  function op_loadx(rt) {
    var a = pop(rt, "load");
    if (a.t === "s") { rt.st.push(I(BigInt(a.v.charCodeAt(0) & 255))); return; }
    if (a.t !== "p" || !a.v) die(rt, "load: not a pointer");
    var v = a.v;
    if (v.k === "n") die(rt, "load: NULL pointer");
    if (v.k === "cell") { rt.st.push(v.ref); return; }
    if (v.k === "argv") {
      var i8 = v.off / 8;
      if (i8 < 0 || i8 > v.arr.length) die(rt, "load: argv out of bounds");
      if (i8 === v.arr.length) rt.st.push(PNUL()); else rt.st.push(v.arr[i8]);
      return;
    }
    if (v.k === "file") die(rt, "load: cannot load a stream pointer");
    if (v.k === "s") {
      var c = v.off < 0 ? 0 : (v.off < v.b.length ? v.b[v.off] : 0);
      rt.st.push(I(BigInt(c)));
      return;
    }
    if (v.free) die(rt, "load: use after free");
    if (v.off < 0 || 8 > v.n) die(rt, "load: out of bounds");
    rt.st.push(I(new DataView(v.buf).getBigInt64(v.off, true)));
  }
  function op_storex(rt) {
    var a = pop(rt, "store");
    var v = pop(rt, "store");
    var pv = ptrCheck(rt, a, "store");
    if (8 > pv.n) die(rt, "store: out of bounds");
    var x = toU(asInt(v)) & 0xFFFFFFFFFFFFFFFFn;
    if (x >= 0x8000000000000000n) x -= M64;
    new DataView(pv.buf).setBigInt64(pv.off, x, true);
  }

  // ---------------- streams: stdout / stderr / stdin / fopen ------------
  function stdStreams(rt) {
    if (!rt.reg.streams) rt.reg.streams = {
      stdout: { kind: "stdout", data: null, pos: 0, unget: [], closed: 0, mode0: "w" },
      stderr: { kind: "stderr", data: null, pos: 0, unget: [], closed: 0, mode0: "w" },
      stdin: { kind: "stdin", data: utf8Enc((rt.reg.env && rt.reg.env.stdin) || ""), pos: 0, unget: [], closed: 0, mode0: "r" }
    };
    return rt.reg.streams;
  }
  // fopen from the run environment's in-memory file table; missing file ->
  // NULL (C: fopen returns NULL and the corpus checks with `v not`).
  function mkFile(rt, name, mode) {
    var env = rt.reg.env || { fs: {} };
    var fs = env.fs || {};
    if (!(name in fs)) return PNUL();
    var f, base;
    if (mode[0] === "r") { f = { name: name, data: utf8Enc(fs[name]), pos: 0, unget: [], closed: 0, mode0: "r" }; }
    else if (mode[0] === "w") { f = { name: name, data: [], pos: 0, unget: [], closed: 0, mode0: "w" }; }
    else if (mode[0] === "a") {
      base = utf8Enc(fs[name]);
      f = { name: name, data: base, pos: base.length, unget: [], closed: 0, mode0: "a" };
    } else die(rt, "fopen: unsupported mode '" + mode + "'");
    return PFILE(f);
  }
  function streamOf(rt, x, op) {
    if (x.t !== "p" || !x.v || x.v.k !== "file") die(rt, op + ": expected stream");
    var f = x.v.f;
    if (f.closed) die(rt, op + ": closed stream");
    return f;
  }
  // Returns the number of bytes written (0 on a failed read-only write).
  function stWriteBytes(rt, s, bytes) {
    if (s.kind === "stdout") { rt.reg.out.push(utf8Dec(bytes)); return bytes.length; }
    if (s.kind === "stderr") { rt.reg.err.push(utf8Dec(bytes)); return bytes.length; }
    if (s.kind === "stdin") die(rt, "write to stdin");
    if (s.mode0 === "r") return 0;
    var need = s.pos + bytes.length;
    while (s.data.length < need) s.data.push(0);
    for (var i = 0; i < bytes.length; i++) s.data[s.pos + i] = bytes[i];
    s.pos = need;
    return bytes.length;
  }
  function stReadBytes(rt, s, n) {
    var out = [], u = s.unget;
    if (s.mode0 === "w" || s.mode0 === "a") return out;
    while (u.length && out.length < n) out.push(u.shift());
    while (out.length < n && s.pos < s.data.length) out.push(s.data[s.pos++]);
    return out;
  }

  // ---------------- byte/string helpers for FFI -------------------------
  // NUL-terminated byte view of a string or string pointer.
  function cstrBytes(rt, x, op) {
    if (x.t === "s") {
      var bs = utf8Enc(x.v);
      for (var i = 0; i < bs.length; i++) if (bs[i] === 0) return bs.slice(0, i);
      return bs;
    }
    if (x.t !== "p") die(rt, op + ": expected string pointer");
    var v = x.v;
    if (v.free) die(rt, op + ": use after free");
    if (v.k === "s") {
      var o = v.off < 0 ? 0 : v.off, out = [];
      while (o < v.b.length && v.b[o] !== 0) out.push(v.b[o++]);
      return out;
    }
    if (v.k === "b" || v.k === "m") {
      var o2 = v.off < 0 ? 0 : v.off, out2 = [], u2 = new Uint8Array(v.buf);
      while (o2 < v.off + v.n && u2[o2] !== 0) out2.push(u2[o2++]);
      return out2;
    }
    die(rt, op + ": expected string pointer, got " + (v.k === "n" ? "NULL" : "pointer kind '" + v.k + "'"));
  }
  // Exactly n bytes from a pointer (or a plain string, padded with the C
  // NUL terminator past its end, as GC strings are NUL-terminated in C).
  function rawPtrBytes(rt, x, op, n) {
    if (x.t === "s" || (x.t === "p" && x.v && x.v.k === "s")) {
      var b = x.t === "s" ? utf8Enc(x.v) : x.v.b;
      var o = x.t === "s" ? 0 : (x.v.off < 0 ? 0 : x.v.off);
      var out = [];
      for (var i = 0; i < n; i++) out.push(o + i < b.length ? b[o + i] : 0);
      return out;
    }
    if (x.t !== "p") die(rt, op + ": expected pointer");
    var v = x.v;
    if (v.k !== "b" && v.k !== "m") die(rt, op + ": expected byte pointer, got '" + v.k + "'");
    if (v.free) die(rt, op + ": use after free");
    var off = v.off < 0 ? 0 : v.off;
    if (off + n > v.off + v.n) die(rt, op + ": buffer out of bounds");
    var out2 = [], u3 = new Uint8Array(v.buf);
    for (var j = 0; j < n; j++) out2.push(u3[off + j]);
    return out2;
  }
  function ptrWrite(rt, x, op, bytes, off) {
    if (x.t !== "p" || !x.v) die(rt, op + ": expected pointer");
    var v = x.v;
    if (v.k !== "b" && v.k !== "m") die(rt, op + ": expected byte pointer, got '" + v.k + "'");
    if (v.free) die(rt, op + ": use after free");
    if (off < 0) die(rt, op + ": negative write offset");
    if (off + bytes.length > v.off + v.n) die(rt, op + ": buffer out of bounds");
    new Uint8Array(v.buf, off, bytes.length).set(bytes);
  }
  // Cell -> printable text for string-typed FFI args (plain string or a
  // pointer to one); anything else falls back to toStr.
  function fmtText(rt, x, op) {
    if (x.t === "s") return x.v;
    if (x.t === "p") return utf8Dec(cstrBytes(rt, x, op));
    return toStr(x);
  }
  function memCopy(rt, dst, src, n, op) {
    if (n <= 0n) return;
    var dv = ptrCheck(rt, dst, op);
    var sb = rawPtrBytes(rt, src, op, Number(n)), ud = new Uint8Array(dv.buf);
    for (var i = 0; i < sb.length; i++) ud[dv.off + i] = sb[i];
  }
  function argInt(x) { return Number(asInt(x)); }

  // ---------------- FFI: glibc-compatible symbol implementations --------
  // Entry signature: fn(rt, A) -> cell | null (null = void). A holds the
  // fixed params in call order followed by the varargs.
  var FFI = {
    printf: function (rt, A) {
      var out = fmtRun(rt, A[0], A.slice(1));
      rt.reg.out.push(out);
      return I(BigInt(out.length));
    },
    fprintf: function (rt, A) {
      var out = fmtRun(rt, A[1], A.slice(2));
      stWriteBytes(rt, streamOf(rt, A[0], "fprintf"), utf8Enc(out));
      return I(BigInt(out.length));
    },
    sprintf: function (rt, A) {
      var out = fmtRun(rt, A[1], A.slice(2));
      var b = utf8Enc(out);
      b.push(0);
      ptrWrite(rt, A[0], "sprintf", b, 0);
      return I(BigInt(out.length));
    },
    puts: function (rt, A) {
      var t = fmtText(rt, A[0], "puts");
      stWriteBytes(rt, stdStreams(rt).stdout, utf8Enc(t + "\n"));
      return I(BigInt(t.length + 1));
    },
    putchar: function (rt, A) {
      var c = argInt(A[0]) & 255;
      stWriteBytes(rt, stdStreams(rt).stdout, [c]);
      return I(BigInt(c));
    },
    getchar: function (rt, A) {
      var b = stReadBytes(rt, stdStreams(rt).stdin, 1);
      return I(BigInt(b.length ? b[0] : -1));
    },
    fgetc: function (rt, A) {
      var b = stReadBytes(rt, streamOf(rt, A[0], "fgetc"), 1);
      return I(BigInt(b.length ? b[0] : -1));
    },
    fputc: function (rt, A) {
      var c = argInt(A[0]) & 255;
      stWriteBytes(rt, streamOf(rt, A[1], "fputc"), [c]);
      return I(BigInt(c));
    },
    ungetc: function (rt, A) {
      var vv = argInt(A[0]), s = streamOf(rt, A[1], "ungetc");
      if (vv < 0) return I(BigInt(vv));
      s.unget.unshift(vv & 255);
      return I(BigInt(vv & 255));
    },
    fputs: function (rt, A) {
      var t = fmtText(rt, A[0], "fputs");
      stWriteBytes(rt, streamOf(rt, A[1], "fputs"), utf8Enc(t));
      return I(BigInt(t.length));
    },
    fwrite: function (rt, A) {
      var sz = argInt(A[1]), cnt = argInt(A[2]);
      if (sz < 0 || cnt < 0) die(rt, "fwrite: negative size");
      var s = streamOf(rt, A[3], "fwrite");
      var wr = stWriteBytes(rt, s, rawPtrBytes(rt, A[0], "fwrite", sz * cnt));
      return I(BigInt(Math.floor(wr / (sz || 1))));
    },
    fread: function (rt, A) {
      var sz = argInt(A[1]), cnt = argInt(A[2]);
      if (sz < 0 || cnt < 0) die(rt, "fread: negative size");
      var s = streamOf(rt, A[3], "fread");
      var bytes = stReadBytes(rt, s, sz * cnt);
      if (bytes.length) ptrWrite(rt, A[0], "fread", bytes, 0);
      return I(BigInt(Math.floor(bytes.length / (sz || 1))));
    },
    fopen: function (rt, A) {
      return mkFile(rt, fmtText(rt, A[0], "fopen"), fmtText(rt, A[1], "fopen"));
    },
    fclose: function (rt, A) {
      streamOf(rt, A[0], "fclose").closed = 1;
      return I(0n);
    },
    strcmp: function (rt, A) {
      var a = cstrBytes(rt, A[0], "strcmp"), b = cstrBytes(rt, A[1], "strcmp");
      var n = Math.min(a.length, b.length), c = 0;
      for (var i = 0; i < n; i++) if (a[i] !== b[i]) { c = a[i] - b[i]; break; }
      if (c === 0) c = a.length - b.length;
      return I(BigInt(c));
    },
    strncmp: function (rt, A) {
      var a = cstrBytes(rt, A[0], "strncmp"), b = cstrBytes(rt, A[1], "strncmp");
      var nn = Math.min(Math.min(a.length, b.length), Math.max(0, argInt(A[2])));
      var c = 0;
      for (var i = 0; i < nn; i++) if (a[i] !== b[i]) { c = a[i] - b[i]; break; }
      return I(BigInt(c));
    },
    strlen: function (rt, A) {
      return I(BigInt(cstrBytes(rt, A[0], "strlen").length));
    },
    strcpy: function (rt, A) {
      var b = cstrBytes(rt, A[1], "strcpy");
      b.push(0);
      ptrWrite(rt, A[0], "strcpy", b, 0);
      return A[0];
    },
    strstr: function (rt, A) {
      var a = cstrBytes(rt, A[0], "strstr"), b = cstrBytes(rt, A[1], "strstr");
      var base = A[0];
      if (b.length === 0) return base;
      var idx = -1;
      outer: for (var i = 0; i + b.length <= a.length; i++) {
        for (var j = 0; j < b.length; j++) if (a[i + j] !== b[j]) continue outer;
        idx = i;
        break outer;
      }
      if (idx < 0) return PNUL();
      if (base.t === "s") return PS(base.v, idx);
      var v = base.v;
      if (v.k === "s") return PS(v.s, (v.off < 0 ? 0 : v.off) + idx);
      if (v.k === "b" || v.k === "m") return P(v.buf, (v.off < 0 ? 0 : v.off) + idx, v.k);
      die(rt, "strstr: unsupported haystack kind '" + (v ? v.k : "?") + "'");
    },
    malloc: function (rt, A) {
      pushMalloc(rt, asInt(A[0]));
      return null;
    },
    calloc: function (rt, A) {
      var nn = asInt(A[0]), ss = asInt(A[1]);
      if (nn < 0n || ss < 0n) die(rt, "calloc: negative size");
      pushMalloc(rt, nn * ss > 0n ? nn * ss : 1n);
      return null;
    },
    realloc: function (rt, A) {
      var p = A[0], n3 = asInt(A[1]);
      if (n3 < 0n) die(rt, "realloc: negative size");
      if (p.t === "p" && p.v && p.v.k === "n") { pushMalloc(rt, n3); return null; }
      if (p.t !== "p" || !p.v) die(rt, "realloc: expected pointer");
      var v = p.v;
      if (n3 === 0n) {
        if (v.k === "b" || v.k === "m") v.free = 1;
        rt.st.push(PNUL());
        return null;
      }
      var oldB = [];
      if (v.k === "s") oldB = v.b.slice(0, Math.min(v.b.length, Number(n3)));
      else if (v.k === "b" || v.k === "m") {
        if (v.free) die(rt, "realloc: use after free");
        var oo = v.off < 0 ? 0 : v.off, cap = v.off + v.n - oo;
        if (cap > Number(n3)) cap = Number(n3);
        var ur = new Uint8Array(v.buf);
        for (var i = 0; i < cap; i++) oldB.push(ur[oo + i]);
      } else die(rt, "realloc: unsupported pointer kind '" + v.k + "'");
      var ab;
      try { ab = new ArrayBuffer(Number(n3)); } catch (e) { die(rt, "realloc: out of memory"); }
      var u = new Uint8Array(ab);
      for (var j = 0; j < oldB.length; j++) u[j] = oldB[j];
      if (v.k === "b" || v.k === "m") v.free = 1;
      rt.st.push(P(ab, 0, "m"));
      return null;
    },
    free: function (rt, A) {
      var x = A[0];
      if (x && x.t === "p" && x.v && (x.v.k === "b" || x.v.k === "m")) x.v.free = 1;
      return null;
    },
    memcpy: function (rt, A) {
      memCopy(rt, A[0], A[1], asInt(A[2]), "memcpy");
      return A[0];
    },
    memmove: function (rt, A) {
      memCopy(rt, A[0], A[1], asInt(A[2]), "memmove");
      return A[0];
    },
    memset: function (rt, A) {
      var v = ptrCheck(rt, A[0], "memset");
      var cc = argInt(A[1]) & 255, n4 = asInt(A[2]);
      if (n4 < 0n) die(rt, "memset: negative size");
      var nn = Number(n4);
      if (nn > v.n) die(rt, "memset: buffer out of bounds");
      var um = new Uint8Array(v.buf);
      for (var i = 0; i < nn; i++) um[v.off + i] = cc;
      return A[0];
    },
    exit: function (rt, A) {
      var vv = argInt(A[0]);
      var e = new Error("exit " + vv);
      e.nkErr = 1;
      e.nkMsg = "exit";
      e.nkExit = vv;
      throw e;
    },
    sqrt: function (rt, A) { return F(Math.sqrt(fOf(A[0]))); },
    sin: function (rt, A) { return F(Math.sin(fOf(A[0]))); },
    cos: function (rt, A) { return F(Math.cos(fOf(A[0]))); },
    tan: function (rt, A) { return F(Math.tan(fOf(A[0]))); },
    exp: function (rt, A) { return F(Math.exp(fOf(A[0]))); },
    log: function (rt, A) { return F(Math.log(fOf(A[0]))); },
    floor: function (rt, A) { return F(Math.floor(fOf(A[0]))); },
    ceil: function (rt, A) { return F(Math.ceil(fOf(A[0]))); },
    abs: function (rt, A) { return F(Math.abs(fOf(A[0]))); },
    pow: function (rt, A) { return F(Math.pow(fOf(A[0]), fOf(A[1]))); },
    fmod: function (rt, A) { return F(fOf(A[0]) % fOf(A[1])); }
  };
  function ffiCall(rt, sym, A) {
    var f = FFI[sym];
    if (!f) die(rt, "no JavaScript FFI implementation for '" + sym + "'");
    return f(rt, A);
  }
  // `extern NAME` pushes a cell pointer; `load` (LOADX) dereferences it.
  // C strings in the env argv are exposed as string pointers (k "s").
  function externCell(rt, name) {
    if (name === "stdout") return PCELL(PFILE(stdStreams(rt).stdout));
    if (name === "stderr") return PCELL(PFILE(stdStreams(rt).stderr));
    if (name === "stdin") return PCELL(PFILE(stdStreams(rt).stdin));
    var env = rt.reg.env || { argv: [] };
    if (name === "nk_argc") return PCELL(I(BigInt((env.argv || []).length)));
    if (name === "nk_argv") {
      var arr = [], argv = env.argv || [];
      for (var i = 0; i < argv.length; i++) arr.push(PS(argv[i], 0));
      return PCELL(PARGV(arr, 0));
    }
    die(rt, "unknown extern '" + name + "'");
  }

  // ============================ concurrency =============================
  // Cooperative scheduling: every emitted segment is an async function, so a
  // run is a set of async task chains on the one event loop. Each task owns
  // its rt (stack/iv/frame); the shared reg carries G, out, and the
  // scheduler bookkeeping. Blocking chan ops RETURN a Promise (the generated
  // code awaits every op call); the waker pushes the value into the blocked
  // task's stack before resolving, so the op's stack effect is complete when
  // the task resumes. A full event-loop round with no progress and no
  // pending timers is a deadlock: the native target hangs, the JS target
  // dies (SPEC "JavaScript target").
  function mkChanV(cap) { return { cap: cap, q: [], head: 0, closed: 0, we: [], wd: [] }; }
  function chanCell(cap, reg) {
    var c = { t: "c", v: mkChanV(cap) };
    reg.chans.push(c.v);
    return c;
  }
  // Wake waiters while the queue allows. Each iteration removes exactly one
  // waiter, so the loop is bounded; waking only queues microtasks, so there
  // is no re-entrancy.
  function balance(r2, c) {
    for (var g = 0; g < 1 << 20; g++) {
      if (c.wd.length > 0) {
        if (c.q.length - c.head > 0) {
          var w = c.wd.shift();
          w.rt.st.push(c.q[c.head++]);
          w.rt.reg.gen++;
          w.res();
        } else if (c.closed) {
          // closed and empty: the waiter gets the sentinel 0
          var wc = c.wd.shift();
          wc.rt.st.push(I(0n));
          wc.rt.reg.gen++;
          wc.res();
        } else break;
      } else if (c.we.length > 0 && c.q.length - c.head < c.cap) {
        var w2 = c.we.shift();
        c.q.push(w2.val);
        w2.rt.reg.gen++;
        w2.res();
      } else break;
    }
  }
  function op_chan(rt) {
    var c = asInt(pop(rt, "chan"));
    rt.st.push(chanCell(c <= 0n ? 16 : c, rt.reg));
  }
  // Enqueue v into the chan object c (closed -> die; room -> push and wake;
  // else block until a slot frees). Shared by op_enq and the stream workers.
  function enqCell(rt, c, v) {
    if (c.closed) die(rt, "ENQ: chan closed");
    if (c.q.length - c.head < c.cap) {
      c.q.push(v);
      balance(rt, c);
      return;
    }
    return new Promise(function (res, rej) { c.we.push({ val: v, res: res, rej: rej, rt: rt }); });
  }
  function op_enq(rt) {
    var v = pop(rt, "enq");
    var h = pop(rt, "enq");
    if (h.t !== "c") die(rt, "ENQ: not a chan");
    return enqCell(rt, h.v, v);
  }
  // Blocks while empty; closed-and-empty yields the sentinel 0 (native
  // op_deq: ring_deq1 failing on a closed ring pushes uf_mki(0)).
  function op_deq(rt) {
    var h = pop(rt, "deq");
    if (h.t !== "c") die(rt, "DEQ: not a chan");
    var c = h.v;
    if (c.q.length - c.head > 0) {
      var v = c.q[c.head++];
      rt.st.push(v);
      balance(rt, c);
      return;
    }
    if (c.closed) { rt.st.push(I(0n)); return; }
    return new Promise(function (res) { c.wd.push({ res: res, rt: rt }); });
  }
  // Close: enq waiters die (native: ring_enq re-checks closed on wake),
  // deq waiters get the sentinel. A closed non-empty chan still drains.
  function op_close(rt) {
    var h = pop(rt, "close");
    if (h.t !== "c") die(rt, "CLOSE: not a chan");
    var c = h.v;
    c.closed = 1;
    var we = c.we.splice(0);
    for (var i = 0; i < we.length; i++) {
      we[i].rt.reg.gen++;
      we[i].res(dieE("ENQ: chan closed"));
    }
    var wd = c.wd.splice(0);
    for (var j = 0; j < wd.length; j++) {
      wd[j].rt.st.push(I(0n));
      wd[j].rt.reg.gen++;
      wd[j].res();
    }
  }
  function op_atom(rt) {
    rt.st.push({ t: "o", v: { x: asInt(pop(rt, "atom")) } });
  }
  function op_aget(rt) {
    var h = pop(rt, "aget");
    if (h.t !== "o") die(rt, "AGET: not an atom");
    rt.st.push(I(h.v.x));
  }
  function op_aset(rt) {
    var v = asInt(pop(rt, "aset"));
    var h = pop(rt, "aset");
    if (h.t !== "o") die(rt, "ASET: not an atom");
    h.v.x = v;
  }
  // fetch_add: pushes the OLD value (native atomic_fetch_add).
  function op_aadd(rt) {
    var n = asInt(pop(rt, "aadd"));
    var h = pop(rt, "aadd");
    if (h.t !== "o") die(rt, "AADD: not an atom");
    var ov = h.v.x;
    h.v.x = ov + n;
    rt.st.push(I(ov));
  }
  function op_cas(rt) {
    var nw = asInt(pop(rt, "cas"));
    var old = asInt(pop(rt, "cas"));
    var h = pop(rt, "cas");
    if (h.t !== "o") die(rt, "CAS: not an atom");
    var ok = h.v.x === old;
    if (ok) h.v.x = nw;
    rt.st.push(I(ok ? 1n : 0n));
  }
  // Spawn: fresh task, fresh cap-1 chan. The body runs as `F_pc(rt, {drain:0,
  // base:0})` in its own state (native uf_spawn_worker: new Ctx,
  // uf_call_addr(body, frame, -1)). On body return the value is enqueued and
  // the chan closed (bare ret -> 0, the Ret arm's empty-vstack default). An
  // uncontained body failure settles the whole run with it (native: a die in
  // a spawned thread kills the process).
  function op_spawn(rt) {
    var c = pop(rt, "spawn");
    if (c.t !== "a" || typeof c.v !== "function") die(rt, "SPAWN: not a code cell");
    var reg = rt.reg;
    var t = taskRt(reg);
    var ch = chanCell(1, reg);
    var rec = reg.tasks[reg.tasks.length - 1];
    c.v(t, { drain: 0, base: 0 }).then(
      function (v) {
        rec.exit = 1;
        reg.gen++;
        ch.v.q.push(v === undefined ? I(0n) : v);
        ch.v.closed = 1;
        balance(null, ch.v);
      },
      function (e) {
        rec.exit = 1;
        reg.gen++;
        if (reg.settle) reg.settle(normErr(e));
      }
    );
    rt.st.push(ch);
  }
  // The run-level scheduler: resolves when main's entry settles, rejects on
  // main error / spawned-task die, and dies on deadlock (a full round with
  // no progress, no timers, and live tasks still blocked).
  function pump(pMain, rt) {
    var reg = rt.reg;
    var done = 0, lastGen = reg.gen;
    return new Promise(function (resolve, reject) {
      function settle(err) {
        if (done) return;
        done = 1;
        if (err) reject(err); else resolve();
      }
      reg.settle = settle;
      pMain.then(
        function () { settle(null); },
        function (e) { settle(normErr(e)); }
      );
      var tick = function () {
        if (done) return;
        if (reg.gen !== lastGen) { lastGen = reg.gen; setTimeout(tick, 0); return; }
        var live = 0;
        for (var i = 0; i < reg.tasks.length; i++) if (!reg.tasks[i].exit) live++;
        if (live > 0 && reg.timers <= 0) {
          settle(dieE("deadlock: " + live + " task(s) blocked"));
          return;
        }
        setTimeout(tick, 0);
      };
      setTimeout(tick, 0);
    });
  }

  // ============================ float engine / fmt / print =================
  // Hand-rolled glibc printf float formatting (no toFixed/toPrecision).
  // Exact BigInt arithmetic on the double's significand; round-half-to-even.
  var POW10 = [1n];
  function p10(n) { while (POW10.length <= n) POW10.push(POW10[POW10.length - 1] * 10n); return POW10[n]; }
  // v = m * 2^e with m a 53-bit int (subnormal: e = -1074); zero -> m = 0
  function splitBits(v) {
    var b = fBits(v);
    var neg = b < 0n;
    var ub = neg ? b + M64 : b;
    var exp = (ub >> 52n) & 0x7ffn;
    var frac = ub & ((1n << 52n) - 1n);
    if (exp === 0n) return { m: frac, e: -1074n, neg: neg };
    // normal: v = m * 2^e with m = 2^52+frac, so e = (exp-1023) - 52
    return { m: frac | (1n << 52n), e: exp - 1023n - 52n, neg: neg };
  }
  // decimal exponent X: 10^X <= v < 10^(X+1)
  function decExp(p) {
    var e = p.e;
    if (e >= 0n) return (p.m << e).toString().length - 1;
    var k = -e;
    var q = p.m >> k;
    if (q > 0n) return q.toString().length - 1;
    var u = Math.max(1, Math.round(Number(k) * 0.30102999566398114 - Math.log10(Number(p.m))));
    while (p.m * p10(u - 1) >= (1n << k)) u--;
    while (p.m * p10(u) < (1n << k)) u++;
    return -u;
  }
  // round(v * 10^E) exact, half-to-even (E may be negative)
  function scaleRound(p, E) {
    var num = p.m, den = 1n;
    if (p.e >= 0n) num = num << p.e; else den = den << (-p.e);
    if (E >= 0) num = num * p10(E); else den = den * p10(-E);
    var qq = num / den, r = num % den;
    if (r * 2n > den) qq++;
    else if (r * 2n === den && (qq & 1n) === 1n) qq++;
    return qq;
  }
  function eForm(p, P, neg) { // P digits after the point (P=0 -> 1 sig digit, no dot)
    if (p.m === 0n) { var z = "0"; if (P > 0) z += "." + "0".repeat(P); return (neg ? "-" : "") + z + "e+00"; }
    var X = decExp(p);
    var N = scaleRound(p, P - X);
    if (N >= p10(P + 1)) { N = N / 10n; X++; }
    var dg = N.toString();
    while (dg.length < P + 1) dg += "0";
    return (neg ? "-" : "") + dg.charAt(0) + (P > 0 ? "." + dg.slice(1) : "") +
      "e" + (X < 0 ? "-" : "+") + String(Math.abs(X)).padStart(2, "0");
  }
  function fForm(p, P, neg) { // P fraction digits
    var N = scaleRound(p, P);
    var ip = N / p10(P), fr = N % p10(P);
    var s = (neg ? "-" : "") + ip.toString();
    if (P > 0) { var fs = fr.toString(); while (fs.length < P) fs = "0" + fs; s += "." + fs; }
    return s;
  }
  function gForm(p, P, flags, neg) { // P significant digits (>=1 here)
    var sg = neg ? "-" : "";
    if (p.m === 0n)
      return sg + "0" + (flags.indexOf("#") >= 0 ? "." + "0".repeat(P - 1) : "");
    var hash = flags.indexOf("#") >= 0;
    var X = decExp(p);
    var body;
    if (X < -4 || X >= P) {
      var N = scaleRound(p, P - 1 - X);
      var X2 = X;
      if (N >= p10(P)) { N = N / 10n; X2++; }
      var dg = N.toString();
      while (dg.length < P) dg += "0";
      var rest = dg.slice(1);
      if (hash) { if (rest.length === 0) rest = "0"; }
      else { while (rest.length && rest.charAt(rest.length - 1) === "0") rest = rest.slice(0, -1); }
      body = dg.charAt(0) + (rest ? "." + rest : "") +
        "e" + (X2 < 0 ? "-" : "+") + String(Math.abs(X2)).padStart(2, "0");
    } else if (X >= 0) {
      var N2 = scaleRound(p, P - 1 - X);
      var X3 = X;
      if (N2 >= p10(P)) { N2 = N2 / 10n; X3++; }
      var dg2 = N2.toString();
      while (dg2.length < P) dg2 += "0";
      var ip = dg2.slice(0, X3 + 1), fr = dg2.slice(X3 + 1);
      if (hash) { body = ip + "." + fr; }
      else { while (fr.length && fr.charAt(fr.length - 1) === "0") fr = fr.slice(0, -1); body = ip + (fr ? "." + fr : ""); }
    } else {
      var N3 = scaleRound(p, P - 1 - X);
      var dg3 = N3.toString();
      while (dg3.length < P) dg3 += "0";
      var b = "0." + "0".repeat(-X - 1) + dg3;
      if (!hash) {
        var e4 = b.length;
        while (e4 > 2 && b.charAt(e4 - 1) === "0") e4--;
        b = b.slice(0, e4);
        if (b === "0.") b = "0";
      }
      body = b;
    }
    return sg + body;
  }
  function fmtWidth(body, flags, width, zeroOk, padStart) {
    if (width <= body.length) return body;
    var pad = width - body.length;
    if (flags.indexOf("-") >= 0) return body + " ".repeat(pad);
    if (zeroOk && flags.indexOf("0") >= 0 && padStart >= 0)
      return body.slice(0, padStart) + "0".repeat(pad) + body.slice(padStart);
    return " ".repeat(pad) + body;
  }
  function fmtFloat(v, flags, width, precSet, prec, conv) {
    var signBit = fBits(v) < 0n;
    var special = isNaN(v) || v === Infinity || v === -Infinity;
    var body;
    if (isNaN(v)) body = (signBit ? "-" : "") + "nan";
    else if (v === Infinity || v === -Infinity) body = (signBit ? "-" : "") + "inf";
    else {
      var p = splitBits(v);
      if (conv === "e" || conv === "E") body = eForm(p, precSet ? prec : 6, signBit);
      else if (conv === "f" || conv === "F") body = fForm(p, precSet ? prec : 6, signBit);
      else { var Pg = precSet ? prec : 6; if (Pg === 0) Pg = 1; body = gForm(p, Pg, flags, signBit); }
    }
    if (conv === "E" || conv === "F" || conv === "G") {
      if (body === "nan") body = "NAN";
      else if (body === "inf") body = "INF";
      else if (body === "-nan") body = "-NAN";
      else if (body === "-inf") body = "-INF";
      else if (body.indexOf("e") >= 0) body = body.replace("e", "E");
    }
    if (body.charAt(0) !== "-" && flags.indexOf("+") >= 0) body = "+" + body;
    else if (body.charAt(0) !== "-" && flags.indexOf(" ") >= 0) body = " " + body;
    var ps = (body.charAt(0) === "-" || body.charAt(0) === "+" || body.charAt(0) === " ") ? 1 : 0;
    // glibc: the 0 flag applies to float output even with explicit precision,
    // but never to inf/nan (space-padded instead)
    return fmtWidth(body, flags, width, !special, ps);
  }
  function printFloatG(v, prec, conv) { return fmtFloat(v, "", 0, true, prec, conv); }
  function fmtCount(f) {
    var c = 0;
    for (var i = 0; i < f.length; i++) {
      if (f.charAt(i) !== "%") continue;
      if (f.charAt(i + 1) === "%") { i++; continue; }
      var q = i + 1;
      while (q < f.length && "-+ #0".indexOf(f.charAt(q)) >= 0) q++;
      c++;
      if (q < f.length && f.charAt(q) === "*") c++;
    }
    return c;
  }
  // Format engine shared by the fmt op and the printf family. f is a plain
  // JS string (the fmt op) or a cell (the FFI handlers; converted with
  // fmtText so string pointers work as formats).
  function fmtRun(rt, f, args) {
    if (typeof f !== "string") f = fmtText(rt, f, "FMT");
    var n = args.length, out = "", ai = 0;
    var i = 0;
    while (i < f.length) {
      if (f.charAt(i) !== "%") { out += f.charAt(i); i++; continue; }
      if (f.charAt(i + 1) === "%") { out += "%"; i += 2; continue; }
      var flags = "";
      i++;
      while (i < f.length && "-+ #0".indexOf(f.charAt(i)) >= 0) { flags += f.charAt(i); i++; }
      var width = 0, precSet = false, prec = 0;
      if (i < f.length && f.charAt(i) === "*") {
        i++;
        if (ai >= n) die(rt, "FMT: not enough args");
        var w = asInt(args[ai++]);
        if (w < 0n) { flags += "-"; w = -w; }
        if (w > 1073741824n) w = 1073741824n; // C is UB at INT64_MIN width; documented cap
        width = Number(w);
      }
      while (i < f.length && f.charAt(i) >= "0" && f.charAt(i) <= "9") { width = width * 10 + (f.charCodeAt(i) - 48); i++; }
      if (i < f.length && f.charAt(i) === ".") { precSet = true; i++; while (i < f.length && f.charAt(i) >= "0" && f.charAt(i) <= "9") { prec = prec * 10 + (f.charCodeAt(i) - 48); i++; } }
      while (i < f.length && "hlLjzt".indexOf(f.charAt(i)) >= 0) i++;
      var conv = i < f.length ? f.charAt(i++) : "d";
      if (ai >= n) die(rt, "FMT: not enough args");
      var ar = args[ai++], piece;
      if (conv === "d" || conv === "i") {
        var raw = asInt(ar);
        var neg2 = raw < 0n;
        var digs = (neg2 ? -raw : raw).toString(10);
        if (precSet && prec === 0 && raw === 0n) digs = ""; // glibc: zero with prec 0 -> empty
        else if (precSet && digs.length < prec) digs = "0".repeat(prec - digs.length) + digs;
        var signC = neg2 ? "-" : (flags.indexOf("+") >= 0 ? "+" : (flags.indexOf(" ") >= 0 ? " " : ""));
        piece = fmtWidth(signC + digs, flags, width, !precSet, signC.length);
        if (piece.length > 127) piece = piece.slice(0, 127);
        out += piece;
      } else if (conv === "u" || conv === "x" || conv === "X" || conv === "o") {
        var raw2 = asInt(ar), u2 = toU(raw2);
        var digs2 = u2.toString(conv === "o" ? 8 : (conv === "x" || conv === "X" ? 16 : 10));
        if (conv === "X") digs2 = digs2.toUpperCase();
        var pref = "";
        if ((conv === "x" || conv === "X") && flags.indexOf("#") >= 0 && raw2 !== 0n) pref = conv === "X" ? "0X" : "0x";
        else if (conv === "o" && flags.indexOf("#") >= 0 && raw2 !== 0n) pref = "0";
        if (precSet && digs2.length < prec) digs2 = "0".repeat(prec - digs2.length) + digs2;
        piece = fmtWidth(pref + digs2, flags, width, !precSet, pref.length);
        if (piece.length > 127) piece = piece.slice(0, 127);
        out += piece;
      } else if (conv === "c") {
        piece = String.fromCharCode(Number(asInt(ar) & 0xffn));
        piece = fmtWidth(piece, flags, width, false, 0);
        if (piece.length > 127) piece = piece.slice(0, 127);
        out += piece;
      } else if (conv === "f" || conv === "F" || conv === "e" || conv === "E" || conv === "g" || conv === "G") {
        piece = fmtFloat(fOf(ar), flags, width, precSet, prec, conv);
        if (piece.length > 127) piece = piece.slice(0, 127);
        out += piece;
      } else if (conv === "s") {
        var sv = fmtText(rt, ar, "FMT %s");
        var nz = sv.indexOf("\0");
        if (nz >= 0) sv = sv.slice(0, nz); // glibc %s stops at NUL
        if (precSet && sv.length > prec) sv = sv.slice(0, prec);
        // glibc: %s is space-padded even with the 0 flag
        out += fmtWidth(sv, flags, width, false, 0);
      } else if (conv === "p") {
        die(rt, "JavaScript target: pointer printf unsupported");
      } else {
        die(rt, "FMT: unsupported directive");
      }
    }
    return out;
  }
  function op_fmt(rt) {
    var f = strOf(rt, pop(rt, "fmt"));
    var n = fmtCount(f);
    if (n > 16) die(rt, "FMT: too many args");
    var args = new Array(n);
    for (var k = n - 1; k >= 0; k--) args[k] = pop(rt, "fmt");
    rt.st.push(S(fmtRun(rt, f, args)));
  }
  function printCell(rt, x, nested) {
    switch (x.t) {
      case "f": if (isNaN(x.v)) return "NaN"; return printFloatG(x.v, 17, "g");
      case "b": return x.v ? "true" : "false";
      case "i": case "y": return x.v.toString();
      case "n": return "null";
      case "s": {
        var s = x.v;
        var nz = s.indexOf("\0");
        if (nz >= 0) s = s.slice(0, nz);
        if (!nested) return s;
        var o = "\"";
        for (var i = 0; i < s.length; i++) {
          var c = s.charCodeAt(i);
          if (c === 34) o += "\\\"";
          else if (c === 92) o += "\\\\";
          else if (c === 10) o += "\\n";
          else if (c === 9) o += "\\t";
          else if (c === 13) o += "\\r";
          else o += s[i];
        }
        return o + "\"";
      }
      case "l": {
        var ob = "[";
        for (var j = 0; j < x.v.length; j++) { if (j) ob += ","; ob += printCell(rt, x.v[j], 1); }
        return ob + "]";
      }
      case "d": {
        var oc = "{", m = x.v, first = 1;
        for (var q = 0; q < m.cap; q++) if (m.st[q] === 1) {
          if (!first) oc += ",";
          first = 0;
          oc += printCell(rt, m.k[q], 1) + ":" + printCell(rt, m.v[q], 1);
        }
        return oc + "}";
      }
      case "v": {
        var ov = "[";
        for (var w = 0; w < x.v.len; w++) { if (w) ov += ","; ov += printCell(rt, vread(x, w), 1); }
        return ov + "]";
      }
      case "m": return "<bitmap>";
      case "tm": return x.v.toString();
      case "ob": return "[object Object]";
      case "bl": return "<bloom>";
      case "c": return "<chan>";
      case "o": return "<atom>";
      case "a": return "<code>";
      case "t": return "<iter>";
      case "p": {
        var pv = x.v;
        if (!pv) return "null";
        if (pv.k === "s") {
          var ps = pv.s.slice(pv.off < 0 ? 0 : pv.off);
          var pn = ps.indexOf("\0");
          if (pn >= 0) ps = ps.slice(0, pn);
          if (!nested) return ps;
          var oq = "\"";
          for (var i2 = 0; i2 < ps.length; i2++) {
            var c2 = ps.charCodeAt(i2);
            if (c2 === 34) oq += "\\\"";
            else if (c2 === 92) oq += "\\\\";
            else if (c2 === 10) oq += "\\n";
            else if (c2 === 9) oq += "\\t";
            else if (c2 === 13) oq += "\\r";
            else oq += ps[i2];
          }
          return oq + "\"";
        }
        if (pv.k === "n") return "null";
        if (pv.k === "cell") return "<ptr>";
        if (pv.k === "argv") return "<argv>";
        if (pv.k === "file") return "<stream>";
        return "<buf>";
      }
    }
    return "";
  }
  function op_print(rt) {
    var v = pop(rt, "print");
    rt.out.push(printCell(rt, v, 0) + "\n");
  }
  // DBG: v -> v | peek-print the stack top to stderr: "dbg <site>: <value>".
  function op_dbg(rt) {
    if (!rt.st.length) die(rt, "dbg: empty stack");
    var v = rt.st[rt.st.length - 1];
    rt.reg.err.push("dbg " + siteHead(rt) + ": " + printCell(rt, v, 0) + "\n");
  }
  // CHECK: v -> - | one-token assert: a falsy value dies "check failed" at this line.
  function op_check(rt) { var v = pop(rt, "check"); if (!truthy(v)) die(rt, "check failed"); }
  // TIMER: - -> ns | monotonic clock in nanoseconds (only differences are meaningful).
  function op_timer(rt) {
    var ns = (typeof process !== "undefined" && process.hrtime) ? process.hrtime.bigint()
      : BigInt(Math.round((performance.now() + performance.timeOrigin) * 1e9));
    rt.st.push(I(ns));
  }
  // MEMSTATS: - -> dict | GC stats {gc, objs, live, alloc}. The JS heap is
  // host-managed (the runtime keeps no collector or allocator), so all four
  // counters read 0 here; the native target reports real numbers.
  function op_memstats(rt) {
    var m = mapNew();
    mapPut(m, S("gc"), I(0)); mapPut(m, S("objs"), I(0));
    mapPut(m, S("live"), I(0)); mapPut(m, S("alloc"), I(0));
    rt.st.push(D(m));
  }

  // ===================== program args / gc / time / bloom ================
  // GC: C op_gc collects; JS is fully managed, no observable effect.
  function op_gc(rt) { }
  function op_obj(rt) {
    var raw = iOf(pop(rt, "obj"));
    var sz = Number(raw & 0xffffffffn);
    var sid = Number(raw >> 32n);
    if (sz <= 0) sz = 8;
    var data = new Array(sz >> 3);
    for (var i = 0; i < data.length; i++) data[i] = I(0n);
    rt.st.push(OB({ sid: sid, esz: sz, data: data }));
  }
  // env.argv[0] carries the program name (node tail passes script path).
  function op_argv(rt) {
    var av = rt.reg.env.argv || [];
    var a = new Array(av.length);
    for (var i = 0; i < av.length; i++) a[i] = S(String(av[i]));
    rt.st.push(L(a));
  }
  function op_hasargs(rt) {
    var av = rt.reg.env.argv || [];
    rt.st.push(I(av.length > 1 ? 1n : 0n));
  }
  function op_argi(rt) {
    var av = rt.reg.env.argv || [];
    var idx = Number(iOf(pop(rt, "argi")));
    if (idx < 0 || idx >= av.length) die(rt, "ARGI: index out of bounds");
    rt.st.push(I(parseInt10(String(av[idx]))));
  }
  function op_now(rt) {
    var ms = Date.now();
    rt.st.push(TM(BigInt(Math.floor(ms / 1000)) * 1000000000n + BigInt(ms % 1000) * 1000000n));
  }
  // TIME: fmt "unix" only (native strptime needs a libc); TIMEF likewise.
  function op_time(rt) {
    var fmt = strOf(rt, pop(rt, "time"));
    var st = strOf(rt, pop(rt, "time"));
    if (fmt !== "unix") die(rt, "TIME: unparseable");
    rt.st.push(TM(i64OfDouble(parseDouble(st) * 1e9)));
  }
  function op_timef(rt) {
    var fmt = strOf(rt, pop(rt, "timef"));
    var t = pop(rt, "timef");
    if (fmt !== "unix") die(rt, "TIMEF: format failed");
    var ns = t.t === "tm" ? t.v : iOf(t);
    // C: snprintf("%.9f", (double)ns/1e9) — seconds, 9 decimals
    rt.st.push(S(fmtFloat(Number(ns) / 1e9, "", 0, true, 9, "f")));
  }
  // ===================== clone / sizeof / shutdown / scan =============
  // C: gc_find then HT_ITER then ARR/TENSOR/MAT. JS managed cells: lists,
  // dicts, arrays, bitmaps, strings, iterators, structs, blooms, atoms.
  function op_clone(rt) {
    var h = pop(rt, "clone");
    if (h.t !== "l" && h.t !== "d" && h.t !== "v" && h.t !== "m" && h.t !== "s"
        && h.t !== "t" && h.t !== "ob" && h.t !== "bl" && h.t !== "o")
      die(rt, "CLONE: not a managed object");
    if (h.t === "t") die(rt, "CLONE: iterators are single-use");
    if (h.t !== "v") die(rt, "CLONE: only arr/tensor/matrix");
    var p = h.v, d = new Array(p.len);
    for (var i = 0; i < p.len; i++) d[i] = p.data[i];
    rt.st.push(V(p.kind, p.ety, p.len, d, p.rows));
  }
  // C: (ty==T_BYTE||ty==T_BOOL)?1:8 over the raw popped payload.
  function op_sizeof(rt) {
    var ty = Number(iOf(pop(rt, "sizeof")));
    rt.st.push(I(ty === 3 || ty === 21 ? 1n : 8n));
  }
  // C sets the shutdown flag on the active weave job; the JS target has no
  // jobs, so this is a documented no-op.
  function op_shutdown(rt) { }
  // C op_scan: fscanf(stdin, fmt) with one cell per directive; the result is
  // a list of the parsed cells with the count appended. The cursor is
  // rt.scanPos into env.stdin (reset per run), matching the native stdin
  // stream. %s takes a whitespace-delimited token (<= 65535 chars).
  function scanIsW(c) {
    return c === " " || c === "\t" || c === "\n" || c === "\r" || c === "\v" || c === "\f";
  }
  function op_scan(rt) {
    var f = strOf(rt, pop(rt, "scan"));
    var inp = rt.reg.env.stdin || "";
    var pos = (rt.scanPos === undefined) ? 0 : rt.scanPos;
    var items = new Array();
    function ws() { while (pos < inp.length && scanIsW(inp[pos])) pos++; }
    var p = 0, n = f.length;
    while (p < n) {
      var c = f[p];
      if (c === "%") {
        if (f[p + 1] === "%") { p += 2; continue; }
        p++;
        while (p < n && (f[p] === "-" || f[p] === "+" || f[p] === " " || f[p] === "#" || f[p] === "0")) p++;
        while (p < n && f[p] >= "0" && f[p] <= "9") p++;
        if (p < n && f[p] === ".") { p++; while (p < n && f[p] >= "0" && f[p] <= "9") p++; }
        while (p < n && (f[p] === "h" || f[p] === "l" || f[p] === "L" || f[p] === "j" || f[p] === "z" || f[p] === "t")) p++;
        var conv = p < n ? f[p] : "";
        p++;
        ws();
        if (conv === "s") {
          var s0 = pos;
          while (pos < inp.length && !scanIsW(inp[pos]) && pos - s0 < 65535) pos++;
          if (pos === s0) die(rt, "SCAN: input error");
          items.push(S(inp.slice(s0, pos)));
          continue;
        }
        if (conv === "f" || conv === "F" || conv === "e" || conv === "E" || conv === "g" || conv === "G") {
          var m2 = inp.slice(pos).match(/^[+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?/);
          if (!m2) die(rt, "SCAN: input error");
          items.push(F(parseFloat(m2[0])));
          pos += m2[0].length;
          continue;
        }
        var sgn = 0, digits = 0, v = 0n;
        if (pos < inp.length && (inp[pos] === "-" || inp[pos] === "+")) { sgn = (inp[pos] === "-") ? -1 : 1; pos++; }
        if (conv === "d" || conv === "i" || conv === "u") {
          while (pos < inp.length && inp[pos] >= "0" && inp[pos] <= "9") { v = v * 10n + BigInt(parseInt(inp[pos], 10)); digits++; pos++; }
        } else if (conv === "x" || conv === "X" || conv === "o") {
          var base = (conv === "o") ? 8 : 16;
          for (;;) {
            var ch = inp[pos];
            if (ch === undefined) break;
            var cc = ch.charCodeAt(0);
            var dv = (cc >= 48 && cc <= 57) ? cc - 48 : (cc >= 97 && cc <= 102) ? cc - 87 : (cc >= 65 && cc <= 70) ? cc - 55 : -1;
            if (dv < 0 || dv >= base) break;
            v = v * BigInt(base) + BigInt(dv); digits++; pos++;
          }
        } else {
          die(rt, "SCAN: unsupported directive");
        }
        if (digits === 0) die(rt, "SCAN: input error");
        if (sgn === -1) v = -v;
        items.push(I(v));
      } else if (scanIsW(c)) {
        ws();
      } else {
        die(rt, "SCAN: literal text in format unsupported");
      }
    }
    rt.scanPos = pos;
    items.push(I(BigInt(items.length)));
    rt.st.push(L(items));
  }
  // ===================== capability discovery ===========================
  // The JS target is never sandboxed: every real capability is granted,
  // fs.workspace is inactive, compute is permitted but always runs on the
  // CPU, and all modules are allowed — the unrestricted C defaults.
  var CAP_NAMES = ["fs.read", "fs.write", "proc", "ffi.use", "ffi.import", "raw.syscall", "raw.mem", "host.argv"];
  function op_cap(rt) {
    var s = strOf(rt, pop(rt, "cap"));
    if (s === "fs.workspace") { rt.st.push(I(0n)); return; }
    if (s === "compute") { rt.st.push(I(1n)); return; }
    var ok = false;
    for (var i = 0; i < 8; i++) if (CAP_NAMES[i] === s) { ok = true; break; }
    if (!ok) die(rt, "CAP: unknown capability (valid: fs.read fs.write proc ffi.use ffi.import raw.syscall raw.mem host.argv fs.workspace compute)");
    rt.st.push(I(1n));
  }
  function op_caps(rt) {
    var m = mapNew();
    mapPut(m, S("policy"), S("none"));
    for (var i = 0; i < 8; i++) mapPut(m, S(CAP_NAMES[i]), I(1n));
    mapPut(m, S("fs.workspace"), I(0n));
    mapPut(m, S("device"), S("cpu"));
    mapPut(m, S("workspace"), L([]));
    mapPut(m, S("modules"), L([S("*")]));
    rt.st.push(D(m));
  }
  // FNV-1a 64 over the same byte views as C uf_bloom_hashes (str: utf8;
  // else the 8 LE bytes of the i64 view; pointer-valued cells use the
  // identity bits, same documented stand-in as mapHash).
  function bloomHashes(v) {
    var h1, h2;
    if (v.t === "s") {
      h1 = fnvBytes(utf8Enc(v.v));
      h2 = fnvBytes(le8(h1 ^ 0x9e3779b97f4a7c15n));
    } else {
      var x = iOf(v);
      h1 = fnvBytes(le8(x));
      h2 = fnvBytes(le8(x ^ 0x9e3779b97f4a7c15n));
    }
    if (h2 === 0n) h2 = 0x100000001bn;
    return [h1, h2];
  }
  function op_bloom(rt) {
    var n = Number(iOf(pop(rt, "bloom")));
    if (n < 1) die(rt, "BLOOM: n < 1");
    var bits = Math.floor(n * 9.585) + 64;
    var w = new Array(Math.ceil(bits / 64));
    for (var i = 0; i < w.length; i++) w[i] = 0n;
    rt.st.push(BL({ len: bits, k: 7, words: w }));
  }
  function op_badd(rt) {
    var v = pop(rt, "badd"), h = pop(rt, "badd");
    if (h.t !== "bl") die(rt, "BADD: not a bloom filter");
    var hs = bloomHashes(v), b = h.v;
    for (var i = 0; i < b.k; i++) {
      var p = toU(hs[0] + BigInt(i) * hs[1]) % BigInt(b.len);
      b.words[Number(p >> 6n)] |= 1n << (p & 63n);
    }
  }
  function op_btest(rt) {
    var v = pop(rt, "btest"), h = pop(rt, "btest");
    if (h.t !== "bl") die(rt, "BTEST: not a bloom filter");
    var hs = bloomHashes(v), b = h.v;
    for (var i = 0; i < b.k; i++) {
      var p = toU(hs[0] + BigInt(i) * hs[1]) % BigInt(b.len);
      if (((b.words[Number(p >> 6n)] >> (p & 63n)) & 1n) === 0n) { rt.st.push(I(0n)); return; }
    }
    rt.st.push(I(1n));
  }

  // ========================= file table + streams =========================
  // env.fs is the in-memory file table: name -> text content. A missing name
  // is a missing file (reads die per op, writes create). This mirrors the
  // sandbox filesystem: every name is writable, no EIO.
  function fsGet(rt, name) {
    var fs = rt.reg.env.fs;
    return (fs && (name in fs)) ? fs[name] : null;
  }
  // getline + the C stream ops' trailing \n/\r-mix strip, as one pass.
  // A missing final newline still yields the last (partial) line.
  function lineOf(c) {
    var out = [], st = 0;
    for (var i = 0; i < c.length; i++) if (c.charCodeAt(i) === 10) {
      var e = i + 1;
      while (e > st && (c.charCodeAt(e - 1) === 10 || c.charCodeAt(e - 1) === 13)) e--;
      out.push(c.slice(st, e));
      st = i + 1;
    }
    if (st < c.length) {
      var e2 = c.length;
      while (e2 > st && (c.charCodeAt(e2 - 1) === 10 || c.charCodeAt(e2 - 1) === 13)) e2--;
      out.push(c.slice(st, e2));
    }
    return out;
  }
  function op_slurp(rt) {
    var p = strOf(rt, pop(rt, "slurp"));
    var c = fsGet(rt, p);
    if (c === null) die(rt, "SLURP: cannot open file");
    rt.st.push(S(c));
  }
  function op_spit(rt) {
    var st = strOf(rt, pop(rt, "spit"));
    var p = strOf(rt, pop(rt, "spit"));
    var fs = rt.reg.env.fs;
    if (fs) fs[p] = st; /* env model: writes never fail */
  }
  function op_mmap(rt) {
    var p = strOf(rt, pop(rt, "mmap"));
    var c = fsGet(rt, p);
    if (c === null) die(rt, "MMAP: cannot open file");
    rt.st.push(S(c));
  }
  async function op_feach(rt) {
    var f = pop(rt, "feach");
    var p = strOf(rt, pop(rt, "feach"));
    var c = fsGet(rt, p);
    if (c === null) die(rt, "FEACH: cannot open file");
    var lines = lineOf(c);
    for (var i = 0; i < lines.length; i++) {
      var base = rt.st.length;
      rt.st.push(S(lines[i]));
      await preludeCall(rt, f.v, 1);
      var k = rt.st.pop();
      rt.st.length = base;
      if (!truthy(k)) break;
    }
  }
  async function op_ffold(rt) {
    var f = pop(rt, "ffold");
    var acc = pop(rt, "ffold");
    var p = strOf(rt, pop(rt, "ffold"));
    var c = fsGet(rt, p);
    if (c === null) die(rt, "FFOLD: cannot open file");
    var lines = lineOf(c);
    for (var i = 0; i < lines.length; i++) {
      var base = rt.st.length;
      rt.st.push(acc);
      rt.st.push(S(lines[i]));
      await preludeCall(rt, f.v, 2);
      acc = rt.st.pop();
      rt.st.length = base;
    }
    rt.st.push(acc);
  }
  // FSPLIT: per-line field state lives on rt (line + flat [off,len] pairs,
  // max 128 fields per line, 128th = truncated remainder) until the loop
  // ends; FGET/FATOI/FATOF/FSGET/FBYTE/FADDTO/FINC read it inside the callback.
  async function op_fsplit(rt) {
    var f = pop(rt, "fsplit");
    var acc = pop(rt, "fsplit");
    var sep = strOf(rt, pop(rt, "fsplit"));
    var p = strOf(rt, pop(rt, "fsplit"));
    var c = fsGet(rt, p);
    if (c === null) die(rt, "FSPLIT: cannot open file");
    if (sep.length === 0) die(rt, "FSPLIT: empty separator");
    var el = sep.length;
    var sep1 = String.fromCharCode(sep.charCodeAt(0));
    var lines = lineOf(c);
    for (var i = 0; i < lines.length; i++) {
      var line = lines[i];
      var cur = 0, nfields = 0, offs = [];
      for (;;) {
        var sp = (el === 1) ? line.indexOf(sep1, cur) : line.indexOf(sep, cur);
        if (sp < 0) { offs.push(cur, line.length - cur); nfields++; break; }
        offs.push(cur, sp - cur); nfields++;
        if (nfields >= 128) break;
        cur = sp + el;
      }
      rt.fsLine = line;
      rt.fsOffs = offs;
      rt.fsN = nfields;
      var base = rt.st.length;
      rt.st.push(acc);
      rt.st.push(I(BigInt(nfields)));
      await preludeCall(rt, f.v, 2);
      acc = rt.st.pop();
      rt.st.length = base;
    }
    rt.fsLine = "";
    rt.fsOffs = null;
    rt.fsN = 0;
    rt.st.push(acc);
  }
  // field bounds helpers: fs state as above; C checks idx first, then range.
  function fsField(rt, op, idx) {
    var n = rt.fsN || 0;
    if (idx < 0 || idx >= n) die(rt, op + ": index out of bounds");
    var o2 = rt.fsOffs, line = rt.fsLine;
    var off = Number(o2[idx * 2]), len = Number(o2[idx * 2 + 1]);
    return [line, off, len];
  }
  function op_fget(rt) {
    var idx = Number(iOf(pop(rt, "fget")));
    var ff = fsField(rt, "FGET", idx);
    rt.st.push(S(ff[0].slice(ff[1], ff[1] + ff[2])));
  }
  function op_fatoi(rt) {
    var idx = Number(iOf(pop(rt, "fatoi")));
    var ff = fsField(rt, "FATOI", idx);
    rt.st.push(I(parseInt10(ff[0].slice(ff[1], ff[1] + ff[2]))));
  }
  function op_fatof(rt) {
    var idx = Number(iOf(pop(rt, "fatof")));
    var ff = fsField(rt, "FATOF", idx);
    rt.st.push(F(parseDouble(ff[0].slice(ff[1], ff[1] + ff[2]))));
  }
  function op_fsget(rt) {
    var len = iOf(pop(rt, "fsget"));
    var off = iOf(pop(rt, "fsget"));
    var idx = Number(iOf(pop(rt, "fsget")));
    var ff = fsField(rt, "FSGET", idx);
    if (Number(off) < 0 || Number(len) < 0 || Number(off) + Number(len) > ff[2]) die(rt, "FSGET: out of bounds");
    rt.st.push(S(ff[0].slice(ff[1] + Number(off), ff[1] + Number(off) + Number(len))));
  }
  function op_fbyte(rt) {
    var off = iOf(pop(rt, "fbyte"));
    var idx = Number(iOf(pop(rt, "fbyte")));
    var ff = fsField(rt, "FBYTE", idx);
    if (Number(off) < 0 || Number(off) >= ff[2]) die(rt, "FBYTE: out of bounds");
    rt.st.push(I(BigInt(ff[0].charCodeAt(ff[1] + Number(off)))));
  }
  // ADDTO: v k h -> h[k] += v (C: no tag check; JS dies instead of UB).
  function op_addto(rt) {
    var v = pop(rt, "addto");
    var k = pop(rt, "addto");
    var h = pop(rt, "addto");
    if (h.t !== "d") die(rt, "ADDTO: not a dict");
    var m = h.v, cur = mapGet(m, k);
    mapPut(m, k, cur === null ? v : cadd2(cur, v));
  }
  function op_faddto(rt) {
    var v = pop(rt, "faddto");
    var idx = Number(iOf(pop(rt, "faddto")));
    var h = pop(rt, "faddto");
    if (h.t !== "d") die(rt, "FADDTO: not a dict");
    var ff = fsField(rt, "FADDTO", idx);
    var key = S(ff[0].slice(ff[1], ff[1] + ff[2]));
    var m = h.v, cur = mapGet(m, key);
    mapPut(m, key, cur === null ? v : cadd2(cur, v));
  }
  function op_finc(rt) {
    var idx = Number(iOf(pop(rt, "finc")));
    var h = pop(rt, "finc");
    if (h.t !== "d") die(rt, "FINC: not a dict");
    var ff = fsField(rt, "FINC", idx);
    var key = S(ff[0].slice(ff[1], ff[1] + ff[2]));
    var m = h.v, cur = mapGet(m, key);
    if (cur === null) mapPut(m, key, I(1n));
    else if (isTInt(cur)) mapPut(m, key, I(wrap64(intRaw(cur) + 1n)));
    else mapPut(m, key, cadd2(cur, I(1n)));
  }
  // FEMIT: h p -> count (any iterable; per-item C formatting: str raw,
  // float %.17g, int/time/dur/bytes decimal, else unjson) + "\n" each.
  async function op_femit(rt) {
    var p = strOf(rt, pop(rt, "femit"));
    var h = pop(rt, "femit");
    var items = await materialize(h, rt);
    var out = "";
    for (var i = 0; i < items.length; i++) {
      var x = items[i];
      if (x.t === "s") out += x.v;
      else if (x.t === "f") out += fmtFloat(x.v, "", 0, true, 17, "g");
      else if (x.t === "i" || x.t === "y" || x.t === "b" || x.t === "tm" || x.t === "n") out += iOf(x).toString();
      else out += unjsonW(rt, x);
      out += "\n";
    }
    var fs = rt.reg.env.fs;
    if (fs) fs[p] = out;
    rt.st.push(I(BigInt(items.length)));
  }
  // FMATCH: pat p -> chan (cap 64) of matching lines; a missing file yields
  // an empty closed chan (native: worker fopen-check, no die).
  function op_fmatch(rt) {
    var pat = strOf(rt, pop(rt, "fmatch"));
    var p = strOf(rt, pop(rt, "fmatch"));
    var content = fsGet(rt, p);
    var ch = chanCell(64, rt.reg);
    var reg = rt.reg;
    var t = taskRt(reg);
    var rec = reg.tasks[reg.tasks.length - 1];
    (async function () {
      if (content !== null) {
        var lines = lineOf(content);
        for (var j = 0; j < lines.length; j++) {
          var caps = newCaps();
          if (rxExec(pat, lines[j], 0, caps, t)) await enqCell(t, ch.v, S(lines[j]));
        }
      }
      ch.v.closed = 1;
      balance(null, ch.v);
    })().then(
      function () { rec.exit = 1; reg.gen++; },
      function (e) { rec.exit = 1; reg.gen++; if (reg.settle) reg.settle(normErr(e)); }
    );
    rt.st.push(ch);
  }

  // ============================ graph searches ============================
  // BFS/DFS/WFIND: f(node) -> iterable of neighbors; the callback runs on
  // the CURRENT task (native runs it in the caller's thread too).
  async function op_bfs(rt) {
    var f = pop(rt, "bfs");
    var start = pop(rt, "bfs");
    var seen = mapNew();
    var order = [];
    var queue = [start];
    var qi = 0;
    mapPut(seen, start, I(1n));
    while (qi < queue.length) {
      var node = queue[qi++];
      order.push(node);
      var base = rt.st.length;
      rt.st.push(node);
      await preludeCall(rt, f.v, 1);
      var nb = rt.st.pop();
      rt.st.length = base;
      var nbs = await materialize(nb, rt);
      for (var i = 0; i < nbs.length; i++) {
        if (mapGet(seen, nbs[i]) === null) { mapPut(seen, nbs[i], I(1n)); queue.push(nbs[i]); }
      }
    }
    rt.st.push(L(order));
  }
  async function op_dfs(rt) {
    var f = pop(rt, "dfs");
    var start = pop(rt, "dfs");
    var seen = mapNew();
    var order = [];
    var stack = [start];
    while (stack.length) {
      var node = stack.pop();
      if (mapGet(seen, node) !== null) continue;
      mapPut(seen, node, I(1n));
      order.push(node);
      var base = rt.st.length;
      rt.st.push(node);
      await preludeCall(rt, f.v, 1);
      var nb = rt.st.pop();
      rt.st.length = base;
      var nbs = await materialize(nb, rt);
      for (var i = nbs.length - 1; i >= 0; i--) stack.push(nbs[i]);
    }
    rt.st.push(L(order));
  }
  async function op_wfind(rt) {
    var pred = pop(rt, "wfind");
    var f = pop(rt, "wfind");
    var start = pop(rt, "wfind");
    var seen = mapNew();
    var queue = [start];
    var qi = 0;
    mapPut(seen, start, I(1n));
    while (qi < queue.length) {
      var node = queue[qi++];
      var base = rt.st.length;
      rt.st.push(node);
      await preludeCall(rt, pred.v, 1);
      var hit = rt.st.pop();
      rt.st.length = base;
      if (truthy(hit)) { rt.st.push(node); return; }
      var base2 = rt.st.length;
      rt.st.push(node);
      await preludeCall(rt, f.v, 1);
      var nb = rt.st.pop();
      rt.st.length = base2;
      var nbs = await materialize(nb, rt);
      for (var i = 0; i < nbs.length; i++) {
        if (mapGet(seen, nbs[i]) === null) { mapPut(seen, nbs[i], I(1n)); queue.push(nbs[i]); }
      }
    }
    rt.st.push(I(0n));
  }

  // ========================== group / agg / chunk =========================
  async function op_group(rt) {
    var f = pop(rt, "group");
    var h = pop(rt, "group");
    var items = await materialize(h, rt);
    var m = mapNew();
    for (var i = 0; i < items.length; i++) {
      var base = rt.st.length;
      rt.st.push(items[i]);
      await preludeCall(rt, f.v, 1);
      var key = rt.st.pop();
      rt.st.length = base;
      var g = mapGet(m, key);
      if (g === null) { g = L([]); mapPut(m, key, g); }
      g.v.push(items[i]);
    }
    rt.st.push(D(m));
  }
  async function op_agg(rt) {
    var f = pop(rt, "agg");
    var h = pop(rt, "agg");
    if (h.t !== "d") die(rt, "AGG: not a dict");
    var m = h.v, r = mapNew();
    for (var q = 0; q < m.cap; q++) if (m.st[q] === 1) {
      var base = rt.st.length;
      rt.st.push(m.v[q]);
      await preludeCall(rt, f.v, 1);
      var v = rt.st.pop();
      rt.st.length = base;
      mapPut(r, m.k[q], v);
    }
    rt.st.push(D(r));
  }
  async function op_chunk(rt) {
    var sz = Number(iOf(pop(rt, "chunk")));
    var h = pop(rt, "chunk");
    if (sz < 1) die(rt, "CHUNK: size < 1");
    if (h.t === "v" && (h.v.kind === "arr" || h.v.kind === "tensor")) {
      var src = h, len = h.v.len, proto = h.v;
      var pa = new Array(Math.ceil(len / sz)), np = 0;
      for (var i = 0; i < len; i += sz) {
        var n = Math.min(sz, len - i);
        var data = new Array(n);
        for (var z = 0; z < n; z++) data[z] = 0n;
        var r2 = V(proto.kind, proto.ety, n, data, 0);
        for (var j = 0; j < n; j++) vwrite(r2, j, vread(src, i + j));
        pa[np++] = r2;
      }
      rt.st.push(L(pa));
      return;
    }
    var items = await materialize(h, rt);
    var pb = new Array(Math.ceil(items.length / sz)), np2 = 0;
    for (var i2 = 0; i2 < items.length; i2 += sz) {
      var n2 = Math.min(sz, items.length - i2);
      pb[np2++] = L(items.slice(i2, i2 + n2));
    }
    rt.st.push(L(pb));
  }

  // ============================== try / retry =============================
  // C try_once: 0-arg call, no drain; success -> [result, 1] (result on
  // bottom), contained die -> [0, 0]. Break/cont still escape the try.
  async function tryOnce(rt, body, sp0) {
    var base = rt.st.length;
    try {
      await preludeCall(rt, body.v, 0);
    } catch (e) {
      if (e === BREAK || e === CONT) throw e;
      rt.st.length = sp0;
      rt.iv.length = 0;
      return [I(0n), I(0n)];
    }
    var r = rt.st.length > base ? rt.st.pop() : I(0n);
    rt.st.length = base;
    return [r, I(1n)];
  }
  async function op_try(rt) {
    var body = pop(rt, "try");
    var sp0 = rt.st.length;
    var res = await tryOnce(rt, body, sp0);
    rt.st.push(L(res));
  }
  async function op_retry(rt) {
    var a = pop(rt, "retry");
    var n = Number(iOf(pop(rt, "retry")));
    for (var k = 0; k <= n; k++) {
      var res = await tryOnce(rt, a, rt.st.length);
      if (res[1].v === 1n) { rt.st.push(L(res)); return; }
    }
    rt.st.push(L([I(0n), I(0n)]));
  }

  // ================================ shell ================================
  // Node-only (child_process), obtained either from the host-injected
  // nkNode.cp param (web/nk.js) or the in-scope require (direct node
  // scripts). Browsers get neither: sh/exec die (documented) and shp
  // degrades to an empty closed chan, the native spawn-fail path.
  var _cp = null;
  function childProc() {
    if (_cp === null) {
      _cp = false;
      if (typeof nkNode !== "undefined" && nkNode && nkNode.cp) _cp = nkNode.cp;
      else if (typeof require === "function") {
        try { _cp = require("child_process"); } catch (e) { _cp = false; }
      }
    }
    return _cp ? _cp : null;
  }
  var SIGMAP = { SIGHUP: 1, SIGINT: 2, SIGQUIT: 3, SIGTRAP: 5, SIGABRT: 6, SIGKILL: 9, SIGTERM: 15 };
  // native uf_wait_status: -1 spawn fail, WEXITSTATUS, 128+signal.
  function shStatus(r, isExec) {
    if (r.error) return isExec ? 127 : -1;
    if (r.status === null) return 128 + (SIGMAP[r.signal] || 0);
    return r.status;
  }
  function op_sh(rt) {
    var cmd = strOf(rt, pop(rt, "sh"));
    var cp = childProc();
    if (!cp) die(rt, "JavaScript target: shell requires node");
    var r = cp.spawnSync("/bin/sh", ["-c", cmd], { encoding: "buffer" });
    if (r.error) die(rt, "SH: fork");
    var out = r.stdout && r.stdout.length ? utf8Dec(r.stdout) : "";
    var err = r.stderr && r.stderr.length ? utf8Dec(r.stderr) : "";
    rt.st.push(L([S(out), S(err), I(BigInt(shStatus(r, 0)))]));
  }
  function op_shp(rt) {
    var cmd = strOf(rt, pop(rt, "shp"));
    var ch = chanCell(64, rt.reg);
    var reg = rt.reg;
    var cp = childProc();
    var t = taskRt(reg);
    var rec = reg.tasks[reg.tasks.length - 1];
    (async function () {
      if (cp) {
        var r = cp.spawnSync("/bin/sh", ["-c", cmd], { encoding: "buffer" });
        if (!r.error) {
          var txt = r.stdout && r.stdout.length ? utf8Dec(r.stdout) : "";
          var lines = lineOf(txt);
          for (var i = 0; i < lines.length; i++) await enqCell(t, ch.v, S(lines[i]));
        }
      }
      ch.v.closed = 1;
      balance(null, ch.v);
    })().then(
      function () { rec.exit = 1; reg.gen++; },
      function (e) { rec.exit = 1; reg.gen++; if (reg.settle) reg.settle(normErr(e)); }
    );
    rt.st.push(ch);
  }
  function op_exec(rt) {
    var h = pop(rt, "exec");
    if (h.t !== "l") die(rt, "EXEC: not a list");
    if (h.v.length === 0) die(rt, "EXEC: empty argv");
    var cp = childProc();
    if (!cp) die(rt, "JavaScript target: shell requires node");
    var av = new Array(h.v.length);
    for (var i = 0; i < h.v.length; i++) av[i] = strOf(rt, h.v[i]);
    var r = cp.spawnSync(av[0], av.slice(1), { encoding: "buffer" });
    /* native execvp inherits the parent fds: the child's writes reach the
       pipe before the parent's block-buffered std streams flush, so queue
       the captured output and prepend it at result time (nkOut/nkErr) */
    if (r.stdout && r.stdout.length) (rt.reg.xout = rt.reg.xout || []).push(utf8Dec(r.stdout));
    if (r.stderr && r.stderr.length) (rt.reg.xerr = rt.reg.xerr || []).push(utf8Dec(r.stderr));
    rt.st.push(I(BigInt(shStatus(r, 1))));
  }
  return {
    I: I, F: F, Y: Y, B: B, S: S, N: N, L: L, D: D, A: A, T: T, V: V,
    makeRt: makeRt, taskRt: taskRt, pump: pump, BREAK: BREAK, CONT: CONT,
    die: die, pop: pop, truthy: truthy, zero: zero,
    asInt: asInt, iOf: iOf, fOf: fOf, toStr: toStr, toNumber: toNumber,
    op_add: op_add, op_sub: op_sub, op_mul: op_mul, op_and: op_and, op_or: op_or,
    op_xor: op_xor, op_shl: op_shl, op_shr: op_shr, op_bnot: op_bnot,
    op_inc: op_inc, op_dec: op_dec, op_pow: op_pow, op_sqrt: op_sqrt,
    op_div: op_div, op_rem: op_rem, op_lt: op_lt, op_gt: op_gt,
    op_lte: op_lte, op_gte: op_gte, op_eq: op_eq, op_seq: op_seq,
    op_sne: op_sne, op_not: op_not, op_orelse: op_orelse, op_drop: op_drop,
    op_cast: op_cast, op_dcast: op_dcast, op_typeof: op_typeof, op_len: op_len,
    op_list: op_list, op_dict: op_dict, op_get: op_get, op_getq: op_getq,
    op_set: op_set, op_del: op_del, op_has: op_has, op_keys: op_keys,
    op_cat: op_cat, op_slice: op_slice, op_push: op_push, op_lpop: op_lpop,
    op_arr: op_arr, op_tensor: op_tensor, op_vget: op_vget, op_vset: op_vset,
    op_vsum: op_vsum, op_vfold: op_vfold,
    op_vemax: op_vemax, op_vemin: op_vemin,
    op_veq: op_veq, op_vlt: op_vlt, op_vgt: op_vgt, op_vge: op_vge,
    op_vle: op_vle, op_vand: op_vand, op_vor: op_vor,
    op_vnot: op_vnot, op_vcount: op_vcount, op_vgather: op_vgather,
    op_vmean: op_vmean, op_vmin: op_vmin, op_vmax: op_vmax,
    op_vmap: op_vmap, op_vargsort: op_vargsort,
    op_vsearchsorted: op_vsearchsorted, op_vwhere: op_vwhere,
    op_transpose: op_transpose,
    op_range: op_range, op_glob: op_glob, op_split: op_split, op_join: op_join,
    op_find: op_find, op_repl: op_repl, op_trim: op_trim, op_up: op_up,
    op_down: op_down, op_starts: op_starts, op_ends: op_ends,
    op_atoi: op_atoi, op_atof: op_atof, op_itoa: op_itoa, op_ftoa: op_ftoa,
    op_match: op_match, op_replace: op_replace, op_rsplit: op_rsplit,
    op_iter: op_iter, op_next: op_next, op_collect: op_collect,
    op_imap: op_imap, op_ifilter: op_ifilter, op_sort: op_sort,
    op_sortkeys: op_sortkeys, op_topn: op_topn, op_rangefold: op_rangefold,
    op_filter: op_filter, op_some: op_some, op_every: op_every,
    op_unique: op_unique, op_flat: op_flat, op_json: op_json,
    op_unjson: op_unjson, op_print: op_print, op_fmt: op_fmt,
    op_dbg: op_dbg, op_check: op_check, op_timer: op_timer, op_memstats: op_memstats,
    op_malloc: op_malloc, op_free: op_free, op_buf: op_buf,
    op_bufcopy: op_bufcopy, op_loadx: op_loadx, op_storex: op_storex,
    op_chan: op_chan, op_enq: op_enq, op_deq: op_deq, op_close: op_close,
    op_atom: op_atom, op_aget: op_aget, op_aset: op_aset, op_aadd: op_aadd,
    op_cas: op_cas, op_spawn: op_spawn,
    op_gc: op_gc, op_obj: op_obj,
    op_argv: op_argv, op_hasargs: op_hasargs, op_argi: op_argi,
    op_now: op_now, op_time: op_time, op_timef: op_timef,
    op_clone: op_clone, op_sizeof: op_sizeof, op_shutdown: op_shutdown,
    op_scan: op_scan, op_cap: op_cap, op_caps: op_caps,
    op_bloom: op_bloom, op_badd: op_badd, op_btest: op_btest,
    op_slurp: op_slurp, op_spit: op_spit, op_mmap: op_mmap,
    op_feach: op_feach, op_ffold: op_ffold, op_fsplit: op_fsplit,
    op_fget: op_fget, op_fatoi: op_fatoi, op_fatof: op_fatof,
    op_fsget: op_fsget, op_fbyte: op_fbyte,
    op_addto: op_addto, op_faddto: op_faddto, op_finc: op_finc,
    op_fmatch: op_fmatch,
    op_bfs: op_bfs, op_dfs: op_dfs, op_wfind: op_wfind,
    op_femit: op_femit,
    op_try: op_try, op_retry: op_retry,
    op_group: op_group, op_agg: op_agg, op_chunk: op_chunk,
    op_sh: op_sh, op_shp: op_shp, op_exec: op_exec,
    P: P, PS: PS, PNUL: PNUL, PCELL: PCELL, PARGV: PARGV, PFILE: PFILE,
    ffiCall: ffiCall, externCell: externCell,
    mapNew: mapNew, mapPut: mapPut,
    materialize: materialize
  };
})();

"##;
