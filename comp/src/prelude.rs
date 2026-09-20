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
static void* uf_gc_alloc(size_t sz, int align){
  sz = sz ? sz : 1;
  if(uf_gc_on && uf_gc_bytes_since + sz > uf_gc_threshold) uf_gc_collect();
  void* p = NULL;
  if(align>0){ if(posix_memalign(&p,(size_t)align,sz))die("alloc failed"); }
  else { p=malloc(sz); }
  if(!p)die("out of memory");
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
  memset(p,0,sizeof(Hdr));
  Hdr* h=(Hdr*)p;
  h->gc_flags = ((uint64_t)atomic_fetch_add(&uf_gc_seq,1))<<GCF_SEQSHIFT;
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
static inline Cell uf_cshr(Cell a){ return uf_mki((int64_t)((uint64_t)uf_i(a)>>1)); }
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
static void op_sub(Ctx*cx){ Cell b=pop(cx),a=pop(cx); if(uf_numarr(a)||uf_numarr(b))pushc(cx,uf_poly_arith(a,b,1,"sub")); else pushc(cx,uf_csub(a,b)); }
static void op_mul(Ctx*cx){ Cell b=pop(cx),a=pop(cx); if(uf_numarr(a)||uf_numarr(b))pushc(cx,uf_poly_arith(a,b,2,"mul")); else pushc(cx,uf_cmul(a,b)); }
static void op_and(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_cand(a,b)); }
static void op_pow(Ctx*cx){ Cell b=pop(cx),a=pop(cx); double x=uf_to_number(a),y=uf_to_number(b); pushc(cx,uf_mkf(pow(x,y))); }
static void op_sqrt(Ctx*cx){ Cell a=pop(cx);
  UF_PROTECT((void**)(void*)&a.i);
#ifdef NK_GPU
  if(a.tag==T_PTR&&a.i){ Hdr*h=uf_gc_find((void*)a.i);
    if(h&&uf_is_arrish(h)&&h->ety==1&&h->len>=(uint64_t)uf_gpu_min()){
      struct UFPC pc; memset(&pc,0,sizeof pc); pc.n0=(int64_t)h->len;
      Hdr*r=uf_arr_like(h,h->len); UF_PROTECT(&r);
      int k=uf_spv_index("esqrt");
      int ok=k>=0&&uf_vk_run(k,h->len,uf_data(h),(size_t)h->len*8,NULL,0,uf_data(r),(size_t)h->len*8,pc);
      UF_UNPROTECT();
      if(ok){ UF_UNPROTECT(); pushp(cx,r); return; }
    } }
#endif
  if(uf_numarr(a)){ Hdr*h=(Hdr*)(void*)a.i; uint64_t n=h->len; Hdr*r=uf_arr_like(h,n); UF_PROTECT(&r);
    if(h->ety==1){ const double*A=(const double*)uf_data(h); double*R=(double*)uf_data(r); for(uint64_t i=0;i<n;i++)R[i]=sqrt(A[i]); }
    else for(uint64_t i=0;i<n;i++) uf_put_el(r,i,sqrt(uf_el(h,i)));
    UF_UNPROTECT(); UF_UNPROTECT(); pushp(cx,r); return; }
  UF_UNPROTECT(); pushc(cx,uf_mkf(sqrt(uf_to_number(a)))); }
static void op_lte(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_clte(a,b)); }
static void op_gte(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_cgte(a,b)); }
static void op_drop(Ctx*cx){ (void)pop(cx); }
static void op_shutdown(Ctx*cx){ (void)cx; if(uf_active_job) atomic_store(&((WeaveJob*)uf_active_job)->shutdown,1); }
static void op_shr(Ctx*cx){ pushc(cx,uf_cshr(pop(cx))); }
static void op_inc(Ctx*cx){ pushc(cx,uf_cinc(pop(cx))); }
static void op_dec(Ctx*cx){ pushc(cx,uf_cdec(pop(cx))); }

/* v10 arithmetic & logic */
static void op_div(Ctx*cx){ Cell b=pop(cx),a=pop(cx); if(uf_numarr(a)||uf_numarr(b))pushc(cx,uf_poly_arith(a,b,3,"div")); else pushc(cx,uf_cdiv(a,b)); }
static void op_rem(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_crem(a,b)); }
static void op_eq(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushi(cx,uf_loose_eq(a,b)?1:0); }
static void op_seq(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushi(cx,uf_strict_eq(a,b)?1:0); }
static void op_sne(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushi(cx,uf_strict_eq(a,b)?0:1); }
static int uf_cmp(Cell a,Cell b,int* ok){ /* -1/0/1; *ok=0 if incomparable (legacy sort order) */
  *ok=1;
  if((a.tag==T_INT||a.tag==T_FLOAT||a.tag==T_TIME||a.tag==T_DUR)&&(b.tag==T_INT||b.tag==T_FLOAT||b.tag==T_TIME||b.tag==T_DUR)){
    double x=uf_f(a),y=uf_f(b); return x<y?-1:x>y?1:0;
  }
  if(uf_is_str(a)&&uf_is_str(b)) return strcmp(uf_sptr(a),uf_sptr(b));
  *ok=0; return 0;
}
static void op_lt(Ctx*cx){ Cell b=pop(cx),a=pop(cx); double x=uf_to_number(a),y=uf_to_number(b); pushi(cx,(isnan(x)||isnan(y))?0:(x<y?1:0)); }
static void op_gt(Ctx*cx){ Cell b=pop(cx),a=pop(cx); double x=uf_to_number(a),y=uf_to_number(b); pushi(cx,(isnan(x)||isnan(y))?0:(x>y?1:0)); }
static void op_not(Ctx*cx){ Cell a=pop(cx); pushi(cx,uf_truthy(a)?0:1); }
static void op_or(Ctx*cx){ Cell b=pop(cx),a=pop(cx); if(a.tag==T_FLOAT||b.tag==T_FLOAT||a.tag==T_PTR||b.tag==T_PTR)die("OR: ints only"); pushi(cx,a.i|b.i); }
static void op_xor(Ctx*cx){ Cell b=pop(cx),a=pop(cx); if(a.tag==T_FLOAT||b.tag==T_FLOAT||a.tag==T_PTR||b.tag==T_PTR)die("XOR: ints only"); pushi(cx,a.i^b.i); }
static void op_shl(Ctx*cx){ Cell b=pop(cx),a=pop(cx); if(a.tag==T_FLOAT||b.tag==T_FLOAT||a.tag==T_PTR||b.tag==T_PTR)die("SHL: ints only"); if(b.i<0||b.i>=64)die("SHL: shift out of range"); pushi(cx,a.i<<b.i); }
static void op_bnot(Ctx*cx){ Cell a=pop(cx); if(a.tag==T_FLOAT||a.tag==T_PTR)die("BNOT: ints only"); pushi(cx,~a.i); }
static void op_orelse(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_truthy(a)?a:b); }

static void* uf_alloc(size_t sz,int align){ void*p=NULL; if(align>0){ if(posix_memalign(&p,(size_t)align,sz?sz:1))die("alloc failed"); } else { p=malloc(sz?sz:1); } if(!p)die("out of memory"); return p; }
static void op_arrn(Ctx*cx,uint64_t tag,int align){ int64_t ty=pop(cx).i; Cell top=pop(cx); int64_t esz=(ty==3)?1:8; if(top.tag==T_PTR && top.i && uf_gc_find((void*)top.i) && ((Hdr*)(void*)top.i)->tag==HT_DYN){
    /* v13: `list type array` — copy the list's elements into a typed array */
    Dyn* d=(Dyn*)(void*)top.i; uint64_t len=d->len;
    UF_PROTECT((void**)(void*)&top.i);
    Hdr*h=(Hdr*)uf_gc_alloc(sizeof(Hdr)+(size_t)len*(size_t)esz,align); h->tag=tag; h->len=len; h->esz=(uint64_t)esz; h->ety=(uint64_t)ty;
    for(uint64_t i=0;i<len;i++){
      Cell c=d->data[i];
      if(ty==3) ((uint8_t*)h->data)[i]=(uint8_t)uf_i(c);
      else if(ty==1) ((double*)h->data)[i]=uf_f(c);
      else ((int64_t*)h->data)[i]=uf_i(c);
    }
    UF_UNPROTECT(); pushp(cx,h); return;
  }
  int64_t len=top.i; if(len<0)die("negative length"); Hdr*h=(Hdr*)uf_gc_alloc(sizeof(Hdr)+(size_t)len*(size_t)esz,align); h->tag=tag; h->len=(uint64_t)len; h->esz=(uint64_t)esz; h->ety=(uint64_t)ty; memset(h->data,0,(size_t)len*(size_t)esz); pushp(cx,h); }
static void op_arr(Ctx*cx){ op_arrn(cx,HT_ARR,0); }
static Hdr* uf_mat_new(uint64_t rows,uint64_t cols,uint64_t ety);
static void op_tensor(Ctx*cx){
  /* v13.1 2-D form: [rows cols] type tensor -> matrix (HT_MAT; len=rows*cols,
     esz=rows, cols=len/rows). 1-D form is unchanged. */
  if(cx->sp>=2){
    Cell shp=cx->ds[cx->sp-2];
    if(shp.tag==T_PTR&&shp.i){
      Hdr*sh=uf_gc_find((void*)shp.i);
      if(sh&&sh->tag==HT_DYN&&sh->len==2){
        Cell tyc=pop(cx); (void)pop(cx);
        Dyn*d=(Dyn*)(void*)shp.i;
        int64_t rows=uf_i(d->data[0]),cols=uf_i(d->data[1]);
        if(rows<0||cols<0)die("tensor: negative shape");
        pushp(cx,uf_mat_new((uint64_t)rows,(uint64_t)cols,(uint64_t)tyc.i));
        return;
      }
      if(sh&&sh->tag==HT_DYN&&sh->len>=3){
        Dyn*d=(Dyn*)(void*)shp.i;
        int64_t rows=uf_i(d->data[0]),cols=uf_i(d->data[1]);
        if(rows<0||cols<0)die("tensor: negative shape");
        if(sh->len==(uint64_t)(rows*cols)+2){
          /* [rows cols v0 v1 ...] type tensor — matrix from flat row-major data */
          Cell tyc=pop(cx); (void)pop(cx);
          uint64_t ety=(uint64_t)tyc.i, eszb=(ety==3)?1:8;
          UF_PROTECT((void**)(void*)&shp.i);
          Hdr*h=uf_mat_new((uint64_t)rows,(uint64_t)cols,ety); UF_PROTECT(&h);
          for(uint64_t i=0;i<(uint64_t)(rows*cols);i++){
            Cell c=d->data[i+2];
            char*dt=uf_data(h);
            if(ety==3)((uint8_t*)dt)[i]=(uint8_t)uf_i(c);
            else if(ety==1)((double*)dt)[i]=uf_f(c);
            else ((int64_t*)dt)[i]=uf_i(c);
          }
          (void)eszb;
          UF_UNPROTECT(); UF_UNPROTECT(); pushp(cx,h); return;
        }
      }
    }
  }
  op_arrn(cx,HT_TENSOR,64);
}
static void op_clone(Ctx*cx){
  Cell h=pop(cx); Hdr*a=(Hdr*)uf_gc_find((void*)h.i);
  if(!a)die("CLONE: not a managed object");
  if(a->tag==HT_ITER)die("CLONE: iterators are single-use");
  if(a->tag!=HT_ARR&&a->tag!=HT_TENSOR&&a->tag!=HT_MAT)die("CLONE: only arr/tensor/matrix");
  size_t nb=(a->tag==HT_MAT)?((size_t)a->len*((a->ety==3)?1:8)):(size_t)a->len*a->esz;
  size_t sz=sizeof(Hdr)+nb; UF_PROTECT((void**)(void*)&h.i);
  Hdr*n=(Hdr*)uf_gc_alloc(sz,a->tag==HT_TENSOR?64:0);
  memcpy(n,a,sz); n->gc_next=0; n->gc_flags=((uint64_t)atomic_fetch_add(&uf_gc_seq,1))<<GCF_SEQSHIFT;
  UF_UNPROTECT(); pushp(cx,n);
}
/* CAST (_cast, static): C-style conversion, no value-content interpretation.
   [v type] -> v'.
   int (0): float truncates to i64, ptr reinterprets its address, byte widens.
   float (1): int/byte widen to double; float is identity; else dies.
   ptr (2): numeric payloads reinterpret as a handle; handles pass through.
   byte (3): truncate the i64 payload to its low 8 bits (byte cell).
   >=1000: checked struct downcast (struct id); dies on mismatch. */
static void op_cast(Ctx*cx){
  Cell id=pop(cx); Cell h=pop(cx); int64_t ty=id.i;
  if(ty>=1000){
    if(!h.i) die("CAST: null handle");
    Hdr*a=(Hdr*)((void*)h.i); int64_t tk=(a->tag==HT_OBJ)?1000+(int64_t)a->len:(int64_t)a->tag;
    if(tk!=ty)die("CAST: type mismatch"); pushc(cx,h); return;
  }
  switch(ty){
    case 0:
      if(h.tag==T_FLOAT) pushi(cx,(int64_t)uf_fbits(h.i));
      else pushi(cx,h.i);
      return;
    case 1:
      if(h.tag==T_FLOAT) pushc(cx,h);
      else if(h.tag==T_INT||h.tag==T_BYTE) pushf(cx,(double)h.i);
      else die("CAST float: not a scalar");
      return;
    case 2:
      if(h.tag==T_PTR) pushc(cx,h);
      else pushp(cx,(void*)h.i);
      return;
    case 3: {
      Cell b; b.tag=T_BYTE; b.i=h.i&0xff; pushc(cx,b); return;
    }
    default: { char _b[96]; snprintf(_b,sizeof(_b),"CAST: unsupported type id %lld",(long long)ty); die(_b); }
  }
}
/* DCAST (cast, dynamic): content-aware conversion — the explicit form of
   universal coercion. [v type] -> v'.
   int (0): universal numeric coercion (strings parsed, single-element lists
   unwrapped), then integer truncation; NaN dies.
   float (1): universal numeric coercion (NaN allowed).
   ptr (2): static reinterpret (no content inspection).
   byte (3): truncate the i64 payload to its low 8 bits.
   str (9, the tag): universal string coercion (rendered representation).
   >=1000: checked struct downcast, exactly as _cast. */
static void op_dcast(Ctx*cx){
  Cell id=pop(cx); Cell v=pop(cx); int64_t ty=id.i;
  if(ty>=1000){
    if(!v.i) die("cast: null handle");
    Hdr*a=(Hdr*)((void*)v.i); int64_t tk=(a->tag==HT_OBJ)?1000+(int64_t)a->len:(int64_t)a->tag;
    if(tk!=ty)die("cast: type mismatch"); pushc(cx,v); return;
  }
  switch(ty){
    case 0: { double d=uf_to_number(v); if(isnan(d))die("cast int: value has no numeric form"); pushi(cx,(int64_t)d); return; }
    case 1: pushf(cx,uf_to_number(v)); return;
    case 2:
      if(v.tag==T_PTR) pushc(cx,v);
      else pushp(cx,(void*)v.i);
      return;
    case 3: { Cell b; b.tag=T_BYTE; b.i=v.i&0xff; pushc(cx,b); return; }
    case 9: pushc(cx,uf_to_string(v)); return;
    default: { char _b[96]; snprintf(_b,sizeof(_b),"cast: unsupported type id %lld",(long long)ty); die(_b); }
  }
}

/* OBJ: size in low 32 bits of the operand, struct id above; esz = byte size,
   len = struct id. Field access via the container protocol. */
static void op_obj(Ctx*cx){ int64_t v=pop(cx).i; int64_t sz=v&0xffffffffLL; int64_t sid=v>>32; if(sz<=0)sz=8; Hdr*h=(Hdr*)uf_gc_alloc(sizeof(Hdr)+(size_t)sz,0); h->tag=HT_OBJ; h->len=(uint64_t)sid; h->esz=(uint64_t)sz; memset(h->data,0,(size_t)sz); pushp(cx,h); }

/* reflection tables, populated by generated uf_init_reflection() */
static long uf_st_n=0; static const int64_t* uf_st_sids=0; static const int64_t* uf_st_nf=0; static const char*** uf_st_fields=0; static const int64_t** uf_st_offs=0;
/* obj field offset by key: int -> raw offset; str -> field name. -1 = missing */
static int64_t uf_obj_off(Hdr* a, Cell k){
  if(k.tag==T_INT) return k.i;
  if(uf_is_str(k)){
    int64_t sid=(int64_t)a->len;
    for(long q=0;q<uf_st_n;q++) if(uf_st_sids[q]==sid){
      for(int64_t f=0;f<uf_st_nf[q];f++) if(strcmp(uf_sptr(k),uf_st_fields[q][f])==0) return uf_st_offs[q][f];
      return -1;
    }
  }
  return -1;
}

/* ================= uniform container protocol ================= */
static uint64_t uf_fnv(const void*p,size_t n){ const unsigned char*s=(const unsigned char*)p; uint64_t h=1469598103934665603ULL; for(size_t i=0;i<n;i++){ h^=s[i]; h*=1099511628211ULL; } return h; }
static uint64_t map_hash(Cell k){
  if(k.tag==T_INT) return (uint64_t)k.i; /* identity — dense 0..n-1 keys (bfs) */
  if(k.tag==T_PTR&&k.i){ Hdr*h=uf_gc_find((void*)k.i); if(h){ if(h->tag==HT_STR)return uf_fnv(uf_sbytes((Str*)h),h->len); return uf_fnv(&k.i,8); } return uf_fnv((void*)k.i,strlen((char*)k.i)); }
  return uf_fnv(&k.i,8);
}
static int map_keyeq(Cell a,Cell b){
  if(a.tag==T_INT&&b.tag==T_INT) return a.i==b.i;
  if(a.tag==T_PTR&&b.tag==T_PTR&&a.i&&b.i){
    Hdr*ha=uf_gc_find((void*)a.i); Hdr*hb=uf_gc_find((void*)b.i);
    if(ha&&hb){ if(ha->tag==HT_STR&&hb->tag==HT_STR){ if(ha->len!=hb->len)return 0; return memcmp(uf_sbytes((Str*)ha),uf_sbytes((Str*)hb),ha->len)==0; } return a.i==b.i; }
    if((ha&&ha->tag==HT_STR)||(hb&&hb->tag==HT_STR)) return strcmp(uf_sptr(a),uf_sptr(b))==0;
    return strcmp((char*)a.i,(char*)b.i)==0;
  }
  return a.i==b.i;
}
static void map_put_raw(Map*m,Cell k,Cell v){
  uint64_t i=map_hash(k)%m->cap;
  for(;;){ if(m->st[i]!=1){ m->st[i]=1; m->keys[i]=k; m->vals[i]=v; m->len++; return; } if(map_keyeq(m->keys[i],k)){ m->vals[i]=v; return; } i=(i+1)%m->cap; }
}
static void map_grow(Map*m){
  uint64_t ncap=m->cap*4; Cell*ok=m->keys,*ov=m->vals; unsigned char*os=m->st; uint64_t ocap=m->cap;
  m->cap=ncap; m->keys=(Cell*)uf_alloc(ncap*sizeof(Cell),0); m->vals=(Cell*)uf_alloc(ncap*sizeof(Cell),0); m->st=(unsigned char*)calloc(ncap,1); m->len=0;
  for(uint64_t i=0;i<ocap;i++) if(os[i]==1) map_put_raw(m,ok[i],ov[i]);
  free(ok); free(ov); free(os);
}
static Map* uf_map_new(void){ Map*m=(Map*)uf_gc_alloc(sizeof(Map),0); m->tag=HT_MAP; m->len=0; m->cap=64; m->keys=(Cell*)uf_alloc(64*sizeof(Cell),0); m->vals=(Cell*)uf_alloc(64*sizeof(Cell),0); m->st=(unsigned char*)calloc(64,1); return m; }
static void map_put(Map*m,Cell k,Cell v){ if((m->len+1)*10>=m->cap*7) map_grow(m); map_put_raw(m,k,v); }
static inline int map_get(Map*m,Cell k,Cell*out){
  if(m->cap==0)return 0;
  uint64_t i=map_hash(k)%m->cap;
  for(;;){ if(m->st[i]==0)return 0; if(m->st[i]==1&&map_keyeq(m->keys[i],k)){ *out=m->vals[i]; return 1; } i=(i+1)%m->cap; }
}
static void map_del(Map*m,Cell k){
  if(m->cap==0)return;
  uint64_t i=map_hash(k)%m->cap;
  for(;;){ if(m->st[i]==0)return; if(m->st[i]==1&&map_keyeq(m->keys[i],k)){ m->st[i]=2; m->len--; return; } i=(i+1)%m->cap; }
}
static Dyn* uf_dyn_new(uint64_t cap){ if(!cap)cap=1; Dyn*d=(Dyn*)uf_gc_alloc(sizeof(Dyn)+cap*sizeof(Cell),0); d->tag=HT_DYN; d->len=0; d->esz=sizeof(Cell); d->cap=cap; return d; }
/* grow-with-move: returns the (possibly new) list; old handle is reclaimed by GC */
static Dyn* uf_dyn_push2(Dyn*d,Cell c){ if(d->len>=d->cap){ Dyn*n=uf_dyn_new(d->cap*2); memcpy(n->data,d->data,d->len*sizeof(Cell)); n->len=d->len; d=n; } d->data[d->len++]=c; return d; }
static void uf_dyn_push(Dyn**pd,Cell c){ *pd=uf_dyn_push2(*pd,c); }
static void uf_dyn_push_str(Dyn**pd,const char*s,size_t n){ Cell c=uf_str_new(s,n); uf_dyn_push(pd,c); }

static Hdr* uf_handle(Cell h,const char* op){
  if(h.tag!=T_PTR||!h.i)die("handle is null");
  /* Non-moving GC: the object's tag is the type. uf_gc_find was a
     use-after-free check that dominated dict/list GET in bfs (~1M map
     probes + ~2.5M list indexes). A stale pointer now dies on a bad
     tag instead of a set lookup. */
  Hdr*a=(Hdr*)(void*)h.i;
  switch(a->tag){
    case HT_ARR: case HT_TENSOR: case HT_DYN: case HT_MAP: case HT_STR:
    case HT_RING: case HT_ATOM: case HT_BUF: case HT_OBJ: case HT_BITMAP:
    case HT_BLOOM: case HT_ITER: case HT_SET: case HT_MAT:
      (void)op; return a;
    default: die("not a managed handle");
  }
}
/* GET: h k -> v */
static inline void op_get(Ctx*cx){
  Cell k=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"GET");
  switch(a->tag){
    case HT_MAP: { Map*m=(Map*)a; Cell v; if(!map_get(m,k,&v))die("GET: missing key"); pushc(cx,v); return; }
    case HT_DYN: {
      Dyn*d=(Dyn*)a;
      if(k.i<0||(uint64_t)k.i>=d->len)die("GET: index out of bounds");
      pushc(cx,d->data[k.i]); return;
    }
    case HT_ARR: case HT_TENSOR: case HT_MAT: pushc(cx,uf_cidx(h,k.i)); return;
    case HT_STR: { Str*s=(Str*)a; if(k.i<0||k.i>=(int64_t)s->len)die("GET: index out of bounds"); pushi(cx,(uint8_t)uf_sbytes(s)[k.i]); return; }
    case HT_OBJ: { int64_t o=uf_obj_off(a,k); if(o<0||(uint64_t)o>=a->esz)die("GET: no such field"); pushc(cx,*(Cell*)(a->data+o)); return; }
    default: die("GET: unsupported handle");
  }
}
/* v13 container literals: consume n cells from the ds and build a list/dict.
   The cells are popped after the Dyn/Map allocation, so they stay rooted on
   the ds for the whole build (no GC hazard). */
static Cell uf_list_build(Ctx*cx, int64_t n){
  if(n<0)die("negative list literal length");
  Dyn* d=uf_dyn_new(n?(uint64_t)n:1); UF_PROTECT(&d);
  for(int64_t i=n-1;i>=0;i--) d->data[i]=pop(cx);
  d->len=(uint64_t)n; UF_UNPROTECT(); return uf_mkp(d);
}
static Cell uf_dict_build(Ctx*cx, int64_t n){
  if(n<0||(n&1))die("dict literal: element count must be even");
  Dyn* pairs=uf_dyn_new(n?n:2); UF_PROTECT(&pairs);
  for(int64_t i=n-1;i>=0;i--) pairs->data[i]=pop(cx);
  pairs->len=(uint64_t)n;
  Map* m=uf_map_new(); UF_PROTECT(&m);
  for(int64_t i=0;i<n;i+=2) map_put(m,pairs->data[i],pairs->data[i+1]);
  UF_UNPROTECT(); UF_UNPROTECT(); return uf_mkp(m);
}
/* GETQ: h k -> v_or_0 (never dies on absence; null handle -> 0) */
static inline void op_getq(Ctx*cx){
  Cell k=pop(cx),h=pop(cx);
  if(h.tag!=T_PTR||!h.i){ pushi(cx,0); return; }
  Hdr*a=uf_gc_find((void*)h.i); if(!a)die("GETQ: not a managed handle");
  switch(a->tag){
    case HT_MAP: { Map*m=(Map*)a; Cell v; if(map_get(m,k,&v))pushc(cx,v); else pushi(cx,0); return; }
    case HT_DYN: case HT_ARR: case HT_TENSOR: case HT_MAT: if(k.i<0||(uint64_t)k.i>=a->len)pushi(cx,0); else pushc(cx,uf_cidx(h,k.i)); return;
    case HT_STR: { Str*s=(Str*)a; if(k.i<0||k.i>=(int64_t)s->len)pushi(cx,0); else pushi(cx,(uint8_t)uf_sbytes(s)[k.i]); return; }
    case HT_OBJ: { int64_t o=uf_obj_off(a,k); if(o<0||(uint64_t)o>=a->esz)pushi(cx,0); else pushc(cx,*(Cell*)(a->data+o)); return; }
    default: die("GETQ: unsupported handle");
  }
}
/* SET: h k v -> v (v12 pass-through) */
static inline void op_set(Ctx*cx){
  Cell v=pop(cx),k=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"SET");
  switch(a->tag){
    case HT_MAP: map_put((Map*)a,k,v); break;
    case HT_DYN: case HT_ARR: case HT_TENSOR: case HT_MAT: uf_cseti(h,k.i,v); break;
    case HT_STR: { Str*s=(Str*)a; if(s->mlen)die("SET: mmap string is read-only"); if(k.i<0||k.i>=(int64_t)s->len)die("SET: index out of bounds"); s->data[k.i]=(char)v.i; break; }
    case HT_OBJ: { int64_t o=uf_obj_off(a,k); if(o<0||(uint64_t)o>=a->esz)die("SET: no such field"); *(Cell*)(a->data+o)=v; break; }
    default: die("SET: unsupported handle");
  }
  pushc(cx,v);
}
/* VGET: handle idx -> value (direct typed array read, no handle validation)
   Bypasses uf_handle/uf_gc_find/tag-switch. Assumes caller knows the handle
   is a valid arr/tensor. Uses the element type (ety) to do the right read. */
static void op_vget(Ctx*cx){
  int64_t idx=pop(cx).i; Cell h=pop(cx);
  Hdr*a=(Hdr*)h.i;
  if(idx<0||(uint64_t)idx>=a->len)die("VGET: index out of bounds");
  char*dt=uf_data(a);
  if(a->ety==1)pushf(cx,((double*)dt)[idx]);
  else if(a->ety==3)pushi(cx,(int64_t)((uint8_t*)dt)[idx]);
  else pushi(cx,((int64_t*)dt)[idx]);
}
/* VSET: handle idx value -> (direct typed array write, no handle validation) */
static void op_vset(Ctx*cx){
  Cell v=pop(cx); int64_t idx=pop(cx).i; Cell h=pop(cx);
  Hdr*a=(Hdr*)h.i;
  if(idx<0||(uint64_t)idx>=a->len)die("VSET: index out of bounds");
  char*dt=uf_data(a);
  if(a->ety==1)((double*)dt)[idx]=uf_f(v);
  else if(a->ety==3)((uint8_t*)dt)[idx]=(uint8_t)v.i;
  else ((int64_t*)dt)[idx]=v.i;
}
/* DEL: h k -> (dict only) */
static void op_del(Ctx*cx){
  Cell k=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"DEL");
  if(a->tag!=HT_MAP)die("DEL: only dict supports del");
  map_del((Map*)a,k);
}
/* HAS: h k -> 0/1 (null handle -> 0) */
static void op_has(Ctx*cx){
  Cell k=pop(cx),h=pop(cx);
  if(h.tag!=T_PTR||!h.i){ pushi(cx,0); return; }
  Hdr*a=uf_gc_find((void*)h.i); if(!a)die("HAS: not a managed handle");
  switch(a->tag){
    case HT_MAP: { Cell v; pushi(cx,map_get((Map*)a,k,&v)?1:0); return; }
    case HT_DYN: case HT_ARR: case HT_TENSOR: case HT_MAT: pushi(cx,(k.i>=0&&(uint64_t)k.i<a->len)?1:0); return;
    case HT_STR: { const char* s=uf_sptr(h); const char* n=uf_sptr(k); pushi(cx,(*n==0||strstr(s,n))?1:0); return; }
    case HT_OBJ: { int64_t o=uf_obj_off(a,k); pushi(cx,(o>=0&&(uint64_t)o<a->esz)?1:0); return; }
    default: die("HAS: unsupported handle");
  }
}
/* KEYS: h -> list (dict keys / obj field names) */
static void op_keys(Ctx*cx){
  Cell h=pop(cx); Hdr*a=uf_handle(h,"KEYS");
  if(a->tag==HT_MAP){
    Map*m=(Map*)a; Dyn*d=uf_dyn_new(m->len?m->len:1); UF_PROTECT(&d);
    for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1) uf_dyn_push(&d,m->keys[i]);
    UF_UNPROTECT(); pushp(cx,d); return;
  }
  if(a->tag==HT_OBJ){
    int64_t sid=(int64_t)a->len; Dyn*d=0;
    for(long q=0;q<uf_st_n;q++) if(uf_st_sids[q]==sid){
      d=uf_dyn_new((uint64_t)uf_st_nf[q]); UF_PROTECT(&d);
      for(int64_t f=0;f<uf_st_nf[q];f++){ Cell c=uf_str_new(uf_st_fields[q][f],strlen(uf_st_fields[q][f])); uf_dyn_push(&d,c); }
      UF_UNPROTECT(); pushp(cx,d); return;
    }
    die("KEYS: unknown struct id");
  }
  die("KEYS: unsupported handle");
}
/* TYPEOF: h -> tag (v10 numbering) */
static void op_typeof(Ctx*cx){
  Cell h=pop(cx);
  if(h.tag==T_PTR){ if(!h.i){pushi(cx,2);return;} Hdr*a=uf_gc_find((void*)h.i); pushi(cx,a?(int64_t)a->tag:2); return; }
  pushi(cx,(int64_t)h.tag);
}
/* LEN: h -> n */
static void op_len(Ctx*cx){
  Cell h=pop(cx); Hdr*a=uf_handle(h,"LEN");
  switch(a->tag){
    case HT_ARR: case HT_TENSOR: case HT_MAT: case HT_DYN: case HT_MAP: case HT_RING: pushi(cx,(int64_t)a->len); return;
    case HT_STR: pushi(cx,(int64_t)a->len); return;
    case HT_BITMAP: pushi(cx,(int64_t)a->len); return;
    case HT_ATOM: pushi(cx,1); return;
    case HT_BLOOM: pushi(cx,(int64_t)a->len); return;
    default: die("LEN: handle has no length");
  }
}
/* CAT: a b -> h' (str concat / arr concat / list concat) */
static void op_cat(Ctx*cx){
  Cell b=pop(cx),a=pop(cx);
  Hdr*ha=a.tag==T_PTR&&a.i?uf_gc_find((void*)a.i):0;
  Hdr*hb=b.tag==T_PTR&&b.i?uf_gc_find((void*)b.i):0;
  uint64_t ta=ha?ha->tag:0, tb=hb?hb->tag:0;
  UF_PROTECT((void**)(void*)&a.i); UF_PROTECT((void**)(void*)&b.i);
  if(ta==HT_DYN||tb==HT_DYN){
    if(ta!=HT_DYN||tb!=HT_DYN)die("CAT: list/str mismatch");
    Dyn*x=(Dyn*)ha,*y=(Dyn*)hb; Dyn*r=uf_dyn_new(x->len+y->len); UF_PROTECT(&r);
    for(uint64_t i=0;i<x->len;i++)uf_dyn_push(&r,x->data[i]);
    for(uint64_t i=0;i<y->len;i++)uf_dyn_push(&r,y->data[i]);
    UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT(); pushp(cx,r); return;
  }
  if((ta==HT_ARR||ta==HT_TENSOR)||(tb==HT_ARR||tb==HT_TENSOR)){
    if((ta!=HT_ARR&&ta!=HT_TENSOR)||(tb!=HT_ARR&&tb!=HT_TENSOR))die("CAT: arr/str mismatch");
    if(ha->ety!=hb->ety)die("CAT: arr element-type mismatch");
    uint64_t n=ha->len+hb->len; Hdr*r=(Hdr*)uf_gc_alloc(sizeof(Hdr)+n*ha->esz,0);
    UF_PROTECT(&r);
    r->tag=HT_ARR; r->len=n; r->esz=ha->esz; r->ety=ha->ety;
    memcpy(r->data,ha->data,ha->len*ha->esz); memcpy(r->data+ha->len*ha->esz,hb->data,hb->len*hb->esz);
    UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT(); pushp(cx,r); return;
  }
  { const char* x=uf_sptr(a),*y=uf_sptr(b); size_t la=strlen(x),lb=strlen(y);
    Str* r=(Str*)uf_gc_alloc(sizeof(Str)+la+lb+1,0); r->tag=HT_STR; r->esz=1; r->len=la+lb; r->mlen=0;
    memcpy(r->data,x,la); memcpy(r->data+la,y,lb+1); UF_UNPROTECT(); UF_UNPROTECT(); pushp(cx,r); }
}
/* SLICE: seq a b -> seq' (tag-dispatched; Python slice semantics) */
static void op_slice(Ctx*cx){
  Cell b=pop(cx),a=pop(cx),st=pop(cx);
  Hdr*h=st.tag==T_PTR&&st.i?uf_gc_find((void*)st.i):0;
  UF_PROTECT((void**)(void*)&st.i);
  if(!h){ /* legacy raw char* */
    const char* S=(const char*)st.i; int64_t n=(int64_t)strlen(S);
    int64_t i=a.i,j=b.i; if(i<0)i+=n; if(j<0)j+=n; if(i<0)i=0; if(j<0)j=0; if(i>n)i=n; if(j>n)j=n; if(j<i)j=i;
    UF_UNPROTECT(); pushc(cx,uf_str_new(S+i,(size_t)(j-i))); return;
  }
  int64_t n=(int64_t)h->len;
  int64_t i=a.i,j=b.i; if(i<0)i+=n; if(j<0)j+=n; if(i<0)i=0; if(j<0)j=0; if(i>n)i=n; if(j>n)j=n; if(j<i)j=i;
  if(h->tag==HT_STR){ Str*s=(Str*)h; UF_UNPROTECT(); pushc(cx,uf_str_new(uf_sbytes(s)+i,(size_t)(j-i))); return; }
  if(h->tag==HT_BUF){ UF_UNPROTECT(); pushc(cx,uf_str_new(h->data+i,(size_t)(j-i))); return; }
  if(h->tag==HT_DYN){
    Dyn*d=(Dyn*)h; Dyn*r=uf_dyn_new((uint64_t)(j-i)); UF_PROTECT(&r);
    for(int64_t q=i;q<j;q++)uf_dyn_push(&r,d->data[q]);
    UF_UNPROTECT(); UF_UNPROTECT(); pushp(cx,r); return;
  }
  if(h->tag==HT_ARR||h->tag==HT_TENSOR){
    Hdr*r=(Hdr*)uf_gc_alloc(sizeof(Hdr)+(size_t)(j-i)*h->esz,0);
    r->tag=h->tag; r->len=(uint64_t)(j-i); r->esz=h->esz; r->ety=h->ety;
    memcpy(r->data,uf_data(h)+i*h->esz,(size_t)(j-i)*h->esz);
    UF_UNPROTECT(); pushp(cx,r); return;
  }
  die("SLICE: unsupported handle");
}

static void op_buf(Ctx*cx){ int64_t sz=pop(cx).i; if(sz<0)die("negative BUF size"); Hdr*h=(Hdr*)uf_gc_alloc(sizeof(Hdr)+(size_t)sz,0); h->tag=HT_BUF; h->len=(uint64_t)sz; h->esz=1; h->gc_flags|=GCF_PINNED; memset(h->data,0,(size_t)sz); pushp(cx,h); }
static void op_bufcopy(Ctx*cx){ int64_t n=pop(cx).i; Cell s=pop(cx),d=pop(cx); if(n>0)memmove(((void*)d.i),((void*)s.i),(size_t)n); }
static void op_loadx(Ctx*cx){ Cell a=pop(cx); if(a.tag==T_PTR&&a.i){ Hdr*h=uf_gc_find((void*)a.i); if(h&&h->tag==HT_STR){ pushi(cx,(int64_t)(unsigned char)uf_sptr(a)[0]); return; } } pushi(cx,*(int64_t*)((void*)a.i)); }
static void op_storex(Ctx*cx){ Cell a=pop(cx); Cell v=pop(cx); *(int64_t*)((void*)a.i)=v.i; }
static void op_malloc(Ctx*cx){ int64_t sz=pop(cx).i; if(sz<0)die("negative MALLOC size"); void*p=malloc((size_t)sz?sz:1); if(!p)die("out of memory"); pushp(cx,p); }
static void op_free(Ctx*cx){ Cell p=pop(cx); free(((void*)p.i)); }
static void op_sizeof(Ctx*cx){ int64_t ty=pop(cx).i; pushi(cx,ty==3?1:8); }

/* ================= fmt / print / scan ================= */
static int uf_count(const char*f){ int c=0; for(;f&&*f;f++){ if(*f=='%'){ if(f[1]=='%'){ f++; } else { const char*q=f+1; while(*q&&strchr("-+ #0",*q))q++; c++; if(*q=='*')c++; } } } return c; }
static char* uf_fmt(const char*f,Cell*a,int n){
  size_t cap=256,bi=0; char*buf=(char*)uf_alloc(cap,0); int ai=0;
  for(const char*p=f;*p;){
    if(*p!='%'){ if(bi+2>cap){cap*=2;buf=(char*)realloc(buf,cap);} buf[bi++]=*p++; continue; }
    if(p[1]=='%'){ if(bi+2>cap){cap*=2;buf=(char*)realloc(buf,cap);} buf[bi++]='%'; p+=2; continue; }
    char d[48]; int di=0; d[di++]='%'; p++;
    while(*p&&strchr("-+ #0",*p)) d[di++]=*p++;
    if(*p=='*'){ p++; if(ai>=n) die("FMT: not enough args"); long w=(long)uf_i(a[ai++]); if(w<0){ d[di++]='-'; w=-w; } di+=snprintf(d+di,sizeof(d)-di-8,"%ld",w); }

    while(*p&&(isdigit((unsigned char)*p)||*p=='.')) d[di++]=*p++;
    while(*p&&strchr("hlLjzt",*p)) p++;
    char conv=*p?*p++:'d';
    if(ai>=n) die("FMT: not enough args");
    Cell ar=a[ai++];
    char tmp[128]; int tl=0;
    d[di]=0;
    switch(conv){
      case 'd': case 'i': case 'u': case 'x': case 'X': case 'o': {
        { size_t l=strlen(d); d[l]='l'; d[l+1]='l'; d[l+2]=conv; d[l+3]=0; }
        tl=snprintf(tmp,sizeof(tmp),d,(unsigned long long)ar.i); break; }
      case 'c': { size_t l=strlen(d); d[l]=conv; d[l+1]=0; tl=snprintf(tmp,sizeof(tmp),d,(int)ar.i); break; }
      case 'f': case 'F': case 'e': case 'E': case 'g': case 'G': {
        size_t l=strlen(d); d[l]=conv; d[l+1]=0;
        tl=snprintf(tmp,sizeof(tmp),d,uf_f(ar)); break; }
      case 's': { size_t l=strlen(d); d[l]=conv; d[l+1]=0;
        Cell str=uf_to_string(ar); const char* sv=uf_sptr(str);
        int need=snprintf(0,0,d,sv);
        while(bi+(size_t)need+1>cap){ cap*=2; buf=(char*)realloc(buf,cap); }
        snprintf(buf+bi,(size_t)need+1,d,sv);
        bi+=(size_t)need; continue; }
      case 'p': { size_t l=strlen(d); d[l]=conv; d[l+1]=0; tl=snprintf(tmp,sizeof(tmp),d,((void*)ar.i)); break; }
      default: die("FMT: unsupported directive");
    }
    if(tl<0) die("FMT failed");
    if((size_t)tl>sizeof(tmp)) tl=sizeof(tmp); /* clamp to actual bytes written */
    while(bi+(size_t)tl+1>cap){ cap*=2; buf=(char*)realloc(buf,cap); }
    memcpy(buf+bi,tmp,(size_t)tl); bi+=(size_t)tl;
  }
  buf[bi]=0; return buf;
}
static void op_fmt(Ctx*cx){ Cell f=pop(cx); int n=uf_count(uf_sptr(f)); Cell args[16]; if(n>16)die("FMT: too many args"); for(int k=n-1;k>=0;k--) args[k]=pop(cx); char*s=uf_fmt(uf_sptr(f),args,n); Cell r=uf_str_new(s,strlen(s)); free(s); pushc(cx,r); }
/* PRINT: v -> (smart recursive printer; top-level strings raw) */
static void uf_print_cell(Cell c,int nested){
  if(c.tag==T_FLOAT){ double d=uf_fbits(c.i); if(isnan(d))printf("NaN"); else printf("%.17g",d); return; }
  if(c.tag==T_BYTE){ printf(c.i?"true":"false"); return; }
  if(c.tag==T_INT){ printf("%lld",(long long)c.i); return; }
  if(c.tag==T_PTR && !c.i){ printf("null"); return; }
  if(c.tag==T_PTR && c.i){
    Hdr*h=uf_gc_find((void*)c.i);
    if(!h){ printf("<ptr %p>",(void*)c.i); return; }
    if(h->tag==HT_STR){
      const char*s=uf_sbytes((Str*)h);
      if(nested){
        printf("\"");
        for(const char*p=s;*p;p++){
          if(*p=='"')printf("\\\"");
          else if(*p=='\\')printf("\\\\");
          else if(*p=='\n')printf("\\n");
          else if(*p=='\t')printf("\\t");
          else if(*p=='\r')printf("\\r");
          else printf("%c",*p);
        }
        printf("\"");
      } else { printf("%s",s); }
      return;
    }
    if(h->tag==HT_DYN){ Dyn*d=(Dyn*)h; printf("["); for(uint64_t i=0;i<d->len;i++){ if(i)printf(","); uf_print_cell(d->data[i],1); } printf("]"); return; }
    if(h->tag==HT_ARR||h->tag==HT_TENSOR||h->tag==HT_MAT){ printf("["); for(uint64_t i=0;i<h->len;i++){ if(i)printf(","); uf_print_cell(uf_cidx(c,(int64_t)i),1); } printf("]"); return; }
    if(h->tag==HT_MAP){ Map*m=(Map*)h; printf("{"); int first=1; for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1){ if(!first)printf(","); first=0; uf_print_cell(m->keys[i],1); printf(":"); uf_print_cell(m->vals[i],1); } printf("}"); return; }
    if(h->tag==HT_OBJ){ printf("[object Object]"); return; }
    printf("<%s>",h->tag==HT_BUF?"buf":h->tag==HT_RING?"chan":h->tag==HT_ATOM?"atom":h->tag==HT_ITER?"iter":h->tag==HT_BITMAP?"bitmap":h->tag==HT_BLOOM?"bloom":"object");
    return;
  }
  printf("%lld",(long long)c.i);
}
static void op_print(Ctx*cx){ Cell v=pop(cx); uf_print_cell(v,0); printf("\n"); }
/* SCAN: fmt -> list */
static void op_scan(Ctx*cx){
  Cell f=pop(cx); const char*p=uf_sptr(f); Dyn* dl=uf_dyn_new(8); UF_PROTECT(&dl); int n=0;
  for(;*p;p++){
    if(*p=='%'){
      if(p[1]=='%'){ p++; continue; }
      p++;
      while(*p&&strchr("-+ #0",*p)) p++;
      while(*p&&(isdigit((unsigned char)*p)||*p=='.')) p++;
      while(*p&&strchr("hlLjzt",*p)) p++;
      char conv=*p?*p:'\0';
      switch(conv){
        case 'd': case 'i': case 'u': case 'x': case 'X': case 'o': {
          long long v; char dbuf[8]; dbuf[0]='%'; dbuf[1]='l'; dbuf[2]='l'; dbuf[3]=conv; dbuf[4]=0;
          if(fscanf(stdin,dbuf,&v)!=1) die("SCAN: input error"); uf_dyn_push(&dl,uf_mki((int64_t)v)); n++; break; }
        case 'f': case 'F': case 'e': case 'E': case 'g': case 'G': {
          double v; char dbuf[8]; dbuf[0]='%'; dbuf[1]='l'; dbuf[2]='f'; dbuf[3]=0;
          if(fscanf(stdin,dbuf,&v)!=1) die("SCAN: input error"); uf_dyn_push(&dl,uf_mkf(v)); n++; break; }
        case 's': {
          char*b=(char*)uf_alloc(1<<16,0);
          if(fscanf(stdin,"%65535s",b)!=1) die("SCAN: input error"); Cell r=uf_str_new(b,strlen(b)); free(b); uf_dyn_push(&dl,r); n++; break; }
        default: die("SCAN: unsupported directive");
      }
    } else if(isspace((unsigned char)*p)) {
      continue;
    } else {
      die("SCAN: literal text in format unsupported");
    }
  }
  uf_dyn_push(&dl,uf_mki((int64_t)n)); UF_UNPROTECT(); pushp(cx,dl);
}

/* ================= list / dict / chan / atom ops ================= */
static void op_list(Ctx*cx){ pushp(cx,uf_dyn_new(8)); }
/* PUSH (was APPEND): h v -> h' (list grows by move; the returned handle is
   the live one — the old handle is reclaimed by the GC) */
static void op_push(Ctx*cx){ Cell v=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"PUSH"); if(a->tag!=HT_DYN)die("PUSH: not a list"); pushp(cx,uf_dyn_push2((Dyn*)a,v)); }
static void op_lpop(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_handle(h,"POP"); if(a->tag!=HT_DYN)die("POP: not a list"); Dyn*d=(Dyn*)a; if(d->len==0)die("POP: empty list"); pushc(cx,d->data[--d->len]); }
static void op_dict(Ctx*cx){ pushp(cx,uf_map_new()); }

/* CHAN: bounded MPSC ring buffer with blocking ENQ/DEQ */
static void ring_enq(Ring*r,Cell v){ pthread_mutex_lock(&r->mu); while(r->len>=r->cap&&!r->closed) pthread_cond_wait(&r->notfull,&r->mu); if(r->closed){ pthread_mutex_unlock(&r->mu); die("ENQ: chan closed"); } r->buf[r->tail]=v; r->tail=(r->tail+1)%r->cap; r->len++; pthread_cond_signal(&r->notempty); pthread_mutex_unlock(&r->mu); }
static void ring_close(Ring*r){ pthread_mutex_lock(&r->mu); r->closed=1; pthread_cond_broadcast(&r->notempty); pthread_cond_broadcast(&r->notfull); pthread_mutex_unlock(&r->mu); }
/* blocking deq with close detection: 0 = got a value, 1 = closed+drained */
static int ring_deq1(Ring*r,Cell*out){ pthread_mutex_lock(&r->mu); while(r->len==0&&!r->closed) pthread_cond_wait(&r->notempty,&r->mu); if(r->len==0){ pthread_mutex_unlock(&r->mu); return 1; } *out=r->buf[r->head]; r->head=(r->head+1)%r->cap; r->len--; pthread_cond_signal(&r->notfull); pthread_mutex_unlock(&r->mu); return 0; }
static Ring* uf_ring_new(uint64_t cap){ if(!cap)cap=16; Ring*r=(Ring*)uf_gc_alloc(sizeof(Ring),0); r->tag=HT_RING; r->len=0; r->cap=cap; r->buf=(Cell*)uf_alloc((size_t)cap*sizeof(Cell),0); r->head=0; r->tail=0; r->closed=0; pthread_mutex_init(&r->mu,0); pthread_cond_init(&r->notfull,0); pthread_cond_init(&r->notempty,0); return r; }
static void op_chan(Ctx*cx){ int64_t cap=pop(cx).i; if(cap<=0)cap=16; pushp(cx,uf_ring_new((uint64_t)cap)); }
static void op_enq(Ctx*cx){ Cell v=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"ENQ"); if(a->tag!=HT_RING)die("ENQ: not a chan"); ring_enq((Ring*)a,v); }
/* DEQ: h -> v  (blocks while empty; closed+empty yields sentinel 0) */
static void op_deq(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_handle(h,"DEQ"); if(a->tag!=HT_RING)die("DEQ: not a chan"); Cell v; if(ring_deq1((Ring*)a,&v))v=uf_mki(0); pushc(cx,v); }
static void op_close(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_handle(h,"CLOSE"); if(a->tag!=HT_RING)die("CLOSE: not a chan"); ring_close((Ring*)a); }

/* ATOM: atomic i64 cell */
static void op_atom(Ctx*cx){ Cell v=pop(cx); Atom*a=(Atom*)uf_gc_alloc(sizeof(Atom),0); a->tag=HT_ATOM; a->len=1; atomic_store(&a->v,v.i); pushp(cx,a); }
static void op_aget(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_handle(h,"AGET"); if(a->tag!=HT_ATOM)die("AGET: not an atom"); pushi(cx,atomic_load(&((Atom*)a)->v)); }
static void op_aset(Ctx*cx){ Cell v=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"ASET"); if(a->tag!=HT_ATOM)die("ASET: not an atom"); atomic_store(&((Atom*)a)->v,v.i); }
static void op_aadd(Ctx*cx){ Cell n=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"AADD"); if(a->tag!=HT_ATOM)die("AADD: not an atom"); pushi(cx,atomic_fetch_add(&((Atom*)a)->v,n.i)); }
static void op_cas(Ctx*cx){ Cell nw=pop(cx),old=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"CAS"); if(a->tag!=HT_ATOM)die("CAS: not an atom"); int64_t e=old.i; pushi(cx,atomic_compare_exchange_strong(&((Atom*)a)->v,&e,nw.i)?1:0); }

/* RANGE: start stop -> list of ints [start, stop) */
static void op_range(Ctx*cx){
  int64_t stop=pop(cx).i, start=pop(cx).i;
  int64_t n = stop>start ? stop-start : 0;
  Dyn* d=uf_dyn_new((uint64_t)n); UF_PROTECT(&d);
  for(int64_t k=start;k<stop;k++) uf_dyn_push(&d,uf_mki(k));
  UF_UNPROTECT(); pushp(cx,d);
}

/* ================= iterators ================= */
static Iter* uf_iter_new(Cell src);
static int uf_iter_next(Ctx*cx, Iter* it, Cell* out){
  switch(it->kind){
    case IT_LIST: { Dyn*d=(Dyn*)uf_gc_find((void*)it->src.i); if(!d)return 0; if((uint64_t)it->idx>=d->len)return 0; *out=d->data[it->idx++]; return 1; }
    case IT_ARR: { Hdr*a=(Hdr*)uf_gc_find((void*)it->src.i); if(!a)return 0; if((uint64_t)it->idx>=a->len)return 0; *out=uf_cidx(it->src,it->idx++); return 1; }
    case IT_DICT: { Map*m=(Map*)uf_gc_find((void*)it->src.i); if(!m)return 0; while((uint64_t)it->idx<m->cap&&m->st[it->idx]!=1)it->idx++; if((uint64_t)it->idx>=m->cap)return 0; *out=m->keys[it->idx++]; return 1; }
    case IT_STR: { Str*s=(Str*)uf_gc_find((void*)it->src.i); if(!s)return 0; if((uint64_t)it->idx>=s->len)return 0; *out=uf_mki((uint8_t)uf_sbytes(s)[it->idx++]); return 1; }
    case IT_CHAN: { Ring*r=(Ring*)uf_gc_find((void*)it->src.i); if(!r)return 0; return ring_deq1(r,out)?0:1; }
    case IT_BITMAP: { Bitmap*b=(Bitmap*)uf_gc_find((void*)it->src.i); if(!b)return 0; uint64_t n=b->len; while((uint64_t)it->idx<n){ uint64_t w=(uint64_t)it->idx>>6, o=(uint64_t)it->idx&63; if((w<(n+63)/64)&&((b->words[w]>>o)&1)){ *out=uf_mki(it->idx++); return 1; } it->idx++; } return 0; }
    case IT_MAP: { Iter* in=(Iter*)uf_gc_find((void*)it->g.i); if(!in)return 0; Cell v; if(!uf_iter_next(cx,in,&v))return 0; pushc(cx,v); uf_call_addr(cx,(const void*)it->f.i,0,-1,1); *out=pop(cx); return 1; }
    case IT_FILTER: { Iter* in=(Iter*)uf_gc_find((void*)it->g.i); if(!in)return 0; Cell v; while(uf_iter_next(cx,in,&v)){ pushc(cx,v); uf_call_addr(cx,(const void*)it->f.i,0,-1,1); Cell r=pop(cx); if(uf_truthy(r)){ *out=v; return 1; } } return 0; }
  }
  return 0;
}
static Iter* uf_iter_new(Cell src){
  if(src.tag!=T_PTR||!src.i)die("ITER: not iterable");
  Hdr* h=uf_gc_find((void*)src.i); if(!h)die("ITER: not iterable");
  int kind;
  switch(h->tag){
    case HT_DYN: kind=IT_LIST; break;
    case HT_ARR: case HT_TENSOR: case HT_MAT: kind=IT_ARR; break;
    case HT_MAP: kind=IT_DICT; break;
    case HT_STR: kind=IT_STR; break;
    case HT_RING: kind=IT_CHAN; break;
    case HT_BITMAP: kind=IT_BITMAP; break;
    default: die("ITER: not iterable");
  }
  Iter* it=(Iter*)uf_gc_alloc(sizeof(Iter),0);
  it->tag=HT_ITER; it->len=1; it->src=src; it->kind=kind; it->idx=0; it->f=uf_mki(0); it->g=uf_mki(0);
  return it;
}
static void op_iter(Ctx*cx){ Cell h=pop(cx); Iter*it=uf_iter_new(h); pushp(cx,it); }
static void op_next(Ctx*cx){
  Cell h=pop(cx); Hdr*a=uf_handle(h,"NEXT"); if(a->tag!=HT_ITER)die("NEXT: not an iter");
  Cell v; Dyn*d=uf_dyn_new(2); UF_PROTECT(&d);
  if(uf_iter_next(cx,(Iter*)a,&v)){ uf_dyn_push(&d,v); uf_dyn_push(&d,uf_mki(1)); }
  else { uf_dyn_push(&d,uf_mki(0)); uf_dyn_push(&d,uf_mki(0)); }
  UF_UNPROTECT(); pushp(cx,d);
}
static Dyn* uf_collect_it(Ctx*cx, Iter* it){
  Dyn* d=uf_dyn_new(8); UF_PROTECT(&d); UF_PROTECT(&it);
  Cell v; while(uf_iter_next(cx,it,&v)) uf_dyn_push(&d,v);
  UF_UNPROTECT(); UF_UNPROTECT(); return d;
}
static void op_collect(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_handle(h,"COLLECT"); if(a->tag!=HT_ITER)die("COLLECT: not an iter"); pushp(cx,uf_collect_it(cx,(Iter*)a)); }
static void op_imap(Ctx*cx){
  Cell f=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"IMAP"); if(a->tag!=HT_ITER)die("IMAP: not an iter");
  Iter* it=(Iter*)uf_gc_alloc(sizeof(Iter),0);
  it->tag=HT_ITER; it->len=1; it->kind=IT_MAP; it->idx=0; it->src=uf_mki(0); it->f=f; it->g=h;
  pushp(cx,it);
}
static void op_ifilter(Ctx*cx){
  Cell f=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"IFILTER"); if(a->tag!=HT_ITER)die("IFILTER: not an iter");
  Iter* it=(Iter*)uf_gc_alloc(sizeof(Iter),0);
  it->tag=HT_ITER; it->len=1; it->kind=IT_FILTER; it->idx=0; it->src=uf_mki(0); it->f=f; it->g=h;
  pushp(cx,it);
}
/* materialize any iterable (or iter, drained) into a list; lists pass through */
static Dyn* uf_materialize(Ctx*cx, Cell h){
  if(h.tag==T_PTR&&h.i){
    Hdr* a=uf_gc_find((void*)h.i);
    if(a){
      if(a->tag==HT_DYN) return (Dyn*)a;
      if(a->tag==HT_ITER) return uf_collect_it(cx,(Iter*)a);
      if(a->tag==HT_ARR||a->tag==HT_TENSOR||a->tag==HT_MAT){
        Dyn* d=uf_dyn_new(a->len); UF_PROTECT(&d);
        for(uint64_t i=0;i<a->len;i++) uf_dyn_push(&d,uf_cidx(h,(int64_t)i));
        UF_UNPROTECT(); return d;
      }
      if(a->tag==HT_MAP||a->tag==HT_STR||a->tag==HT_RING||a->tag==HT_BITMAP){
        Iter* it=uf_iter_new(h); UF_PROTECT(&it); Dyn* d=uf_collect_it(cx,it); UF_UNPROTECT(); return d;
      }
    }
  }
  die("not a sequence");
  return 0;
}

/* ================= weave: static task DAG + dynamic fanout ================= */
typedef void(*UfRun)(Ctx*,long);
typedef struct WeaveTaskS { long pc; int ninputs; int* inputs; long count; Cell result; _Atomic int state; double t0,t1; long items; long retries; long tolerated; } WeaveTask;
static void uf_weave_mark(struct WeaveJobS* j){ if(!j)return; for(int i=0;i<j->n;i++) uf_mark_cell(j->ts[i].result); }
static double uf_nowd(void){ struct timespec ts; clock_gettime(CLOCK_MONOTONIC,&ts); return ts.tv_sec+ts.tv_nsec/1e9; }

/* fanout coordination: feeder drains the (iterable) first input into an
   internal bounded chan; `count` workers pull items dynamically. Broadcast
   inputs (declared after the first) sit beneath the item on each worker's
   initial stack. Results collect in completion order. */
typedef struct { WeaveJob* j; WeaveTask* t; Ring* q; Dyn* results; pthread_mutex_t* rmu; Iter* it; Ctx* fcx; } UfFan;
static void* uf_fan_feeder(void* arg){
  UfFan* f=(UfFan*)arg;
  Cell v;
  while(uf_iter_next(f->fcx,f->it,&v)) ring_enq(f->q,v);
  ring_close(f->q);
  return 0;
}
static void* uf_fan_worker(void* arg){
  UfFan* f=(UfFan*)arg;
  WeaveTask* t=f->t; WeaveJob* j=f->j;
  Ctx* c=ctx_new(1<<16,1<<12);
  uf_cur_task=t;
  Cell item;
  while(ring_deq1(f->q,&item)==0){
    /* initial stack: the fanout item deepest, broadcast inputs (declared
       after the first) above it — the reversed param pops bind the first
       declared input to the item and later inputs to the broadcasts */
    pushc(c,item);
    for(int k=1;k<t->ninputs;k++) pushc(c,j->ts[t->inputs[k]].result);
    j->run(c,t->pc);
    Cell r = c->sp>0 ? c->ds[c->sp-1] : uf_mki(0);
    c->sp=0; c->csp=0; c->lsp=0; c->local_base=0; c->local_fsp=0;
    pthread_mutex_lock(f->rmu);
    f->results=uf_dyn_push2(f->results,r);
    t->items++;
    pthread_mutex_unlock(f->rmu);
  }
  uf_cur_task=0;
  ctx_free(c);
  return 0;
}
static void uf_run_fanout(WeaveJob* j, WeaveTask* t){
  Cell input = j->ts[t->inputs[0]].result;
  Iter* it = uf_iter_new(input); /* dies if not iterable */
  Ring* q = uf_ring_new(64);
  Dyn* results = uf_dyn_new(8);
  pthread_mutex_t rmu; pthread_mutex_init(&rmu,0);
  UF_PROTECT(&it); UF_PROTECT(&q); UF_PROTECT(&results);
  UfFan f; f.j=j; f.t=t; f.q=q; f.results=results; f.rmu=&rmu; f.it=it;
  f.fcx=ctx_new(1<<12,1<<8);
  pthread_t ft; if(pthread_create(&ft,0,uf_fan_feeder,&f))die("WEAVE: feeder thread");
  long nw=t->count; if(nw<1)nw=1; if(nw>64)nw=64;
  pthread_t th[64];
  for(long i=0;i<nw;i++) if(pthread_create(&th[i],0,uf_fan_worker,&f))die("WEAVE: worker thread");
  for(long i=0;i<nw;i++) pthread_join(th[i],0);
  pthread_join(ft,0);
  ctx_free(f.fcx);
  UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT();
  t->result=uf_mkp(f.results);
  pthread_mutex_destroy(&rmu);
}
static void* uf_worker(void*arg){
  WeaveJob*j=(WeaveJob*)arg;
  for(;;){
    int pick=-1;
    /* graceful shutdown: stop scheduling new tasks. Running tasks finish,
       then the weave drains and run returns; tasks that never started stay
       pending (their results remain uninitialized). */
    if(atomic_load(&j->shutdown)){
      int running = 0;
      for(int i=0;i<j->n;i++) if(atomic_load(&j->ts[i].state)==1){running=1;break;}
      if(!running) return 0;
      sched_yield(); continue;
    }
    for(int i=0;i<j->n;i++){
      if(atomic_load(&j->ts[i].state)!=0) continue;
      int ready=1;
      for(int k=0;k<j->ts[i].ninputs;k++) if(atomic_load(&j->ts[j->ts[i].inputs[k]].state)!=2){ready=0;break;}
      if(!ready) continue;
      int exp=0;
      if(atomic_compare_exchange_strong(&j->ts[i].state,&exp,1)){ pick=i; break; }
    }
    if(pick<0){
      int alldone=1; for(int i=0;i<j->n;i++) if(atomic_load(&j->ts[i].state)!=2){alldone=0;break;}
      if(alldone) return 0;
      sched_yield(); continue;
    }
    WeaveTask*t=&j->ts[pick];
    t->t0=uf_nowd();
    uf_cur_task=t;
    if(t->count>1){
      uf_run_fanout(j,t);
    } else {
      Ctx*c=ctx_new(1<<16,1<<12);
      for(int k=0;k<t->ninputs;k++) pushc(c,j->ts[t->inputs[k]].result);
      j->run(c,t->pc);
      t->result = c->sp>0 ? c->ds[c->sp-1] : uf_mki(0);
      t->items = 1;
      ctx_free(c);
    }
    uf_cur_task=0;
    t->t1=uf_nowd();
    atomic_store(&t->state,2);
  }
}
static void uf_weave(Ctx*cx,WeaveTask*ts,int n,UfRun run){
  (void)cx;
  long ncpu=sysconf(_SC_NPROCESSORS_ONLN);
  long total=0; for(int i=0;i<n;i++) total += ts[i].count>1?ts[i].count:1;
  int nw=(int)total; if(ncpu>0&&(long)nw>ncpu)nw=(int)ncpu; if(nw<1)nw=1; if(nw>64)nw=64;
  WeaveJob j={ts,n,run};
  uf_active_job=&j;
  if(nw<=1){ uf_worker(&j); }
  else {
    pthread_t th[64];
    for(int i=0;i<nw-1;i++) pthread_create(&th[i],0,uf_worker,&j);
    uf_worker(&j);
    for(int i=0;i<nw-1;i++) pthread_join(th[i],0);
  }
  uf_active_job=0;
  if(getenv("NK_WEAVE_DEBUG")){
    for(int i=0;i<n;i++){
      WeaveTask*t=&ts[i];
      fprintf(stderr,"weave: task pc=%ld wall=%.3fms workers=%ld items=%ld retries=%ld tolerated=%ld\n",
        t->pc,(t->t1-t->t0)*1e3,t->count,t->items,t->retries,t->tolerated);
    }
  }
}

/* ================= shared string helpers ================= */
static char* uf_read_all(FILE*f){
  size_t cap=4096,n=0; char*b=(char*)uf_alloc(cap,0); size_t m;
  while((m=fread(b+n,1,cap-1-n,f))>0){ n+=m; if(cap-1-n==0){ cap*=2; b=(char*)realloc(b,cap); if(!b)die("out of memory"); } }
  b[n]=0; return b;
}
static int uf_wait_status(int r){
#ifdef _WIN32
  return r;
#else
  if(r==-1)return -1;
  if(WIFEXITED(r))return WEXITSTATUS(r);
  if(WIFSIGNALED(r))return 128+WTERMSIG(r);
  return r;
#endif
}

/* ================= shell ops ================= */
/* SH (merged SH+SHX): cmd -> out err status (always captures both streams
   as fresh strings; status on top: -1 spawn failure, 128+signal) */
static void op_sh(Ctx*cx){
  Cell c=pop(cx); const char*cmd=uf_sptr(c);
#ifdef _WIN32
  char tmp[256]; tmpnam(tmp);
  char*full=(char*)uf_alloc(strlen(cmd)+strlen(tmp)+8,0);
  sprintf(full,"%s 2>%s",cmd,tmp);
  FILE* f=_popen(full,"r"); if(!f){ pushc(cx,uf_str_new("",0)); pushc(cx,uf_str_new("",0)); pushi(cx,-1); return; }
  char*out=uf_read_all(f); int st=uf_wait_status(_pclose(f));
  FILE* ef=fopen(tmp,"r"); char*err;
  if(ef){ err=uf_read_all(ef); fclose(ef); remove(tmp); } else err=uf_alloc(1,0),err[0]=0;
  Cell so=uf_str_new(out,strlen(out)); Cell se=uf_str_new(err,strlen(err));
  free(out); free(err); free(full);
  Dyn* shd=uf_dyn_new(3); UF_PROTECT(&shd); uf_dyn_push(&shd,so); uf_dyn_push(&shd,se); uf_dyn_push(&shd,uf_mki((int64_t)st)); UF_UNPROTECT(); pushp(cx,shd);
#else
  int pfd[2]; if(pipe(pfd))die("SH: pipe");
  FILE* ef=tmpfile(); if(!ef)die("SH: tmpfile");
  pid_t pid=fork();
  if(pid<0)die("SH: fork");
  if(pid==0){
    close(pfd[0]);
    if(dup2(pfd[1],1)<0)_exit(127);
    if(dup2(fileno(ef),2)<0)_exit(127);
    execl("/bin/sh","sh","-c",cmd,(char*)0);
    _exit(127);
  }
  close(pfd[1]);
  FILE* f=fdopen(pfd[0],"r"); if(!f)die("SH: fdopen");
  char*out=uf_read_all(f); fclose(f);
  int rs=0; waitpid(pid,&rs,0);
  int st=uf_wait_status(rs);
  rewind(ef); char*err=uf_read_all(ef); fclose(ef);
  Cell so=uf_str_new(out,strlen(out)); Cell se=uf_str_new(err,strlen(err));
  free(out); free(err);
  Dyn* shd=uf_dyn_new(3); UF_PROTECT(&shd); uf_dyn_push(&shd,so); uf_dyn_push(&shd,se); uf_dyn_push(&shd,uf_mki((int64_t)st)); UF_UNPROTECT(); pushp(cx,shd);
#endif
}
/* SHP: cmd -> chan (worker thread streams stdout lines, closes chan at exit) */
typedef struct { Ring* r; char* cmd; } UfShp;
static void* uf_shp_worker(void*arg){
  UfShp* g=(UfShp*)arg;
#ifdef _WIN32
  FILE* f=_popen(g->cmd,"r");
#else
  FILE* f=popen(g->cmd,"r");
#endif
  if(f){
#ifdef _WIN32
    char line[16384];
    while(fgets(line,sizeof(line),f)){ size_t m=strlen(line); while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0; Cell v=uf_str_new(line,m); ring_enq(g->r,v); }
    _pclose(f);
#else
    char*line=0; size_t ncap=0; ssize_t m;
    while((m=getline(&line,&ncap,f))>=0){ while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0; Cell v=uf_str_new(line,(size_t)m); ring_enq(g->r,v); }
    free(line); pclose(f);
#endif
  }
  ring_close(g->r);
  free(g);
  return 0;
}
static void op_shp(Ctx*cx){
  Cell c=pop(cx);
  Ring*r=uf_ring_new(64);
  UfShp* g=(UfShp*)malloc(sizeof(UfShp)); if(!g)die("out of memory"); g->r=r; g->cmd=strdup(uf_sptr(c));
  pthread_t th; if(pthread_create(&th,0,uf_shp_worker,g)){ ring_close(r); die("SHP: thread"); }
  pthread_detach(th);
  pushp(cx,r);
}
/* EXEC: list -> status (argv list, no shell) */
static void op_exec(Ctx*cx){
  Cell h=pop(cx); Hdr*a=uf_handle(h,"EXEC"); if(a->tag!=HT_DYN)die("EXEC: not a list");
  Dyn*d=(Dyn*)a;
  if(d->len==0)die("EXEC: empty argv");
  char**argv=(char**)malloc((d->len+1)*sizeof(char*)); if(!argv)die("out of memory");
  for(uint64_t i=0;i<d->len;i++)argv[i]=(char*)uf_sptr(d->data[i]);
  argv[d->len]=0;
#ifdef _WIN32
  intptr_t r=_spawnvp(_P_WAIT,argv[0],(const char* const*)argv);
  int st=(r==-1)?-1:(int)r;
#else
  pid_t pid=fork();
  if(pid<0)die("EXEC: fork");
  if(pid==0){ execvp(argv[0],argv); _exit(127); }
  int rs=0; waitpid(pid,&rs,0); int st=uf_wait_status(rs);
#endif
  free(argv); pushi(cx,st);
}

/* ================= embedded regex (unchanged from v9) =================
   Syntax: literals, '.', '*', '+', '?', '[...]' (ranges, '^' negation),
   '^' at alternative start, '$' at end, '|' alternation, '(' ')' groups
   (<=9, \\1..\\9 backrefs in REPLACE). Greedy with backtracking. */
typedef struct { const char* s; const char* e; } RxCap;
enum { RXA_LIT=0, RXA_DOT=1, RXA_CLS=2, RXA_GRP=3 };
typedef struct { int type; char ch; const char* cs; const char* ce; const char* gs; const char* ge; int cap; } RxAtom;
static int rx_cls_find(const char* p, const char** close){
  if(*p=='^')p++;
  if(*p==']')p++;
  while(*p){
    if(*p=='\\'&&p[1]){ p+=2; continue; }
    if(*p==']'){ *close=p; return 1; }
    p++;
  }
  return 0;
}
static int rx_cls_in(const char* cs, const char* ce, char c){
  int neg=0; const char* p=cs;
  if(p<ce&&*p=='^'){ neg=1; p++; }
  int ok=0; int first=1;
  while(p<ce){
    char lo;
    if(*p=='\\'&&p+1<ce){ lo=p[1]; p+=2; } else lo=*p++;
    if(first&&lo==']'){ if(c==']')ok=1; first=0; continue; }
    first=0;
    if(p<ce&&*p=='-'&&p+1<ce){
      p++; char hi;
      if(*p=='\\'&&p+1<ce){ hi=p[1]; p+=2; } else hi=*p++;
      if((unsigned char)c>=(unsigned char)lo&&(unsigned char)c<=(unsigned char)hi)ok=1;
    } else if(c==lo)ok=1;
  }
  return neg?!ok:ok;
}
static int rx_group_index(const char* pat0, const char* p){
  int n=0; const char* q=pat0;
  while(q<p){
    if(*q=='\\'&&q[1]){ q+=2; continue; }
    if(*q=='['){ const char* cl; if(rx_cls_find(q+1,&cl)){ q=cl+1; continue; } }
    if(*q=='(')n++;
    q++;
  }
  return n;
}
static const char* rx_parse_atom(const char* p, RxAtom* a, const char* pat0){
  memset(a,0,sizeof(*a));
  char c=*p;
  if(c=='\\'){ if(!p[1])die("MATCH: trailing backslash"); a->type=RXA_LIT; a->ch=p[1]; return p+2; }
  if(c=='.'){ a->type=RXA_DOT; return p+1; }
  if(c=='['){ const char* cl; if(!rx_cls_find(p+1,&cl))die("MATCH: unbalanced ["); a->type=RXA_CLS; a->cs=p+1; a->ce=cl; return cl+1; }
  if(c=='('){
    int depth=1; const char* q=p+1;
    while(*q&&depth){
      if(*q=='\\'&&q[1]){ q+=2; continue; }
      if(*q=='['){ const char* cl; if(rx_cls_find(q+1,&cl)){ q=cl+1; continue; } q++; continue; }
      if(*q=='(')depth++;
      else if(*q==')')depth--;
      q++;
    }
    if(depth)die("MATCH: unbalanced (");
    a->type=RXA_GRP; a->gs=p+1; a->ge=q-1; a->cap=rx_group_index(pat0,p)+1;
    if(a->cap>9)die("MATCH: more than 9 groups");
    return q;
  }
  a->type=RXA_LIT; a->ch=c; return p+1;
}
static const char* rx_seq(const char* p, const char* pend, const char* s, RxCap* caps, const char* pat0);
static const char* rx_atom1(RxAtom* a, const char* s, RxCap* caps, const char* pat0){
  switch(a->type){
  case RXA_LIT: return (*s&&*s==a->ch)?s+1:0;
  case RXA_DOT: return *s?s+1:0;
  case RXA_CLS: return (*s&&rx_cls_in(a->cs,a->ce,*s))?s+1:0;
  case RXA_GRP: {
    const char* alt=a->gs;
    for(;;){
      const char* ae=alt; int depth=0;
      while(ae<a->ge){
        if(*ae=='\\'&&ae+1<a->ge){ ae+=2; continue; }
        if(*ae=='['){ const char* cl; if(rx_cls_find(ae+1,&cl)&&cl<a->ge){ ae=cl+1; continue; } }
        if(*ae=='(')depth++;
        else if(*ae==')')depth--;
        else if(*ae=='|'&&depth==0)break;
        ae++;
      }
      const char* r=rx_seq(alt,ae,s,caps,pat0);
      if(r){ caps[a->cap].s=s; caps[a->cap].e=r; return r; }
      if(ae>=a->ge)return 0;
      alt=ae+1;
    }
  }
  }
  return 0;
}
static const char* rx_seq(const char* p, const char* pend, const char* s, RxCap* caps, const char* pat0){
  if(p>=pend)return s;
  if(*p=='$'&&p+1==pend)return *s==0?s:0;
  RxAtom a; const char* q=rx_parse_atom(p,&a,pat0);
  long min=1,max=1;
  if(q<pend&&(*q=='*'||*q=='+'||*q=='?')){
    char t=*q; q++;
    if(t=='*'){min=0;max=1<<30;} else if(t=='+'){min=1;max=1<<30;} else {min=0;max=1;}
  }
  if(max==1){
    const char* r=rx_atom1(&a,s,caps,pat0);
    if(r){ const char* e=rx_seq(q,pend,r,caps,pat0); if(e)return e; }
    if(min==0)return rx_seq(q,pend,s,caps,pat0);
    return 0;
  }
  size_t capn=16,n=0; const char**v=(const char**)malloc(capn*sizeof(char*));
  if(!v)die("out of memory");
  v[n++]=s;
  while((long)n-1<max){
    const char* r=rx_atom1(&a,v[n-1],caps,pat0);
    if(!r||r==v[n-1])break;
    if(n==capn){ capn*=2; v=(const char**)realloc(v,capn*sizeof(char*)); if(!v)die("out of memory"); }
    v[n++]=r;
  }
  const char* ok=0;
  for(long k=(long)n-1;k>=min;k--){
    const char* e=rx_seq(q,pend,v[k],caps,pat0);
    if(e){ ok=e; break; }
  }
  free(v);
  return ok;
}
static int rx_exec(const char* pat, const char* str, RxCap* caps){
  const char* alt=pat;
  for(;;){
    const char* ae=alt; int depth=0;
    while(*ae){
      if(*ae=='\\'&&ae[1]){ ae+=2; continue; }
      if(*ae=='['){ const char* cl; if(rx_cls_find(ae+1,&cl)){ ae=cl+1; continue; } }
      if(*ae=='(')depth++;
      else if(*ae==')')depth--;
      else if(*ae=='|'&&depth==0)break;
      ae++;
    }
    const char* p0=alt; int anch=0;
    if(p0<ae&&*p0=='^'){ anch=1; p0++; }
    const char* pos=str;
    for(;;){
      for(int i=0;i<10;i++){ caps[i].s=0; caps[i].e=0; }
      const char* r=rx_seq(p0,ae,pos,caps,pat);
      if(r){ caps[0].s=pos; caps[0].e=r; return 1; }
      if(anch||!*pos)break;
      pos++;
    }
    if(!*ae)return 0;
    alt=ae+1;
  }
}

/* MATCH (was RX): str pat -> [groups found] */
static void op_match(Ctx*cx){
  Cell pat=pop(cx),st=pop(cx);
  const char* P=uf_sptr(pat); const char* S=uf_sptr(st);
  RxCap caps[10];
  int ntotal=rx_group_index(P,P+strlen(P));
  if(rx_exec(P,S,caps)){
    Dyn*d=uf_dyn_new((uint64_t)ntotal+1); UF_PROTECT(&d);
    for(int i=0;i<=ntotal;i++){
      if(caps[i].s) uf_dyn_push_str(&d,caps[i].s,(size_t)(caps[i].e-caps[i].s));
      else uf_dyn_push_str(&d,"",0);
    }
    Dyn*r=uf_dyn_new(2); UF_PROTECT(&r); uf_dyn_push(&r,uf_mkp(d)); uf_dyn_push(&r,uf_mki(1)); UF_UNPROTECT(); UF_UNPROTECT();
    pushp(cx,r);
  } else {
    Dyn*d=uf_dyn_new(1); UF_PROTECT(&d); Dyn*r=uf_dyn_new(2); UF_PROTECT(&r); uf_dyn_push(&r,uf_mkp(d)); uf_dyn_push(&r,uf_mki(0)); UF_UNPROTECT(); UF_UNPROTECT();
    pushp(cx,r);
  }
}
/* REPLACE (was RXSUB): str pat repl -> str' (all matches; \\1..\\9, \\\\) */
static void op_replace(Ctx*cx){
  Cell repl=pop(cx),pat=pop(cx),st=pop(cx);
  const char* R=uf_sptr(repl); const char* P=uf_sptr(pat); const char* S=uf_sptr(st);
  size_t cap=256,n=0; char*out=(char*)uf_alloc(cap,0);
  RxCap caps[10];
  const char* cur=S;
#define UF_APP(src,L) do{ size_t _l=(size_t)(L); while(n+_l+1>cap){ cap*=2; out=(char*)realloc(out,cap); if(!out)die("out of memory"); } memcpy(out+n,(src),_l); n+=_l; }while(0)
  while(rx_exec(P,cur,caps)){
    UF_APP(cur,caps[0].s-cur);
    for(const char* r=R; *r; ){
      if(*r=='\\'&&r[1]){
        if(r[1]>='1'&&r[1]<='9'){ int g=r[1]-'0'; if(caps[g].s)UF_APP(caps[g].s,caps[g].e-caps[g].s); r+=2; }
        else { UF_APP(r+1,1); r+=2; }
      } else { UF_APP(r,1); r++; }
    }
    if(caps[0].e==caps[0].s){ if(!*cur)break; UF_APP(cur,1); cur++; }
    else cur=caps[0].e;
  }
  UF_APP(cur,strlen(cur));
#undef UF_APP
  out[n]=0; Cell r=uf_str_new(out,n); free(out); pushc(cx,r);
}
/* RSPLIT (was RXSPLIT): str pat -> list */
static void op_rsplit(Ctx*cx){
  Cell pat=pop(cx),st=pop(cx);
  const char* P=uf_sptr(pat); const char* cur=uf_sptr(st);
  Dyn*d=uf_dyn_new(8); UF_PROTECT(&d); RxCap caps[10];
  while(rx_exec(P,cur,caps)){
    if(caps[0].e==caps[0].s){ if(!*cur)break; cur++; continue; }
    uf_dyn_push_str(&d,cur,(size_t)(caps[0].s-cur));
    cur=caps[0].e;
  }
  uf_dyn_push_str(&d,cur,strlen(cur));
  UF_UNPROTECT();
  pushp(cx,d);
}

/* ================= string ops ================= */
#ifdef _WIN32
static int uf_glob_match(const char* pat,const char* s){
  while(*pat){
    if(*pat=='*'){
      while(*pat=='*')pat++;
      if(!*pat)return 1;
      for(const char* t=s;;t++){ if(uf_glob_match(pat,t))return 1; if(!*t)break; }
      return 0;
    }
    if(*pat=='?'){ if(!*s)return 0; pat++; s++; continue; }
    if(*pat=='['){
      const char* cl; if(rx_cls_find(pat+1,&cl)){
        if(!*s)return 0;
        int neg=0; const char* p=pat+1;
        if(p<cl&&*p=='!'){ neg=1; p++; }
        int ok=0;
        while(p<cl){
          char lo=*p++;
          if(p<cl&&*p=='-'&&p+1<cl){ p++; char hi=*p++; if((unsigned char)*s>=(unsigned char)lo&&(unsigned char)*s<=(unsigned char)hi)ok=1; }
          else if(*s==lo)ok=1;
        }
        if(neg)ok=!ok;
        if(!ok)return 0;
        pat=cl+1; s++; continue;
      }
    }
    if(*pat=='\\'&&pat[1])pat++;
    if(*pat!=*s)return 0;
    if(!*s)return 0;
    pat++; s++;
  }
  return *s==0;
}
#endif
/* GLOB: str pat -> 0/1 */
static void op_glob(Ctx*cx){
  Cell pat=pop(cx),st=pop(cx);
#ifdef _WIN32
  pushi(cx,uf_glob_match(uf_sptr(pat),uf_sptr(st))?1:0);
#else
  pushi(cx,fnmatch(uf_sptr(pat),uf_sptr(st),0)==0?1:0);
#endif
}
/* SPLIT: str sep -> list (true zero-copy: NUL-terminate fields in-place) */
static void op_split(Ctx*cx){
  Cell sep=pop(cx),st=pop(cx);
  /* get mutable pointer — parent must be a GC Str with inline/mmap data */
  Hdr* parent = uf_gc_find((void*)st.i);
  char* S;
  if(parent && parent->tag==HT_STR){
    Str* sp=(Str*)parent;
    S = (sp->mlen || sp->gc_parent) ? (char*)sp->mdata : sp->data;
  } else {
    S = (char*)uf_sptr(st); /* legacy raw char* */
  }
  const char* E=uf_sptr(sep);
  if(!S)die("SPLIT: not a string");
  if(!*E)die("SPLIT: empty separator");
  size_t el=strlen(E);
  Dyn*d=uf_dyn_new(8); UF_PROTECT(&d);
  char* cur=S;
  for(;;){
    char* m=strstr(cur,E);
    if(!m)break;
    *m=0; /* NUL-terminate the field in-place */
    Str* v=(Str*)uf_gc_alloc(sizeof(Str),0);
    v->tag=HT_STR; v->esz=1; v->len=(size_t)(m-cur); v->mlen=0;
    v->mdata=cur; v->gc_parent=parent; /* view: keep parent alive, own nothing */
    uf_dyn_push(&d,uf_mkp(v));
    cur=m+el;
  }
  size_t flen=strlen(cur);
  Str* v=(Str*)uf_gc_alloc(sizeof(Str),0);
  v->tag=HT_STR; v->esz=1; v->len=flen; v->mlen=0;
  v->mdata=cur; v->gc_parent=parent;
  uf_dyn_push(&d,uf_mkp(v));
  UF_UNPROTECT();
  pushp(cx,d);
}
/* JOIN: list sep -> str */
static void op_join(Ctx*cx){
  Cell sep=pop(cx),h=pop(cx);
  Hdr*a=uf_handle(h,"JOIN"); if(a->tag!=HT_DYN)die("JOIN: not a list");
  Dyn*d=(Dyn*)a;
  const char* E=uf_sptr(sep); size_t el=strlen(E);
  size_t cap=64; for(uint64_t i=0;i<d->len;i++)cap+=strlen(uf_sptr(d->data[i]))+el;
  char*out=(char*)uf_alloc(cap,0); size_t n=0;
  for(uint64_t i=0;i<d->len;i++){
    if(i){ memcpy(out+n,E,el); n+=el; }
    const char* s=uf_sptr(d->data[i]); size_t L=strlen(s); memcpy(out+n,s,L); n+=L;
  }
  out[n]=0; Cell r=uf_str_new(out,n); free(out); pushc(cx,r);
}
/* FIND: str sub -> idx (-1 on miss; byte index) */
static void op_find(Ctx*cx){
  Cell sub=pop(cx),st=pop(cx);
  const char* S=uf_sptr(st);
  const char* m=strstr(S,uf_sptr(sub));
  pushi(cx,m?m-S:-1);
}
/* REPL: str old new -> str' (literal, replace all) */
static void op_repl(Ctx*cx){
  Cell nw=pop(cx),old=pop(cx),st=pop(cx);
  const char* S=uf_sptr(st); const char* O=uf_sptr(old); const char* N=uf_sptr(nw);
  if(!*O)die("REPL: empty pattern");
  size_t ol=strlen(O),nl=strlen(N);
  size_t cap=strlen(S)+64,n=0; char*out=(char*)uf_alloc(cap,0);
  const char* cur=S;
  for(;;){
    const char* m=strstr(cur,O);
    if(!m)break;
    size_t pre=(size_t)(m-cur);
    while(n+pre+nl+1>cap){ cap*=2; out=(char*)realloc(out,cap); if(!out)die("out of memory"); }
    memcpy(out+n,cur,pre); n+=pre; memcpy(out+n,N,nl); n+=nl;
    cur=m+ol;
  }
  size_t tail=strlen(cur);
  while(n+tail+1>cap){ cap*=2; out=(char*)realloc(out,cap); if(!out)die("out of memory"); }
  memcpy(out+n,cur,tail); n+=tail;
  out[n]=0; Cell r=uf_str_new(out,n); free(out); pushc(cx,r);
}
/* TRIM: str -> str' */
static void op_trim(Ctx*cx){
  Cell st=pop(cx);
  const char* s=uf_sptr(st); size_t n=strlen(s);
  while(n>0&&isspace((unsigned char)s[0])){ s++; n--; }
  while(n>0&&isspace((unsigned char)s[n-1]))n--;
  pushc(cx,uf_str_new(s,n));
}
/* UP/DOWN: str -> str' (ASCII case) */
static void op_up(Ctx*cx){ Cell st=pop(cx); const char*s=uf_sptr(st); size_t n=strlen(s); char*r=(char*)uf_alloc(n+1,0); for(size_t i=0;i<n;i++)r[i]=(s[i]>='a'&&s[i]<='z')?(char)(s[i]-32):s[i]; r[n]=0; Cell c=uf_str_new(r,n); free(r); pushc(cx,c); }
static void op_down(Ctx*cx){ Cell st=pop(cx); const char*s=uf_sptr(st); size_t n=strlen(s); char*r=(char*)uf_alloc(n+1,0); for(size_t i=0;i<n;i++)r[i]=(s[i]>='A'&&s[i]<='Z')?(char)(s[i]+32):s[i]; r[n]=0; Cell c=uf_str_new(r,n); free(r); pushc(cx,c); }
/* STARTS/ENDS: str affix -> 0/1 */
static void op_starts(Ctx*cx){ Cell af=pop(cx),st=pop(cx); const char*s=uf_sptr(st); const char*a=uf_sptr(af); pushi(cx,strncmp(s,a,strlen(a))==0?1:0); }
static void op_ends(Ctx*cx){ Cell af=pop(cx),st=pop(cx); const char*s=uf_sptr(st); const char*a=uf_sptr(af); size_t ls=strlen(s),la=strlen(a); pushi(cx,(la<=ls&&strcmp(s+ls-la,a)==0)?1:0); }

/* ATOI/ATOF/ITOA/FTOA: explicit string<->number conversion */
static void op_atoi(Ctx*cx){ Cell s=pop(cx); pushi(cx,(int64_t)strtoll(uf_sptr(s),0,10)); }
static void op_atof(Ctx*cx){ Cell s=pop(cx); pushf(cx,strtod(uf_sptr(s),0)); }
static void op_itoa(Ctx*cx){ Cell n=pop(cx); char b[32]; snprintf(b,sizeof(b),"%lld",(long long)uf_i(n)); pushc(cx,uf_str_new(b,strlen(b))); }
static void op_ftoa(Ctx*cx){ Cell n=pop(cx); char b[32]; snprintf(b,sizeof(b),"%g",uf_f(n)); pushc(cx,uf_str_new(b,strlen(b))); }

/* ================= sequences: sort/filter/some/every ================= */
/* stable mergesort of a cell array with uf_cmp order; dies on incomparable */
static void uf_msort(Cell* v, Cell* tmp, uint64_t lo, uint64_t hi){
  if(hi-lo<2)return;
  uint64_t mid=(lo+hi)/2;
  uf_msort(v,tmp,lo,mid); uf_msort(v,tmp,mid,hi);
  uint64_t i=lo,j=mid,k=lo;
  while(i<mid&&j<hi){ int ok; int c=uf_cmp(v[i],v[j],&ok); if(!ok)die("SORT: incomparable element types"); if(c<=0)tmp[k++]=v[i++]; else tmp[k++]=v[j++]; }
  while(i<mid)tmp[k++]=v[i++];
  while(j<hi)tmp[k++]=v[j++];
  for(i=lo;i<hi;i++)v[i]=tmp[i];
}
/* SORT: seq -> seq' (fresh, stable; list -> list, arr -> arr by tag) */
static void op_sort(Ctx*cx){
  Cell h=pop(cx);
  Hdr* a=h.tag==T_PTR&&h.i?uf_gc_find((void*)h.i):0;
  Dyn* d=uf_materialize(cx,h); UF_PROTECT(&d);
  if(d->len){
    Cell* tmp=(Cell*)uf_alloc(d->len*sizeof(Cell),0);
    uf_msort(d->data,tmp,0,d->len);
    free(tmp);
  }
  if(a&&(a->tag==HT_ARR||a->tag==HT_TENSOR)){
    Hdr* r=(Hdr*)uf_gc_alloc(sizeof(Hdr)+d->len*a->esz,0);
    UF_PROTECT(&r);
    r->tag=a->tag; r->len=d->len; r->esz=a->esz; r->ety=a->ety;
    for(uint64_t i=0;i<d->len;i++) uf_cseti(uf_mkp(r),(int64_t)i,d->data[i]);
    UF_UNPROTECT(); UF_UNPROTECT();
    pushp(cx,r); return;
  }
  Dyn* r=uf_dyn_new(d->len); UF_PROTECT(&r);
  for(uint64_t i=0;i<d->len;i++)uf_dyn_push(&r,d->data[i]);
  UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,r);
}
/* SORTKEYS: dict -> sorted key list (keys + sort fused) */
static void op_sortkeys(Ctx*cx){
  Cell h=pop(cx); Map*m=(Map*)uf_handle(h,"SORTKEYS");
  Dyn*d=uf_dyn_new(m->len?m->len:1); UF_PROTECT(&d);
  for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1) uf_dyn_push(&d,m->keys[i]);
  if(d->len){
    Cell* tmp=(Cell*)uf_alloc(d->len*sizeof(Cell),0);
    uf_msort(d->data,tmp,0,d->len);
    free(tmp);
  }
  UF_UNPROTECT(); pushp(cx,d);
}
/* TOPN: dict n -> list of [key value] pairs, top-n by value desc, ties by key asc */
static void op_topn(Ctx*cx){
  int64_t n=uf_i(pop(cx));
  Cell h=pop(cx); Map*m=(Map*)uf_handle(h,"TOPN");
  if(n<0) n=0;
  /* collect [key value] pairs */
  Dyn*pairs=uf_dyn_new(m->len?m->len:1); UF_PROTECT(&pairs);
  for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1){
    Dyn*p=uf_dyn_new(2); uf_dyn_push(&p,m->keys[i]); uf_dyn_push(&p,m->vals[i]);
    uf_dyn_push(&pairs,uf_mkp(p));
  }
  /* selection sort top-n: find max value (ties: min key) each pass */
  Dyn*result=uf_dyn_new((uint64_t)(n<(int64_t)pairs->len?(uint64_t)n:pairs->len)); UF_PROTECT(&result);
  uint64_t plen=pairs->len;
  for(int64_t rank=0; rank<n && plen>0; rank++){
    uint64_t best=0;
    Hdr*bp=(Hdr*)uf_gc_find((void*)pairs->data[0].i); Dyn*bpair=(Dyn*)bp;
    double bestv=uf_f(bpair->data[1]); Cell bestk=bpair->data[0];
    for(uint64_t j=1;j<plen;j++){
      Hdr*hp=(Hdr*)uf_gc_find((void*)pairs->data[j].i); Dyn*pair=(Dyn*)hp;
      double v=uf_f(pair->data[1]);
      int ok; int c=uf_cmp(pair->data[0],bestk,&ok);
      if(v>bestv || (v==bestv && ok && c<0)){ bestv=v; bestk=pair->data[0]; best=j; }
    }
    uf_dyn_push(&result,pairs->data[best]);
    pairs->data[best]=pairs->data[plen-1]; plen--;
  }
  UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,result);
}
/* RANGEFOLD: count init fn_addr -> scalar
   Fold over range 0..count. Each iteration pushes (acc, k) and calls
   fn, which must leave one value (the new acc). */
static void op_rangefold(Ctx*cx){
  Cell f=pop(cx),acc=pop(cx); int64_t cnt=uf_i(pop(cx));
  long fr=cx->lsp++; if(cx->lsp>=64)die("loops nested too deep");
  cx->loops[fr].cspl=cx->csp;
  for(int64_t k=0;k<cnt;k++){
    pushc(cx,acc); pushi(cx,k);
    uf_call_addr(cx,(const void*)f.i,0,-1,2); acc=pop(cx);
  }
  cx->lsp=fr;
  pushc(cx,acc);
}
/* FILTER: list pred_addr -> list' */
static void op_filter(Ctx*cx){
  Cell f=pop(cx),h=pop(cx);
  Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  Dyn* r=uf_dyn_new(8); UF_PROTECT(&r);
  for(uint64_t i=0;i<s->len;i++){
    pushc(cx,s->data[i]); uf_call_addr(cx,(const void*)f.i,0,-1,1); Cell k=pop(cx);
    if(uf_truthy(k)) uf_dyn_push(&r,s->data[i]);
  }
  UF_UNPROTECT(); UF_UNPROTECT();
  pushp(cx,r);
}
/* SOME/EVERY: list pred_addr -> 0/1 (short-circuit) */
static void op_some(Ctx*cx){
  Cell f=pop(cx),h=pop(cx); Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  int r=0;
  for(uint64_t i=0;i<s->len;i++){ pushc(cx,s->data[i]); uf_call_addr(cx,(const void*)f.i,0,-1,1); if(uf_truthy(pop(cx))){r=1;break;} }
  UF_UNPROTECT(); pushi(cx,r);
}
static void op_every(Ctx*cx){
  Cell f=pop(cx),h=pop(cx); Dyn* s=uf_materialize(cx,h); UF_PROTECT(&s);
  int r=1;
  for(uint64_t i=0;i<s->len;i++){ pushc(cx,s->data[i]); uf_call_addr(cx,(const void*)f.i,0,-1,1); if(!uf_truthy(pop(cx))){r=0;break;} }
  UF_UNPROTECT(); pushi(cx,r);
}

/* ================= vector ops + bitmap masks ================= */
static double uf_el(Hdr*a,uint64_t i){ char*dt=uf_data(a); if(a->ety==1)return ((double*)dt)[i]; if(a->ety==3)return (double)((uint8_t*)dt)[i]; return (double)((int64_t*)dt)[i]; }
static void uf_put_el(Hdr*a,uint64_t i,double d){ char*dt=uf_data(a); if(a->ety==1)((double*)dt)[i]=d; else if(a->ety==3)((uint8_t*)dt)[i]=(uint8_t)d; else ((int64_t*)dt)[i]=(int64_t)d; }
static Hdr* uf_arr_like(Hdr*a,uint64_t n){
  /* HT_MAT: esz is rows, not element bytes — derive byte size from ety */
  uint64_t nb=(a->tag==HT_MAT)?(n*((a->ety==3)?1:8)):(n*a->esz);
  /* data is fully overwritten by every caller (elementwise ops, memcpy) — no zero-fill */
  Hdr*r=(Hdr*)uf_gc_alloc_nz(sizeof(Hdr)+nb,0); r->tag=a->tag; r->len=n; r->esz=a->esz; r->ety=a->ety; return r;
}

/* ================= GPU compute offloading (Vulkan, v13.1) =================
   Enabled when the compiler embedded the SPIR-V blobs and defined NK_GPU
   (automatic when glslc is present and device mode != cpu). The shims below
   lazily initialize Vulkan, pick the device (auto = most free VRAM via
   VK_EXT_memory_budget, discrete preferred; or the pinned --device bake),
   and LAUNCH prebuilt kernels for eligible ops when the element count clears
   NK_GPU_MIN (env, default 65536). Any failure or too-small workload falls
   back to the CPU implementation — the GPU is a fast path, never a
   correctness dependency. Kernels are float64; other element types stay CPU.
   NOTE: `div` on the GPU yields inf/nan for zero divisors where the CPU dies
   (documented divergence). */
#ifdef NK_GPU
#include <vulkan/vulkan.h>
static VkInstance uf_vk_inst;
static VkPhysicalDevice uf_vk_pd;
static VkDevice uf_vk_dev;
static VkQueue uf_vk_q;
static uint32_t uf_vk_qf;
static VkCommandPool uf_vk_pool;
static VkCommandBuffer uf_vk_cb;
static VkPipelineLayout uf_vk_playout;
static VkDescriptorSetLayout uf_vk_dsl;
static VkDescriptorPool uf_vk_dpool;
static VkBuffer uf_vk_dummy;
static VkDeviceMemory uf_vk_dummy_mem;
static VkPipeline uf_vk_pipe[sizeof(uf_spv_all)/sizeof(uf_spv_all[0])];
static int uf_vk_ready, uf_vk_broken;
static long uf_gpu_min(void){ const char*e=getenv("NK_GPU_MIN"); long v=e?atol(e):65536; return v>0?v:65536; }
/* v13.2: per-op arith offload floor. A fused region does O(ops) work per
   element for ONE transfer — worth it on the GPU at any size. A single
   per-op add/mul moves ~3×n×8 bytes for one op — on host-visible staging
   (~2GB/s) that loses to the CPU fast path until n is large. Fused-region
   and reduce paths keep the plain uf_gpu_min(). */
static long uf_gpu_arith_min(void){ const char*e=getenv("NK_GPU_ARITH_MIN"); long v=e?atol(e):(8L<<20); return v>0?v:(8L<<20); }
/* Static first-run matmul floor (no timing / no calibration): offload only
   when ra*ca*cb FLOPs clear this. 512³=134M loses to the CPU even after
   Vulkan is up (transfer+init); 1024³=1.07B wins. Default 400M sits between
   512³ and 768³. Env NK_GPU_MATMUL_MIN overrides. */
static long uf_gpu_matmul_min(void){ const char*e=getenv("NK_GPU_MATMUL_MIN"); long v=e?atol(e):(400L<<20); return v>0?v:(400L<<20); }
/* auto = pick CPU when the static estimate says so; vk<N> pins GPU. */
static int uf_dev_is_auto(void){ return !(uf_device[0]=='v' && uf_device[1]=='k'); }
static pthread_mutex_t uf_gpu_mu = PTHREAD_MUTEX_INITIALIZER;
static int uf_spv_index(const char*name){ for(size_t i=0;i<sizeof(uf_spv_all)/sizeof(uf_spv_all[0]);i++) if(!strcmp(uf_spv_all[i].name,name))return (int)i; return -1; }

static uint64_t uf_vk_free_mem(VkPhysicalDevice pd, int have_budget, int*discrete){
  VkPhysicalDeviceProperties props; vkGetPhysicalDeviceProperties(pd,&props);
  *discrete = (props.deviceType==VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU);
  if(have_budget){
    VkPhysicalDeviceMemoryProperties2 mp; memset(&mp,0,sizeof mp); mp.sType=VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MEMORY_PROPERTIES_2;
    VkPhysicalDeviceMemoryBudgetPropertiesEXT bp; memset(&bp,0,sizeof bp); bp.sType=VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MEMORY_BUDGET_PROPERTIES_EXT;
    mp.pNext=&bp;
    vkGetPhysicalDeviceMemoryProperties2(pd,&mp);
    uint64_t free=0;
    for(uint32_t i=0;i<mp.memoryProperties.memoryHeapCount;i++)
      if(mp.memoryProperties.memoryHeaps[i].flags&VK_MEMORY_HEAP_DEVICE_LOCAL_BIT){
        uint64_t b=bp.heapBudget[i],u=bp.heapUsage[i];
        free += (b>u)?(b-u):0;
      }
    if(free) return free;
  }
  vkGetPhysicalDeviceProperties2;
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(pd,&mp);
  uint64_t tot=0;
  for(uint32_t i=0;i<mp.memoryHeapCount;i++) if(mp.memoryHeaps[i].flags&VK_MEMORY_HEAP_DEVICE_LOCAL_BIT) tot+=mp.memoryHeaps[i].size;
  return tot;
}

static void uf_vk_init_body(void);
static void uf_vk_init(void){
  if(uf_vk_ready||uf_vk_broken) return;
  static pthread_once_t once=PTHREAD_ONCE_INIT;
  pthread_once(&once, uf_vk_init_body);
}
static void uf_vk_init_body(void){
  VkApplicationInfo app; memset(&app,0,sizeof app); app.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO; app.pApplicationName="nmerkar"; app.apiVersion=VK_API_VERSION_1_1;
  VkInstanceCreateInfo ci; memset(&ci,0,sizeof ci); ci.sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO; ci.pApplicationInfo=&app;
  if(vkCreateInstance(&ci,0,&uf_vk_inst)!=VK_SUCCESS){ uf_vk_broken=1; return; }
  uint32_t nd=0; vkEnumeratePhysicalDevices(uf_vk_inst,&nd,0);
  if(!nd){ uf_vk_broken=1; return; }
  VkPhysicalDevice* pds=(VkPhysicalDevice*)malloc(nd*sizeof(VkPhysicalDevice));
  vkEnumeratePhysicalDevices(uf_vk_inst,&nd,pds);
  /* device selection: auto = most free VRAM (discrete preferred); vk<N> pinned */
  int sel=-1;
  if(uf_device[0]=='v'&&uf_device[1]=='k'&&uf_device[2]>='0'&&uf_device[2]<='9'){
    long want=atol(uf_device+2);
    if((uint32_t)want>=nd){
      char msg[2048]; int n=snprintf(msg,sizeof msg,"device '%s' requested but only %u Vulkan device(s) found:",uf_device,nd);
      for(uint32_t i=0;i<nd&&n>0&&n<(int)sizeof(msg)-256;i++){
        VkPhysicalDeviceProperties pr; vkGetPhysicalDeviceProperties(pds[i],&pr);
        n+=snprintf(msg+n,sizeof(msg)-n," vk%u: '%s',",i,pr.deviceName);
      }
      if(n>0&&msg[n-1]==',')msg[n-1]=0;
      free(pds); vkDestroyInstance(uf_vk_inst,0); uf_vk_broken=1; die(msg);
    }
    sel=(int)atol(uf_device+2);
  } else {
    /* hardware first (discrete > integrated > software), most free VRAM
       within the class (llvmpipe advertises RAM-sized budgets) */
    uint64_t best=0; int bcls=-1;
    for(uint32_t i=0;i<nd;i++){
      uint32_t nec=0; vkEnumerateDeviceExtensionProperties(pds[i],0,&nec,0);
      VkExtensionProperties* ex=(VkExtensionProperties*)malloc(nec*sizeof(VkExtensionProperties));
      vkEnumerateDeviceExtensionProperties(pds[i],0,&nec,ex);
      int hb=0; for(uint32_t k=0;k<nec;k++) if(!strcmp(ex[k].extensionName,"VK_EXT_memory_budget")) hb=1;
      free(ex);
      VkPhysicalDeviceProperties pr; vkGetPhysicalDeviceProperties(pds[i],&pr);
      int cls = pr.deviceType==VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU?2
              : pr.deviceType==VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU?1 : 0;
      int disc; uint64_t fm=uf_vk_free_mem(pds[i],hb,&disc);
      if(cls>bcls||(cls==bcls&&fm>best)){ best=fm; bcls=cls; sel=(int)i; }
    }
  }
  if(sel<0){ free(pds); vkDestroyInstance(uf_vk_inst,0); uf_vk_broken=1; return; }
  uf_vk_pd=pds[sel]; free(pds);
  uint32_t nq=0; vkGetPhysicalDeviceQueueFamilyProperties(uf_vk_pd,&nq,0);
  VkQueueFamilyProperties* qf=(VkQueueFamilyProperties*)malloc(nq*sizeof(VkQueueFamilyProperties));
  vkGetPhysicalDeviceQueueFamilyProperties(uf_vk_pd,&nq,qf);
  uf_vk_qf=UINT32_MAX;
  for(uint32_t i=0;i<nq;i++) if(qf[i].queueFlags&VK_QUEUE_COMPUTE_BIT){ uf_vk_qf=i; break; }
  free(qf);
  if(uf_vk_qf==UINT32_MAX){ vkDestroyInstance(uf_vk_inst,0); uf_vk_broken=1; return; }
  float pri=1.0f;
  VkDeviceQueueCreateInfo qci; memset(&qci,0,sizeof qci); qci.sType=VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO; qci.queueFamilyIndex=uf_vk_qf; qci.queueCount=1; qci.pQueuePriorities=&pri;
  const char* devexts[1]; uint32_t ndevext=0;
  uint32_t nec=0; vkEnumerateDeviceExtensionProperties(uf_vk_pd,0,&nec,0);
  VkExtensionProperties* ex=(VkExtensionProperties*)malloc(nec*sizeof(VkExtensionProperties));
  vkEnumerateDeviceExtensionProperties(uf_vk_pd,0,&nec,ex);
  for(uint32_t k=0;k<nec;k++) if(!strcmp(ex[k].extensionName,"VK_EXT_memory_budget")) devexts[ndevext++]=ex[k].extensionName;
  free(ex);
  VkDeviceCreateInfo dci; memset(&dci,0,sizeof dci); dci.sType=VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO;
  dci.queueCreateInfoCount=1; dci.pQueueCreateInfos=&qci;
  dci.enabledExtensionCount=ndevext; dci.ppEnabledExtensionNames=devexts;
  if(vkCreateDevice(uf_vk_pd,&dci,0,&uf_vk_dev)!=VK_SUCCESS){ vkDestroyInstance(uf_vk_inst,0); uf_vk_broken=1; return; }
  vkGetDeviceQueue(uf_vk_dev,uf_vk_qf,0,&uf_vk_q);
  VkCommandPoolCreateInfo pci; memset(&pci,0,sizeof pci); pci.sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO; pci.queueFamilyIndex=uf_vk_qf; pci.flags=VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
  if(vkCreateCommandPool(uf_vk_dev,&pci,0,&uf_vk_pool)!=VK_SUCCESS){ uf_vk_broken=1; return; }
  VkCommandBufferAllocateInfo cai; memset(&cai,0,sizeof cai); cai.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO; cai.commandPool=uf_vk_pool; cai.level=VK_COMMAND_BUFFER_LEVEL_PRIMARY; cai.commandBufferCount=1;
  if(vkAllocateCommandBuffers(uf_vk_dev,&cai,&uf_vk_cb)!=VK_SUCCESS){ uf_vk_broken=1; return; }
  /* layouts: 3 storage buffers + 48-byte push constants */
  VkDescriptorSetLayoutBinding lb[8];
  for(int i=0;i<8;i++){ lb[i].binding=(uint32_t)i; lb[i].descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; lb[i].descriptorCount=1; lb[i].stageFlags=VK_SHADER_STAGE_COMPUTE_BIT; lb[i].pImmutableSamplers=0; }
  VkDescriptorSetLayoutCreateInfo dsl; memset(&dsl,0,sizeof dsl); dsl.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO; dsl.bindingCount=8; dsl.pBindings=lb;
  vkCreateDescriptorSetLayout(uf_vk_dev,&dsl,0,&uf_vk_dsl);
  VkPushConstantRange pr; pr.stageFlags=VK_SHADER_STAGE_COMPUTE_BIT; pr.offset=0; pr.size=sizeof(struct UFPC);
  VkPipelineLayoutCreateInfo pl; memset(&pl,0,sizeof pl); pl.sType=VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO; pl.setLayoutCount=1; pl.pSetLayouts=&uf_vk_dsl; pl.pushConstantRangeCount=1; pl.pPushConstantRanges=&pr;
  vkCreatePipelineLayout(uf_vk_dev,&pl,0,&uf_vk_playout);
  VkDescriptorPoolSize ps; ps.type=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; ps.descriptorCount=128;
  VkDescriptorPoolCreateInfo dpi; memset(&dpi,0,sizeof dpi); dpi.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO; dpi.flags=VK_DESCRIPTOR_POOL_CREATE_FREE_DESCRIPTOR_SET_BIT; dpi.maxSets=16; dpi.poolSizeCount=1; dpi.pPoolSizes=&ps;
  vkCreateDescriptorPool(uf_vk_dev,&dpi,0,&uf_vk_dpool);
  /* dummy buffer for unused bindings */
  VkBufferCreateInfo dbi; memset(&dbi,0,sizeof dbi); dbi.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO; dbi.size=16; dbi.usage=VK_BUFFER_USAGE_STORAGE_BUFFER_BIT;
  vkCreateBuffer(uf_vk_dev,&dbi,0,&uf_vk_dummy);
  VkMemoryRequirements mr; vkGetBufferMemoryRequirements(uf_vk_dev,uf_vk_dummy,&mr);
  VkMemoryAllocateInfo mai; memset(&mai,0,sizeof mai); mai.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO; mai.allocationSize=mr.size;
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(uf_vk_pd,&mp);
  for(uint32_t i=0;i<mp.memoryTypeCount;i++)
    if(mp.memoryTypes[i].propertyFlags&VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT){ mai.memoryTypeIndex=i; break; }
  vkAllocateMemory(uf_vk_dev,&mai,0,&uf_vk_dummy_mem);
  vkBindBufferMemory(uf_vk_dev,uf_vk_dummy,uf_vk_dummy_mem,0);
  /* pipelines from embedded SPIR-V */
  for(size_t i=0;i<sizeof(uf_spv_all)/sizeof(uf_spv_all[0]);i++){
    VkShaderModuleCreateInfo sm; memset(&sm,0,sizeof sm); sm.sType=VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO; sm.codeSize=uf_spv_all[i].words*4; sm.pCode=uf_spv_all[i].code;
    VkShaderModule mod;
    if(vkCreateShaderModule(uf_vk_dev,&sm,0,&mod)!=VK_SUCCESS){ uf_vk_broken=1; return; }
    VkComputePipelineCreateInfo cp; memset(&cp,0,sizeof cp); cp.sType=VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO; cp.stage.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO; cp.stage.stage=VK_SHADER_STAGE_COMPUTE_BIT; cp.stage.module=mod; cp.stage.pName="main"; cp.layout=uf_vk_playout;
    if(vkCreateComputePipelines(uf_vk_dev,0,1,&cp,0,&uf_vk_pipe[i])!=VK_SUCCESS){ uf_vk_broken=1; return; }
    vkDestroyShaderModule(uf_vk_dev,mod,0);
  }
  uf_vk_ready=1;
}
static int uf_vk_ensure_pipe(int k){
  if(k<0||(size_t)k>=sizeof(uf_spv_all)/sizeof(uf_spv_all[0])) return 0;
  if(uf_vk_pipe[k]) return 1;
  VkShaderModuleCreateInfo sm; memset(&sm,0,sizeof sm); sm.sType=VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO; sm.codeSize=uf_spv_all[k].words*4; sm.pCode=uf_spv_all[k].code;
  VkShaderModule mod;
  if(vkCreateShaderModule(uf_vk_dev,&sm,0,&mod)!=VK_SUCCESS) return 0;
  VkComputePipelineCreateInfo cp; memset(&cp,0,sizeof cp); cp.sType=VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO; cp.stage.sType=VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO; cp.stage.stage=VK_SHADER_STAGE_COMPUTE_BIT; cp.stage.module=mod; cp.stage.pName="main"; cp.layout=uf_vk_playout;
  int ok=vkCreateComputePipelines(uf_vk_dev,0,1,&cp,0,&uf_vk_pipe[k])==VK_SUCCESS;
  vkDestroyShaderModule(uf_vk_dev,mod,0);
  return ok;
}

/* v13.2 device buffer pool: ONE persistent HOST_VISIBLE (HOST_COHERENT when
   available) allocation, suballocated per launch via offsets and grown on
   demand. The old path did vkCreateBuffer+vkAllocateMemory+vkMapMemory (and
   the matching destroys) per operand per op — tens of milliseconds each on
   RADV, which is why per-op offloaded programs (blackscholes: ~40 ops) spent
   seconds in the driver. Launches stay fully synchronous: upload, dispatch,
   wait, download, next call reuses the pool from offset 0. Growth destroys
   and recreates the buffer BEFORE any data is staged, so nothing is lost.
   Region offsets are 256B-aligned (minStorageBufferOffsetAlignment ≤ 256). */
#define UF_VK_ALIGN 256
typedef struct { VkBuffer buf; VkDeviceMemory mem; char* mapped; VkDeviceSize cap; int coherent; } UFCache;
static UFCache uf_vk_bpool;
static VkFence uf_vk_fence;
static VkDeviceSize uf_vk_al(VkDeviceSize s){ return (s+(UF_VK_ALIGN-1))&~((VkDeviceSize)UF_VK_ALIGN-1); }
/* non-coherent cached pool: make CPU writes visible to the GPU / GPU writes
   visible to the CPU. No-ops on a coherent heap. Offsets are pool-relative;
   ranges are rounded out to UF_VK_ALIGN (>= any known nonCoherentAtomSize
   multiple used here). */
static void uf_vk_flush(VkDeviceSize off, VkDeviceSize sz){
  if(uf_vk_bpool.coherent||!sz||!uf_vk_bpool.mem) return;
  VkMappedMemoryRange r; memset(&r,0,sizeof r); r.sType=VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE;
  r.memory=uf_vk_bpool.mem; r.offset=off; r.size=uf_vk_al(sz);
  vkFlushMappedMemoryRanges(uf_vk_dev,1,&r);
}
static void uf_vk_inval(VkDeviceSize off, VkDeviceSize sz){
  if(uf_vk_bpool.coherent||!sz||!uf_vk_bpool.mem) return;
  VkMappedMemoryRange r; memset(&r,0,sizeof r); r.sType=VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE;
  r.memory=uf_vk_bpool.mem; r.offset=off; r.size=uf_vk_al(sz);
  vkInvalidateMappedMemoryRanges(uf_vk_dev,1,&r);
}
static int uf_vk_pool_reserve(VkDeviceSize total){
  if(uf_vk_bpool.cap>=total&&uf_vk_bpool.buf) return 1;
  VkDeviceSize want=uf_vk_al(total); VkDeviceSize min=64UL<<20; if(want<min)want=min;
  if(uf_vk_bpool.buf) vkDestroyBuffer(uf_vk_dev,uf_vk_bpool.buf,0);
  if(uf_vk_bpool.mem){ vkUnmapMemory(uf_vk_dev,uf_vk_bpool.mem); vkFreeMemory(uf_vk_dev,uf_vk_bpool.mem,0); }
  memset(&uf_vk_pool,0,sizeof uf_vk_pool);
  VkBufferCreateInfo bi; memset(&bi,0,sizeof bi); bi.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO; bi.size=want;
  bi.usage=VK_BUFFER_USAGE_STORAGE_BUFFER_BIT|VK_BUFFER_USAGE_TRANSFER_SRC_BIT|VK_BUFFER_USAGE_TRANSFER_DST_BIT;
  if(vkCreateBuffer(uf_vk_dev,&bi,0,&uf_vk_bpool.buf)!=VK_SUCCESS) return 0;
  VkMemoryRequirements mr; vkGetBufferMemoryRequirements(uf_vk_dev,uf_vk_bpool.buf,&mr);
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(uf_vk_pd,&mp);
  int found=0; VkDeviceSize sz=mr.size>want?mr.size:want;
  /* Prefer cached host memory: region outputs are read back by the CPU after
     the fence, and uncached coherent mappings cap that memcpy at PCIe-ish
     speeds (~1.4GB/s here) while cached ones stream from DRAM. On AMD the
     cached heap is typically NON-coherent, so accept it and use explicit
     flush/invalidate barriers (uf_vk_flush/uf_vk_inval) — pass order:
     cached+coherent, cached, coherent, any. */
  int coherent=1;
  for(int pass=0;pass<4&&!found;pass++)
    for(uint32_t i=0;i<mp.memoryTypeCount;i++){
      VkMemoryPropertyFlags f=mp.memoryTypes[i].propertyFlags;
      int wantcached=(pass==0||pass==1), wantcoherent=(pass==0||pass==2);
      if((mp.memoryTypes[i].propertyFlags&VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT)
        &&(!wantcached||(f&VK_MEMORY_PROPERTY_HOST_CACHED_BIT))
        &&(!wantcoherent||(mp.memoryTypes[i].propertyFlags&VK_MEMORY_PROPERTY_HOST_COHERENT_BIT))
        &&(mr.memoryTypeBits&(1u<<i))){
        VkMemoryAllocateInfo ai; memset(&ai,0,sizeof ai); ai.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO; ai.allocationSize=sz; ai.memoryTypeIndex=i;
        if(vkAllocateMemory(uf_vk_dev,&ai,0,&uf_vk_bpool.mem)==VK_SUCCESS){ found=1; coherent=(f&VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)?1:0;
          if(getenv("NK_VK_DEBUG"))fprintf(stderr,"[vk-pool] type=%u flags=0x%x cached=%d coherent=%d cap=%lluMB\n",i,(unsigned)f,(f&VK_MEMORY_PROPERTY_HOST_CACHED_BIT)?1:0,coherent,(unsigned long long)(sz>>20));
          break; }
      }
    }
  if(!found||vkBindBufferMemory(uf_vk_dev,uf_vk_bpool.buf,uf_vk_bpool.mem,0)!=VK_SUCCESS
     ||vkMapMemory(uf_vk_dev,uf_vk_bpool.mem,0,sz,0,(void**)&uf_vk_bpool.mapped)!=VK_SUCCESS){
    if(uf_vk_bpool.mem)vkFreeMemory(uf_vk_dev,uf_vk_bpool.mem,0);
    if(uf_vk_bpool.buf)vkDestroyBuffer(uf_vk_dev,uf_vk_bpool.buf,0);
    memset(&uf_vk_pool,0,sizeof uf_vk_pool); return 0;
  }
  uf_vk_bpool.cap=sz; uf_vk_bpool.coherent=coherent;
  if(!uf_vk_fence){ VkFenceCreateInfo fci; memset(&fci,0,sizeof fci); fci.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO; vkCreateFence(uf_vk_dev,&fci,0,&uf_vk_fence); }
  return 1;
}
/* DEVICE_LOCAL twin of the staging pool. Matmul and fused kernels execute
   here so shaders do not stream their working sets from host memory over
   PCIe. The mapped pool remains the upload/download staging area. */
static UFCache uf_vk_dl;
static int uf_vk_dl_reserve(VkDeviceSize total){
  if(uf_vk_dl.cap>=total&&uf_vk_dl.buf) return 1;
  VkDeviceSize want=uf_vk_al(total); VkDeviceSize min=64UL<<20; if(want<min)want=min;
  if(uf_vk_dl.buf) vkDestroyBuffer(uf_vk_dev,uf_vk_dl.buf,0);
  if(uf_vk_dl.mem) vkFreeMemory(uf_vk_dev,uf_vk_dl.mem,0);
  memset(&uf_vk_dl,0,sizeof uf_vk_dl);
  VkBufferCreateInfo bi; memset(&bi,0,sizeof bi); bi.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO; bi.size=want;
  bi.usage=VK_BUFFER_USAGE_STORAGE_BUFFER_BIT|VK_BUFFER_USAGE_TRANSFER_SRC_BIT|VK_BUFFER_USAGE_TRANSFER_DST_BIT;
  if(vkCreateBuffer(uf_vk_dev,&bi,0,&uf_vk_dl.buf)!=VK_SUCCESS) return 0;
  VkMemoryRequirements mr; vkGetBufferMemoryRequirements(uf_vk_dev,uf_vk_dl.buf,&mr);
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(uf_vk_pd,&mp);
  int found=0; VkDeviceSize sz=mr.size>want?mr.size:want;
  for(int pass=0;pass<2&&!found;pass++)
    for(uint32_t i=0;i<mp.memoryTypeCount;i++){
      VkMemoryPropertyFlags f=mp.memoryTypes[i].propertyFlags;
      int ok = (mr.memoryTypeBits&(1u<<i)) && (f&VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
      if(pass==0) ok = ok && !(f&VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT);
      if(!ok) continue;
      VkMemoryAllocateInfo ai; memset(&ai,0,sizeof ai); ai.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO; ai.allocationSize=sz; ai.memoryTypeIndex=i;
      if(vkAllocateMemory(uf_vk_dev,&ai,0,&uf_vk_dl.mem)==VK_SUCCESS){ found=1; break; }
    }
  if(!found||vkBindBufferMemory(uf_vk_dev,uf_vk_dl.buf,uf_vk_dl.mem,0)!=VK_SUCCESS){
    if(uf_vk_dl.mem)vkFreeMemory(uf_vk_dev,uf_vk_dl.mem,0);
    if(uf_vk_dl.buf)vkDestroyBuffer(uf_vk_dev,uf_vk_dl.buf,0);
    memset(&uf_vk_dl,0,sizeof uf_vk_dl); return 0;
  }
  uf_vk_dl.cap=sz; return 1;
}
/* submit the prebuilt command buffer and wait (persistent fence) */
static int uf_vk_submit_wait(void){
  if(!uf_vk_fence) return 0;
  vkResetFences(uf_vk_dev,1,&uf_vk_fence);
  VkSubmitInfo si; memset(&si,0,sizeof si); si.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO; si.commandBufferCount=1; si.pCommandBuffers=&uf_vk_cb;
  return vkQueueSubmit(uf_vk_q,1,&si,uf_vk_fence)==VK_SUCCESS && vkWaitForFences(uf_vk_dev,1,&uf_vk_fence,VK_TRUE,UINT64_MAX)==VK_SUCCESS;
}

/* dispatch kernel k over n work items; reads A (and B when non-null) into
   device memory, writes R (rsz bytes) back. Returns 0 on failure. */
static int uf_vk_run(int k,uint64_t n,const void*A,size_t asz,const void*B,size_t bsz,void*R,size_t rsz,struct UFPC pc){
  if(uf_vk_broken) return 0;
  uf_vk_init();
  if(!uf_vk_ready) return 0;
  pthread_mutex_lock(&uf_gpu_mu);
  if(!uf_vk_ensure_pipe(k)){ pthread_mutex_unlock(&uf_gpu_mu); return 0; }
  double _t0=uf_nowd();
  if(getenv("NK_VK_DEBUG"))fprintf(stderr,"[vk] bufs a=%zu b=%zu r=%zu\n",asz,bsz,rsz);
  VkDeviceSize oa=0, ob=uf_vk_al((VkDeviceSize)asz), orr=uf_vk_al(ob+(VkDeviceSize)bsz);
  if(!uf_vk_pool_reserve(orr+uf_vk_al((VkDeviceSize)rsz))){ pthread_mutex_unlock(&uf_gpu_mu); return 0; }
  if(A&&asz) memcpy(uf_vk_bpool.mapped+oa,A,asz);
  if(B&&bsz) memcpy(uf_vk_bpool.mapped+ob,B,bsz);
  uf_vk_flush(oa, (ob+(VkDeviceSize)bsz)-oa);
  int is_mm=(k==uf_spv_index("matmul") && pc.n1>0 && pc.n3>0);
  if(is_mm && !uf_vk_dl_reserve(orr+uf_vk_al((VkDeviceSize)rsz))) is_mm=0;
  VkBuffer cbuf=is_mm?uf_vk_dl.buf:uf_vk_bpool.buf;
  VkDescriptorSetAllocateInfo dsai; memset(&dsai,0,sizeof dsai); dsai.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO; dsai.descriptorPool=uf_vk_dpool; dsai.descriptorSetCount=1; dsai.pSetLayouts=&uf_vk_dsl;
  VkDescriptorSet ds;
  VkResult ar=vkAllocateDescriptorSets(uf_vk_dev,&dsai,&ds);
  if(getenv("NK_VK_DEBUG"))fprintf(stderr,"[vk] dsalloc=%d\n",(int)ar);
  if(ar!=VK_SUCCESS){ pthread_mutex_unlock(&uf_gpu_mu); return 0; }
  VkWriteDescriptorSet w[3]; VkDescriptorBufferInfo bi[3];
  memset(w,0,sizeof w); memset(bi,0,sizeof bi);
  bi[0].buffer=cbuf; bi[0].offset=oa; bi[0].range=asz?asz:16;
  bi[1].buffer=(B&&bsz)?cbuf:uf_vk_dummy; bi[1].offset=(B&&bsz)?ob:0; bi[1].range=(B&&bsz)?bsz:16;
  bi[2].buffer=cbuf; bi[2].offset=orr; bi[2].range=rsz?rsz:16;
  for(int i=0;i<3;i++){ w[i].sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET; w[i].dstSet=ds; w[i].dstBinding=(uint32_t)i; w[i].descriptorCount=1; w[i].descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; w[i].pBufferInfo=&bi[i]; }
  vkUpdateDescriptorSets(uf_vk_dev,3,w,0,0);
  if(getenv("NK_VK_DEBUG"))fprintf(stderr,"[vk] updated\n");
  VkCommandBufferBeginInfo cbbi; memset(&cbbi,0,sizeof cbbi); cbbi.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
  if(getenv("NK_VK_DEBUG"))fprintf(stderr,"[vk] run k=%d n=%llu begin\n",k,(unsigned long long)n);
  vkBeginCommandBuffer(uf_vk_cb,&cbbi);
  if(is_mm){
    if(A&&asz){ VkBufferCopy c={oa,oa,(VkDeviceSize)asz}; vkCmdCopyBuffer(uf_vk_cb,uf_vk_bpool.buf,uf_vk_dl.buf,1,&c); }
    if(B&&bsz){ VkBufferCopy c={ob,ob,(VkDeviceSize)bsz}; vkCmdCopyBuffer(uf_vk_cb,uf_vk_bpool.buf,uf_vk_dl.buf,1,&c); }
    VkMemoryBarrier mb; memset(&mb,0,sizeof mb); mb.sType=VK_STRUCTURE_TYPE_MEMORY_BARRIER;
    mb.srcAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT; mb.dstAccessMask=VK_ACCESS_SHADER_READ_BIT;
    vkCmdPipelineBarrier(uf_vk_cb,VK_PIPELINE_STAGE_TRANSFER_BIT,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,0,1,&mb,0,0,0,0);
  }
  vkCmdBindPipeline(uf_vk_cb,VK_PIPELINE_BIND_POINT_COMPUTE,uf_vk_pipe[k]);
  vkCmdPushConstants(uf_vk_cb,uf_vk_playout,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pc,&pc);
  vkCmdBindDescriptorSets(uf_vk_cb,VK_PIPELINE_BIND_POINT_COMPUTE,uf_vk_playout,0,1,&ds,0,0);
  /* tiled matmul shader is local_size 16×16; everyone else is 256×1 */
  if(k==uf_spv_index("matmul") && pc.n1>0 && pc.n3>0){
    uint32_t gx=(uint32_t)((pc.n3+15)/16), gy=(uint32_t)((pc.n1+15)/16);
    if(!gx)gx=1; if(!gy)gy=1;
    vkCmdDispatch(uf_vk_cb,gx,gy,1);
  } else {
    uint64_t groups=(n+255)/256; if(!groups)groups=1;
    vkCmdDispatch(uf_vk_cb,(uint32_t)(groups>0x7fffffff?0x7fffffff:groups),1,1);
  }
  if(is_mm&&R&&rsz){
    VkMemoryBarrier mb; memset(&mb,0,sizeof mb); mb.sType=VK_STRUCTURE_TYPE_MEMORY_BARRIER;
    mb.srcAccessMask=VK_ACCESS_SHADER_WRITE_BIT; mb.dstAccessMask=VK_ACCESS_TRANSFER_READ_BIT;
    vkCmdPipelineBarrier(uf_vk_cb,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,VK_PIPELINE_STAGE_TRANSFER_BIT,0,1,&mb,0,0,0,0);
    VkBufferCopy c={orr,orr,(VkDeviceSize)rsz}; vkCmdCopyBuffer(uf_vk_cb,uf_vk_dl.buf,uf_vk_bpool.buf,1,&c);
  }
  vkEndCommandBuffer(uf_vk_cb);
  if(getenv("NK_VK_DEBUG"))fprintf(stderr,"[vk] submit\n");
  int ok = uf_vk_submit_wait();
  uf_vk_inval(orr, (VkDeviceSize)rsz);
  if(ok&&R&&rsz) memcpy(R,uf_vk_bpool.mapped+orr,rsz);
  if(getenv("NK_VK_DEBUG"))fprintf(stderr,"[vk] total=%.1fms\n",(uf_nowd()-_t0)*1e3);
  vkFreeDescriptorSets(uf_vk_dev,uf_vk_dpool,1,&ds);
  pthread_mutex_unlock(&uf_gpu_mu);
  return ok;
}

/* ---- fused weave-task dispatch (v13.1) ----
   One kernel launch for a whole compilable task body: inputs are the top n
   cells of the ds (peeked, not popped — the ret epilogue drains), the task
   kernel table follows the static library pipelines. Declines (no device,
   non-float/mismatched inputs, below NK_GPU_MIN, any Vulkan failure) return
   the sentinel and the CPU body runs instead. */
static Cell uf_gpu_decline(void);
static Cell uf_gpu_task(Ctx*cx,int k,int n){
  if(uf_vk_broken||n<1||n>7||cx->sp<(uint64_t)n) return uf_gpu_decline();
  Hdr* ins[7];
  for(int j=0;j<n;j++){
    Cell c=cx->ds[cx->sp-n+j];
    if(!uf_numarr(c)) return uf_gpu_decline();
    ins[j]=(Hdr*)(void*)c.i;
    if(ins[j]->ety!=1) return uf_gpu_decline();
    if(ins[j]->len!=ins[0]->len) return uf_gpu_decline();
  }
  uint64_t len=ins[0]->len;
  if(len<(uint64_t)uf_gpu_min()) return uf_gpu_decline();
  uf_vk_init();
  if(!uf_vk_ready) return uf_gpu_decline();
  if(k<0||k>=(int)(sizeof(uf_spv_all)/sizeof(uf_spv_all[0]))) return uf_gpu_decline();
  pthread_mutex_lock(&uf_gpu_mu);
  if(!uf_vk_ensure_pipe(k)){ pthread_mutex_unlock(&uf_gpu_mu); return uf_gpu_decline(); }
  size_t bufsz=(size_t)len*8;
  VkDeviceSize offs[8]; VkDeviceSize cur=0;
  for(int j=0;j<=n;j++){ offs[j]=cur; cur=uf_vk_al(cur+(VkDeviceSize)bufsz); }
  int ok=uf_vk_pool_reserve(cur);
  int use_dl=ok&&uf_vk_dl_reserve(cur);
  VkBuffer cbuf=use_dl?uf_vk_dl.buf:uf_vk_bpool.buf;
  int dbg=getenv("NK_VK_DEBUG")!=0;
  double _t0=dbg?uf_nowd():0;
  Hdr* r=ok?uf_arr_like(ins[0],len):0; UF_PROTECT(&r);
  if(ok){
    for(int j=0;j<n;j++) memcpy(uf_vk_bpool.mapped+offs[j],uf_data(ins[j]),bufsz);
    uf_vk_flush(offs[0], (offs[n-1]+bufsz)-offs[0]);
    VkDescriptorSetAllocateInfo dsai; memset(&dsai,0,sizeof dsai); dsai.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO; dsai.descriptorPool=uf_vk_dpool; dsai.descriptorSetCount=1; dsai.pSetLayouts=&uf_vk_dsl;
    VkDescriptorSet ds;
    ok=vkAllocateDescriptorSets(uf_vk_dev,&dsai,&ds)==VK_SUCCESS;
    if(ok){
      VkWriteDescriptorSet w[8]; VkDescriptorBufferInfo bi[8];
      memset(w,0,sizeof w); memset(bi,0,sizeof bi);
      for(int j=0;j<=n;j++){ bi[j].buffer=cbuf; bi[j].offset=offs[j]; bi[j].range=bufsz;
        w[j].sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET; w[j].dstSet=ds; w[j].dstBinding=(uint32_t)j; w[j].descriptorCount=1; w[j].descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; w[j].pBufferInfo=&bi[j]; }
      vkUpdateDescriptorSets(uf_vk_dev,n+1,w,0,0);
      struct UFPC pc; memset(&pc,0,sizeof pc); pc.n0=(int64_t)len;
      VkCommandBufferBeginInfo cbbi; memset(&cbbi,0,sizeof cbbi); cbbi.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
      ok=vkBeginCommandBuffer(uf_vk_cb,&cbbi)==VK_SUCCESS;
      if(ok){
        if(use_dl){
          for(int j=0;j<n;j++){ VkBufferCopy c={offs[j],offs[j],(VkDeviceSize)bufsz}; vkCmdCopyBuffer(uf_vk_cb,uf_vk_bpool.buf,uf_vk_dl.buf,1,&c); }
          VkMemoryBarrier mb; memset(&mb,0,sizeof mb); mb.sType=VK_STRUCTURE_TYPE_MEMORY_BARRIER;
          mb.srcAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT; mb.dstAccessMask=VK_ACCESS_SHADER_READ_BIT;
          vkCmdPipelineBarrier(uf_vk_cb,VK_PIPELINE_STAGE_TRANSFER_BIT,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,0,1,&mb,0,0,0,0);
        }
        vkCmdBindPipeline(uf_vk_cb,VK_PIPELINE_BIND_POINT_COMPUTE,uf_vk_pipe[k]);
        vkCmdPushConstants(uf_vk_cb,uf_vk_playout,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pc,&pc);
        vkCmdBindDescriptorSets(uf_vk_cb,VK_PIPELINE_BIND_POINT_COMPUTE,uf_vk_playout,0,1,&ds,0,0);
        uint64_t groups=(len+255)/256; if(!groups)groups=1;
        vkCmdDispatch(uf_vk_cb,(uint32_t)(groups>0x7fffffff?0x7fffffff:groups),1,1);
        if(use_dl){
          VkMemoryBarrier mb; memset(&mb,0,sizeof mb); mb.sType=VK_STRUCTURE_TYPE_MEMORY_BARRIER;
          mb.srcAccessMask=VK_ACCESS_SHADER_WRITE_BIT; mb.dstAccessMask=VK_ACCESS_TRANSFER_READ_BIT;
          vkCmdPipelineBarrier(uf_vk_cb,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,VK_PIPELINE_STAGE_TRANSFER_BIT,0,1,&mb,0,0,0,0);
          VkBufferCopy c={offs[n],offs[n],(VkDeviceSize)bufsz}; vkCmdCopyBuffer(uf_vk_cb,uf_vk_dl.buf,uf_vk_bpool.buf,1,&c);
        }
        vkEndCommandBuffer(uf_vk_cb);
        ok=uf_vk_submit_wait();
        uf_vk_inval(offs[n], (VkDeviceSize)bufsz);
        if(ok) memcpy(uf_data(r),uf_vk_bpool.mapped+offs[n],bufsz);
      }
      vkFreeDescriptorSets(uf_vk_dev,uf_vk_dpool,1,&ds);
    }
  }
  if(dbg)fprintf(stderr,"[vk-task] n=%llu in=%d dl=%d total=%.1fms\n",(unsigned long long)len,n,use_dl,(uf_nowd()-_t0)*1e3);
  UF_UNPROTECT();
  pthread_mutex_unlock(&uf_gpu_mu);
  return ok?uf_mkp(r):uf_gpu_decline();
}

/* ---- v13.2 fused region dispatch: one kernel launch for a straight-line
   elementwise chain in plain (non-weave) code, with up to 4 outputs. Inputs
   arrive explicitly as Cells (compiler-passed locals). Returns 1 and fills
   outs[0..nout) on success; 0 = declined, caller falls back to the fused CPU
   loop / per-op path. */
static int uf_region_try(int k,int n,int nout,Cell*ins,Cell*outs){
  if(uf_vk_broken||n<1||n>7||nout<1||nout>4||n+nout>8) return 0;
  Hdr* hs[7];
  for(int j=0;j<n;j++){
    if(!uf_numarr(ins[j])) return 0;
    hs[j]=(Hdr*)(void*)ins[j].i;
    if(hs[j]->ety!=1) return 0;
    if(hs[j]->len!=hs[0]->len) return 0;
  }
  uint64_t len=hs[0]->len;
  if(len<(uint64_t)uf_gpu_min()) return 0;
  /* Matrices must not take the elementwise fused kernel (mul is matmul). */
  for(int j=0;j<n;j++) if(hs[j]->tag==HT_MAT) return 0;
  /* Static first-run estimate: skip GPU when init has not happened and n
     is below the per-op arith floor (blackscholes N=2M stays CPU).
     Pinned vk<N> still launches. */
  if(uf_dev_is_auto() && !uf_vk_ready && len<(uint64_t)uf_gpu_arith_min()) return 0;
  uf_vk_init();
  if(!uf_vk_ready) return 0;
  if(k<0||k>=(int)(sizeof(uf_spv_all)/sizeof(uf_spv_all[0]))) return 0;
  pthread_mutex_lock(&uf_gpu_mu);
  if(!uf_vk_ensure_pipe(k)){ pthread_mutex_unlock(&uf_gpu_mu); return 0; }
  size_t bufsz=(size_t)len*8;
  VkDeviceSize offs[8]; VkDeviceSize cur=0;
  for(int j=0;j<n+nout;j++){ offs[j]=cur; cur=uf_vk_al(cur+(VkDeviceSize)bufsz); }
  int ok=uf_vk_pool_reserve(cur);
  int use_dl=ok&&uf_vk_dl_reserve(cur);
  VkBuffer cbuf=use_dl?uf_vk_dl.buf:uf_vk_bpool.buf;
  int dbg=getenv("NK_VK_DEBUG")!=0;
  double _t0=dbg?uf_nowd():0;
  double _tin=0,_tsub=0,_tout=0,_tm;
  Hdr* rs[4]; for(int j=0;j<nout;j++){ rs[j]=0; UF_PROTECT(&rs[j]); }
  if(ok){
    if(dbg)_tm=uf_nowd();
    for(int j=0;j<nout;j++) rs[j]=uf_arr_like(hs[0],len);
    for(int j=0;j<n;j++) memcpy(uf_vk_bpool.mapped+offs[j],uf_data(hs[j]),bufsz);
    uf_vk_flush(offs[0], (offs[n-1]+bufsz)-offs[0]);
    if(dbg){_tin=uf_nowd()-_tm;_tm=uf_nowd();}
    VkDescriptorSetAllocateInfo dsai; memset(&dsai,0,sizeof dsai); dsai.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO; dsai.descriptorPool=uf_vk_dpool; dsai.descriptorSetCount=1; dsai.pSetLayouts=&uf_vk_dsl;
    VkDescriptorSet ds;
    ok=vkAllocateDescriptorSets(uf_vk_dev,&dsai,&ds)==VK_SUCCESS;
    if(ok){
      VkWriteDescriptorSet w[8]; VkDescriptorBufferInfo bi[8];
      memset(w,0,sizeof w); memset(bi,0,sizeof bi);
      for(int j=0;j<n+nout;j++){ bi[j].buffer=cbuf; bi[j].offset=offs[j]; bi[j].range=bufsz;
        w[j].sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET; w[j].dstSet=ds; w[j].dstBinding=(uint32_t)j; w[j].descriptorCount=1; w[j].descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; w[j].pBufferInfo=&bi[j]; }
      vkUpdateDescriptorSets(uf_vk_dev,n+nout,w,0,0);
      struct UFPC pc; memset(&pc,0,sizeof pc); pc.n0=(int64_t)len;
      VkCommandBufferBeginInfo cbbi; memset(&cbbi,0,sizeof cbbi); cbbi.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
      ok=vkBeginCommandBuffer(uf_vk_cb,&cbbi)==VK_SUCCESS;
      if(ok){
        if(use_dl){
          for(int j=0;j<n;j++){ VkBufferCopy c={offs[j],offs[j],(VkDeviceSize)bufsz}; vkCmdCopyBuffer(uf_vk_cb,uf_vk_bpool.buf,uf_vk_dl.buf,1,&c); }
          VkMemoryBarrier mb; memset(&mb,0,sizeof mb); mb.sType=VK_STRUCTURE_TYPE_MEMORY_BARRIER;
          mb.srcAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT; mb.dstAccessMask=VK_ACCESS_SHADER_READ_BIT;
          vkCmdPipelineBarrier(uf_vk_cb,VK_PIPELINE_STAGE_TRANSFER_BIT,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,0,1,&mb,0,0,0,0);
        }
        vkCmdBindPipeline(uf_vk_cb,VK_PIPELINE_BIND_POINT_COMPUTE,uf_vk_pipe[k]);
        vkCmdPushConstants(uf_vk_cb,uf_vk_playout,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pc,&pc);
        vkCmdBindDescriptorSets(uf_vk_cb,VK_PIPELINE_BIND_POINT_COMPUTE,uf_vk_playout,0,1,&ds,0,0);
        uint64_t groups=(len+255)/256; if(!groups)groups=1;
        vkCmdDispatch(uf_vk_cb,(uint32_t)(groups>0x7fffffff?0x7fffffff:groups),1,1);
        if(use_dl){
          VkMemoryBarrier mb; memset(&mb,0,sizeof mb); mb.sType=VK_STRUCTURE_TYPE_MEMORY_BARRIER;
          mb.srcAccessMask=VK_ACCESS_SHADER_WRITE_BIT; mb.dstAccessMask=VK_ACCESS_TRANSFER_READ_BIT;
          vkCmdPipelineBarrier(uf_vk_cb,VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,VK_PIPELINE_STAGE_TRANSFER_BIT,0,1,&mb,0,0,0,0);
          for(int j=0;j<nout;j++){ int q=n+j; VkBufferCopy c={offs[q],offs[q],(VkDeviceSize)bufsz}; vkCmdCopyBuffer(uf_vk_cb,uf_vk_dl.buf,uf_vk_bpool.buf,1,&c); }
        }
        vkEndCommandBuffer(uf_vk_cb);
        ok=uf_vk_submit_wait();
        uf_vk_inval(offs[n], (offs[n+nout-1]+bufsz)-offs[n]);
        if(dbg){_tsub=uf_nowd()-_tm;_tm=uf_nowd();}
        if(ok) for(int j=0;j<nout;j++) memcpy(uf_data(rs[j]),uf_vk_bpool.mapped+offs[n+j],bufsz);
        if(dbg)_tout=uf_nowd()-_tm;
      }
      vkFreeDescriptorSets(uf_vk_dev,uf_vk_dpool,1,&ds);
    }
  }
  if(dbg)fprintf(stderr,"[vk-region] n=%llu in=%d out=%d dl=%d in=%.1fms sub=%.1fms out=%.1fms total=%.1fms\n",(unsigned long long)len,n,nout,use_dl,_tin*1e3,_tsub*1e3,_tout*1e3,(uf_nowd()-_t0)*1e3);
  for(int j=0;j<nout;j++) UF_UNPROTECT();
  pthread_mutex_unlock(&uf_gpu_mu);
  if(ok){ for(int j=0;j<nout;j++){ outs[j].tag=T_PTR; outs[j].i=(int64_t)(void*)rs[j]; } return 1; }
  return 0;
}

/* ---- op shims (return a T_INT 0 Cell when declined; caller falls back) ---- */
static Cell uf_gpu_decline(void){ Cell c; c.tag=T_INT; c.i=0; return c; }
static Cell uf_gpu_arith(Cell a,Cell b,int op){
  Hdr*ha=uf_numarr(a)?(Hdr*)(void*)a.i:0;
  Hdr*hb=uf_numarr(b)?(Hdr*)(void*)b.i:0;
  static const char* eb[4]={"eadd","esub","emul","ediv"};
  static const char* bb_[4]={"badd","bsub","bmul","bdiv"};
  if(ha&&hb){
    if(ha->ety!=1||hb->ety!=1) return uf_gpu_decline();
    uint64_t n=ha->len;
    struct UFPC pc; memset(&pc,0,sizeof pc); pc.n0=(int64_t)n;
    if(op==2&&ha->tag==HT_MAT&&hb->tag==HT_MAT){
      uint64_t ra=ha->esz, ca=ha->len/ra, rb=hb->esz, cb=hb->len/rb;
      if(ca!=rb) return uf_gpu_decline(); /* let CPU produce the die() */
      pc.n1=(int64_t)ra; pc.n2=(int64_t)ca; pc.n3=(int64_t)cb;
      Hdr*r=uf_mat_new(ra,cb,1); UF_PROTECT(&r);
      int k=uf_spv_index("matmul");
      int ok=k>=0&&uf_vk_run(k,ra*cb,uf_data(ha),(size_t)n*8,uf_data(hb),(size_t)hb->len*8,uf_data(r),(size_t)(ra*cb)*8,pc);
      UF_UNPROTECT();
      return ok?uf_mkp(r):uf_gpu_decline();
    }
    if(op==2&&ha->tag==HT_MAT&&hb->tag!=HT_MAT){
      uint64_t ra=ha->esz, ca=ha->len/ra;
      if(hb->len!=ca) return uf_gpu_decline();
      pc.n1=(int64_t)ra; pc.n2=(int64_t)ca;
      Hdr*r=uf_arr_like(hb,ra); UF_PROTECT(&r);
      int k=uf_spv_index("matvec");
      int ok=k>=0&&uf_vk_run(k,ra,uf_data(ha),(size_t)n*8,uf_data(hb),(size_t)hb->len*8,uf_data(r),(size_t)ra*8,pc);
      UF_UNPROTECT();
      return ok?uf_mkp(r):uf_gpu_decline();
    }
    if(op==2&&ha->tag!=HT_MAT&&hb->tag==HT_MAT){
      uint64_t rb=hb->esz, cb2=hb->len/rb;
      if(ha->len!=rb) return uf_gpu_decline();
      pc.n1=(int64_t)rb; pc.n2=(int64_t)cb2;
      Hdr*r=uf_arr_like(ha,cb2); UF_PROTECT(&r);
      int k=uf_spv_index("matvec"); /* reversed: computes b·a via same kernel */
      int ok=k>=0&&uf_vk_run(k,cb2,uf_data(hb),(size_t)hb->len*8,uf_data(ha),(size_t)n*8,uf_data(r),(size_t)cb2*8,pc);
      UF_UNPROTECT();
      return ok?uf_mkp(r):uf_gpu_decline();
    }
    if(ha->len!=hb->len) return uf_gpu_decline();
    Hdr*r=uf_arr_like(ha,n); UF_PROTECT(&r);
    int k=uf_spv_index(eb[op]);
    int ok=k>=0&&uf_vk_run(k,n,uf_data(ha),(size_t)n*8,uf_data(hb),(size_t)n*8,uf_data(r),(size_t)n*8,pc);
    UF_UNPROTECT();
    return ok?uf_mkp(r):uf_gpu_decline();
  }
  /* broadcast */
  Hdr*h=ha?ha:hb;
  Cell sc=ha?b:a;
  if(!h||h->ety!=1||sc.tag==T_PTR) return uf_gpu_decline();
  uint64_t n=h->len;
  struct UFPC pc; memset(&pc,0,sizeof pc); pc.n0=(int64_t)n; pc.s=uf_f(sc); pc.rev=ha?0:1;
  Hdr*r=uf_arr_like(h,n); UF_PROTECT(&r);
  int k=uf_spv_index(bb_[op]);
  int ok=k>=0&&uf_vk_run(k,n,uf_data(h),(size_t)n*8,NULL,0,uf_data(r),(size_t)n*8,pc);
  UF_UNPROTECT();
  return ok?uf_mkp(r):uf_gpu_decline();
}
static int uf_gpu_reduce(const double*d,uint64_t n,const char*kname,double*out){
  uint64_t groups=(n+255)/256;
  double* partials=(double*)malloc(groups*sizeof(double));
  struct UFPC pc; memset(&pc,0,sizeof pc); pc.n0=(int64_t)n;
  int k=uf_spv_index(kname);
  if(k<0||!uf_vk_run(k,n,d,n*8,NULL,0,partials,groups*8,pc)){ free(partials); return 0; }
  /* second pass: reduce the partials (single group when small) */
  double final=0;
  if(groups==1){ final=partials[0]; }
  else if(groups<=256){
    double one; pc.n0=(int64_t)groups;
    if(!uf_vk_run(k,groups,partials,groups*8,NULL,0,&one,8,pc)){ free(partials); return 0; }
    final=one;
  } else {
    /* chain until one group remains */
    uint64_t g=groups;
    double* cur=partials;
    while(g>1){
      uint64_t ng=(g+255)/256;
      double* nxt=(double*)malloc(ng*sizeof(double));
      pc.n0=(int64_t)g;
      if(!uf_vk_run(k,g,cur,g*8,NULL,0,nxt,ng*8,pc)){ free(cur!=partials?cur:0); free(nxt); if(cur!=partials)free(partials); return 0; }
      if(cur!=partials) free(cur);
      cur=nxt; g=ng;
    }
    final=cur[0];
    if(cur!=partials) free(cur);
    free(partials);
  }
  if(groups==1) free(partials);
  *out=final;
  return 1;
}
#endif /* UF_GPU */

/* ================= polymorphic matrices (v13.1) =================
   A matrix is HT_MAT with len=rows*cols (row-major flat), esz=rows,
   cols=len/rows; element size derives from ety (byte=1, else 8) like Str.
   `mul` on two matrices is matmul; matrix·vector is matvec; add/sub/div are
   elementwise (shape-checked) or scalar-broadcast. */
static int uf_numarr(Cell c){ if(c.tag!=T_PTR||!c.i)return 0; Hdr*h=uf_gc_find((void*)c.i); return h&&(h->tag==HT_ARR||h->tag==HT_TENSOR||h->tag==HT_MAT); }
static Hdr* uf_mat_new(uint64_t rows,uint64_t cols,uint64_t ety){
  if(!rows||!cols)die("matrix: zero dimension");
  uint64_t esz=(ety==3)?1:8;
  Hdr*h=(Hdr*)uf_gc_alloc(sizeof(Hdr)+(size_t)rows*cols*esz,0);
  h->tag=HT_MAT; h->len=rows*cols; h->esz=rows; h->ety=ety;
  memset(h->data,0,(size_t)rows*cols*esz);
  return h;
}
static void uf_dims(Hdr*m,char*buf,size_t cap){ snprintf(buf,cap,"%llux%llu",(unsigned long long)m->esz,(unsigned long long)(m->len/m->esz)); }
/* matmul: (ra x ca)·(ca x cb); float fast path, else int64 */
static Hdr* uf_matmul(Hdr*ha,Hdr*hb){
  uint64_t ra=ha->esz, ca=ha->len/ra, rb=hb->esz, cb=hb->len/rb;
  if(ca!=rb){ char da[32],db[32]; uf_dims(ha,da,sizeof da); uf_dims(hb,db,sizeof db); char m[128]; snprintf(m,sizeof m,"mul: matmul inner dims mismatch (%s · %s)",da,db); die(m); }
  uint64_t ety=(ha->ety==1||hb->ety==1)?1:((ha->ety==3&&hb->ety==3)?3:0);
  Hdr*r=uf_mat_new(ra,cb,ety); UF_PROTECT(&r);
  if(ety==1){
    const double*A=(const double*)uf_data(ha); const double*B=(const double*)uf_data(hb); double*C=(double*)uf_data(r);
    /* 64×64 tiles keep A/B/C panels in L1/L2; the old ikj loop is fine for
       N≤512 but thrashes L3 at the GPU-table N=2048 size. */
    const uint64_t BS=64;
    for(uint64_t i0=0;i0<ra;i0+=BS){
      uint64_t i1=i0+BS; if(i1>ra)i1=ra;
      for(uint64_t k0=0;k0<ca;k0+=BS){
        uint64_t k1=k0+BS; if(k1>ca)k1=ca;
        for(uint64_t j0=0;j0<cb;j0+=BS){
          uint64_t j1=j0+BS; if(j1>cb)j1=cb;
          for(uint64_t i=i0;i<i1;i++){
            double*Ci=C+i*cb;
            for(uint64_t k=k0;k<k1;k++){
              double aik=A[i*ca+k]; if(aik==0.0)continue;
              const double*Bk=B+k*cb;
              for(uint64_t j=j0;j<j1;j++) Ci[j]+=aik*Bk[j];
            }
          }
        }
      }
    }
  } else {
    const int64_t*A=(const int64_t*)uf_data(ha); const int64_t*B=(const int64_t*)uf_data(hb); int64_t*C=(int64_t*)uf_data(r);
    for(uint64_t i=0;i<ra;i++) for(uint64_t k=0;k<ca;k++){ int64_t aik=A[i*ca+k]; if(!aik)continue; const int64_t*Bk=B+k*cb; int64_t*Ci=C+i*cb; for(uint64_t j=0;j<cb;j++) Ci[j]+=aik*Bk[j]; }
  }
  UF_UNPROTECT(); return r;
}
/* matrix (ra x ca) · vector (ca) -> vector (ra) */
static Hdr* uf_matvec(Hdr*m,Hdr*v){
  uint64_t r=m->esz, c=m->len/r;
  if(v->len!=c){ char dm[32]; uf_dims(m,dm,sizeof dm); char msg[128]; snprintf(msg,sizeof msg,"mul: matvec dims mismatch (%s · len %llu)",dm,(unsigned long long)v->len); die(msg); }
  uint64_t ety=(m->ety==1||v->ety==1)?1:0;
  Hdr*res=uf_arr_like(ety==1?(m->ety==1?m:v):(m->ety==0?m:v), r);
  /* uf_arr_like copies esz from the prototype — for a MAT prototype esz is
     rows, wrong for a 1-D result; fix up */
  if(res->tag==HT_MAT){ res->tag=HT_ARR; res->esz=(ety==3)?1:8; }
  UF_PROTECT(&res);
  for(uint64_t i=0;i<r;i++){ double s=0; for(uint64_t k=0;k<c;k++) s+=uf_el(m,i*c+k)*uf_el(v,k); uf_put_el(res,i,s); }
  UF_UNPROTECT(); return res;
}
/* vector (ra) · matrix (ra x cb) -> vector (cb) */
static Hdr* uf_vecmat(Hdr*v,Hdr*m){
  uint64_t r=m->esz, c=m->len/r;
  if(v->len!=r){ char dm[32]; uf_dims(m,dm,sizeof dm); char msg[128]; snprintf(msg,sizeof msg,"mul: vecmat dims mismatch (len %llu · %s)",(unsigned long long)v->len,dm); die(msg); }
  uint64_t ety=(m->ety==1||v->ety==1)?1:0;
  Hdr*res=uf_arr_like(ety==1?(m->ety==1?m:v):(m->ety==0?m:v), c);
  if(res->tag==HT_MAT){ res->tag=HT_ARR; res->esz=(ety==3)?1:8; }
  UF_PROTECT(&res);
  for(uint64_t j=0;j<c;j++){ double s=0; for(uint64_t i=0;i<r;i++) s+=uf_el(v,i)*uf_el(m,i*c+j); uf_put_el(res,j,s); }
  UF_UNPROTECT(); return res;
}
/* polymorphic + - * / for array/matrix/scalar operand mixes */
static Cell uf_poly_arith(Cell a,Cell b,int op,const char*opn){
  Hdr*ha=uf_numarr(a)?(Hdr*)(void*)a.i:0;
  Hdr*hb=uf_numarr(b)?(Hdr*)(void*)b.i:0;
  /* operands are popped from the ds before we allocate the result — keep them
     rooted or a GC triggered by the result allocation would sweep them */
  UF_PROTECT((void**)(void*)&a.i); UF_PROTECT((void**)(void*)&b.i);
#ifdef NK_GPU
  if(ha&&hb&&ha->ety==1&&hb->ety==1){
    uint64_t work = (op==2&&ha->tag==HT_MAT&&hb->tag==HT_MAT) ? (ha->esz*(hb->len/hb->esz))
      : (op==2&&ha->tag==HT_MAT) ? ha->esz
      : (op==2&&hb->tag==HT_MAT) ? (hb->len/hb->esz)
      : (ha->len>hb->len?ha->len:hb->len);
    long _amin;
    if(op==2&&ha->tag==HT_MAT&&hb->tag==HT_MAT){
      if(uf_dev_is_auto()){
        uint64_t ra=ha->esz, ca=ha->len/ra, cb=hb->len/hb->esz;
        work = ra*ca*cb; /* static FLOP estimate — never a timed trial */
        _amin = uf_gpu_matmul_min();
      } else {
        _amin = uf_gpu_min(); /* --device vk<N>: honor the pin */
      }
    } else if(op==2&&(ha->tag==HT_MAT||hb->tag==HT_MAT)) {
      _amin = uf_gpu_min();
    } else {
      _amin = uf_gpu_arith_min();
    }
    if(work>=(uint64_t)_amin){
      Cell g=uf_gpu_arith(a,b,op);
      if(!(g.tag==T_INT&&g.i==0)){ UF_UNPROTECT(); UF_UNPROTECT(); return g; }
    }
  }
  if((ha&&!hb&&ha->ety==1&&ha->len>=(uint64_t)uf_gpu_arith_min())||(hb&&!ha&&hb->ety==1&&hb->len>=(uint64_t)uf_gpu_arith_min())){
    Cell g=uf_gpu_arith(a,b,op);
    if(!(g.tag==T_INT&&g.i==0)){ UF_UNPROTECT(); UF_UNPROTECT(); return g; }
  }
#endif
  if(ha&&hb){
    if(op==2&&ha->tag==HT_MAT&&hb->tag==HT_MAT){ Hdr*r=uf_matmul(ha,hb); UF_UNPROTECT(); UF_UNPROTECT(); return uf_mkp(r); }
    if(op==2&&ha->tag==HT_MAT&&hb->tag!=HT_MAT){ Hdr*r=uf_matvec(ha,hb); UF_UNPROTECT(); UF_UNPROTECT(); return uf_mkp(r); }
    if(op==2&&ha->tag!=HT_MAT&&hb->tag==HT_MAT){ Hdr*r=uf_vecmat(ha,hb); UF_UNPROTECT(); UF_UNPROTECT(); return uf_mkp(r); }
    /* elementwise (arrays, or matrices of identical shape) */
    if(ha->len!=hb->len){ char m[96]; snprintf(m,sizeof m,"%s: length mismatch (%llu vs %llu)",opn,(unsigned long long)ha->len,(unsigned long long)hb->len); die(m); }
    if(ha->tag==HT_MAT&&hb->tag==HT_MAT&&ha->esz!=hb->esz){ char da[32],db[32]; uf_dims(ha,da,sizeof da); uf_dims(hb,db,sizeof db); char m[128]; snprintf(m,sizeof m,"%s: matrix shape mismatch (%s vs %s)",opn,da,db); die(m); }
    uint64_t n=ha->len; Hdr*r=uf_arr_like(ha,n); UF_PROTECT(&r);
    /* v13.2 f64 fast path: raw typed loops, no per-element ety/tag dispatch
       (mirrors the matmul path) — vectorizable by cc at -O2 */
    if(ha->ety==1&&hb->ety==1){
      const double*A=(const double*)uf_data(ha); const double*B=(const double*)uf_data(hb); double*R=(double*)uf_data(r);
      if(op==0){ for(uint64_t i=0;i<n;i++)R[i]=A[i]+B[i]; }
      else if(op==1){ for(uint64_t i=0;i<n;i++)R[i]=A[i]-B[i]; }
      else if(op==2){ for(uint64_t i=0;i<n;i++)R[i]=A[i]*B[i]; }
      else { for(uint64_t i=0;i<n;i++){ double y=B[i]; if(y==0.0){ char m[64]; snprintf(m,sizeof m,"%s: zero divisor",opn); UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT(); die(m); } R[i]=A[i]/y; } }
      UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT(); return uf_mkp(r);
    }
    if(op==3) for(uint64_t i=0;i<n;i++) if(uf_el(hb,i)==0.0){ char m[64]; snprintf(m,sizeof m,"%s: zero divisor",opn); UF_UNPROTECT(); die(m); }
    for(uint64_t i=0;i<n;i++){
      double x=uf_el(ha,i),y=uf_el(hb,i);
      double v = op==0?x+y : op==1?x-y : op==2?x*y : x/y;
      uf_put_el(r,i,v);
    }
    UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT(); return uf_mkp(r);
  }
  /* scalar broadcast */
  if(ha&&!hb){
    uint64_t n=ha->len; Hdr*r=uf_arr_like(ha,n); UF_PROTECT(&r);
    if(ha->ety==1&&ha->tag!=HT_MAT){
      /* v13.2 f64 fast path (raw typed loop) */
      const double*A=(const double*)uf_data(ha); double*R=(double*)uf_data(r); double d=uf_f(b);
      if(op==3&&d==0.0){ UF_UNPROTECT(); UF_UNPROTECT(); die("div: zero divisor"); }
      if(op==0){ for(uint64_t i=0;i<n;i++)R[i]=A[i]+d; }
      else if(op==1){ for(uint64_t i=0;i<n;i++)R[i]=A[i]-d; }
      else if(op==2){ for(uint64_t i=0;i<n;i++)R[i]=A[i]*d; }
      else { for(uint64_t i=0;i<n;i++)R[i]=A[i]/d; }
      UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT(); return uf_mkp(r);
    }
    if(ha->ety==1||b.tag==T_FLOAT){ double d=uf_f(b); if(op==3&&d==0.0){ UF_UNPROTECT(); die("div: zero divisor"); } for(uint64_t i=0;i<n;i++){ double x=uf_el(ha,i); uf_put_el(r,i, op==0?x+d : op==1?x-d : op==2?x*d : x/d); } }
    else { int64_t d=b.i; if(op==3&&d==0){ UF_UNPROTECT(); die("div: zero divisor"); } for(uint64_t i=0;i<n;i++){ double x=uf_el(ha,i); uf_put_el(r,i, op==0?x+(double)d : op==1?x-(double)d : op==2?x*(double)d : x/(double)d); } }
    UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT(); return uf_mkp(r);
  }
  if(hb&&!ha){
    uint64_t n=hb->len; Hdr*r=uf_arr_like(hb,n); UF_PROTECT(&r);
    if(hb->ety==1&&hb->tag!=HT_MAT){
      /* v13.2 f64 fast path (raw typed loop, scalar on the left) */
      const double*B=(const double*)uf_data(hb); double*R=(double*)uf_data(r); double d=uf_f(a);
      if(op==3&&d==0.0){ UF_UNPROTECT(); UF_UNPROTECT(); die("div: zero divisor"); }
      if(op==0){ for(uint64_t i=0;i<n;i++)R[i]=d+B[i]; }
      else if(op==1){ for(uint64_t i=0;i<n;i++)R[i]=d-B[i]; }
      else if(op==2){ for(uint64_t i=0;i<n;i++)R[i]=d*B[i]; }
      else { for(uint64_t i=0;i<n;i++)R[i]=d/B[i]; }
      UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT(); return uf_mkp(r);
    }
    if(hb->ety==1||a.tag==T_FLOAT){ double d=uf_f(a); if(op==3&&d==0.0){ UF_UNPROTECT(); die("div: zero divisor"); } for(uint64_t i=0;i<n;i++){ double y=uf_el(hb,i); uf_put_el(r,i, op==0?d+y : op==1?d-y : op==2?d*y : d/y); } }
    else { int64_t d=a.i; if(op==3&&d==0){ UF_UNPROTECT(); die("div: zero divisor"); } for(uint64_t i=0;i<n;i++){ double y=uf_el(hb,i); uf_put_el(r,i, op==0?(double)d+y : op==1?(double)d-y : op==2?(double)d*y : (double)d/y); } }
    UF_UNPROTECT(); UF_UNPROTECT(); UF_UNPROTECT(); return uf_mkp(r);
  }
  die("poly arith: no array operand");
}

/* TRANSPOSE: mat -> mat' (rows/cols swapped) */
static void op_transpose(Ctx*cx){
  Cell h=pop(cx); Hdr*a=uf_handle(h,"transpose");
  if(a->tag!=HT_MAT)die("transpose: not a matrix (build one with [rows cols] type tensor)");
  uint64_t r=a->esz,c=a->len/r;
  UF_PROTECT((void**)(void*)&h.i);
#ifdef NK_GPU
  if(a->ety==1&&(r*c)>=(uint64_t)uf_gpu_min()){
    Hdr*g=uf_mat_new(c,r,1); UF_PROTECT(&g);
    struct UFPC pc; memset(&pc,0,sizeof pc); pc.n1=(int64_t)r; pc.n2=(int64_t)c;
    int k=uf_spv_index("transpose");
    int ok=k>=0&&uf_vk_run(k,r*c,uf_data(a),(size_t)(r*c)*8,NULL,0,uf_data(g),(size_t)(r*c)*8,pc);
    UF_UNPROTECT();
    if(ok){ UF_UNPROTECT(); pushp(cx,g); return; }
    /* fall through to CPU */
  }
#endif
  Hdr*t=uf_mat_new(c,r,a->ety); UF_PROTECT(&t);
  for(uint64_t i=0;i<r;i++) for(uint64_t j=0;j<c;j++) uf_put_el(t,j*r+i,uf_el(a,i*c+j));
  UF_UNPROTECT(); UF_UNPROTECT(); pushp(cx,t);
}
static Hdr* uf_vcheck(Cell h,const char*op){ Hdr*a=uf_handle(h,op); if(a->tag!=HT_ARR&&a->tag!=HT_TENSOR&&a->tag!=HT_MAT)die("vector op: not an arr"); return a; }
/* scalar arr ops: arr scalar -> arr' */
#define UF_VSOP(NAME,EXPR,RAWEXPR,ZERO_DIE) \
static void NAME(Ctx*cx){ Cell s=pop(cx),h=pop(cx); Hdr*a=uf_vcheck(h,#NAME); \
  UF_PROTECT((void**)(void*)&h.i); \
  uint64_t n=a->len; Hdr*r=uf_arr_like(a,n); UF_PROTECT(&r); \
  if(a->ety==1&&a->tag!=HT_MAT){ const double*A=(const double*)uf_data(a); double*R=(double*)uf_data(r); double d=uf_f(s); if(ZERO_DIE&&d==0.0)die(#NAME ": zero scalar"); for(uint64_t i=0;i<n;i++)R[i]=(RAWEXPR); } \
  else if(s.tag==T_FLOAT){ double d=uf_f(s); if(ZERO_DIE&&d==0.0)die(#NAME ": zero scalar"); for(uint64_t i=0;i<n;i++)uf_put_el(r,i,(EXPR)); } \
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
