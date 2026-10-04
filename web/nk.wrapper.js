// Nmerkar JavaScript target — the nk compiler as a JS library.
//
//   const nk = require("./nk.js");       // node
//   // browser: globalThis.nk after a <script src="nk.js">
//
//   const prog = await nk.compile(source);   // a handle of the compiled program
//   const r = await prog.run(env);          // {output, err, rc, error?}
//
// env (optional) is the run environment: { argv: [..], stdin: "text",
// fs: { "name": "file text" } }. output/err are the whole run's stdout/
// stderr; rc is the exit code (0 clean, the exit op's value, 1 on a runtime
// error); error carries the die message when rc !== 0.
//
// The line below is replaced by web/build.sh with the base64 of nk.wasm
// (the same Rust compiler as the native nk, built for
// wasm32-unknown-unknown; C-ABI exports from comp/src/wasm_api.rs:
// nk_js_alloc / nk_js_free / nk_js_compile / nk_js_msg_off).
var __NK_WASM_B64 = "__WASM_B64__";

(function (global) {
  "use strict";

  function b64ToBytes(b64) {
    if (typeof Buffer !== "undefined") {
      return new Uint8Array(Buffer.from(b64, "base64"));
    }
    var bin = atob(b64);
    var out = new Uint8Array(bin.length);
    for (var i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }

  // Named wasmModule (not `module`): a local `var module` inside this
  // function would shadow the CommonJS module object the footer needs.
  var wasmModule = new WebAssembly.Module(b64ToBytes(__NK_WASM_B64));
  var ctx = { i: null, ex: null, mem: null };

  function reset() {
    ctx.i = new WebAssembly.Instance(wasmModule, {});
    ctx.ex = ctx.i.exports;
    ctx.mem = ctx.i.exports.memory;
  }
  reset();

  // NUL-terminated UTF-8 string at an absolute linear-memory offset.
  function cstr(view, off, cap) {
    var max = cap !== undefined ? Math.min(view.length, off + cap) : view.length;
    var end = off;
    while (end < max && view[end] !== 0) end++;
    return new TextDecoder().decode(view.subarray(off, end));
  }

  function compileSync(src) {
    // msgOff is read while the instance is alive: on a panic the export call
    // sites are dead, but the old instance's linear memory (ctx.mem) is still
    // readable and the message region is written before the abort. The alloc
    // must come first: it initializes the wasm heap and pins BASE, so
    // nk_js_msg_off returns an absolute offset.
    var mem = ctx.mem;
    var bytes = new TextEncoder().encode(src);
    var p = ctx.ex.nk_js_alloc(bytes.length);
    var msgOff = ctx.ex.nk_js_msg_off();
    new Uint8Array(mem.buffer, p).set(bytes);
    var out;
    try {
      out = ctx.ex.nk_js_compile(p, bytes.length);
    } catch (e) {
      reset();
      var msg = cstr(new Uint8Array(mem.buffer), msgOff, 128 << 10);
      return { error: msg === "" ? "unknown compile error" : msg };
    }
    var view = new Uint8Array(mem.buffer, out);
    var status = view[0];
    var payload = cstr(view, 1);
    ctx.ex.nk_js_free(out);
    ctx.ex.nk_js_free(p);
    if (status === 0) return { js: payload };
    return { error: payload };
  }

  // A Program is a runnable handle of compiled Nmerkar source. run executes
  // the whole program with an optional env ({argv, stdin, fs}) and resolves
  // to {output, err, rc, error?}.
  function Program(js) {
    this.js = js;
  }

  Program.prototype.run = function (env) {
    var api;
    try {
      // A fresh machine per run: the native target is a new process each
      // run, so re-evaluate the emitted program instead of reusing state.
      // The emitted program defines __NK_API in function scope; new
      // Function keeps it out of the global namespace. The new-Function
      // scope has no require (it is not a module), so under node the host
      // hands over child_process via the nkNode param for the shell ops;
      // a browser gets null and sh/exec die as documented.
      var shim = (typeof require === "function" && typeof process !== "undefined")
        ? { cp: require("child_process") } : null;
      api = new Function("nkNode", this.js + "\n;return __NK_API;")(shim);
    } catch (e) {
      return Promise.resolve({
        output: "",
        err: "",
        rc: 1,
        error: "nk: " + (e && e.message ? e.message : String(e))
      });
    }
    try {
      // api.run drives the cooperative scheduler to quiescence and resolves
      // to the result object above; it never rejects.
      return Promise.resolve(api.run(env || {}));
    } catch (e) {
      return Promise.resolve({
        output: api.output(),
        err: api.err(),
        rc: 1,
        error: "nk: " + (e && e.message ? e.message : String(e))
      });
    }
  };

  var nk = {
    compile: function (src) {
      return new Promise(function (resolve, reject) {
        if (typeof src !== "string") {
          reject(new TypeError("nk.compile: source must be a string"));
          return;
        }
        var r = compileSync(src);
        if (r.error !== undefined) reject(new Error("nk: " + r.error));
        else resolve(new Program(r.js));
      });
    }
  };

  if (typeof module !== "undefined" && module.exports) module.exports = nk;
  global.nk = nk;
})(typeof globalThis !== "undefined" ? globalThis : this);
