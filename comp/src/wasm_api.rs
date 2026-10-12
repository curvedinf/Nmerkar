//! WebAssembly API for the JavaScript-target compiler.
//!
//! Exports (C ABI, no imports; the module exports its linear `memory`):
//!
//!   nk_js_alloc(len) -> u32
//!       Bump-allocate `len` bytes in the module's linear memory. The JS
//!       glue copies input bytes into the returned offset.
//!
//!   nk_js_free(ptr)
//!       Return a region allocated by nk_js_alloc / nk_js_compile. Regions
//!       at the bump top are reclaimed immediately; the rest go to a free
//!       list and are reused by later allocations.
//!
//!   nk_js_compile(src, len) -> u32
//!       Compile the UTF-8 Nmerkar source at [src, src+len) to the
//!       standalone JavaScript-target program (crate::compile_js). Returns
//!       a pointer to `[status:u8][payload][NUL]`. status 0: payload is the
//!       emitted JS. status 1: payload is the error message.
//!
//!   nk_js_msg_off() -> u32
//!       Offset of the panic-message region in linear memory.
//!
//! Panic contract: on wasm32-unknown-unknown a panic aborts the instance
//! (this toolchain's panic_unwind crate does not support unwinding, so
//! catch_unwind cannot fire). nk_js_compile installs a panic hook that
//! writes the panic message, NUL-terminated, into the message region
//! before the abort. The JS glue detects the throw at the call site,
//! reads the region, and re-instantiates the module; the old instance is
//! dead and must be dropped.

use std::alloc::{alloc, handle_alloc_error, Layout};
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

// One 16 MiB linear-memory chunk, allocated once and never moved; the
// bump pointer walks it. Chunks from the Rust allocator are stable, so
// offsets handed to JS stay valid for the module's lifetime.
const CHUNK: usize = 16 << 20;
// The last 128 KiB of the chunk is reserved for the panic-message region.
// It is never handed out by nk_js_alloc.
const MSG_SIZE: usize = 128 << 10;

fn msg_off() -> usize {
    CHUNK - MSG_SIZE
}

// Written once at first heap init; the panic hook must read the chunk
// base without taking the HEAP lock (a panic may have happened while the
// lock's frame was abandoned, so the mutex can appear permanently locked).
static BASE: AtomicUsize = AtomicUsize::new(0);

struct Heap {
    base: usize,
    top: usize,
    lens: HashMap<usize, usize>, // offset -> allocated length
    free: Vec<(usize, usize)>,   // (offset, length), not merged
}

static HEAP: Mutex<Option<Heap>> = Mutex::new(None);

fn heap() -> std::sync::MutexGuard<'static, Option<Heap>> {
    let mut g = HEAP.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        let layout = Layout::from_size_align(CHUNK, 8).expect("align");
        let p = unsafe { alloc(layout) };
        if p.is_null() {
            handle_alloc_error(layout);
        }
        BASE.store(p as usize, Ordering::Relaxed);
        *g = Some(Heap { base: p as usize, top: 0, lens: HashMap::new(), free: Vec::new() });
    }
    g
}

fn panic_hook(info: &std::panic::PanicHookInfo<'_>) {
    // Diagnostic payloads (user compile errors) carry their message directly;
    // legacy `panic!` payloads are the plain message (no "panicked at"
    // decoration).
    let msg: String = crate::diag::payload_message(info.payload())
        .unwrap_or_else(|| "unknown panic".to_string());
    let base = BASE.load(Ordering::Relaxed);
    if base == 0 {
        return; // heap never started; nowhere to write
    }
    let n = msg.len().min(MSG_SIZE - 1);
    let bytes = msg.as_bytes()[..n].as_ptr();
    unsafe {
        std::ptr::copy_nonoverlapping(bytes, (base + msg_off()) as *mut u8, n);
        *((base + msg_off() + n) as *mut u8) = 0;
    }
}

#[no_mangle]
pub extern "C" fn nk_js_alloc(len: u32) -> u32 {
    let n = len as usize;
    let mut h = heap();
    let h = h.as_mut().expect("heap initialized");
    if let Some(i) = h.free.iter().position(|&(_, l)| l >= n) {
        let (off, l) = h.free.remove(i);
        if l > n + 1024 {
            h.free.push((off + n, l - n));
        }
        h.lens.insert(off, n);
        return (h.base + off) as u32;
    }
    if h.top + n > CHUNK - MSG_SIZE {
        panic!("nk_js_alloc: out of linear memory ({n} bytes requested)");
    }
    let off = h.top;
    h.top += n;
    h.lens.insert(off, n);
    (h.base + off) as u32
}

#[no_mangle]
pub extern "C" fn nk_js_free(ptr: u32) {
    let mut h = heap();
    let h = h.as_mut().expect("heap initialized");
    let Some(l) = h.lens.remove(&(ptr as usize)) else {
        return; // never handed out (or double free): ignore
    };
    if ptr as usize + l as usize == h.top {
        h.top = ptr as usize; // top reclaim: nothing to book
    } else {
        h.free.push((ptr as usize, l as usize));
    }
}

/// Offset (in the module's linear memory) of the panic-message region,
/// readable by the glue after a failed nk_js_compile aborts the instance.
#[no_mangle]
pub extern "C" fn nk_js_msg_off() -> u32 {
    let base = BASE.load(Ordering::Relaxed);
    (base + msg_off()) as u32
}

#[no_mangle]
pub extern "C" fn nk_js_compile(src: u32, len: u32) -> u32 {
    // Install the message hook before the first pipeline run. The hook
    // never allocates from or locks the HEAP; it only writes the fixed
    // message region.
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        std::panic::set_hook(Box::new(panic_hook));
    });
    let bytes = unsafe { std::slice::from_raw_parts(src as *const u8, len as usize) };
    let finish_error = |msg: &str| -> u32 {
        let mut buf = Vec::with_capacity(1 + msg.len() + 1);
        buf.push(1);
        buf.extend_from_slice(msg.as_bytes());
        buf.push(0);
        let ptr = nk_js_alloc(buf.len() as u32);
        unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), ptr as *mut u8, buf.len()) };
        ptr
    };
    let text = match std::str::from_utf8(bytes) {
        Ok(t) => t,
        Err(_) => return finish_error("source is not valid UTF-8"),
    };
    // If this toolchain gains wasm32 unwinding, this path handles errors
    // without the hook; on 1.90 it cannot fire (see module docs).
    let compiled = catch_unwind(AssertUnwindSafe(|| crate::compile_js(text)));
    let out: (u8, String) = match compiled {
        Ok(js) => (0, js),
        Err(payload) => (1, payload_msg(payload)),
    };
    let mut buf = Vec::with_capacity(1 + out.1.len() + 1);
    buf.push(out.0);
    buf.extend_from_slice(out.1.as_bytes());
    buf.push(0);
    let ptr = nk_js_alloc(buf.len() as u32);
    unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), ptr as *mut u8, buf.len()) };
    ptr
}

fn payload_msg(p: Box<dyn std::any::Any + Send>) -> String {
    crate::diag::payload_message(p.as_ref()).unwrap_or_else(|| "compile failed".to_string())
}
