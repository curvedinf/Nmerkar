Warning: truncated output (original token count: 50170)
Total output lines: 3908

// ---------------- C prelude (v10) ----------------
pub const PRELUDE: &str = r#"
#include <stdint.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include <math.h>
#include <time.h>
#include <unistd.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <sched.h>
#include <setjmp.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#ifdef _WIN32
#include <process.h>
#else
#include <sys/wait.h>
#include <fnmatch.h>
#endif

/* v10 type tags (SPEC_v10_proposal.md): 0 int, 1 float, 2 ptr, 3 byte,
   4 void, 5 arr, 6 tensor, 7 list, 8 dict, 9 str, 10 chan, 11 atom,
   12 buf, 13 obj, 14 bitmap, 15 time, 16 dur, 17 bloom, 18 iter */
enum { T_INT=0, T_FLOAT=1, T_PTR=2, T_BYTE=3, T_TIME=15, T_DUR=16 };
enum { HT_ARR=5, HT_TENSOR=6, HT_DYN=7, HT_MAP=8, HT_STR=9, HT_RING=10, HT_ATOM=11, HT_BUF=12, HT_OBJ=13, HT_BITMAP=14, HT_BLOOM=17, HT_ITER=18, HT_SET=19, HT_MAT=20 };
typedef struct { int tag; int64_t i; } Cell;

/* GC header prefix shared by every tagged object: gc_next links the global
   allocation list; gc_flags holds the mark bit, pin bit, mmap bit and the
   allocation sequence (objects younger than a collection's start are never
   swept, which closes the alloc-then-publish race window). */
#define UFHDR void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety
#define GCF_MARK 1
#define GCF_PINNED 2
#define GCF_MMAP 4
#define GCF_SEQSHIFT 8
typedef struct { UFHDR; char data[]; } Hdr;
typedef struct { UFHDR; uint64_t cap; Cell data[]; } Dyn;
typedef struct { UFHDR; uint64_t cap; Cell* keys; Cell* vals; unsigned char* st; } Map;
typedef struct { UFHDR; uint64_t cap; Cell* buf; uint64_t head; uint64_t tail; pthread_mutex_t mu; pthread_cond_t notfull; pthread_cond_t notempty; int closed; } Ring;
typedef struct { UFHDR; _Atomic int64_t v; } Atom;
typedef struct { UFHDR; uint64_t mlen; const char* mdata; char data[]; } Str; /* mlen>0: mmap'd (mdata, munmap on sweep); else inline data[] */
typedef struct { UFHDR; uint64_t words[]; } Bitmap; /* len = bit count, LSB-first u64 words */
typedef struct { UFHDR; int k; uint64_t words[]; } Bloom; /* len = bit count */
enum { IT_LIST=0, IT_ARR, IT_DICT, IT_STR, IT_CHAN, IT_BITMAP, IT_MAP, IT_FILTER };
typedef struct { UFHDR; Cell src; int kind; int64_t idx; Cell f; Cell g; } Iter;
static char* uf_data(Hdr*a){ return a->tag==HT_DYN ? (char*)((Dyn*)a)->data : (char*)a->data; }

/* v11: per-label local frame sizes. Indexed by instruction PC; zero means
   the label has no locals. Populated by the generated code's uf_init_locals(). */
static long* uf_local_counts; /* [pc] = frame size */

/* per-task execution context: each weave task / spawn runs with its own stacks.
   loops: dynamic loop-frame stack for BREAK/CONT unwinding.
   locals: v11 flat array of local-variable cells; local_base is the start of
   the current call's frame. local_frames/local_fsp save/restore local_base
   across CALL/RET boundaries. */
typedef struct { const void* end; const void* cont; long cspl; } UfLoop;
typedef struct CtxS {
  Cell* ds; long sp; long dcap;
  const void** cs; long csp; long ccap;
  long* rsps; /* parallel to cs: the caller's data-stack sp saved per call */
  UfLoop loops[64]; long lsp;
  Cell* locals; long local_base; long local_cap;
  long* local_frames; long local_fsp; long local_fcap;
  long* call_pcs; long call_csp; long call_ccap; /* debug: callee PCs parallel to cs[] */
  struct CtxS* gc_prev;
} Ctx;

/* weave job: task array + scheduler entry. Defined early so op_shutdown can
   set the graceful-shutdown flag. */
typedef void(*UfRun)(Ctx*,long);
typedef struct WeaveTaskS WeaveTask;
typedef struct WeaveJobS { WeaveTask* ts; int n; UfRun run; _Atomic int shutdown; } WeaveJob;

static void die(const char*m);
static _Thread_local const char* uf_cur_op;
static void nk_run(Ctx*cx, long pc);
static _Thread_local const void* uf_entry_addr;
static void uf_call_addr(Ctx*cx, const void* a, long frame, long entry_pc, long nargs){
  if(cx->csp>=cx->ccap){char _b[128];snprintf(_b,sizeof(_b),"call stack overflow in %s (csp=%ld, cap=%ld)",uf_cur_op,cx->csp,cx->ccap);die(_b);}
  /* save the pre-argument data-stack pointer: the callee's param pops guard
     against it, and its RET drains back to it */
  long _sp0 = cx->sp - nargs; if(_sp0 < 0) _sp0 = 0;
  cx->rsps[cx->csp]=_sp0; cx->cs[cx->csp++]=0; cx->local_frames[cx->local_fsp++]=cx->local_base; cx->call_pcs[cx->call_csp++]=entry_pc; cx->local_base+=frame; uf_entry_addr=a; nk_run(cx,-1); cx->local_base=cx->local_frames[--cx->local_fsp]; cx->call_csp--; }
/* push a continuation with its saved caller-sp; checked against cs capacity */
static inline void uf_cspush(Ctx*cx, const void* k, long sp0){
  if(cx->csp>=cx->ccap){char _b[128];snprintf(_b,sizeof(_b),"call stack overflow in %s (csp=%ld, cap=%ld)",uf_cur_op,cx->csp,cx->ccap);die(_b);}
  cx->rsps[cx->csp]=sp0; cx->cs[cx->csp++]=k;
}

/* ---- error containment (try/retry): die unwinds to the nearest setjmp
   checkpoint; with no checkpoint die is fatal, as before ---- */
typedef struct UfTry { jmp_buf jb; struct UfTry* prev; long sp; long csp; long local_base; long local_fsp; } UfTry;
static _Thread_local UfTry* uf_try_top = 0;
static _Thread_local void* uf_cur_task; /* WeaveTask* for debug counters */
static _Thread_local Ctx* uf_current_ctx = 0;
static _Thread_local const char* uf_cur_op = "<startup>";
static int uf_debug_mode = 0;
static const char** uf_labnames; static long uf_labnames_n;
static const char*** uf_ln_tab; static long* uf_ln_cnt;
static const char** uf_vnames;
/* Forward declarations for crash dump functions */
static int uf_is_str(Cell c);
static const char* uf_sptr(Cell c);
static inline double uf_fbits(int64_t i);
static inline double uf_f(Cell c);
static Cell** uf_var_roots; static long uf_nvar_roots;

static void uf_dump_cell(Cell c){
  if(c.tag==T_FLOAT) fprintf(stderr,"%g",uf_f(c));
  else if(c.tag==T_PTR && c.i && uf_is_str(c)) { const char*s=uf_sptr(c); fprintf(stderr,"\"%s\"",s?s:"<null>"); }
  else if(c.tag==T_PTR && c.i) fprintf(stderr,"<ptr %p>",(void*)c.i);
  else fprintf(stderr,"%lld",(long long)c.i);
}

static void uf_crash_dump(Ctx*cx){
  fprintf(stderr,"\n--- nk crash dump ---\n");
  fprintf(stderr,"  call stack:\n");
  for(long i=cx->call_csp-1;i>=0;i--){
    long pc=cx->call_pcs[i];
    const char* nm = (pc>=0&&pc<uf_labnames_n)?uf_labnames[pc]:0;
    fprintf(stderr,"    #%ld  %s (pc=%ld)\n", cx->call_csp-1-i, nm?nm:"<unknown>", pc);
  }
  fprintf(stderr,"  locals:\n");
  for(long i=cx->call_csp-1;i>=0;i--){
    long pc=cx->call_pcs[i];
    /* local_frames has one extra entry (the nk_run entry frame) that
       has no corresponding call_pcs entry, so call_pcs[i] maps to
       local_frames[i+1] as the saved local_base before this frame's bump.
       The frame's actual locals start at saved_base + frame_size. */
    long fi = i+1;
    long fsize = (pc>=0&&pc<uf_labnames_n)?uf_ln_cnt[pc]:0;
    long base = (fi < cx->local_fsp) ? (cx->local_frames[fi] + fsize) : cx->local_base;
    long cnt = fsize;
    for(long s=0;s<cnt;s++){
      fprintf(stderr,"    #%ld %s = ", cx->call_csp-1-i, uf_ln_tab[pc][s]);
      uf_dump_cell(cx->locals[base+s]);
      fprintf(stderr,"\n");
    }
  }
  fprintf(stderr,"  shared:\n");
  for(long i=0;i<uf_nvar_roots;i++){
    fprintf(stderr,"    %s = ", uf_vnames?uf_vnames[i]:"<?>");
    uf_dump_cell(*uf_var_roots[i]);
    fprintf(stderr,"\n");
  }
  fprintf(stderr,"--- end crash dump ---\n\n");
}

static void die(const char*m){
  if(uf_try_top){ UfTry*t=uf_try_top; longjmp(t->jb,1); }
  if(uf_debug_mode && uf_current_ctx) uf_crash_dump(uf_current_ctx);
  fprintf(stderr,"nk: %s\n",m); exit(1);
}

/* ================= sandbox capability state =================
   Defaults are unrestricted; the compiler bakes assignments into main()
   (see sandbox::c_bake). uf_sb_caps order mirrors CAP_NAMES in sandbox.rs:
   fs.read fs.write proc ffi.use ffi.import raw.syscall raw.mem host.argv */
long uf_sb_on=0;
const char* uf_sb_policy="none";
long uf_sb_caps[8]={1,1,1,1,1,1,1,1};
const char* uf_ws_roots[16]; long uf_ws_nroots=0;
const char* uf_device="auto";
const char* uf_mod_allow[32]; long uf_mod_allow_n=-1; /* -1 = all modules allowed */
const char* uf_mod_deny[32]; long uf_mod_deny_n=0;
static const char* uf_cap_names[8]={"fs.read","fs.write","proc","ffi.use","ffi.import","raw.syscall","raw.mem","host.argv"};
static long uf_cap_index(const char*n){ for(int i=0;i<8;i++) if(!strcmp(n,uf_cap_names[i]))return i; return -1; }

/* sandbox workspace gate: resolve the path (its parent for not-yet-existing
   write targets) and require it under a workspace root. No-op when the
   sandbox or workspace enforcement is off. */
static void uf_fs_gate(const char* path,int write){
  (void)write;
  if(!uf_sb_on||uf_ws_nroots<=0)return;
  char buf[4096],pbuf[4096],msg[8192];
  const char* r=realpath(path,buf);
  if(!r){
    snprintf(pbuf,sizeof(pbuf),"%s",path);
    char* slash=strrchr(pbuf,'/');
    if(slash&&slash!=pbuf)*slash=0;
    else if(!slash)snprintf(pbuf,sizeof(pbuf),".");
    r=realpath(pbuf,buf);
  }
  if(r){
    for(long i=0;i<uf_ws_nroots;i++){
      size_t rl=strlen(uf_ws_roots[i]);
      if(strncmp(r,uf_ws_roots[i],rl)==0&&(r[rl]=='/'||r[rl]==0))return;
    }
  }
  int n=snprintf(msg,sizeof(msg),"sandbox: path '%s' (resolved '%s') is outside the workspace roots (policy '%s'): ",
                 path,r?r:"?",uf_sb_policy);
  for(long i=0;i<uf_ws_nroots&&n>0&&n<(int)sizeof(msg)-256;i++)
    n+=snprintf(msg+n,sizeof(msg)-n,"%s%s",i?", ":"",uf_ws_roots[i]);
  die(msg);
}

/* ================= garbage collector: malloc-based mark-sweep with hash set.
   Every allocation goes through malloc + linked list + address hash set.
   In single-threaded mode (default) no mutex is needed; when threads are
   spawned uf_gc_mt is set to 1 and the mutex is taken on the alloc path. */
static void* uf_gc_list; /* linked list of all allocations */
static _Atomic uint64_t uf_gc_seq = 1;
static _Atomic int uf_gc_mt = 0; /* set to 1 when threads are spawned */
static uint64_t uf_gc_bytes_since, uf_gc_threshold = 1<<20, uf_gc_live;
static int uf_gc_on = 1;
static pthread_mutex_t uf_gc_mu = PTHREAD_MUTEX_INITIALIZER;
/* address hash set for all allocated objects */
static void** uf_gc_set; static uint64_t uf_gc_setcap, uf_gc_setlen;
/* context registry: every Ctx's data stack is a precise root set */
#define UF_MAXCTX 512
static Ctx* uf_ctxs[UF_MAXCTX]; static _Atomic int uf_nctxs;
static void uf_gc_set_insert(void* p);
static void uf_gc_set_grow(void){
  uint64_t oc=uf_gc_setcap; void** os=uf_gc_set;
  uf_gc_setcap = oc? oc*2 : 256; uf_gc_setlen = 0;
  uf_gc_set = (void**)calloc(uf_gc_setcap, sizeof(void*)); if(!uf_gc_set)die("out of memory");
  for(uint64_t i=0;i<oc;i++) if(os[i]&&os[i]!=(void*)1) uf_gc_set_insert(os[i]);
  free(os);
}
/* fast inline hash for pointer → set slot */
static inline uint64_t uf_gc_hash(void* p){
  return (((uint64_t)p >> 4) * 11400714819323198485ULL >> 32) % uf_gc_setcap;
}
static void uf_gc_set_insert(void* p){
  if((uf_gc_setlen+1)*10 >= uf_gc_setcap*7) uf_gc_set_grow();
  uint64_t i = uf_gc_hash(p);
  while(uf_gc_set[i] && uf_gc_set[i]!=p) i=(i+1)%uf_gc_setcap;
  if(!uf_gc_set[i]){ uf_gc_set[i]=p; uf_gc_setlen++; }
}
/* fast-path insert for uf_gc_alloc: check load factor, then direct slot write */
static void uf_gc_set_insert_fast(void* p){
  if((uf_gc_setlen+1)*10 >= uf_gc_setcap*7) uf_gc_set_grow();
  uint64_t i = uf_gc_hash(p);
  if(__builtin_expect(!uf_gc_set[i],1)){ uf_gc_set[i]=p; uf_gc_setlen++; return; }
  if(uf_gc_set[i]==p) return;
  while(uf_gc_set[i] && uf_gc_set[i]!=p) i=(i+1)%uf_gc_setcap;
  if(!uf_gc_set[i]){ uf_gc_set[i]=p; uf_gc_setlen++; }
}
static Ctx main_cx_store; /* fwd: defined fully below */
/* uf_gc_find inline cache: 4-entry LRU of recently validated pointers.
   Hot loops access the same 1-3 dict handles millions of times;
   the cache eliminates the hash+probe for these repeat lookups. */
static _Thread_local void* uf_gc_cache[4] = {0,0,0,0};
static inline Hdr* uf_gc_find(void* p){
  if(!p || p==(void*)1) return 0;
  /* inline cache: check 4 recently-seen pointers */
  if(p==uf_gc_cache[0]||p==uf_gc_cache[1]||p==uf_gc_cache[2]||p==uf_gc_cache[3]) return (Hdr*)p;
  if(!uf_gc_setcap) return 0;
  uint64_t i = ((uint64_t)p >> 4) * 11400714819323198485ULL >> 32; i %= uf_gc_setcap;
  while(uf_gc_set[i]){ if(uf_gc_set[i]==p){
    /* cache miss → insert: shift down, put new entry at slot 0 */
    uf_gc_cache[3]=uf_gc_cache[2]; uf_gc_cache[2]=uf_gc_cache[1]; uf_gc_cache[1]=uf_gc_cache[0]; uf_gc_cache[0]=p;
    return (Hdr*)p;
  } i=(i+1)%uf_gc_setcap; }
  return 0;
}
/* context registry: every Ctx's data stack is a precise root set */
static void ctx_register(Ctx*c){ pthread_mutex_lock(&uf_gc_mu); int i=uf_nctxs; if(i<UF_MAXCTX){ uf_ctxs[i]=c; uf_nctxs=i+1; } pthread_mutex_unlock(&uf_gc_mu); }
static void ctx_unregister(Ctx*c){ pthread_mutex_lock(&uf_gc_mu); for(int i=0;i<uf_nctxs;i++) if(uf_ctxs[i]==c){ uf_ctxs[i]=uf_ctxs[uf_nctxs-1]; uf_nctxs--; break; } pthread_mutex_unlock(&uf_gc_mu); }
/* variable roots, registered by generated code */
static void uf_gc_setroots(Cell** r, long n){ uf_var_roots=r; uf_nvar_roots=n; }

/* ================= v14 shared variables =================
   Plain-name vars resolved shared by the compile-time scope pass are stored
   as SEQLOCKED Cells: every read is a consistent snapshot, every write is
   atomic, and `x++`/`x+=` compile to a writer-locked read-modify-write.
   Portable by construction: only C11 <stdatomic.h> atomics — no platform
   intrinsics, no torn 16-byte Cells, works anywhere the runtime already
   compiles (same dependency set as the GC's atomics).
   Memory model: writer lock acquire/release; reader retries on odd/changed
   sequence. Cross-thread visibility is sequentially consistent enough for
   the "transparent shared state" contract; racing RMWs serialize on the
   writer lock, so x+= under concurrency loses no updates. */
typedef struct UFShVar { _Atomic uint64_t seq; Cell v; } UFShVar;
static double uf_f(Cell c);
static Cell uf_mkf(double v);
static Cell uf_mki(int64_t v);
static Cell uf_sh_get(UFShVar*s){
  uint64_t a,b; Cell c;
  do{
    a=atomic_load_explicit(&s->seq,memory_order_acquire);
    if(a&1ULL) continue;                 /* writer in flight */
    c=s->v;                              /* may tear — validated below */
    b=atomic_load_explicit(&s->seq,memory_order_acquire);
  }while(a!=b);
  return c;
}
static void uf_sh_set(UFShVar*s,Cell c){
  uint64_t exp=atomic_load_explicit(&s->seq,memory_order_relaxed);
  for(;;){
    if(exp&1ULL){ exp=atomic_load_explicit(&s->seq,memory_order_relaxed); continue; }
    if(atomic_compare_exchange_weak_explicit(&s->seq,&exp,exp+1,memory_order_acq_rel,memory_order_acquire)) break;
  }
  s->v=c;
  atomic_store_explicit(&s->seq,exp+2,memory_order_release);
}
/* atomic read-modify-write: v = f(v, delta) under the writer lock;
   numeric add for int/float cells, falls back to overwrite for others */
static Cell uf_sh_add(UFShVar*s,Cell d){
  uint64_t exp=atomic_load_explicit(&s->seq,memory_order_relaxed);
  for(;;){
    if(exp&1ULL){ exp=atomic_load_explicit(&s->seq,memory_order_relaxed); continue; }
    if(atomic_compare_exchange_weak_explicit(&s->seq,&exp,exp+1,memory_order_acq_rel,memory_order_acquire)) break;
  }
  Cell c=s->v, r;
  if(c.tag==1||d.tag==1) r=uf_mkf(uf_f(c)+uf_f(d));
  else r=uf_mki(c.i+d.i);
  s->v=r;
  atomic_store_explicit(&s->seq,exp+2,memory_order_release);
  return r;
}
static Cell uf_sh_add1(UFShVar*s){ Cell d; d.tag=0; d.i=1; return uf_sh_add(s,d); }
/* GC roots for shared vars: snapshot each consistently (collect runs at a
   safepoint; concurrent writers are mid-seqlock and retry until even) */
static UFShVar* uf_shvars_tbl[1024]; static long uf_nshvars;
static void uf_gc_setshared(UFShVar** t, long n){ for(long i=0;i<n&&i<1024;i++) uf_shvars_tbl[i]=t[i]; uf_nshvars=n; }
/* tmp roots for builder ops (in-progress containers while they grow).
   v13.2: PER-THREAD stacks. The old shared counter broke two ways under
   weave workers: (a) a thread's publish window (counter bumped before the
   slot write) let a concurrent collect miss a just-protected operand and
   sweep it mid-loop; (b) cross-thread unprotects popped OTHER threads'
   entries (LIFO only holds per-thread). Each thread now owns a fixed slot
   array published with release stores; collectors acquire-load the count.
   Slots are zeroed on pop so a concurrent marker never marks stale values. */
#define UF_MAXTMP 1024
typedef struct UF_TR { struct UF_TR* next; pthread_t tid; void*** slots; _Atomic int n; } UF_TR;
static UF_TR* uf_trs; static pthread_mutex_t uf_tr_mu = PTHREAD_MUTEX_INITIALIZER;
static _Thread_local UF_TR* uf_tr_mine;
static void uf_tr_init(void){
  if(uf_tr_mine) return;
  UF_TR* t=(UF_TR*)calloc(1,sizeof(UF_TR)); t->slots=(void***)calloc(UF_MAXTMP,sizeof(void**)); t->tid=pthread_self();
  pthread_mutex_lock(&uf_tr_mu); t->next=uf_trs; uf_trs=t; pthread_mutex_unlock(&uf_tr_mu);
  uf_tr_mine=t;
}
#define UF_PROTECT(pp) do{ uf_tr_init(); UF_TR* _t=uf_tr_mine; int _i=atomic_load_explicit(&_t->n,memory_order_relaxed); \
  if(_i<UF_MAXTMP){ _t->slots[_i]=(void**)(pp); atomic_store_explicit(&_t->n,_i+1,memory_order_release); } }while(0)
#define UF_UNPROTECT() do{ UF_TR* _t=uf_tr_mine; if(_t){ int _i=atomic_load_explicit(&_t->n,memory_order_relaxed); \
  if(_i>0){ atomic_store_explicit(&_t->n,_i-1,memory_order_release); _t->slots[_i-1]=0; } } }while(0)
static void uf_mark_cell(Cell c);
static void uf_mark_obj(Hdr* h);
static void uf_mark_ptr(void* p){
  Hdr* h = uf_gc_find(p);
  if(h) uf_mark_obj(h);
}
static void uf_mark_cell(Cell c){ if(c.tag==T_PTR && c.i) uf_mark_ptr((void*)c.i); }
static void uf_mark_obj(Hdr* h){
  if(h->gc_flags & GCF_MARK) return;
  h->gc_flags |= GCF_MARK;
  if(h->gc_parent) uf_mark_ptr(h->gc_parent);
  switch(h->tag){
    case HT_DYN: { Dyn* d=(Dyn*)h; for(uint64_t i=0;i<d->len;i++) uf_mark_cell(d->data[i]); break; }
    case HT_MAP: { Map* m=(Map*)h; for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1){ uf_mark_cell(m->keys[i]); uf_mark_cell(m->vals[i]); } break; }
    case HT_SET: { Map* m=(Map*)h; for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1) uf_mark_cell(m->keys[i]); break; }
    case HT_RING: { Ring* r=(Ring*)h; for(uint64_t j=0;j<r->len;j++) uf_mark_cell(r->buf[(r->head+j)%r->cap]); break; }
    case HT_OBJ: { uint64_t n=h->esz/8; Cell* f=(Cell*)h->data; for(uint64_t i=0;i<n;i++) uf_mark_cell(f[i]); break; }
    case HT_ITER: { Iter* it=(Iter*)h; uf_mark_cell(it->src); uf_mark_cell(it->f); uf_mark_cell(it->g); break; }
    default: break; /* arr/tensor/str/bitmap/bloom/atom/buf: leaf bytes */
  }
}
struct WeaveJobS; static struct WeaveJobS* uf_active_job;
static void uf_weave_mark(struct WeaveJobS* j);
static void uf_gc_free_obj(Hdr* h){
  /* v15: large ARR/TENSOR blocks are whole-block mmap'd (aligned VMA gets
     real THP; malloc's offset mmap does not) — munmap instead of free */
  if((h->gc_flags&GCF_MMAP)&&(h->tag==HT_TENSOR||h->tag==HT_ARR)){
    munmap(h,sizeof(Hdr)+(size_t)h->len*(size_t)h->esz); return;
  }
  switch(h->tag){
    case HT_MAP: case HT_SET: { Map* m=(Map*)h; free(m->keys); free(m->vals); free(m->st); break; }
    case HT_RING: { Ring* r=(Ring*)h; pthread_mutex_destroy(&r->mu); pthread_cond_destroy(&r->notfull); pthread_cond_destroy(&r->notempty); free(r->buf); break; }
    case HT_STR: { Str* s=(Str*)h; if(s->mlen) munmap((void*)s->mdata,(size_t)s->mlen); else if(s->gc_parent==(void*)1) free((void*)s->mdata); break; }
    default: break;
  }
  free(h);
}
static void uf_gc_collect(void){
  pthread_mutex_lock(&uf_gc_mu);
  uint64_t start_seq = uf_gc_seq;
  /* mark all roots */
  for(long i=0;i<uf_nvar_roots;i++) uf_mark_cell(*uf_var_roots[i]);
  for(long i=0;i<uf_nshvars;i++) uf_mark_cell(uf_sh_get(uf_shvars_tbl[i]));
  int nc = uf_nctxs;
  for(int i=0;i<nc;i++){ Ctx* c=uf_ctxs[i]; for(long s=0;s<c->sp;s++) uf_mark_cell(c->ds[s]);
    /* v13.2: mark the full locals array, not just [0,local_base). local_base
       is the START of the innermost frame, so the old scan skipped every live
       local of the running frame (invisible only because runners pinned
       --gc-threshold above total allocation). Slots above the live frames
       hold stale Cells; marking them is safe (freed objects are absent from
       uf_gc_set) and only over-retains until the slot is reused. */
    for(long s=0;s<c->local_cap;s++) uf_mark_cell(c->locals[s]);
  }
  pthread_mutex_lock(&uf_tr_mu);
  for(UF_TR* t=uf_trs;t;t=t->next){
    if(!pthread_equal(t->tid,pthread_self()) && pthread_kill(t->tid,0)==ESRCH) continue; /* dead thread: its slots are moot */
    int nt=atomic_load_explicit(&t->n,memory_order_acquire); if(nt>UF_MAXTMP)nt=UF_MAXTMP;
    for(int i=0;i<nt;i++){ void** pp=t->slots[i]; if(pp&&*pp) uf_mark_ptr(*pp); }
  }
  pthread_mutex_unlock(&uf_tr_mu);
  if(uf_active_job) uf_weave_mark(uf_active_job);
  /* sweep gc_list: free unmarked, unpinned objects with seq < start_seq */
  void** pp = &uf_gc_list;
  while(*pp){
    Hdr* h=(Hdr*)*pp;
    if(!(h->gc_flags&GCF_PINNED) && !(h->gc_flags&GCF_MARK) && ((h->gc_flags>>GCF_SEQSHIFT) < start_seq)){
      *pp = h->gc_next; uf_gc_free_obj(h);
    } else {
      h->gc_flags &= ~(uint64_t)GCF_MARK; pp = &h->gc_next;
    }
  }
  /* rebuild hash set from live objects (eliminates tombstones from freed slots) */
  uf_gc_setlen = 0;
  if(uf_gc_set) memset(uf_gc_set, 0, uf_gc_setcap * sizeof(void*));
  { void* q = uf_gc_list;
    while(q){ uf_gc_set_insert(q); q = ((Hdr*)q)->gc_next; }
  }
  uf_gc_bytes_since = 0;
  /* invalidate find cache — freed objects may still be cached */
  uf_gc_cache[0]=uf_gc_cache[1]=uf_gc_cache[2]=uf_gc_cache[3]=0;
  pthread_mutex_unlock(&uf_gc_mu);
}
/* v15: large GC blocks (tensor/array data) opt into THP — the kernel is in
   madvise mode, so without this every multi-hundred-MB region output faults
   at 4K granularity and the copy-back streams under TLB pressure. */
#ifdef __linux__
#include <sys/mman.h>
static void uf_gc_thp(void* p, size_t sz){
  if(sz >= (size_t)2<<20) madvise(p, sz, MADV_HUGEPAGE);
}
#else
static void uf_gc_thp(void* p, size_t sz){ (void)p; (void)sz; }
#endif
static void* uf_gc_alloc(size_t sz, int align){
  sz = sz ? sz : 1;
  if(uf_gc_on && uf_gc_bytes_since + sz > uf_gc_threshold) uf_gc_collect();
  void* p = NULL;
  if(align>0){ if(posix_memalign(&p,(size_t)align,sz))die("alloc failed"); }
  else { p=malloc(sz); }
  if(!p)die("out of memory");
  uf_gc_thp(p,sz);
  memset(p,0,sz);
  Hdr* h=(Hdr*)p;
  h->gc_flags = ((uint64_t)atomic_fetch_add(&uf_gc_seq,1))<<GCF_SEQSHIFT;
  uf_gc_bytes_since += sz;
  if(atomic_load(&uf_gc_mt)){ pthread_mutex_lock(&uf_gc_mu); h->gc_next=uf_gc_list; uf_gc_list=p; uf_gc_set_insert_fast(p); pthread_mutex_unlock(&uf_gc_mu); }
  else { h->gc_next=uf_gc_list; uf_gc_list=p; uf_gc_set_insert_fast(p); }
  return p;
}
/* v13.2: allocation whose data region the caller fully overwrites (e.g.
   elementwise tensor results). Skips the whole-block memset — for a 16MB
   tensor result that halves write traffic and the faults it causes. The
   Hdr itself is still zeroed (gc_parent etc.). */
static void* uf_gc_alloc_nz(size_t sz, int align){
  sz = sz ? sz : 1;
  if(uf_gc_on && uf_gc_bytes_since + sz > uf_gc_threshold) uf_gc_collect();
  void* p = NULL;
  if(align>0){ if(posix_memalign(&p,(size_t)align,sz))die("alloc failed"); }
  else { p=malloc(sz); }
  if(!p)die("out of memory");
  uf_gc_thp(p,sz);
  memset(p,0,sizeof(Hdr));
  Hdr* h=(Hdr*)p;
  h->gc_flags = ((uint64_t)atomic_fetch_add(&uf_gc_seq,1))<<GCF_SEQSHIFT;
  uf_gc_bytes_since += sz;
  if(atomic_load(&uf_gc_mt)){ pthread_mutex_lock(&uf_gc_mu); h->gc_next=uf_gc_list; uf_gc_list=p; uf_gc_set_insert_fast(p); pthread_mutex_unlock(&uf_gc_mu); }
  else { h->gc_next=uf_gc_list; uf_gc_list=p; uf_gc_set_insert_fast(p); }
  return p;
}
/* v15: allocate the block for a large tensor/array result. >=2MB blocks use
   a fresh page-aligned mmap with MADV_HUGEPAGE — the aligned VMA is the
   case where madvise-mode THP actually delivers 2MB pages, removing the
   4K fault storm on region copy-outs. Small blocks stay on malloc. The
   caller MUST set tag/len/esz/ety (arr semantics) and not realloc. */
static void* uf_gc_arr_block_t(size_t nb, uint64_t tag){
  size_t sz=sizeof(Hdr)+nb;
  if(uf_gc_on && uf_gc_bytes_since + sz > uf_gc_threshold) uf_gc_collect();
  void* p;
  int mapped=0;
  /* only tags whose byte size reconstructs as len*esz (see free path);
     HT_MAT's esz is a row count, so it stays on malloc */
  if(sz>=(size_t)2<<20&&(tag==HT_TENSOR||tag==HT_ARR)){
    p=mmap(NULL,sz,PROT_READ|PROT_WRITE,MAP_PRIVATE|MAP_ANONYMOUS,-1,0);
    if(p!=MAP_FAILED){
#ifdef MADV_HUGEPAGE
      madvise(p,sz,MADV_HUGEPAGE);
#endif
      mapped=1;
    }
    else p=NULL;
  }
  if(!p){ p=malloc(sz); if(!p)die("out of memory"); }
  memset(p,0,sizeof(Hdr));
  Hdr* h=(Hdr*)p;
  h->gc_flags = ((uint64_t)atomic_fetch_add(&uf_gc_seq,1))<<GCF_SEQSHIFT;
  if(mapped) h->gc_flags|=GCF_MMAP;
  uf_gc_bytes_since += sz;
  if(atomic_load(&uf_gc_mt)){ pthread_mutex_lock(&uf_gc_mu); h->gc_next=uf_gc_list; uf_gc_list=p; uf_gc_set_insert_fast(p); pthread_mutex_unlock(&uf_gc_mu); }
  else { h->gc_next=uf_gc_list; uf_gc_list=p; uf_gc_set_insert_fast(p); }
  return p;
}
/* register a static object (string literal): linked, pinned, never swept */
static void uf_gc_register_static(void* p){
  pthread_mutex_lock(&uf_gc_mu);
  Hdr* h=(Hdr*)p; h->gc_next=uf_gc_list; uf_gc_list=p;
  h->gc_flags = GCF_PINNED;
  uf_gc_set_insert(p);
  pthread_mutex_unlock(&uf_gc_mu);
}
static void uf_init_lits(void** lits, long n){ for(long i=0;i<n;i++) uf_gc_register_static(lits[i]); }
static void uf_gc_init(void){
  const char* e=getenv("NK_GC_THRESHOLD");
  if(e&&*e){ uint64_t v=strtoull(e,0,0); if(v) uf_gc_threshold=v; }
  (void)UF_MAXTMP;
  ctx_register(&main_cx_store);
}
static void op_gc(Ctx*cx){ (void)cx; uf_gc_collect(); }

static Ctx* ctx_new(long dcap,long ccap){ Ctx*c=(Ctx*)calloc(1,sizeof(Ctx)); if(!c)die("out of memory"); c->ds=(Cell*)malloc(dcap*sizeof(Cell)); c->cs=(const void**)malloc(ccap*sizeof(void*)); c->rsps=(long*)malloc(ccap*sizeof(long)); c->locals=(Cell*)calloc(65536,sizeof(Cell)); c->local_frames=(long*)malloc(65536*sizeof(long)); c->call_pcs=(long*)malloc(ccap*sizeof(long)); if(!c->ds||!c->cs||!c->rsps||!c->locals||!c->local_frames||!c->call_pcs)die("out of memory"); c->dcap=dcap; c->ccap=ccap; c->local_cap=65536; c->local_fcap=65536; c->call_ccap=ccap; atomic_store(&uf_gc_mt,1); ctx_register(c); return c; }
static void ctx_free(Ctx*c){ ctx_unregister(c); free(c->ds); free((void*)c->cs); free(c->rsps); free(c->locals); free(c->local_frames); free(c->call_pcs); free(c); }
static Cell main_ds[1<<20]; static const void* main_cs[1<<16];
static long main_rsps[1<<16];
static Cell main_locals[65536]; static long main_local_frames[65536];
static long main_call_pcs[1<<16];
static Ctx main_cx_store = { main_ds, 0, 1<<20, main_cs, 0, 1<<16, main_rsps, {{0,0,0}}, 0, main_locals, 0, 65536, main_local_frames, 0, 65536, main_call_pcs, 0, 1<<16, 0 };
static Ctx* main_cx = &main_cx_store;
int64_t nk_argc=0; void* nk_argv=0; /* program args, reachable via EXTERN "nk_argc"/"nk_argv" + LOADX, or ARGV */

static inline void pushc(Ctx*cx,Cell c){ if(cx->sp>=cx->dcap){char _b[128];snprintf(_b,sizeof(_b),"stack overflow in %s (sp=%ld, cap=%ld)",uf_cur_op,cx->sp,cx->dcap);die(_b);} cx->ds[cx->sp++]=c; }
static inline Cell uf_mki(int64_t v){ Cell c; c.tag=T_INT; c.i=v; return c; }
static inline Cell uf_mkp(void* v){ Cell c; c.tag=T_PTR; c.i=(int64_t)v; return c; }
static inline double uf_fbits(int64_t i){ union{int64_t i;double f;}u;u.i=i;return u.f; }
static inline int64_t uf_ibits(double f){ union{int64_t i;double f;}u;u.f=f;return u.i; }
static inline double uf_f(Cell c){ if(c.tag==T_FLOAT)return uf_fbits(c.i); if(c.tag==T_PTR&&c.i&&uf_is_str(c)) return strtod(uf_sptr(c),0); return (double)c.i; }
static inline int64_t uf_i(Cell c){ if(c.tag==T_PTR&&c.i&&uf_is_str(c)) return strtoll(uf_sptr(c),0,10); return c.i; }
static inline Cell uf_mkf(double v){ Cell c; c.tag=T_FLOAT; c.i=uf_ibits(v); return c; }
static inline Cell uf_fromf(double v){ return uf_mkf(v); }
static inline int uf_zero(Cell c){ return c.tag==T_FLOAT?(int64_t)uf_fbits(c.i)==0:c.i==0; }
static inline double uf_to_number(Cell c){
  if(c.tag==T_INT)return (double)c.i;
  if(c.tag==T_FLOAT)return uf_fbits(c.i);
  if(c.tag==T_PTR && c.i){
    Hdr*h=uf_gc_find((void*)c.i);
    if(h){
      if(h->tag==HT_DYN){
        Dyn*d=(Dyn*)h;
        if(d->len==0)return 0.0;
        if(d->len==1)return uf_to_number(d->data[0]);
        return NAN;
      }
      if(h->tag==HT_STR){
        const char*s=uf_sptr(c); char*end; double v=strtod(s,&end);
        if(end==s)return NAN;
        while(isspace((unsigned char)*end))end++;
        if(*end)return NAN;
        return v;
      }
    }
  }
  return (double)c.i;
}
static inline int uf_truthy(Cell c){
  if(c.tag==T_FLOAT){ double d=uf_fbits(c.i); return d!=0.0 && !isnan(d); }
  if(c.tag==T_INT || c.tag==T_BYTE)return c.i!=0;
  if(c.tag==T_PTR && c.i){
    Hdr*h=uf_gc_find((void*)c.i);
    if(h){
      if(h->tag==HT_STR)return h->len!=0;
      if(h->tag==HT_DYN)return ((Dyn*)h)->len!=0;
      if(h->tag==HT_MAP)return ((Map*)h)->len!=0;
      if(h->tag==HT_ARR||h->tag==HT_TENSOR||h->tag==HT_BITMAP||h->tag==HT_BLOOM||h->tag==HT_MAT)return h->len!=0;
      return 1;
    }
    /* Untracked non-null pointer (raw FFI handle: FILE*, malloc'd, ...):
       truthy per SPEC — falsy is only 0/""/null/NaN/empty collections. */
    return 1;
  }
  return 0;
}
static inline int uf_loose_eq(Cell a,Cell b){
  if(a.tag==T_PTR && b.tag==T_PTR && a.i && b.i){
    Hdr*ha=uf_gc_find((void*)a.i); Hdr*hb=uf_gc_find((void*)b.i);
    if(ha && hb && ha->tag==HT_STR && hb->tag==HT_STR){
      if(ha->len!=hb->len)return 0;
      return memcmp(uf_sptr(a),uf_sptr(b),ha->len)==0;
    }
  }
  double x=uf_to_number(a), y=uf_to_number(b);
  return x==y;
}
static inline int uf_strict_eq(Cell a,Cell b){
  if(a.tag!=b.tag)return 0;
  if(a.tag==T_FLOAT)return a.i==b.i;
  if(a.tag==T_PTR && a.i && b.i){
    Hdr*ha=uf_gc_find((void*)a.i); Hdr*hb=uf_gc_find((void*)b.i);
    if(ha && hb && ha->tag==HT_STR && hb->tag==HT_STR){
      if(ha->len!=hb->len)return 0;
      return memcmp(uf_sptr(a),uf_sptr(b),ha->len)==0;
    }
  }
  return a.i==b.i;
}
static int uf_numarr(Cell c);
static Cell uf_poly_arith(Cell a,Cell b,int op,const char*opn);
static Hdr* uf_arr_like(Hdr*a,uint64_t n);
static double uf_el(Hdr*a,uint64_t i);
static void uf_put_el(Hdr*a,uint64_t i,double d);
static int uf_is_arrish(Hdr*a){ return a->tag==HT_ARR||a->tag==HT_TENSOR||a->tag==HT_MAT; }
#ifdef NK_GPU
struct UFPC { int64_t n0,n1,n2,n3; double s; int64_t rev; };
static long uf_gpu_min(void);
static int uf_spv_index(const char*name);
static int uf_vk_run(int k,uint64_t n,const void*A,size_t asz,const void*B,size_t bsz,void*R,size_t rsz,struct UFPC pc);
#endif
static inline int uf_rawptr(Cell c){ return c.tag==T_PTR&&c.i&&!uf_gc_find((void*)c.i); }
/* char* semantics: string + int / int + string is data-pointer arithmetic */
static inline Cell uf_stradd(Cell a,Cell b,int sub){
  Cell s=a; Cell n=b; int flip=0;
  if(b.tag==T_PTR&&b.i&&uf_is_str(b)&&a.tag==T_INT){ s=b; n=a; flip=1; }
  if(!(s.tag==T_PTR&&s.i&&uf_is_str(s)&&n.tag==T_INT)) return uf_mki(-4200000001LL);
  const char* p=uf_sptr(s);
  int64_t off=sub? -n.i : n.i;
  if(flip&&sub) return uf_mki(-4200000001LL);
  return uf_mkp((void*)(p+off));
}
static inline Cell uf_cadd(Cell a,Cell b){ if(a.tag==T_PTR||b.tag==T_PTR){ if(uf_numarr(a)||uf_numarr(b))return uf_poly_arith(a,b,0,"add"); if(a.tag==T_INT&&uf_rawptr(b))return uf_mkp((void*)(a.i+b.i)); if(uf_rawptr(a)&&b.tag==T_INT)return uf_mkp((void*)(a.i+b.i)); Cell s=uf_stradd(a,b,0); if(s.i!=-4200000001LL)return s; } double x=uf_to_number(a),y=uf_to_number(b); if(isnan(x)||isnan(y))return uf_mkf(NAN); if(a.tag==T_INT&&b.tag==T_INT)return uf_mki(a.i+b.i); return uf_mkf(x+y); }
static inline Cell uf_csub(Cell a,Cell b){ if(a.tag==T_PTR||b.tag==T_PTR){ if(uf_numarr(a)||uf_numarr(b))return uf_poly_arith(a,b,1,"sub"); if(uf_rawptr(a)&&b.tag==T_INT)return uf_mkp((void*)(a.i-b.i)); Cell s=uf_stradd(a,b,1); if(s.i!=-4200000001LL)return s; } double x=uf_to_number(a),y=uf_to_number(b); if(isnan(x)||isnan(y))return uf_mkf(NAN); if(a.tag==T_INT&&b.tag==T_INT)return uf_mki(a.i-b.i); return uf_mkf(x-y); }
static inline Cell uf_cmul(Cell a,Cell b){ if(a.tag==T_INT&&a.i==1&&b.tag!=T_FLOAT&&!uf_numarr(b))return b; if(b.tag==T_INT&&b.i==1&&a.tag!=T_FLOAT&&!uf_numarr(a))return a; if(a.tag==T_PTR||b.tag==T_PTR){ if(uf_numarr(a)||uf_numarr(b))return uf_poly_arith(a,b,2,"mul"); } double x=uf_to_number(a),y=uf_to_number(b); if(isnan(x)||isnan(y))return uf_mkf(NAN); if(a.tag==T_INT&&b.tag==T_INT)return uf_mki(a.i*b.i); return uf_mkf(x*y); }
static inline Cell uf_cand(Cell a,Cell b){ return uf_mki(uf_i(a)&uf_i(b)); }
static inline Cell uf_cshr(Cell a,Cell b){ if(a.tag==T_FLOAT||b.tag==T_FLOAT||a.tag==T_PTR||b.tag==T_PTR)die("SHR: ints only"); if(b.i<0||b.i>=64)die("SHR: shift out of range"); return uf_mki((int64_t)((uint64_t)a.i>>b.i)); }
static inline Cell uf_cinc(Cell a){ double x=uf_to_number(a); if(isnan(x))return uf_mkf(NAN); if(a.tag==T_INT)return uf_mki(a.i+1); return uf_mkf(x+1.0); }
static inline Cell uf_cdec(Cell a){ double x=uf_to_number(a); if(isnan(x))return uf_mkf(NAN); if(a.tag==T_INT)return uf_mki(a.i-1); return uf_mkf(x-1.0); }
/* Division and remainder are *not* inlined by the C compiler. If they were,
   literal `1 0 div` would be constant-folded to an undefined (1/0) expression
   and the runtime zero-check / longjmp into try/retry would be bypassed. */
#ifdef __GNUC__
#define UF_NOINLINE __attribute__((noinline))
#else
#define UF_NOINLINE
#endif
static Cell UF_NOINLINE uf_cdiv(Cell a,Cell b){ if(a.tag==T_PTR||b.tag==T_PTR){ if(uf_numarr(a)||uf_numarr(b))return uf_poly_arith(a,b,3,"div"); } double x=uf_to_number(a),y=uf_to_number(b); if(isnan(x)||isnan(y))return uf_mkf(NAN); if(y==0.0)die("DIV: division by zero"); if(a.tag==T_INT&&b.tag==T_INT)return uf_mki(a.i/b.i); return uf_mkf(x/y); }
static Cell UF_NOINLINE uf_crem(Cell a,Cell b){ double x=uf_to_number(a),y=uf_to_number(b); if(isnan(x)||isnan(y))return uf_mkf(NAN); if(y==0.0)die("REM: division by zero"); if(a.tag==T_INT&&b.tag==T_INT)return uf_mki(a.i%b.i); return uf_mkf(fmod(x,y)); }
static inline Cell uf_clt(Cell a,Cell b){ double x=uf_to_number(a),y=uf_to_number(b); return uf_mki(isnan(x)||isnan(y)?0:(x<y?1:0)); }
static inline Cell uf_cgt(Cell a,Cell b){ double x=uf_to_number(a),y=uf_to_number(b); return uf_mki(isnan(x)||isnan(y)?0:(x>y?1:0)); }
static inline Cell uf_clte(Cell a,Cell b){ double x=uf_to_number(a),y=uf_to_number(b); return uf_mki(isnan(x)||isnan(y)?0:(x<=y?1:0)); }
static inline Cell uf_cgte(Cell a,Cell b){ double x=uf_to_number(a),y=uf_to_number(b); return uf_mki(isnan(x)||isnan(y)?0:(x>=y?1:0)); }
static inline Cell uf_ceq(Cell a,Cell b){ return uf_mki(uf_loose_eq(a,b)?1:0); }
static inline Cell uf_cnot(Cell a){ return uf_mki(uf_truthy(a)?0:1); }
static inline Cell uf_cor(Cell a,Cell b){ return uf_mki(a.i|b.i); }
static inline Cell uf_cxor(Cell a,Cell b){ return uf_mki(a.i^b.i); }
static inline Cell uf_cvget(Cell h,int64_t idx){ Hdr*a=(Hdr*)h.i; char*dt=uf_data(a); if(a->ety==1)return uf_mkf(((double*)dt)[idx]); if(a->ety==3)return uf_mki((int64_t)((uint8_t*)dt)[idx]); return uf_mki(((int64_t*)dt)[idx]); }
static inline void uf_cvset(Cell h,int64_t idx,Cell v){ Hdr*a=(Hdr*)h.i; char*dt=uf_data(a); if(a->ety==1)((double*)dt)[idx]=uf_f(v); else if(a->ety==3)((uint8_t*)dt)[idx]=(uint8_t)v.i; else ((int64_t*)dt)[idx]=v.i; }

/* ---- string access: every core string is a tag-9 Str object; raw char*
   from IMPORTed C functions is still accepted (legacy ptr) ---- */
static int uf_is_str(Cell c){ if(c.tag!=T_PTR||!c.i)return 0; Hdr*h=uf_gc_find((void*)c.i); return h&&h->tag==HT_STR; }
static const char* uf_sbytes(Str*s){ return (s->mlen||s->gc_parent)?s->mdata:s->data; }
static const char* uf_sptr(Cell c){ if(c.tag==T_PTR&&c.i){ Hdr*h=uf_gc_find((void*)c.i); if(h&&h->tag==HT_STR){ Str*s=(Str*)h; return (s->mlen||s->gc_parent)?s->mdata:s->data; } if(h&&h->tag==HT_BUF){ return h->data; } return (const char*)c.i; /* raw ptr from malloc/FFI */ } if(c.tag==T_INT&&c.i>(int64_t)65536) return (const char*)c.i; if(c.tag==T_INT&&!c.i) return (const char*)0; die("expected string, got non-pointer cell"); }
static int64_t uf_slen(Cell c){ if(c.tag==T_PTR&&c.i){ Hdr*h=uf_gc_find((void*)c.i); if(h&&h->tag==HT_STR)return (int64_t)h->len; } return (int64_t)strlen((const char*)c.i); }
static Cell uf_str_new(const char* s, size_t n){
  Str* r=(Str*)uf_gc_alloc(sizeof(Str)+n+1,0);
  r->tag=HT_STR; r->len=n; r->esz=1; r->mlen=0;
  memcpy(r->data,s,n); r->data[n]=0;
  return uf_mkp(r);
}
static Cell uf_str_dup(Cell c){ const char* s=uf_sptr(c); return uf_str_new(s,strlen(s)); }
static void* uf_alloc(size_t sz,int align); /* forward decl for string coercion */
/* universal string coercion */
static Cell uf_to_string(Cell c){
  char tmp[64];
  if(c.tag==T_FLOAT){ double d=uf_fbits(c.i); if(isnan(d)) return uf_str_new("NaN",3); snprintf(tmp,sizeof(tmp),"%.17g",d); return uf_str_new(tmp,strlen(tmp)); }
  if(c.tag==T_BYTE){ return c.i?uf_str_new("true",4):uf_str_new("false",5); }
  if(c.tag==T_INT){ snprintf(tmp,sizeof(tmp),"%lld",(long long)c.i); return uf_str_new(tmp,strlen(tmp)); }
  if(c.tag==T_PTR && !c.i) return uf_str_new("null",4);
  if(c.tag==T_PTR && c.i){
    Hdr*h=uf_gc_find((void*)c.i);
    if(h){
      if(h->tag==HT_STR){ return uf_str_new(uf_sbytes((Str*)h),h->len); }
      if(h->tag==HT_DYN){
        Dyn*d=(Dyn*)h; size_t cap=16,n=0; char*b=(char*)uf_alloc(cap,0);
        for(uint64_t i=0;i<d->len;i++){
          if(i){ while(n+1>=cap){cap*=2;b=(char*)realloc(b,cap);} b[n++]=','; }
          Cell cs=uf_to_string(d->data[i]); const char*p=uf_sptr(cs); size_t l=strlen(p);
          while(n+l+1>cap){ cap*=2; b=(char*)realloc(b,cap); }
          memcpy(b+n,p,l); n+=l;
        }
        b[n]=0; Cell r=uf_str_new(b,n); free(b); return r;
      }
      if(h->tag==HT_MAP||h->tag==HT_OBJ) return uf_str_new("[object Object]",15);
    }
  }
  snprintf(tmp,sizeof(tmp),"%lld",(long long)c.i); return uf_str_new(tmp,strlen(tmp));
}

/* arr element access honors the element type (ety): 0 int (8B), 1 float (8B), 3 byte (1B) */
static inline Cell uf_cidx(Cell h,int64_t ix){ Hdr*a=(Hdr*)h.i; if(ix<0||(uint64_t)ix>=a->len)die("index out of bounds"); char*dt=uf_data(a); if(a->tag==HT_DYN)return ((Cell*)dt)[ix]; if(a->ety==3)return uf_mki((int64_t)((uint8_t*)dt)[ix]); if(a->ety==1)return uf_mkf(((double*)dt)[ix]); return uf_mki(((int64_t*)dt)[ix]); }
static inline void uf_cseti(Cell h,int64_t ix,Cell v){ Hdr*a=(Hdr*)h.i; if(ix<0||(uint64_t)ix>=a->len)die("index out of bounds"); char*dt=uf_data(a); if(a->tag==HT_DYN){((Cell*)dt)[ix]=v;return;} if(a->ety==3){((uint8_t*)dt)[ix]=(uint8_t)v.i;return;} if(a->ety==1){((double*)dt)[ix]=uf_f(v);return;} ((int64_t*)dt)[ix]=v.i; }
static inline void pushi(Ctx*cx,int64_t v){ pushc(cx,uf_mki(v)); }
static inline void pushf(Ctx*cx,double v){ pushc(cx,uf_mkf(v)); }
static inline void pushp(Ctx*cx,void* v){ pushc(cx,uf_mkp(v)); }
static inline Cell pop(Ctx*cx){ if(cx->sp<=0){char _b[128];snprintf(_b,sizeof(_b),"stack underflow in %s (sp=%ld)",uf_cur_op,cx->sp);die(_b);} return cx->ds[--cx->sp]; }
static void op_nop(Ctx*cx){ (void)cx; }

static int uf_numarr(Cell c);
static Cell uf_poly_arith(Cell a,Cell b,int op,const char*opn);
static void op_add(Ctx*cx){ Cell b=pop(cx),a=pop(cx); if(uf_numarr(a)||uf_numarr(b))pushc(cx,uf_poly_arith(a,b,0,"add")); else pushc(cx,uf_cadd(a,b)); }
static void op_sub(Ctx*cx){ Cell b=pop(cx),a=p…30170 tokens truncated…PR)); } \
  else { int64_t d=s.i; if(ZERO_DIE&&d==0)die(#NAME ": zero scalar"); for(uint64_t i=0;i<n;i++)uf_put_el(r,i,(EXPR)); } \
  UF_UNPROTECT(); UF_UNPROTECT(); pushp(cx,r); }
UF_VSOP(op_vadd, uf_el(a,i)+d, A[i]+d, 0)
UF_VSOP(op_vsub, uf_el(a,i)-d, A[i]-d, 0)
UF_VSOP(op_vmul, uf_el(a,i)*d, A[i]*d, 0)
UF_VSOP(op_vdiv, uf_el(a,i)/d, A[i]/d, 1)
/* elementwise arr arr ops: length mismatch dies */
#define UF_VEOP(NAME,EXPR,RAWEXPR,ZERO_DIE) \
static void NAME(Ctx*cx){ Cell h2=pop(cx),h1=pop(cx); Hdr*a=uf_vcheck(h1,#NAME); Hdr*b=uf_vcheck(h2,#NAME); \
  if(a->len!=b->len)die(#NAME ": length mismatch"); \
  UF_PROTECT((void**)(void*)&h1.i); UF_PROTECT((void**)(void*)&h2.i); \
  uint64_t n=a->len; Hdr*r=uf_arr_like(a,n); UF_PROTECT(&r); \
  if(a->ety==1&&b->ety==1&&a->tag!=HT_MAT&&b->tag!=HT_MAT){ const double*A=(const double*)uf_data(a); const double*B=(const double*)uf_data(b); double*R=(double*)uf_data(r); for(uint64_t i=0;i<n;i++)R[i]=(RAWEXPR); } \
  else { \
  if(ZERO_DIE) for(uint64_t i=0;i<n;i++) if(uf_el(b,i)==0.0)die(#NAME ": zero divisor"); \
  for(uint64_t i=0;i<n;i++)uf_put_el(r,i,(EXPR)); } \
  UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT(); pushp(cx,r); }
UF_VEOP(op_veadd, uf_el(a,i)+uf_el(b,i), A[i]+B[i], 0)
UF_VEOP(op_vesub, uf_el(a,i)-uf_el(b,i), A[i]-B[i], 0)
UF_VEOP(op_vemul, uf_el(a,i)*uf_el(b,i), A[i]*B[i], 0)
UF_VEOP(op_vediv, uf_el(a,i)/uf_el(b,i), A[i]/B[i], 1)
UF_VEOP(op_vemax, uf_el(a,i)>uf_el(b,i)?uf_el(a,i):uf_el(b,i), A[i]>B[i]?A[i]:B[i], 0)
UF_VEOP(op_vemin, uf_el(a,i)<uf_el(b,i)?uf_el(a,i):uf_el(b,i), A[i]<B[i]?A[i]:B[i], 0)
/* comparisons: arr scalar -> bitmap */
static Bitmap* uf_bm_new(uint64_t nbits){ Bitmap*b=(Bitmap*)uf_gc_alloc(sizeof(Bitmap)+((nbits+63)/64)*8,0); b->tag=HT_BITMAP; b->len=nbits; b->esz=8; memset(b->words,0,((nbits+63)/64)*8); return b; }
#define UF_VCOP(NAME,EXPR) \
static void NAME(Ctx*cx){ Cell s=pop(cx),h=pop(cx); Hdr*a=uf_vcheck(h,#NAME); \
  UF_PROTECT((void**)(void*)&h.i); \
  uint64_t n=a->len; Bitmap*r=uf_bm_new(n); UF_PROTECT(&r); \
  double d=uf_f(s); \
  for(uint64_t i=0;i<n;i++) if(EXPR) r->words[i>>6]|=(1ULL<<(i&63)); \
  UF_UNPROTECT(); UF_UNPROTECT(); pushp(cx,r); }
UF_VCOP(op_veq, uf_el(a,i)==d)
UF_VCOP(op_vlt, uf_el(a,i)<d)
UF_VCOP(op_vgt, uf_el(a,i)>d)
UF_VCOP(op_vge, uf_el(a,i)>=d)
UF_VCOP(op_vle, uf_el(a,i)<=d)
/* bitmap logic */
static Bitmap* uf_bm_check(Cell h){ Hdr*a=uf_handle(h,"bitmap op"); if(a->tag!=HT_BITMAP)die("bitmap op: not a bitmap"); return (Bitmap*)a; }
static void op_vand(Ctx*cx){ Cell h2=pop(cx),h1=pop(cx); Bitmap*a=uf_bm_check(h1),*b=uf_bm_check(h2); if(a->len!=b->len)die("VAND: length mismatch"); uint64_t w=(a->len+63)/64; Bitmap*r=uf_bm_new(a->len); UF_PROTECT(&r); for(uint64_t i=0;i<w;i++)r->words[i]=a->words[i]&b->words[i]; UF_UNPROTECT(); pushp(cx,r); }
static void op_vor(Ctx*cx){ Cell h2=pop(cx),h1=pop(cx); Bitmap*a=uf_bm_check(h1),*b=uf_bm_check(h2); if(a->len!=b->len)die("VOR: length mismatch"); uint64_t w=(a->len+63)/64; Bitmap*r=uf_bm_new(a->len); UF_PROTECT(&r); for(uint64_t i=0;i<w;i++)r->words[i]=a->words[i]|b->words[i]; UF_UNPROTECT(); pushp(cx,r); }
static void op_vnot(Ctx*cx){ Cell h=pop(cx); Bitmap*a=uf_bm_check(h); uint64_t w=(a->len+63)/64; Bitmap*r=uf_bm_new(a->len); UF_PROTECT(&r); for(uint64_t i=0;i<w;i++)r->words[i]=~a->words[i]; if(a->len&63)r->words[w-1]&=(1ULL<<(a->len&63))-1; UF_UNPROTECT(); pushp(cx,r); }
static void op_vcount(Ctx*cx){ Cell h=pop(cx); Bitmap*a=uf_bm_check(h); uint64_t w=(a->len+63)/64,n=0; for(uint64_t i=0;i<w;i++)n+=(uint64_t)__builtin_popcountll(a->words[i]); pushi(cx,(int64_t)n); }
/* VGATHER: arr bm -> arr' (keep set-bit elements) */
static void op_vgather(Ctx*cx){
  Cell h2=pop(cx),h1=pop(cx); Hdr*a=uf_vcheck(h1,"VGATHER"); Bitmap*b=uf_bm_check(h2);
  if(a->len!=b->len)die("VGATHER: length mismatch");
  UF_PROTECT((void**)(void*)&h1.i);
  uint64_t n=0; for(uint64_t i=0;i<a->len;i++) if((b->words[i>>6]>>(i&63))&1)n++;
  Hdr*r=uf_arr_like(a,n); UF_PROTECT(&r);
  uint64_t k=0; for(uint64_t i=0;i<a->len;i++) if((b->words[i>>6]>>(i&63))&1)uf_put_el(r,k++,uf_el(a,i));
  UF_UNPROTECT(); UF_UNPROTECT(); pushp(cx,r);
}
/* reductions */
static void op_vsum(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VSUM");
#ifdef NK_GPU
  if(a->ety==1&&a->len>=(uint64_t)uf_gpu_min()
     &&!(uf_dev_is_auto()&&!uf_vk_ready&&a->len<(uint64_t)uf_gpu_arith_min())){
    double _r; if(uf_gpu_reduce((const double*)uf_data(a),a->len,"rsum",&_r)){ pushf(cx,_r); return; } }
#endif
 if(a->ety==1){ const double*A=(const double*)uf_data(a); double s=0; for(uint64_t i=0;i<a->len;i++)s+=A[i]; pushf(cx,s); return; }
 double s=0; for(uint64_t i=0;i<a->len;i++)s+=uf_el(a,i); if(a->ety==1)pushf(cx,s); else pushi(cx,(int64_t)s); }
static void op_vmean(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VMEAN"); if(!a->len)die("VMEAN: empty arr");
#ifdef NK_GPU
  if(a->ety==1&&a->len>=(uint64_t)uf_gpu_min()
     &&!(uf_dev_is_auto()&&!uf_vk_ready&&a->len<(uint64_t)uf_gpu_arith_min())){
    double _r; if(uf_gpu_reduce((const double*)uf_data(a),a->len,"rsum",&_r)){ pushf(cx,_r/(double)a->len); return; } }
#endif
 double s=0; for(uint64_t i=0;i<a->len;i++)s+=uf_el(a,i); pushf(cx,s/(double)a->len); }
static void op_vmin(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VMIN"); if(!a->len)die("VMIN: empty arr");
#ifdef NK_GPU
  if(a->ety==1&&a->len>=(uint64_t)uf_gpu_min()
     &&!(uf_dev_is_auto()&&!uf_vk_ready&&a->len<(uint64_t)uf_gpu_arith_min())){
    double _r; if(uf_gpu_reduce((const double*)uf_data(a),a->len,"rmin",&_r)){ pushf(cx,_r); return; } }
#endif
 double s=uf_el(a,0); for(uint64_t i=1;i<a->len;i++){double d=uf_el(a,i);if(d<s)s=d;} if(a->ety==1)pushf(cx,s); else pushi(cx,(int64_t)s); }
static void op_vmax(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VMAX"); if(!a->len)die("VMAX: empty arr");
#ifdef NK_GPU
  if(a->ety==1&&a->len>=(uint64_t)uf_gpu_min()
     &&!(uf_dev_is_auto()&&!uf_vk_ready&&a->len<(uint64_t)uf_gpu_arith_min())){
    double _r; if(uf_gpu_reduce((const double*)uf_data(a),a->len,"rmax",&_r)){ pushf(cx,_r); return; } }
#endif
 double s=uf_el(a,0); for(uint64_t i=1;i<a->len;i++){double d=uf_el(a,i);if(d>s)s=d;} if(a->ety==1)pushf(cx,s); else pushi(cx,(int64_t)s); }
/* VMAP: arr fn_addr -> arr' ; VFOLD: arr init fn_addr -> acc */
static void op_vmap(Ctx*cx){
  Cell f=pop(cx),h=pop(cx); Hdr*a=uf_vcheck(h,"VMAP");
  Hdr*r=uf_arr_like(a,a->len); UF_PROTECT(&r);
  for(uint64_t i=0;i<a->len;i++){
    if(a->ety==1)pushf(cx,uf_el(a,i)); else pushi(cx,(int64_t)uf_el(a,i));
    uf_call_addr(cx,(const void*)f.i,0,-1,1);
    uf_put_el(r,i,uf_f(pop(cx)));
  }
  UF_UNPROTECT(); pushp(cx,r);
}
static void op_vfold(Ctx*cx){
  Cell f=pop(cx),acc=pop(cx),h=pop(cx);
  Hdr*a=uf_handle(h,"VFOLD");
  if(a->tag==HT_DYN){ /* list input: convert to a float tensor once */ Dyn*d=(Dyn*)a; Hdr*t=(Hdr*)uf_gc_alloc(sizeof(Hdr)+d->len*8,0); t->tag=HT_TENSOR; t->len=d->len; t->esz=8; t->ety=1; UF_PROTECT((void**)(void*)&t); for(uint64_t q=0;q<d->len;q++){ ((double*)t->data)[q]=uf_f(d->data[q]); } UF_UNPROTECT(); a=t; }
  else if(a->tag!=HT_ARR&&a->tag!=HT_TENSOR&&a->tag!=HT_MAT)die("vector op: not an arr");
  for(uint64_t i=0;i<a->len;i++){
    pushc(cx,acc);
    if(a->ety==1)pushf(cx,uf_el(a,i)); else pushi(cx,(int64_t)uf_el(a,i));
    uf_call_addr(cx,(const void*)f.i,0,-1,2);
    acc=pop(cx);
  }
  pushc(cx,acc);
}
/* VARGSORT: arr -> idx_arr (stable) */
static void op_vargsort(Ctx*cx){
  Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VARGSORT");
  uint64_t n=a->len;
  int64_t* idx=(int64_t*)uf_alloc((n?n:1)*8,0); int64_t* tmp=(int64_t*)uf_alloc((n?n:1)*8,0);
  for(uint64_t i=0;i<n;i++)idx[i]=(int64_t)i;
  /* stable mergesort of indices by element value */
  for(uint64_t w=1;w<n;w*=2){
    for(uint64_t lo=0;lo<n;lo+=2*w){
      uint64_t mid=lo+w<n?lo+w:n, hi=lo+2*w<n?lo+2*w:n;
      uint64_t i=lo,j=mid,k=lo;
      while(i<mid&&j<hi){ if(uf_el(a,(uint64_t)idx[i])<=uf_el(a,(uint64_t)idx[j]))tmp[k++]=idx[i++]; else tmp[k++]=idx[j++]; }
      while(i<mid)tmp[k++]=idx[i++];
      while(j<hi)tmp[k++]=idx[j++];
    }
    int64_t* t=idx; idx=tmp; tmp=t;
  }
  Hdr*r=(Hdr*)uf_gc_alloc(sizeof(Hdr)+n*8,0); r->tag=HT_ARR; r->len=n; r->esz=8; r->ety=0;
  memcpy(r->data,idx,n*8); free(idx); free(tmp);
  pushp(cx,r);
}
/* VSEARCHSORTED: sorted_arr val -> idx (insertion point) */
static void op_vsearchsorted(Ctx*cx){
  Cell v=pop(cx),h=pop(cx); Hdr*a=uf_vcheck(h,"VSEARCHSORTED");
  double d=uf_f(v); uint64_t lo=0,hi=a->len;
  while(lo<hi){ uint64_t mid=(lo+hi)/2; if(uf_el(a,mid)<d)lo=mid+1; else hi=mid; }
  pushi(cx,(int64_t)lo);
}
/* VWHERE: arr arr bm -> arr' (bit set -> first arr, else second) */
static void op_vwhere(Ctx*cx){
  Cell h3=pop(cx),h2=pop(cx),h1=pop(cx);
  Hdr*a=uf_vcheck(h1,"VWHERE"); Hdr*b=uf_vcheck(h2,"VWHERE"); Bitmap*m=uf_bm_check(h3);
  if(a->len!=b->len||a->len!=m->len)die("VWHERE: length mismatch");
  Hdr*r=uf_arr_like(a,a->len); UF_PROTECT(&r);
  for(uint64_t i=0;i<a->len;i++) uf_put_el(r,i, ((m->words[i>>6]>>(i&63))&1)?uf_el(a,i):uf_el(b,i));
  UF_UNPROTECT(); pushp(cx,r);
}

/* ================= data ops: group/agg/unique/flat/chunk ================= */
/* GROUP: list fn_addr -> dict (key -> list of elems, insertion-ordered keys) */
static void op_group(Ctx*cx){
  Cell f=pop(cx),h=pop(cx);
  Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  Map* m=uf_map_new(); UF_PROTECT(&m);
  Dyn* order=uf_dyn_new(8); UF_PROTECT(&order);
  for(uint64_t i=0;i<s->len;i++){
    pushc(cx,s->data[i]); uf_call_addr(cx,(const void*)f.i,0,-1,1); Cell key=pop(cx);
    Cell lst;
    if(map_get(m,key,&lst)){ Dyn* nd=uf_dyn_push2((Dyn*)uf_gc_find((void*)lst.i),s->data[i]); if((void*)nd!=(void*)lst.i) map_put(m,key,uf_mkp(nd)); }
    else { Dyn* d=uf_dyn_new(4); d=uf_dyn_push2(d,s->data[i]); map_put(m,key,uf_mkp(d)); uf_dyn_push(&order,key); }
  }
  UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,m);
}
/* AGG: dict fn_addr -> dict' (map each group's value-list through fn) */
static void op_agg(Ctx*cx){
  Cell f=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"AGG"); if(a->tag!=HT_MAP)die("AGG: not a dict");
  Map* m=(Map*)a;
  Map* r=uf_map_new(); UF_PROTECT(&r);
  for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1){
    pushc(cx,m->vals[i]); uf_call_addr(cx,(const void*)f.i,0,-1,1); Cell v=pop(cx);
    map_put(r,m->keys[i],v);
  }
  UF_UNPROTECT(); pushp(cx,r);
}
/* UNIQUE: list -> list' (dedup, first-occurrence order; dict-hashable elems) */
static void op_unique(Ctx*cx){
  Cell h=pop(cx);
  Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  Map* seen=uf_map_new(); UF_PROTECT(&seen);
  Dyn* r=uf_dyn_new(s->len); UF_PROTECT(&r);
  for(uint64_t i=0;i<s->len;i++){
    Cell v;
    if(!map_get(seen,s->data[i],&v)){ map_put(seen,s->data[i],uf_mki(1)); uf_dyn_push(&r,s->data[i]); }
  }
  UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,r);
}
/* FLAT: list -> list' (flatten one level; non-list elements pass through) */
static void op_flat(Ctx*cx){
  Cell h=pop(cx);
  Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  Dyn* r=uf_dyn_new(8); UF_PROTECT(&r);
  for(uint64_t i=0;i<s->len;i++){
    Cell e=s->data[i]; Hdr* eh=e.tag==T_PTR&&e.i?uf_gc_find((void*)e.i):0;
    if(eh&&eh->tag==HT_DYN){ Dyn* d=(Dyn*)eh; for(uint64_t j=0;j<d->len;j++)uf_dyn_push(&r,d->data[j]); }
    else uf_dyn_push(&r,e);
  }
  UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,r);
}
/* CHUNK: seq size -> list of pieces (last may be short) */
static void op_chunk(Ctx*cx){
  int64_t sz=pop(cx).i; Cell h=pop(cx);
  if(sz<1)die("CHUNK: size < 1");
  Hdr* a=h.tag==T_PTR&&h.i?uf_gc_find((void*)h.i):0;
  int isarr = a&&(a->tag==HT_ARR||a->tag==HT_TENSOR);
  Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  Dyn* r=uf_dyn_new(s->len/(uint64_t)sz+1); UF_PROTECT(&r);
  for(uint64_t i=0;i<s->len;i+=(uint64_t)sz){
    uint64_t n=s->len-i<(uint64_t)sz?s->len-i:(uint64_t)sz;
    if(isarr){
      Hdr* p=(Hdr*)uf_gc_alloc(sizeof(Hdr)+n*a->esz,0);
      p->tag=a->tag; p->len=n; p->esz=a->esz; p->ety=a->ety;
      for(uint64_t j=0;j<n;j++)uf_cseti(uf_mkp(p),(int64_t)j,s->data[i+j]);
      uf_dyn_push(&r,uf_mkp(p));
    } else {
      Dyn* p=uf_dyn_new(n);
      for(uint64_t j=0;j<n;j++)p=uf_dyn_push2(p,s->data[i+j]);
      uf_dyn_push(&r,uf_mkp(p));
    }
  }
  UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,r);
}

/* ================= time ops (scalar cells: tag 15 time, 16 dur, i64 nanos) ================= */
static void op_now(Ctx*cx){ struct timespec ts; clock_gettime(CLOCK_REALTIME,&ts); Cell c; c.tag=T_TIME; c.i=(int64_t)ts.tv_sec*1000000000LL+(int64_t)ts.tv_nsec; pushc(cx,c); }
/* TIME: str fmt -> t ; fmt "unix" (float s) or strptime(3) */
static void op_time(Ctx*cx){
  Cell fmt=pop(cx),st=pop(cx);
  const char* F=uf_sptr(fmt); const char* S=uf_sptr(st);
  int64_t ns;
  if(strcmp(F,"unix")==0){ double d=strtod(S,0); ns=(int64_t)(d*1e9); }
  else {
    struct tm tm; memset(&tm,0,sizeof tm);
    if(!strptime(S,F,&tm))die("TIME: unparseable");
    time_t t=mktime(&tm);
    ns=(int64_t)t*1000000000LL;
  }
  Cell c; c.tag=T_TIME; c.i=ns; pushc(cx,c);
}
/* TIMEF: t fmt -> str ; fmt "unix" or strftime(3) (process TZ via libc) */
static void op_timef(Ctx*cx){
  Cell fmt=pop(cx),t=pop(cx);
  const char* F=uf_sptr(fmt);
  char buf[256];
  if(strcmp(F,"unix")==0){ snprintf(buf,sizeof buf,"%.9f",(double)t.i/1e9); }
  else {
    time_t s=(time_t)(t.i/1000000000LL);
    struct tm tm; localtime_r(&s,&tm);
    if(!strftime(buf,sizeof buf,F,&tm))die("TIMEF: format failed");
  }
  pushc(cx,uf_str_new(buf,strlen(buf)));
}

/* ================= bloom filter (tag 17; double-hashed FNV-1a) ================= */
static void op_bloom(Ctx*cx){
  int64_t n=pop(cx).i; if(n<1)die("BLOOM: n < 1");
  uint64_t bits=(uint64_t)((double)n*9.585)+64; /* ~1% FP */
  Bloom* b=(Bloom*)uf_gc_alloc(sizeof(Bloom)+((bits+63)/64)*8,0);
  b->tag=HT_BLOOM; b->len=bits; b->esz=8; b->k=7;
  memset(b->words,0,((bits+63)/64)*8);
  pushp(cx,b);
}
static void uf_bloom_hashes(Cell v,uint64_t* h1,uint64_t* h2){
  if(uf_is_str(v)){ const char* s=uf_sptr(v); size_t n=strlen(s); *h1=uf_fnv(s,n); *h2=uf_fnv(s,n)^0x9e3779b97f4a7c15ULL; *h2=uf_fnv(h2,8); }
  else { *h1=uf_fnv(&v.i,8); uint64_t x=(uint64_t)v.i^0x9e3779b97f4a7c15ULL; *h2=uf_fnv(&x,8); }
  if(!*h2)*h2=0x100000001b3ULL;
}
static void op_badd(Ctx*cx){
  Cell v=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"BADD"); if(a->tag!=HT_BLOOM)die("BADD: not a bloom filter");
  Bloom* b=(Bloom*)a; uint64_t h1,h2; uf_bloom_hashes(v,&h1,&h2);
  for(int i=0;i<b->k;i++){ uint64_t p=(h1+(uint64_t)i*h2)%b->len; b->words[p>>6]|=(1ULL<<(p&63)); }
}
static void op_btest(Ctx*cx){
  Cell v=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"BTEST"); if(a->tag!=HT_BLOOM)die("BTEST: not a bloom filter");
  Bloom* b=(Bloom*)a; uint64_t h1,h2; uf_bloom_hashes(v,&h1,&h2);
  for(int i=0;i<b->k;i++){ uint64_t p=(h1+(uint64_t)i*h2)%b->len; if(!((b->words[p>>6]>>(p&63))&1)){ pushi(cx,0); return; } }
  pushi(cx,1);
}

/* ================= script I/O ================= */
/* SLURP: path -> str (whole file; not found/unreadable: dies) */
static void op_slurp(Ctx*cx){
  Cell p=pop(cx);
  uf_fs_gate(uf_sptr(p),0);
  FILE* f=fopen(uf_sptr(p),"rb"); if(!f)die("SLURP: cannot open file");
  char* b=uf_read_all(f); fclose(f);
  Cell r=uf_str_new(b,strlen(b)); free(b); pushc(cx,r);
}
/* SPIT: path str ->  (create/truncate; error: dies) */
static void op_spit(Ctx*cx){
  Cell st=pop(cx),p=pop(cx);
  uf_fs_gate(uf_sptr(p),1);
  FILE* f=fopen(uf_sptr(p),"wb"); if(!f)die("SPIT: cannot open file");
  const char* s=uf_sptr(st); size_t n=strlen(s);
  if(n&&fwrite(s,1,n,f)!=n){ fclose(f); die("SPIT: write failed"); }
  fclose(f);
}
/* ARGV: -> list of strings */
static void op_argv(Ctx*cx){
  Dyn* d=uf_dyn_new((uint64_t)nk_argc); UF_PROTECT(&d);
  char** av=(char**)nk_argv;
  for(int64_t i=0;i<nk_argc;i++){ Cell s=uf_str_new(av[i],strlen(av[i])); uf_dyn_push(&d,s); }
  UF_UNPROTECT(); pushp(cx,d);
}
/* HASARGS: -> int (1 if argv has >1 element, else 0) */
static void op_hasargs(Ctx*cx){
  pushi(cx, nk_argc>1 ? 1 : 0);
}
/* ARGI: index -> int (argv[index] parsed as integer) */
static void op_argi(Ctx*cx){
  int64_t idx=uf_i(pop(cx));
  if(idx<0||idx>=nk_argc) die("ARGI: index out of bounds");
  pushi(cx,(int64_t)strtoll(((char**)nk_argv)[idx],0,10));
}

/* ================= zero-copy file access + streaming ================= */
/* MMAP: path -> str (read-only zero-copy; tag str, GC-registered, munmap'd
   when swept; falls back to an owned copy for page-aligned sizes so the
   NUL terminator never crosses the mapping) */
static void op_mmap(Ctx*cx){
  Cell p=pop(cx);
  const char* path=uf_sptr(p);
  uf_fs_gate(path,0);
  int fd=open(path,O_RDONLY); if(fd<0)die("MMAP: cannot open file");
  struct stat sb; if(fstat(fd,&sb)<0){ close(fd); die("MMAP: stat failed"); }
  uint64_t n=(uint64_t)sb.st_size;
  if(n%4096==0){ /* no tail slack for the NUL: owned copy fallback */
    Str* s=(Str*)uf_gc_alloc(sizeof(Str)+n+1,0);
    s->tag=HT_STR; s->len=n; s->esz=1; s->mlen=0;
    ssize_t got=read(fd,s->data,n); close(fd);
    if(got<0)die("MMAP: read failed");
    s->len=(uint64_t)got; s->data[got]=0;
    pushp(cx,s); return;
  }
  /* last partial page zero-fills past EOF, so data[n]==0 for free */
  uint64_t flen=(n+4095)&~(uint64_t)4095;
  void* fm=mmap(0,flen,PROT_READ,MAP_PRIVATE,fd,0);
  close(fd);
  if(fm==MAP_FAILED)die("MMAP: map failed");
  Str* s=(Str*)uf_gc_alloc(sizeof(Str),0);
  s->tag=HT_STR; s->len=n; s->esz=1; s->mlen=flen; s->mdata=(const char*)fm;
  s->gc_flags|=GCF_MMAP;
  pushp(cx,s);
}
/* FEACH: path fn_addr ->  (fn: line -> cont; streamed; 0 stops early) */
static void op_feach(Ctx*cx){
  Cell f=pop(cx),p=pop(cx);
  uf_fs_gate(uf_sptr(p),0);
  FILE* fp=fopen(uf_sptr(p),"r"); if(!fp)die("FEACH: cannot open file");
  char* line=0; size_t ncap=0; ssize_t m;
  while((m=getline(&line,&ncap,fp))>=0){
    while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0;
    Cell ls=uf_str_new(line,(size_t)m);
    pushc(cx,ls); uf_call_addr(cx,(const void*)f.i,0,-1,1); Cell k=pop(cx);
    if(!uf_truthy(k))break;
  }
  free(line); fclose(fp);
}
/* FFOLD: path init fn_addr -> acc (fn: acc line -> acc) */
static void op_ffold(Ctx*cx){
  Cell f=pop(cx),acc=pop(cx),p=pop(cx);
  uf_fs_gate(uf_sptr(p),0);
  FILE* fp=fopen(uf_sptr(p),"r"); if(!fp)die("FFOLD: cannot open file");
  char* line=0; size_t ncap=0; ssize_t m;
  while((m=getline(&line,&ncap,fp))>=0){
    while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0;
    Cell ls=uf_str_new(line,(size_t)m);
    pushc(cx,acc); pushc(cx,ls); uf_call_addr(cx,(const void*)f.i,0,-1,2); acc=pop(cx);
  }
  free(line); fclose(fp);
  pushc(cx,acc);
}
/* FSPLIT: path sep init fn_addr -> acc
   Streaming file read + split. Callback receives (acc, line_list) where
   line_list is a special field-list storing offsets into the line buffer.
   Access fields via FGET (field-list index -> str). Eliminates per-field
   GC allocations — only the line string and field-list are GC-managed. */
/* thread-local current fsplit state */
static _Thread_local char* uf_fsplit_line = 0;
static _Thread_local Hdr* uf_fsplit_parent = 0; /* GC line string for view tracing */
static _Thread_local int64_t uf_fsplit_offsets[256];
static _Thread_local int uf_fsplit_nfields = 0;
static void op_fsplit(Ctx*cx){
  Cell f=pop(cx),acc=pop(cx),sep=pop(cx),p=pop(cx);
  uf_fs_gate(uf_sptr(p),0);
  FILE* fp=fopen(uf_sptr(p),"r"); if(!fp)die("FSPLIT: cannot open file");
  const char* E=uf_sptr(sep);
  if(!*E)die("FSPLIT: empty separator");
  size_t el=strlen(E);
  char* line=0; size_t ncap=0; ssize_t m;
  while((m=getline(&line,&ncap,fp))>=0){
    while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0;
    /* create GC line string (owns the data, parent for fget views) */
    Cell lc=uf_str_new(line,(size_t)m);
    uf_fsplit_parent=uf_gc_find((void*)lc.i);
    uf_fsplit_line=(char*)uf_sptr(lc);
    /* record field offsets + NUL-terminate in place */
    uf_fsplit_nfields=0;
    char* cur=uf_fsplit_line;
    while(uf_fsplit_nfields<128){
      char* sp=el==1?(char*)memchr(cur,E[0],strlen(cur)):strstr(cur,E);
      if(!sp){
        uf_fsplit_offsets[uf_fsplit_nfields*2]=(int64_t)(cur-uf_fsplit_line);
        uf_fsplit_offsets[uf_fsplit_nfields*2+1]=(int64_t)strlen(cur);
        uf_fsplit_nfields++;
        break;
      }
      *sp=0;
      uf_fsplit_offsets[uf_fsplit_nfields*2]=(int64_t)(cur-uf_fsplit_line);
      uf_fsplit_offsets[uf_fsplit_nfields*2+1]=(int64_t)(sp-cur);
      uf_fsplit_nfields++;
      cur=sp+el;
    }
    pushc(cx,acc); pushi(cx,uf_fsplit_nfields); uf_call_addr(cx,(const void*)f.i,0,-1,2); acc=pop(cx);
  }
  free(line); fclose(fp);
  uf_fsplit_line=0; uf_fsplit_parent=0;
  pushc(cx,acc);
}
/* FGET: field_index -> str (zero-copy view into current fsplit line) */
static void op_fget(Ctx*cx){
  int64_t idx=pop(cx).i;
  if(idx<0||idx>=uf_fsplit_nfields)die("FGET: index out of bounds");
  int64_t off=uf_fsplit_offsets[idx*2], len=uf_fsplit_offsets[idx*2+1];
  Str* v=(Str*)uf_gc_alloc(sizeof(Str),0);
  v->tag=HT_STR; v->esz=1; v->len=(uint64_t)len; v->mlen=0;
  v->mdata=uf_fsplit_line+off; v->gc_parent=uf_fsplit_parent;
  pushp(cx,v);
}
/* FATOI: field_index -> int (parse field directly from line buffer, no alloc) */
static void op_fatoi(Ctx*cx){
  int64_t idx=pop(cx).i;
  if(idx<0||idx>=uf_fsplit_nfields)die("FATOI: index out of bounds");
  pushi(cx,(int64_t)strtoll(uf_fsplit_line+uf_fsplit_offsets[idx*2],0,10));
}
/* FATOF: field_index -> float (parse field directly, no alloc) */
static void op_fatof(Ctx*cx){
  int64_t idx=pop(cx).i;
  if(idx<0||idx>=uf_fsplit_nfields)die("FATOF: index out of bounds");
  pushf(cx,strtod(uf_fsplit_line+uf_fsplit_offsets[idx*2],0));
}
/* FSGET: field_idx offset len -> str_view (zero-alloc substring of a field) */
static void op_fsget(Ctx*cx){
  int64_t len=pop(cx).i, off=pop(cx).i, idx=pop(cx).i;
  if(idx<0||idx>=uf_fsplit_nfields)die("FSGET: index out of bounds");
  int64_t base=uf_fsplit_offsets[idx*2], flen=uf_fsplit_offsets[idx*2+1];
  if(off<0||len<0||off+len>flen)die("FSGET: out of bounds");
  Str* v=(Str*)uf_gc_alloc(sizeof(Str),0);
  v->tag=HT_STR; v->esz=1; v->len=(uint64_t)len; v->mlen=0;
  v->mdata=uf_fsplit_line+base+off; v->gc_parent=uf_fsplit_parent;
  pushp(cx,v);
}
/* FBYTE: field_idx offset -> int (single byte from field, no alloc) */
static void op_fbyte(Ctx*cx){
  int64_t off=pop(cx).i, idx=pop(cx).i;
  if(idx<0||idx>=uf_fsplit_nfields)die("FBYTE: index out of bounds");
  int64_t base=uf_fsplit_offsets[idx*2], flen=uf_fsplit_offsets[idx*2+1];
  if(off<0||off>=flen)die("FBYTE: out of bounds");
  pushi(cx,(uint8_t)uf_fsplit_line[base+off]);
}
/* ADDTO: dict key amount -> (dict[key] += amount; missing starts at 0) */
static void op_addto(Ctx*cx){
  Cell v=pop(cx),k=pop(cx),h=pop(cx); Map*m=(Map*)uf_handle(h,"ADDTO");
  Cell cur; if(map_get(m,k,&cur)) cur=uf_cadd(cur,v); else cur=v;
  map_put(m,k,cur);
}
/* FADDTO: dict field_idx amount -> (dict[field] += amount, no Str alloc)
   Hashes the raw field from the fsplit line buffer directly against
   stored Str keys — bypasses Cell/gc_find/Str allocation entirely. */
static void op_faddto(Ctx*cx){
  Cell v=pop(cx); int64_t idx=pop(cx).i; Cell h=pop(cx);
  Map*m=(Map*)uf_handle(h,"FADDTO");
  if(idx<0||idx>=uf_fsplit_nfields)die("FADDTO: field index out of bounds");
  const char* fk=uf_fsplit_line+uf_fsplit_offsets[idx*2];
  int64_t flen=uf_fsplit_offsets[idx*2+1];
  uint64_t fh=uf_fnv(fk,(uint64_t)flen);
  /* probe: compare raw bytes against stored Str keys */
  uint64_t i=fh%m->cap;
  for(;;){
    if(m->st[i]==0) break; /* not found */
    if(m->st[i]==1){
      Cell ek=m->keys[i];
      Hdr*eh=ek.tag==T_PTR?(Hdr*)(void*)ek.i:0;
      if(eh&&eh->tag==HT_STR&&eh->len==(uint64_t)flen&&
         memcmp(uf_sbytes((Str*)eh),fk,(size_t)flen)==0){
        if(m->vals[i].tag==T_INT&&v.tag==T_INT) m->vals[i].i+=v.i;
        else if(m->vals[i].tag==T_FLOAT&&v.tag==T_FLOAT) m->vals[i]=uf_mkf(uf_f(m->vals[i])+uf_f(v));
        else m->vals[i]=uf_cadd(m->vals[i],v);
        return; /* found: add */
      }
    }
    i=(i+1)%m->cap;
  }
  /* not found: copy the key so it survives beyond the callback */
  Str* sk2=(Str*)uf_gc_alloc(sizeof(Str)+flen+1,0);
  sk2->tag=HT_STR; sk2->esz=1; sk2->len=(uint64_t)flen; sk2->mlen=0;
  memcpy(sk2->data,fk,(size_t)flen); sk2->data[flen]=0;
  map_put(m,uf_mkp(sk2),v);
}
/* FINC: dict field_idx -> (dict[field] += 1, no Str alloc on repeat keys) */
static void op_finc(Ctx*cx){
  int64_t idx=pop(cx).i; Cell h=pop(cx);
  Map*m=(Map*)uf_handle(h,"FINC");
  if(idx<0||idx>=uf_fsplit_nfields)die("FINC: field index out of bounds");
  const char* fk=uf_fsplit_line+uf_fsplit_offsets[idx*2];
  int64_t flen=uf_fsplit_offsets[idx*2+1];
  uint64_t fh=uf_fnv(fk,(uint64_t)flen);
  uint64_t i=fh%m->cap;
  for(;;){
    if(m->st[i]==0) break;
    if(m->st[i]==1){
      Cell ek=m->keys[i];
      Hdr*eh=ek.tag==T_PTR?(Hdr*)(void*)ek.i:0;
      if(eh&&eh->tag==HT_STR&&eh->len==(uint64_t)flen&&
         memcmp(uf_sbytes((Str*)eh),fk,(size_t)flen)==0){
        if(m->vals[i].tag==T_INT) m->vals[i].i++;
        else m->vals[i]=uf_cadd(m->vals[i],uf_mki(1));
        return;
      }
    }
    i=(i+1)%m->cap;
  }
  Str* sk2=(Str*)uf_gc_alloc(sizeof(Str)+flen+1,0);
  sk2->tag=HT_STR; sk2->esz=1; sk2->len=(uint64_t)flen; sk2->mlen=0;
  memcpy(sk2->data,fk,(size_t)flen); sk2->data[flen]=0;
  map_put(m,uf_mkp(sk2),uf_mki(1));
}
/* FMATCH: path pat -> chan (producer thread streams matching lines, cap 64) */
typedef struct { Ring* r; char* path; char* pat; } UfFm;
static void* uf_fmatch_worker(void* arg){
  UfFm* g=(UfFm*)arg;
  FILE* fp=fopen(g->path,"r");
  if(fp){
    char* line=0; size_t ncap=0; ssize_t m; RxCap caps[10];
    while((m=getline(&line,&ncap,fp))>=0){
      while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0;
      if(rx_exec(g->pat,line,caps)){ Cell v=uf_str_new(line,(size_t)m); ring_enq(g->r,v); }
    }
    free(line); fclose(fp);
  }
  ring_close(g->r);
  free(g->path); free(g->pat); free(g);
  return 0;
}
static void op_fmatch(Ctx*cx){
  Cell pat=pop(cx),p=pop(cx);
  uf_fs_gate(uf_sptr(p),0);
  Ring* r=uf_ring_new(64);
  UfFm* g=(UfFm*)malloc(sizeof(UfFm)); if(!g)die("out of memory");
  g->r=r; g->path=strdup(uf_sptr(p)); g->pat=strdup(uf_sptr(pat));
  pthread_t th; if(pthread_create(&th,0,uf_fmatch_worker,g)){ ring_close(r); die("FMATCH: thread"); }
  pthread_detach(th);
  pushp(cx,r);
}

/* ================= graph traversal (visited set via dict probe loop) ================= */
/* BFS: start fn_addr -> list (fn: node -> list of neighbors) */
static void op_bfs(Ctx*cx){
  Cell f=pop(cx),start=pop(cx);
  Map* seen=uf_map_new(); UF_PROTECT(&seen);
  Dyn* order=uf_dyn_new(8); UF_PROTECT(&order);
  Dyn* queue=uf_dyn_new(8); UF_PROTECT(&queue);
  map_put(seen,start,uf_mki(1));
  uf_dyn_push(&queue,start);
  uint64_t qi=0;
  while(qi<queue->len){
    Cell node=queue->data[qi++];
    uf_dyn_push(&order,node);
    pushc(cx,node); uf_call_addr(cx,(const void*)f.i,0,-1,1); Cell nb=pop(cx);
    Dyn* nbs=uf_materialize(cx,nb); UF_PROTECT(&nbs);
    for(uint64_t i=0;i<nbs->len;i++){
      Cell v;
      if(!map_get(seen,nbs->data[i],&v)){ map_put(seen,nbs->data[i],uf_mki(1)); uf_dyn_push(&queue,nbs->data[i]); }
    }
    UF_UNPROTECT();
  }
  UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,order);
}
/* DFS: start fn_addr -> list (pre-order) */
static void op_dfs(Ctx*cx){
  Cell f=pop(cx),start=pop(cx);
  Map* seen=uf_map_new(); UF_PROTECT(&seen);
  Dyn* order=uf_dyn_new(8); UF_PROTECT(&order);
  Dyn* stack=uf_dyn_new(8); UF_PROTECT(&stack);
  uf_dyn_push(&stack,start);
  while(stack->len){
    Cell node=stack->data[--stack->len];
    Cell v;
    if(map_get(seen,node,&v))continue;
    map_put(seen,node,uf_mki(1));
    uf_dyn_push(&order,node);
    pushc(cx,node); uf_call_addr(cx,(const void*)f.i,0,-1,1); Cell nb=pop(cx);
    Dyn* nbs=uf_materialize(cx,nb); UF_PROTECT(&nbs);
    for(uint64_t i=nbs->len;i>0;i--) uf_dyn_push(&stack,nbs->data[i-1]);
    UF_UNPROTECT();
  }
  UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,order);
}
/* WFIND: start fn_addr pred_addr -> v_or_0 (BFS, early exit) */
static void op_wfind(Ctx*cx){
  Cell pred=pop(cx),f=pop(cx),start=pop(cx);
  Map* seen=uf_map_new(); UF_PROTECT(&seen);
  Dyn* queue=uf_dyn_new(8); UF_PROTECT(&queue);
  map_put(seen,start,uf_mki(1));
  uf_dyn_push(&queue,start);
  uint64_t qi=0; int found=0; Cell result=uf_mki(0);
  while(qi<queue->len&&!found){
    Cell node=queue->data[qi++];
    pushc(cx,node); uf_call_addr(cx,(const void*)pred.i,0,-1,1); Cell k=pop(cx);
    if(uf_truthy(k)){ result=node; found=1; break; }
    pushc(cx,node); uf_call_addr(cx,(const void*)f.i,0,-1,1); Cell nb=pop(cx);
    Dyn* nbs=uf_materialize(cx,nb); UF_PROTECT(&nbs);
    for(uint64_t i=0;i<nbs->len;i++){
      Cell v;
      if(!map_get(seen,nbs->data[i],&v)){ map_put(seen,nbs->data[i],uf_mki(1)); uf_dyn_push(&queue,nbs->data[i]); }
    }
    UF_UNPROTECT();
  }
  UF_UNPROTECT(); UF_UNPROTECT();
  pushc(cx,result);
}

/* ================= JSON ================= */
typedef struct { const char* p; } JCur;
static void j_ws(JCur* j){ while(*j->p&&isspace((unsigned char)*j->p))j->p++; }
static Cell j_parse(JCur* j);
static Cell j_str(JCur* j){
  j->p++; /* opening quote */
  size_t cap=32,n=0; char* b=(char*)uf_alloc(cap,0);
  for(;;){
    char c=*j->p;
    if(!c){ free(b); die("JSON: unterminated string"); }
    j->p++;
    if(c=='"')break;
    if(c=='\\'){
      char e=*j->p++; 
      switch(e){
        case 'n': c='\n'; break; case 't': c='\t'; break; case 'r': c='\r'; break;
        case 'b': c='\b'; break; case 'f': c='\f'; break; case '/': c='/'; break;
        case '\\': c='\\'; break; case '"': c='"'; break;
        case 'u': { /* keep the ASCII byte of \u00XX; else '?') */
          if(j->p[0]=='0'&&j->p[1]=='0'){ char hex[3]={j->p[2],j->p[3],0}; c=(char)strtol(hex,0,16); }
          else c='?';
          j->p+=4; break; }
        default: free(b); die("JSON: bad escape");
      }
    }
    if(n+2>cap){ cap*=2; b=(char*)realloc(b,cap); if(!b)die("out of memory"); }
    b[n++]=c;
  }
  Cell r=uf_str_new(b,n); free(b); return r;
}
static Cell j_parse(JCur* j){
  j_ws(j);
  char c=*j->p;
  if(c=='{'){
    j->p++; Map* m=uf_map_new(); UF_PROTECT(&m);
    j_ws(j);
    if(*j->p=='}'){ j->p++; UF_UNPROTECT(); return uf_mkp(m); }
    for(;;){
      j_ws(j); if(*j->p!='"')die("JSON: object key must be a string");
      Cell k=j_str(j);
      j_ws(j); if(*j->p!=':')die("JSON: expected ':'");
      j->p++;
      Cell v=j_parse(j);
      map_put(m,k,v);
      j_ws(j);
      if(*j->p==','){ j->p++; continue; }
      if(*j->p=='}'){ j->p++; break; }
      die("JSON: expected ',' or '}'");
    }
    UF_UNPROTECT(); return uf_mkp(m);
  }
  if(c=='['){
    j->p++; Dyn* d=uf_dyn_new(8); UF_PROTECT(&d);
    j_ws(j);
    if(*j->p==']'){ j->p++; UF_UNPROTECT(); return uf_mkp(d); }
    for(;;){
      Cell v=j_parse(j); uf_dyn_push(&d,v);
      j_ws(j);
      if(*j->p==','){ j->p++; continue; }
      if(*j->p==']'){ j->p++; break; }
      die("JSON: expected ',' or ']'");
    }
    UF_UNPROTECT(); return uf_mkp(d);
  }
  if(c=='"') return j_str(j);
  if(!strncmp(j->p,"true",4)){ j->p+=4; return uf_mki(1); }
  if(!strncmp(j->p,"false",5)){ j->p+=5; return uf_mki(0); }
  if(!strncmp(j->p,"null",4)){ j->p+=4; return uf_mki(0); }
  if(c=='-'||isdigit((unsigned char)c)){
    const char* s=j->p; char* e;
    double d=strtod(s,&e);
    if(e==s)die("JSON: bad number");
    int isint=1;
    for(const char* q=s;q<e;q++) if(*q=='.'||*q=='e'||*q=='E'){isint=0;break;}
    j->p=e;
    if(isint){ int64_t v=strtoll(s,0,10); return uf_mki(v); }
    return uf_mkf(d);
  }
  die("JSON: malformed input");
  return uf_mki(0);
}
static void op_json(Ctx*cx){
  Cell st=pop(cx);
  JCur j={uf_sptr(st)};
  Cell v=j_parse(&j);
  j_ws(&j);
  if(*j.p)die("JSON: trailing garbage");
  pushc(cx,v);
}
/* UNJSON: v -> str (dict keys must be strings; atom/chan/iter/bitmap/bloom: dies) */
static void uf_unjson_w(Cell v,char** bp,size_t* np,size_t* capp){
#define UW(src,L) do{ size_t _l=(size_t)(L); while(*np+_l+1>*capp){ *capp*=2; *bp=(char*)realloc(*bp,*capp); if(!*bp)die("out of memory"); } memcpy(*bp+*np,(src),_l); *np+=_l; }while(0)
  char tmp[64];
  Hdr* a=v.tag==T_PTR&&v.i?uf_gc_find((void*)v.i):0;
  if(a){
    switch(a->tag){
      case HT_STR: {
        UW("\"",1);
        const char* s=uf_sbytes((Str*)a);
        for(uint64_t i=0;i<a->len;i++){ char c=s[i];
          if(c=='"'||c=='\\'){ UW("\\",1); UW(&c,1); }
          else if(c=='\n')UW("\\n",2);
          else if(c=='\t')UW("\\t",2);
          else if(c=='\r')UW("\\r",2);
          else if((unsigned char)c<0x20){ snprintf(tmp,sizeof tmp,"\\u%04x",c); UW(tmp,6); }
          else UW(&c,1);
        }
        UW("\"",1); return; }
      case HT_DYN: {
        Dyn* d=(Dyn*)a; UW("[",1);
        for(uint64_t i=0;i<d->len;i++){ if(i)UW(",",1); uf_unjson_w(d->data[i],bp,np,capp); }
        UW("]",1); return; }
      case HT_MAP: {
        Map* m=(Map*)a; UW("{",1); int first=1;
        for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1){
          if(!uf_is_str(m->keys[i]))die("UNJSON: dict keys must be strings");
          if(!first)UW(",",1); first=0;
          uf_unjson_w(m->keys[i],bp,np,capp); UW(":",1); uf_unjson_w(m->vals[i],bp,np,capp);
        }
        UW("}",1); return; }
      case HT_ARR: case HT_TENSOR: case HT_MAT: {
        UW("[",1);
        for(uint64_t i=0;i<a->len;i++){ if(i)UW(",",1); uf_unjson_w(uf_cidx(v,(int64_t)i),bp,np,capp); }
        UW("]",1); return; }
      default: die("UNJSON: unsupported handle (atom/chan/iter/bitmap/bloom)");
    }
  }
  if(v.tag==T_FLOAT){ snprintf(tmp,sizeof tmp,"%.17g",uf_f(v)); UW(tmp,strlen(tmp)); return; }
  snprintf(tmp,sizeof tmp,"%lld",(long long)v.i); UW(tmp,strlen(tmp));
#undef UW
}
static void op_unjson(Ctx*cx){
  Cell v=pop(cx);
  size_t cap=256,n=0; char* b=(char*)uf_alloc(cap,0);
  uf_unjson_w(v,&b,&n,&cap);
  Cell r=uf_str_new(b,n); free(b); pushc(cx,r);
}

/* ================= streaming sink ================= */
/* FEMIT: it path -> n (path on top; one item per line; ints/floats decimal,
   strings as-is, everything else unjson) */
static void op_femit(Ctx*cx){
  Cell p=pop(cx),h=pop(cx);
  uf_fs_gate(uf_sptr(p),1);
  FILE* f=fopen(uf_sptr(p),"w"); if(!f)die("FEMIT: cannot open file");
  Hdr* a=h.tag==T_PTR&&h.i?uf_gc_find((void*)h.i):0;
  Iter* it = (a&&a->tag==HT_ITER) ? (Iter*)a : uf_iter_new(h);
  UF_PROTECT(&it);
  Cell v; int64_t n=0;
  while(uf_iter_next(cx,it,&v)){
    Hdr* e=v.tag==T_PTR&&v.i?uf_gc_find((void*)v.i):0;
    if(e&&e->tag==HT_STR){ fwrite(uf_sbytes((Str*)e),1,e->len,f); }
    else if(v.tag==T_FLOAT){ fprintf(f,"%.17g",uf_f(v)); }
    else if(v.tag==T_INT||v.tag==T_TIME||v.tag==T_DUR){ fprintf(f,"%lld",(long long)v.i); }
    else {
      size_t cap=256,m=0; char* b=(char*)uf_alloc(cap,0);
      uf_unjson_w(v,&b,&m,&cap);
      fwrite(b,1,m,f); free(b);
    }
    fputc('\n',f); n++;
  }
  UF_UNPROTECT();
  fclose(f);
  pushi(cx,n);
}

/* ================= capability discovery ================= */
/* CAP: name -> 0/1. Pseudo-caps: fs.workspace (a workspace restriction is
   active), compute (GPU offload permitted). Never sandbox-gated itself. */
static void op_cap(Ctx*cx){
  Cell n=pop(cx);
  const char* s=uf_sptr(n);
  if(!strcmp(s,"fs.workspace")){ pushi(cx,(uf_sb_on&&uf_ws_nroots>0)?1:0); return; }
  if(!strcmp(s,"compute")){ pushi(cx,1); return; }
  long i=uf_cap_index(s);
  if(i<0)die("CAP: unknown capability (valid: fs.read fs.write proc ffi.use ffi.import raw.syscall raw.mem host.argv fs.workspace compute)");
  pushi(cx,uf_sb_caps[i]);
}
/* CAPS: -> dict {policy, <each capability>, fs.workspace, workspace, modules, device} */
static void op_caps(Ctx*cx){
  Map* d=uf_map_new(); UF_PROTECT(&d);
  map_put(d,uf_str_new("policy",6),uf_str_new(uf_sb_policy,strlen(uf_sb_policy)));
  for(int i=0;i<8;i++)
    map_put(d,uf_str_new(uf_cap_names[i],strlen(uf_cap_names[i])),uf_mki(uf_sb_caps[i]));
  map_put(d,uf_str_new("fs.workspace",12),uf_mki((uf_sb_on&&uf_ws_nroots>0)?1:0));
  map_put(d,uf_str_new("device",6),uf_str_new(uf_device,strlen(uf_device)));
  {
    Dyn* ws=uf_dyn_new((uint64_t)(uf_ws_nroots>0?(uint64_t)uf_ws_nroots:1)); UF_PROTECT(&ws);
    for(long i=0;i<uf_ws_nroots;i++)
      uf_dyn_push(&ws,uf_str_new(uf_ws_roots[i],strlen(uf_ws_roots[i])));
    Cell wc; wc.tag=T_PTR; wc.i=(int64_t)(void*)ws;
    map_put(d,uf_str_new("workspace",9),wc);
    UF_UNPROTECT();
  }
  {
    Dyn* ms=uf_dyn_new(8); UF_PROTECT(&ms);
    if(uf_mod_allow_n<0){
      Cell sc=uf_str_new("*",1);
      uf_dyn_push(&ms,sc);
    } else {
      for(long i=0;i<uf_mod_allow_n;i++)
        uf_dyn_push(&ms,uf_str_new(uf_mod_allow[i],strlen(uf_mod_allow[i])));
    }
    Cell mc; mc.tag=T_PTR; mc.i=(int64_t)(void*)ms;
    map_put(d,uf_str_new("modules",7),mc);
    UF_UNPROTECT();
  }
  UF_UNPROTECT();
  Cell rc; rc.tag=T_PTR; rc.i=(int64_t)(void*)d;
  pushc(cx,rc);
}

/* ================= error containment ================= */
static int uf_try_once(Ctx*cx, const void* a, Cell* out){
  UfTry t; t.prev=uf_try_top; t.sp=cx->sp; t.csp=cx->csp; t.local_base=cx->local_base; t.local_fsp=cx->local_fsp; uf_try_top=&t;
  if(setjmp(t.jb)==0){
    uf_call_addr(cx,a,0,-1,0);
    uf_try_top=t.prev;
    Cell r = cx->sp>t.sp ? pop(cx) : uf_mki(0);
    cx->sp=t.sp;
    *out=r;
    return 1;
  }
  uf_try_top=t.prev;
  cx->sp=t.sp;
  cx->csp=t.csp;
  cx->local_base=t.local_base;
  cx->local_fsp=t.local_fsp;
  if(uf_cur_task)((WeaveTask*)uf_cur_task)->tolerated++;
  return 0;
}
/* TRY: body_addr -> [result ok] */
static void op_try(Ctx*cx){
  Cell a=pop(cx); Cell r; Dyn*d=uf_dyn_new(2); UF_PROTECT(&d);
  if(uf_try_once(cx,(const void*)a.i,&r)){ uf_dyn_push(&d,r); uf_dyn_push(&d,uf_mki(1)); }
  else { uf_dyn_push(&d,uf_mki(0)); uf_dyn_push(&d,uf_mki(0)); }
  UF_UNPROTECT(); pushp(cx,d);
}
/* RETRY: n body_addr -> [result ok] (up to n+1 attempts, first success stops) */
static void op_retry(Ctx*cx){
  Cell a=pop(cx); int64_t n=pop(cx).i; Cell r;
  for(int64_t k=0;;k++){
    if(uf_try_once(cx,(const void*)a.i,&r)){
      if(k&&uf_cur_task)((WeaveTask*)uf_cur_task)->retries+=k;
      Dyn*d=uf_dyn_new(2); UF_PROTECT(&d); uf_dyn_push(&d,r); uf_dyn_push(&d,uf_mki(1)); UF_UNPROTECT(); pushp(cx,d); return;
    }
    if(k>=n){ Dyn*d=uf_dyn_new(2); UF_PROTECT(&d); uf_dyn_push(&d,uf_mki(0)); uf_dyn_push(&d,uf_mki(0)); UF_UNPROTECT(); pushp(cx,d); return; }
  }
}

/* ================= detached threads ================= */
typedef struct { const void* body; Ring* r; } UfSpawn;
const void* const* uf_spawn_ltab; const long* uf_spawn_frames;
static long nk_spawn_frame(const void* body);
static void* uf_spawn_worker(void* arg){
  UfSpawn* g=(UfSpawn*)arg;
  Ctx* c=ctx_new(1<<16,1<<12);
  /* v14: spawned bodies may bind locals — the generated code defines
     uf_spawn_ltab/uf_spawn_frames (address -> frame size); bump the callee
     frame exactly like _call does. */
  long f=nk_spawn_frame(g->body);
  uf_call_addr(c,g->body,f,-1,0);
  Cell r = c->sp>0 ? c->ds[c->sp-1] : uf_mki(0);
  ring_enq(g->r,r);
  ring_close(g->r);
  ctx_free(c);
  free(g);
  return 0;
}
/* SPAWN: body_addr -> chan (cap 1; body's top-of-stack enqueued at end,
   then closed — deq on it is a join) */
static long nk_spawn_frame(const void* body){
  if(!uf_spawn_ltab) return 0;
  for(long q=0; uf_spawn_ltab[q]; q++){ if(uf_spawn_ltab[q]==body) return uf_spawn_frames[q]; }
  return 0;
}
static void op_spawn(Ctx*cx){
  Cell a=pop(cx);
  Ring* r=uf_ring_new(1);
  UfSpawn* g=(UfSpawn*)malloc(sizeof(UfSpawn)); if(!g)die("out of memory");
  g->body=(const void*)a.i; g->r=r;
  pthread_t th; if(pthread_create(&th,0,uf_spawn_worker,g)){ ring_close(r); die("SPAWN: thread"); }
  pthread_detach(th);
  pushp(cx,r);
}
/* init-TU worker: fire-and-forget thread for init.nd entry points.
   Same as uf_spawn_worker minus the chan — process exit kills these. */
static void* uf_init_worker(void* arg){
  Ctx* c=ctx_new(1<<16,1<<12);
  uf_call_addr(c,arg,0,-1,0);
  ctx_free(c);
  return 0;
}
"#;
