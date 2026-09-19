// link: cc <this-file>.c -lpthread -lm

#include <stdint.h>
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
static void uflux_run(Ctx*cx, long pc);
static _Thread_local const void* uf_entry_addr;
static void uf_call_addr(Ctx*cx, const void* a, long frame, long entry_pc, long nargs){
  if(cx->csp>=cx->ccap){char _b[128];snprintf(_b,sizeof(_b),"call stack overflow in %s (csp=%ld, cap=%ld)",uf_cur_op,cx->csp,cx->ccap);die(_b);}
  /* save the pre-argument data-stack pointer: the callee's param pops guard
     against it, and its RET drains back to it */
  long _sp0 = cx->sp - nargs; if(_sp0 < 0) _sp0 = 0;
  cx->rsps[cx->csp]=_sp0; cx->cs[cx->csp++]=0; cx->local_frames[cx->local_fsp++]=cx->local_base; cx->call_pcs[cx->call_csp++]=entry_pc; cx->local_base+=frame; uf_entry_addr=a; uflux_run(cx,-1); cx->local_base=cx->local_frames[--cx->local_fsp]; cx->call_csp--; }
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
  fprintf(stderr,"\n--- uflux crash dump ---\n");
  fprintf(stderr,"  call stack:\n");
  for(long i=cx->call_csp-1;i>=0;i--){
    long pc=cx->call_pcs[i];
    const char* nm = (pc>=0&&pc<uf_labnames_n)?uf_labnames[pc]:0;
    fprintf(stderr,"    #%ld  %s (pc=%ld)\n", cx->call_csp-1-i, nm?nm:"<unknown>", pc);
  }
  fprintf(stderr,"  locals:\n");
  for(long i=cx->call_csp-1;i>=0;i--){
    long pc=cx->call_pcs[i];
    /* local_frames has one extra entry (the uflux_run entry frame) that
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
  fprintf(stderr,"  globals:\n");
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
  fprintf(stderr,"uflux: %s\n",m); exit(1);
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
/* tmp roots for builder ops (in-progress containers while they grow) */
#define UF_MAXTMP 1024
static void*** uf_tmp_roots = 0; static _Atomic int uf_ntmp;
#define UF_PROTECT(pp) do{ int _i=atomic_fetch_add(&uf_ntmp,1); if(_i<UF_MAXTMP)uf_tmp_roots[_i]=(void**)(pp); }while(0)
#define UF_UNPROTECT() atomic_fetch_sub(&uf_ntmp,1)
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
  int nc = uf_nctxs;
  for(int i=0;i<nc;i++){ Ctx* c=uf_ctxs[i]; for(long s=0;s<c->sp;s++) uf_mark_cell(c->ds[s]);
    /* v11: mark active local-variable frame */
    for(long s=0;s<c->local_base;s++) uf_mark_cell(c->locals[s]);
  }
  int nt = uf_ntmp; if(nt>UF_MAXTMP)nt=UF_MAXTMP;
  for(int i=0;i<nt;i++){ void** pp=uf_tmp_roots[i]; if(pp&&*pp) uf_mark_ptr(*pp); }
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
  const char* e=getenv("UF_GC_THRESHOLD");
  if(e&&*e){ uint64_t v=strtoull(e,0,0); if(v) uf_gc_threshold=v; }
  uf_tmp_roots=(void***)calloc(UF_MAXTMP,sizeof(void**));
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
int64_t uf_argc=0; void* uf_argv=0; /* program args, reachable via EXTERN "uf_argc"/"uf_argv" + LOADX, or ARGV */

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
#ifdef UF_GPU
struct UFPC { int64_t n0,n1,n2,n3; double s; int64_t rev; };
static long uf_gpu_min(void);
static int uf_spv_index(const char*name);
static int uf_vk_run(int k,uint64_t n,const void*A,size_t asz,const void*B,size_t bsz,void*R,size_t rsz,struct UFPC pc);
#endif
static inline int uf_rawptr(Cell c){ return c.tag==T_PTR&&c.i&&!uf_gc_find((void*)c.i); }
static inline Cell uf_cadd(Cell a,Cell b){ if(a.tag==T_PTR||b.tag==T_PTR){ if(uf_numarr(a)||uf_numarr(b))return uf_poly_arith(a,b,0,"add"); if(a.tag==T_INT&&uf_rawptr(b))return uf_mkp((void*)(a.i+b.i)); if(uf_rawptr(a)&&b.tag==T_INT)return uf_mkp((void*)(a.i+b.i)); } double x=uf_to_number(a),y=uf_to_number(b); if(isnan(x)||isnan(y))return uf_mkf(NAN); if(a.tag==T_INT&&b.tag==T_INT)return uf_mki(a.i+b.i); return uf_mkf(x+y); }
static inline Cell uf_csub(Cell a,Cell b){ if(a.tag==T_PTR||b.tag==T_PTR){ if(uf_numarr(a)||uf_numarr(b))return uf_poly_arith(a,b,1,"sub"); if(uf_rawptr(a)&&b.tag==T_INT)return uf_mkp((void*)(a.i-b.i)); if(a.tag==T_PTR&&b.tag==T_PTR&&uf_rawptr(a)&&uf_rawptr(b))return uf_mki(a.i-b.i); } double x=uf_to_number(a),y=uf_to_number(b); if(isnan(x)||isnan(y))return uf_mkf(NAN); if(a.tag==T_INT&&b.tag==T_INT)return uf_mki(a.i-b.i); return uf_mkf(x-y); }
static inline Cell uf_cmul(Cell a,Cell b){ if(a.tag==T_PTR||b.tag==T_PTR){ if(uf_numarr(a)||uf_numarr(b))return uf_poly_arith(a,b,2,"mul"); } double x=uf_to_number(a),y=uf_to_number(b); if(isnan(x)||isnan(y))return uf_mkf(NAN); if(a.tag==T_INT&&b.tag==T_INT)return uf_mki(a.i*b.i); return uf_mkf(x*y); }
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
#ifdef UF_GPU
  if(a.tag==T_PTR&&a.i){ Hdr*h=uf_gc_find((void*)a.i);
    if(h&&uf_is_arrish(h)&&h->ety==1&&h->len>=(uint64_t)uf_gpu_min()){
      struct UFPC pc; memset(&pc,0,sizeof pc); pc.n0=(int64_t)h->len;
      Hdr*r=uf_arr_like(h,h->len); UF_PROTECT(&r);
      int k=uf_spv_index("esqrt");
      int ok=k>=0&&uf_vk_run(k,h->len,uf_data(h),(size_t)h->len*8,NULL,0,uf_data(r),(size_t)h->len*8,pc);
      UF_UNPROTECT();
      if(ok){ pushp(cx,r); return; }
    } }
#endif
  if(uf_numarr(a)){ Hdr*h=(Hdr*)(void*)a.i; uint64_t n=h->len; Hdr*r=uf_arr_like(h,n); UF_PROTECT(&r);
    for(uint64_t i=0;i<n;i++) uf_put_el(r,i,sqrt(uf_el(h,i)));
    UF_UNPROTECT(); pushp(cx,r); return; }
  pushc(cx,uf_mkf(sqrt(uf_to_number(a)))); }
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
    Hdr*h=(Hdr*)uf_gc_alloc(sizeof(Hdr)+(size_t)len*(size_t)esz,align); h->tag=tag; h->len=len; h->esz=(uint64_t)esz; h->ety=(uint64_t)ty;
    for(uint64_t i=0;i<len;i++){
      Cell c=d->data[i];
      if(ty==3) ((uint8_t*)h->data)[i]=(uint8_t)uf_i(c);
      else if(ty==1) ((double*)h->data)[i]=uf_f(c);
      else ((int64_t*)h->data)[i]=uf_i(c);
    }
    pushp(cx,h); return;
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
          Hdr*h=uf_mat_new((uint64_t)rows,(uint64_t)cols,ety); UF_PROTECT(&h);
          for(uint64_t i=0;i<(uint64_t)(rows*cols);i++){
            Cell c=d->data[i+2];
            char*dt=uf_data(h);
            if(ety==3)((uint8_t*)dt)[i]=(uint8_t)uf_i(c);
            else if(ety==1)((double*)dt)[i]=uf_f(c);
            else ((int64_t*)dt)[i]=uf_i(c);
          }
          (void)eszb;
          UF_UNPROTECT(); pushp(cx,h); return;
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
  size_t sz=sizeof(Hdr)+nb; Hdr*n=(Hdr*)uf_gc_alloc(sz,a->tag==HT_TENSOR?64:0);
  memcpy(n,a,sz); n->gc_next=0; n->gc_flags=((uint64_t)atomic_fetch_add(&uf_gc_seq,1))<<GCF_SEQSHIFT;
  pushp(cx,n);
}
static void op_cast(Ctx*cx){ Cell id=pop(cx); Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); int64_t tk=(a->tag==HT_OBJ)?1000+(int64_t)a->len:(int64_t)a->tag; if(tk!=id.i)die("CAST: type mismatch"); pushc(cx,h); }

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
  if(k.tag==T_PTR&&k.i){ Hdr*h=uf_gc_find((void*)k.i); if(h){ if(h->tag==HT_STR)return uf_fnv(uf_sbytes((Str*)h),h->len); return uf_fnv(&k.i,8); } return uf_fnv((void*)k.i,strlen((char*)k.i)); }
  return uf_fnv(&k.i,8);
}
static int map_keyeq(Cell a,Cell b){
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
  uint64_t ncap=m->cap*2; Cell*ok=m->keys,*ov=m->vals; unsigned char*os=m->st; uint64_t ocap=m->cap;
  m->cap=ncap; m->keys=(Cell*)uf_alloc(ncap*sizeof(Cell),0); m->vals=(Cell*)uf_alloc(ncap*sizeof(Cell),0); m->st=(unsigned char*)calloc(ncap,1); m->len=0;
  for(uint64_t i=0;i<ocap;i++) if(os[i]==1) map_put_raw(m,ok[i],ov[i]);
  free(ok); free(ov); free(os);
}
static Map* uf_map_new(void){ Map*m=(Map*)uf_gc_alloc(sizeof(Map),0); m->tag=HT_MAP; m->len=0; m->cap=16; m->keys=(Cell*)uf_alloc(16*sizeof(Cell),0); m->vals=(Cell*)uf_alloc(16*sizeof(Cell),0); m->st=(unsigned char*)calloc(16,1); return m; }
static void map_put(Map*m,Cell k,Cell v){ if((m->len+1)*10>=m->cap*7) map_grow(m); map_put_raw(m,k,v); }
static int map_get(Map*m,Cell k,Cell*out){
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

static Hdr* uf_handle(Cell h,const char* op){ if(h.tag!=T_PTR||!h.i)die("handle is null"); Hdr*a=uf_gc_find((void*)h.i); if(!a)die("not a managed handle"); (void)op; return a; }
/* GET: h k -> v */
static inline void op_get(Ctx*cx){
  Cell k=pop(cx),h=pop(cx); Hdr*a=uf_handle(h,"GET");
  switch(a->tag){
    case HT_MAP: { Map*m=(Map*)a; Cell v; if(!map_get(m,k,&v))die("GET: missing key"); pushc(cx,v); return; }
    case HT_DYN: case HT_ARR: case HT_TENSOR: case HT_MAT: pushc(cx,uf_cidx(h,k.i)); return;
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
  if(ta==HT_DYN||tb==HT_DYN){
    if(ta!=HT_DYN||tb!=HT_DYN)die("CAT: list/str mismatch");
    Dyn*x=(Dyn*)ha,*y=(Dyn*)hb; Dyn*r=uf_dyn_new(x->len+y->len); UF_PROTECT(&r);
    for(uint64_t i=0;i<x->len;i++)uf_dyn_push(&r,x->data[i]);
    for(uint64_t i=0;i<y->len;i++)uf_dyn_push(&r,y->data[i]);
    UF_UNPROTECT(); pushp(cx,r); return;
  }
  if((ta==HT_ARR||ta==HT_TENSOR)||(tb==HT_ARR||tb==HT_TENSOR)){
    if((ta!=HT_ARR&&ta!=HT_TENSOR)||(tb!=HT_ARR&&tb!=HT_TENSOR))die("CAT: arr/str mismatch");
    if(ha->ety!=hb->ety)die("CAT: arr element-type mismatch");
    uint64_t n=ha->len+hb->len; Hdr*r=(Hdr*)uf_gc_alloc(sizeof(Hdr)+n*ha->esz,0);
    UF_PROTECT(&r);
    r->tag=HT_ARR; r->len=n; r->esz=ha->esz; r->ety=ha->ety;
    memcpy(r->data,ha->data,ha->len*ha->esz); memcpy(r->data+ha->len*ha->esz,hb->data,hb->len*hb->esz);
    UF_UNPROTECT(); pushp(cx,r); return;
  }
  { const char* x=uf_sptr(a),*y=uf_sptr(b); size_t la=strlen(x),lb=strlen(y);
    Str* r=(Str*)uf_gc_alloc(sizeof(Str)+la+lb+1,0); r->tag=HT_STR; r->esz=1; r->len=la+lb; r->mlen=0;
    memcpy(r->data,x,la); memcpy(r->data+la,y,lb+1); pushp(cx,r); }
}
/* SLICE: seq a b -> seq' (tag-dispatched; Python slice semantics) */
static void op_slice(Ctx*cx){
  Cell b=pop(cx),a=pop(cx),st=pop(cx);
  Hdr*h=st.tag==T_PTR&&st.i?uf_gc_find((void*)st.i):0;
  if(!h){ /* legacy raw char* */
    const char* S=(const char*)st.i; int64_t n=(int64_t)strlen(S);
    int64_t i=a.i,j=b.i; if(i<0)i+=n; if(j<0)j+=n; if(i<0)i=0; if(j<0)j=0; if(i>n)i=n; if(j>n)j=n; if(j<i)j=i;
    pushc(cx,uf_str_new(S+i,(size_t)(j-i))); return;
  }
  int64_t n=(int64_t)h->len;
  int64_t i=a.i,j=b.i; if(i<0)i+=n; if(j<0)j+=n; if(i<0)i=0; if(j<0)j=0; if(i>n)i=n; if(j>n)j=n; if(j<i)j=i;
  if(h->tag==HT_STR){ Str*s=(Str*)h; pushc(cx,uf_str_new(uf_sbytes(s)+i,(size_t)(j-i))); return; }
  if(h->tag==HT_BUF){ pushc(cx,uf_str_new(h->data+i,(size_t)(j-i))); return; }
  if(h->tag==HT_DYN){
    Dyn*d=(Dyn*)h; Dyn*r=uf_dyn_new((uint64_t)(j-i)); UF_PROTECT(&r);
    for(int64_t q=i;q<j;q++)uf_dyn_push(&r,d->data[q]);
    UF_UNPROTECT(); pushp(cx,r); return;
  }
  if(h->tag==HT_ARR||h->tag==HT_TENSOR){
    Hdr*r=(Hdr*)uf_gc_alloc(sizeof(Hdr)+(size_t)(j-i)*h->esz,0);
    r->tag=h->tag; r->len=(uint64_t)(j-i); r->esz=h->esz; r->ety=h->ety;
    memcpy(r->data,uf_data(h)+i*h->esz,(size_t)(j-i)*h->esz);
    pushp(cx,r); return;
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
static int uf_count(const char*f){ int c=0; for(;f&&*f;f++){ if(*f=='%'){ if(f[1]=='%'){ f++; } else { c++; if(f[1]=='*')c++; } } } return c; }
static char* uf_fmt(const char*f,Cell*a,int n){
  size_t cap=256,bi=0; char*buf=(char*)uf_alloc(cap,0); int ai=0;
  for(const char*p=f;*p;){
    if(*p!='%'){ if(bi+2>cap){cap*=2;buf=(char*)realloc(buf,cap);} buf[bi++]=*p++; continue; }
    if(p[1]=='%'){ if(bi+2>cap){cap*=2;buf=(char*)realloc(buf,cap);} buf[bi++]='%'; p+=2; continue; }
    char d[32]; int di=0; d[di++]='%'; p++;
    while(*p&&strchr("-+ #0",*p)) d[di++]=*p++;
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
static int uf_vargc(Ctx*cx){ for(int t=0;t<cx->sp;t++){ Cell fc=cx->ds[cx->sp-1-t]; if(fc.tag==T_PTR&&((void*)fc.i)&&uf_count(uf_sptr(fc))==t) return t; } die("vararg call: format string not found"); return 0; }

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
  if(getenv("UF_WEAVE_DEBUG")){
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
  Hdr*r=(Hdr*)uf_gc_alloc(sizeof(Hdr)+nb,0); r->tag=a->tag; r->len=n; r->esz=a->esz; r->ety=a->ety; return r;
}

/* ================= GPU compute offloading (Vulkan, v13.1) =================
   Enabled when the compiler embedded the SPIR-V blobs and defined UF_GPU
   (automatic when glslc is present and device mode != cpu). The shims below
   lazily initialize Vulkan, pick the device (auto = most free VRAM via
   VK_EXT_memory_budget, discrete preferred; or the pinned --device bake),
   and LAUNCH prebuilt kernels for eligible ops when the element count clears
   UF_GPU_MIN (env, default 65536). Any failure or too-small workload falls
   back to the CPU implementation — the GPU is a fast path, never a
   correctness dependency. Kernels are float64; other element types stay CPU.
   NOTE: `div` on the GPU yields inf/nan for zero divisors where the CPU dies
   (documented divergence). */
#ifdef UF_GPU
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
static long uf_gpu_min(void){ const char*e=getenv("UF_GPU_MIN"); long v=e?atol(e):65536; return v>0?v:65536; }
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

static void uf_vk_init(void){
  if(uf_vk_ready||uf_vk_broken) return;
  VkApplicationInfo app; memset(&app,0,sizeof app); app.sType=VK_STRUCTURE_TYPE_APPLICATION_INFO; app.pApplicationName="uflux"; app.apiVersion=VK_API_VERSION_1_1;
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

typedef struct { VkBuffer buf; VkDeviceMemory mem; void* mapped; } UFBuf;
static int uf_vk_buf(size_t sz, const void*init, UFBuf*out){
  memset(out,0,sizeof *out);
  VkBufferCreateInfo bi; memset(&bi,0,sizeof bi); bi.sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO; bi.size=sz?sz:16; bi.usage=VK_BUFFER_USAGE_STORAGE_BUFFER_BIT;
  if(vkCreateBuffer(uf_vk_dev,&bi,0,&out->buf)!=VK_SUCCESS) return 0;
  VkMemoryRequirements mr; vkGetBufferMemoryRequirements(uf_vk_dev,out->buf,&mr);
  VkMemoryAllocateInfo ai; memset(&ai,0,sizeof ai); ai.sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO; ai.allocationSize=mr.size;
  VkPhysicalDeviceMemoryProperties mp; vkGetPhysicalDeviceMemoryProperties(uf_vk_pd,&mp);
  int found=0;
  for(uint32_t i=0;i<mp.memoryTypeCount;i++)
    if((mp.memoryTypes[i].propertyFlags&VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT)&&(mr.memoryTypeBits&(1u<<i))){ ai.memoryTypeIndex=i; found=1; break; }
  if(!found||vkAllocateMemory(uf_vk_dev,&ai,0,&out->mem)!=VK_SUCCESS){ vkDestroyBuffer(uf_vk_dev,out->buf,0); return 0; }
  vkBindBufferMemory(uf_vk_dev,out->buf,out->mem,0);
  if(vkMapMemory(uf_vk_dev,out->mem,0,sz?sz:16,0,&out->mapped)!=VK_SUCCESS){ vkFreeMemory(uf_vk_dev,out->mem,0); vkDestroyBuffer(uf_vk_dev,out->buf,0); return 0; }
  if(init&&sz) memcpy(out->mapped,init,sz);
  return 1;
}
static void uf_vk_buf_free(UFBuf*b){ if(b->mem){ vkUnmapMemory(uf_vk_dev,b->mem); vkFreeMemory(uf_vk_dev,b->mem,0); } if(b->buf) vkDestroyBuffer(uf_vk_dev,b->buf,0); memset(b,0,sizeof *b); }

/* dispatch kernel k over n work items; reads A (and B when non-null) into
   device memory, writes R (rsz bytes) back. Returns 0 on failure. */
static int uf_vk_run(int k,uint64_t n,const void*A,size_t asz,const void*B,size_t bsz,void*R,size_t rsz,struct UFPC pc){
  if(uf_vk_broken) return 0;
  uf_vk_init();
  if(!uf_vk_ready) return 0;
  pthread_mutex_lock(&uf_gpu_mu);
  if(getenv("UF_VK_DEBUG"))fprintf(stderr,"[vk] bufs a=%zu b=%zu r=%zu\n",asz,bsz,rsz);
  UFBuf ba,bb,br; memset(&ba,0,sizeof ba); memset(&bb,0,sizeof bb); memset(&br,0,sizeof br);
  if(!uf_vk_buf(asz,A,&ba)){ pthread_mutex_unlock(&uf_gpu_mu); return 0; }
  if(B&&!uf_vk_buf(bsz,B,&bb)){ uf_vk_buf_free(&ba); pthread_mutex_unlock(&uf_gpu_mu); return 0; }
  if(!uf_vk_buf(rsz,NULL,&br)){ uf_vk_buf_free(&ba); uf_vk_buf_free(&bb); pthread_mutex_unlock(&uf_gpu_mu); return 0; }
  VkDescriptorSetAllocateInfo dsai; memset(&dsai,0,sizeof dsai); dsai.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO; dsai.descriptorPool=uf_vk_dpool; dsai.descriptorSetCount=1; dsai.pSetLayouts=&uf_vk_dsl;
  VkDescriptorSet ds;
  VkResult ar=vkAllocateDescriptorSets(uf_vk_dev,&dsai,&ds);
  if(getenv("UF_VK_DEBUG"))fprintf(stderr,"[vk] dsalloc=%d\n",(int)ar);
  if(ar!=VK_SUCCESS){ uf_vk_buf_free(&ba); uf_vk_buf_free(&bb); uf_vk_buf_free(&br); return 0; }
  VkWriteDescriptorSet w[3]; VkDescriptorBufferInfo bi[3];
  memset(w,0,sizeof w); memset(bi,0,sizeof bi);
  bi[0].buffer=ba.buf; bi[0].offset=0; bi[0].range=VK_WHOLE_SIZE;
  bi[1].buffer=B?bb.buf:uf_vk_dummy; bi[1].offset=0; bi[1].range=VK_WHOLE_SIZE;
  bi[2].buffer=br.buf; bi[2].offset=0; bi[2].range=VK_WHOLE_SIZE;
  for(int i=0;i<3;i++){ w[i].sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET; w[i].dstSet=ds; w[i].dstBinding=(uint32_t)i; w[i].descriptorCount=1; w[i].descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; w[i].pBufferInfo=&bi[i]; }
  vkUpdateDescriptorSets(uf_vk_dev,3,w,0,0);
  if(getenv("UF_VK_DEBUG"))fprintf(stderr,"[vk] updated\n");
  VkCommandBufferBeginInfo cbbi; memset(&cbbi,0,sizeof cbbi); cbbi.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
  if(getenv("UF_VK_DEBUG"))fprintf(stderr,"[vk] run k=%d n=%llu begin\n",k,(unsigned long long)n);
  vkBeginCommandBuffer(uf_vk_cb,&cbbi);
  vkCmdBindPipeline(uf_vk_cb,VK_PIPELINE_BIND_POINT_COMPUTE,uf_vk_pipe[k]);
  vkCmdPushConstants(uf_vk_cb,uf_vk_playout,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pc,&pc);
  vkCmdBindDescriptorSets(uf_vk_cb,VK_PIPELINE_BIND_POINT_COMPUTE,uf_vk_playout,0,1,&ds,0,0);
  uint64_t groups=(n+255)/256; if(!groups)groups=1;
  vkCmdDispatch(uf_vk_cb,(uint32_t)(groups>0x7fffffff?0x7fffffff:groups),1,1);
  vkEndCommandBuffer(uf_vk_cb);
  VkSubmitInfo si; memset(&si,0,sizeof si); si.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO; si.commandBufferCount=1; si.pCommandBuffers=&uf_vk_cb;
  if(getenv("UF_VK_DEBUG"))fprintf(stderr,"[vk] submit\n");
  VkFence fence; VkFenceCreateInfo fci; memset(&fci,0,sizeof fci); fci.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO;
  vkCreateFence(uf_vk_dev,&fci,0,&fence);
  int ok = vkQueueSubmit(uf_vk_q,1,&si,fence)==VK_SUCCESS && vkWaitForFences(uf_vk_dev,1,&fence,VK_TRUE,UINT64_MAX)==VK_SUCCESS;
  vkDestroyFence(uf_vk_dev,fence,0);
  if(ok&&R&&rsz) memcpy(R,br.mapped,rsz);
  vkFreeDescriptorSets(uf_vk_dev,uf_vk_dpool,1,&ds);
  uf_vk_buf_free(&ba); uf_vk_buf_free(&bb); uf_vk_buf_free(&br);
  pthread_mutex_unlock(&uf_gpu_mu);
  return ok;
}

/* ---- fused weave-task dispatch (v13.1) ----
   One kernel launch for a whole compilable task body: inputs are the top n
   cells of the ds (peeked, not popped — the ret epilogue drains), the task
   kernel table follows the static library pipelines. Declines (no device,
   non-float/mismatched inputs, below UF_GPU_MIN, any Vulkan failure) return
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
  UFBuf bufs[8]; memset(bufs,0,sizeof bufs);
  int ok=1;
  for(int j=0;j<n&&ok;j++) ok=uf_vk_buf((size_t)len*8,uf_data(ins[j]),&bufs[j]);
  Hdr* r=ok?uf_arr_like(ins[0],len):0; UF_PROTECT(&r);
  if(ok) ok=uf_vk_buf((size_t)len*8,NULL,&bufs[n]);
  if(ok){
    VkDescriptorSetAllocateInfo dsai; memset(&dsai,0,sizeof dsai); dsai.sType=VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO; dsai.descriptorPool=uf_vk_dpool; dsai.descriptorSetCount=1; dsai.pSetLayouts=&uf_vk_dsl;
    VkDescriptorSet ds;
    ok=vkAllocateDescriptorSets(uf_vk_dev,&dsai,&ds)==VK_SUCCESS;
    if(ok){
      VkWriteDescriptorSet w[8]; VkDescriptorBufferInfo bi[8];
      memset(w,0,sizeof w); memset(bi,0,sizeof bi);
      for(int j=0;j<=n;j++){ bi[j].buffer=bufs[j].buf; bi[j].range=VK_WHOLE_SIZE;
        w[j].sType=VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET; w[j].dstSet=ds; w[j].dstBinding=(uint32_t)j; w[j].descriptorCount=1; w[j].descriptorType=VK_DESCRIPTOR_TYPE_STORAGE_BUFFER; w[j].pBufferInfo=&bi[j]; }
      vkUpdateDescriptorSets(uf_vk_dev,n+1,w,0,0);
      struct UFPC pc; memset(&pc,0,sizeof pc); pc.n0=(int64_t)len;
      VkCommandBufferBeginInfo cbbi; memset(&cbbi,0,sizeof cbbi); cbbi.sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
      ok=vkBeginCommandBuffer(uf_vk_cb,&cbbi)==VK_SUCCESS;
      if(ok){
        vkCmdBindPipeline(uf_vk_cb,VK_PIPELINE_BIND_POINT_COMPUTE,uf_vk_pipe[k]);
        vkCmdPushConstants(uf_vk_cb,uf_vk_playout,VK_SHADER_STAGE_COMPUTE_BIT,0,sizeof pc,&pc);
        vkCmdBindDescriptorSets(uf_vk_cb,VK_PIPELINE_BIND_POINT_COMPUTE,uf_vk_playout,0,1,&ds,0,0);
        uint64_t groups=(len+255)/256; if(!groups)groups=1;
        vkCmdDispatch(uf_vk_cb,(uint32_t)(groups>0x7fffffff?0x7fffffff:groups),1,1);
        vkEndCommandBuffer(uf_vk_cb);
        VkSubmitInfo si; memset(&si,0,sizeof si); si.sType=VK_STRUCTURE_TYPE_SUBMIT_INFO; si.commandBufferCount=1; si.pCommandBuffers=&uf_vk_cb;
        VkFence fence; VkFenceCreateInfo fci; memset(&fci,0,sizeof fci); fci.sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO;
        vkCreateFence(uf_vk_dev,&fci,0,&fence);
        ok=vkQueueSubmit(uf_vk_q,1,&si,fence)==VK_SUCCESS && vkWaitForFences(uf_vk_dev,1,&fence,VK_TRUE,UINT64_MAX)==VK_SUCCESS;
        vkDestroyFence(uf_vk_dev,fence,0);
        if(ok) memcpy(uf_data(r),bufs[n].mapped,(size_t)len*8);
      }
      vkFreeDescriptorSets(uf_vk_dev,uf_vk_dpool,1,&ds);
    }
  }
  UF_UNPROTECT();
  for(int j=0;j<=n;j++) if(bufs[j].buf||bufs[j].mem) uf_vk_buf_free(&bufs[j]);
  pthread_mutex_unlock(&uf_gpu_mu);
  return ok?uf_mkp(r):uf_gpu_decline();
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
    for(uint64_t i=0;i<ra;i++) for(uint64_t k=0;k<ca;k++){ double aik=A[i*ca+k]; if(aik==0.0)continue; const double*Bk=B+k*cb; double*Ci=C+i*cb; for(uint64_t j=0;j<cb;j++) Ci[j]+=aik*Bk[j]; }
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
#ifdef UF_GPU
  if(ha&&hb&&ha->ety==1&&hb->ety==1){
    uint64_t work = (op==2&&ha->tag==HT_MAT&&hb->tag==HT_MAT) ? (ha->esz*(hb->len/hb->esz))
      : (op==2&&ha->tag==HT_MAT) ? ha->esz
      : (op==2&&hb->tag==HT_MAT) ? (hb->len/hb->esz)
      : (ha->len>hb->len?ha->len:hb->len);
    if(work>=(uint64_t)uf_gpu_min()){
      Cell g=uf_gpu_arith(a,b,op);
      if(!(g.tag==T_INT&&g.i==0)) return g;
    }
  }
  if((ha&&!hb&&ha->ety==1&&ha->len>=(uint64_t)uf_gpu_min())||(hb&&!ha&&hb->ety==1&&hb->len>=(uint64_t)uf_gpu_min())){
    Cell g=uf_gpu_arith(a,b,op);
    if(!(g.tag==T_INT&&g.i==0)) return g;
  }
#endif
  if(ha&&hb){
    if(op==2&&ha->tag==HT_MAT&&hb->tag==HT_MAT){ Hdr*r=uf_matmul(ha,hb); return uf_mkp(r); }
    if(op==2&&ha->tag==HT_MAT&&hb->tag!=HT_MAT){ Hdr*r=uf_matvec(ha,hb); return uf_mkp(r); }
    if(op==2&&ha->tag!=HT_MAT&&hb->tag==HT_MAT){ Hdr*r=uf_vecmat(ha,hb); return uf_mkp(r); }
    /* elementwise (arrays, or matrices of identical shape) */
    if(ha->len!=hb->len){ char m[96]; snprintf(m,sizeof m,"%s: length mismatch (%llu vs %llu)",opn,(unsigned long long)ha->len,(unsigned long long)hb->len); die(m); }
    if(ha->tag==HT_MAT&&hb->tag==HT_MAT&&ha->esz!=hb->esz){ char da[32],db[32]; uf_dims(ha,da,sizeof da); uf_dims(hb,db,sizeof db); char m[128]; snprintf(m,sizeof m,"%s: matrix shape mismatch (%s vs %s)",opn,da,db); die(m); }
    uint64_t n=ha->len; Hdr*r=uf_arr_like(ha,n); UF_PROTECT(&r);
    if(op==3) for(uint64_t i=0;i<n;i++) if(uf_el(hb,i)==0.0){ char m[64]; snprintf(m,sizeof m,"%s: zero divisor",opn); UF_UNPROTECT(); die(m); }
    for(uint64_t i=0;i<n;i++){
      double x=uf_el(ha,i),y=uf_el(hb,i);
      double v = op==0?x+y : op==1?x-y : op==2?x*y : x/y;
      uf_put_el(r,i,v);
    }
    UF_UNPROTECT(); return uf_mkp(r);
  }
  /* scalar broadcast */
  if(ha&&!hb){
    uint64_t n=ha->len; Hdr*r=uf_arr_like(ha,n); UF_PROTECT(&r);
    if(ha->ety==1||b.tag==T_FLOAT){ double d=uf_f(b); if(op==3&&d==0.0){ UF_UNPROTECT(); die("div: zero divisor"); } for(uint64_t i=0;i<n;i++){ double x=uf_el(ha,i); uf_put_el(r,i, op==0?x+d : op==1?x-d : op==2?x*d : x/d); } }
    else { int64_t d=b.i; if(op==3&&d==0){ UF_UNPROTECT(); die("div: zero divisor"); } for(uint64_t i=0;i<n;i++){ double x=uf_el(ha,i); uf_put_el(r,i, op==0?x+(double)d : op==1?x-(double)d : op==2?x*(double)d : x/(double)d); } }
    UF_UNPROTECT(); return uf_mkp(r);
  }
  if(hb&&!ha){
    uint64_t n=hb->len; Hdr*r=uf_arr_like(hb,n); UF_PROTECT(&r);
    if(hb->ety==1||a.tag==T_FLOAT){ double d=uf_f(a); if(op==3&&d==0.0){ UF_UNPROTECT(); die("div: zero divisor"); } for(uint64_t i=0;i<n;i++){ double y=uf_el(hb,i); uf_put_el(r,i, op==0?d+y : op==1?d-y : op==2?d*y : d/y); } }
    else { int64_t d=a.i; if(op==3&&d==0){ UF_UNPROTECT(); die("div: zero divisor"); } for(uint64_t i=0;i<n;i++){ double y=uf_el(hb,i); uf_put_el(r,i, op==0?(double)d+y : op==1?(double)d-y : op==2?(double)d*y : (double)d/y); } }
    UF_UNPROTECT(); return uf_mkp(r);
  }
  die("poly arith: no array operand");
}

/* TRANSPOSE: mat -> mat' (rows/cols swapped) */
static void op_transpose(Ctx*cx){
  Cell h=pop(cx); Hdr*a=uf_handle(h,"transpose");
  if(a->tag!=HT_MAT)die("transpose: not a matrix (build one with [rows cols] type tensor)");
  uint64_t r=a->esz,c=a->len/r;
#ifdef UF_GPU
  if(a->ety==1&&(r*c)>=(uint64_t)uf_gpu_min()){
    Hdr*g=uf_mat_new(c,r,1); UF_PROTECT(&g);
    struct UFPC pc; memset(&pc,0,sizeof pc); pc.n1=(int64_t)r; pc.n2=(int64_t)c;
    int k=uf_spv_index("transpose");
    int ok=k>=0&&uf_vk_run(k,r*c,uf_data(a),(size_t)(r*c)*8,NULL,0,uf_data(g),(size_t)(r*c)*8,pc);
    UF_UNPROTECT();
    if(ok){ pushp(cx,g); return; }
    /* fall through to CPU */
  }
#endif
  Hdr*t=uf_mat_new(c,r,a->ety); UF_PROTECT(&t);
  for(uint64_t i=0;i<r;i++) for(uint64_t j=0;j<c;j++) uf_put_el(t,j*r+i,uf_el(a,i*c+j));
  UF_UNPROTECT(); pushp(cx,t);
}
static Hdr* uf_vcheck(Cell h,const char*op){ Hdr*a=uf_handle(h,op); if(a->tag!=HT_ARR&&a->tag!=HT_TENSOR&&a->tag!=HT_MAT)die("vector op: not an arr"); return a; }
/* scalar arr ops: arr scalar -> arr' */
#define UF_VSOP(NAME,EXPR,ZERO_DIE) \
static void NAME(Ctx*cx){ Cell s=pop(cx),h=pop(cx); Hdr*a=uf_vcheck(h,#NAME); \
  uint64_t n=a->len; Hdr*r=uf_arr_like(a,n); UF_PROTECT(&r); \
  if(a->ety==1||s.tag==T_FLOAT){ double d=uf_f(s); if(ZERO_DIE&&d==0.0)die(#NAME ": zero scalar"); for(uint64_t i=0;i<n;i++)uf_put_el(r,i,(EXPR)); } \
  else { int64_t d=s.i; if(ZERO_DIE&&d==0)die(#NAME ": zero scalar"); for(uint64_t i=0;i<n;i++)uf_put_el(r,i,(EXPR)); } \
  UF_UNPROTECT(); pushp(cx,r); }
UF_VSOP(op_vadd, uf_el(a,i)+d, 0)
UF_VSOP(op_vsub, uf_el(a,i)-d, 0)
UF_VSOP(op_vmul, uf_el(a,i)*d, 0)
UF_VSOP(op_vdiv, uf_el(a,i)/d, 1)
/* elementwise arr arr ops: length mismatch dies */
#define UF_VEOP(NAME,EXPR,ZERO_DIE) \
static void NAME(Ctx*cx){ Cell h2=pop(cx),h1=pop(cx); Hdr*a=uf_vcheck(h1,#NAME); Hdr*b=uf_vcheck(h2,#NAME); \
  if(a->len!=b->len)die(#NAME ": length mismatch"); \
  uint64_t n=a->len; Hdr*r=uf_arr_like(a,n); UF_PROTECT(&r); \
  if(ZERO_DIE) for(uint64_t i=0;i<n;i++) if(uf_el(b,i)==0.0)die(#NAME ": zero divisor"); \
  for(uint64_t i=0;i<n;i++)uf_put_el(r,i,(EXPR)); \
  UF_UNPROTECT(); pushp(cx,r); }
UF_VEOP(op_veadd, uf_el(a,i)+uf_el(b,i), 0)
UF_VEOP(op_vesub, uf_el(a,i)-uf_el(b,i), 0)
UF_VEOP(op_vemul, uf_el(a,i)*uf_el(b,i), 0)
UF_VEOP(op_vediv, uf_el(a,i)/uf_el(b,i), 1)
UF_VEOP(op_vemax, uf_el(a,i)>uf_el(b,i)?uf_el(a,i):uf_el(b,i), 0)
UF_VEOP(op_vemin, uf_el(a,i)<uf_el(b,i)?uf_el(a,i):uf_el(b,i), 0)
/* comparisons: arr scalar -> bitmap */
static Bitmap* uf_bm_new(uint64_t nbits){ Bitmap*b=(Bitmap*)uf_gc_alloc(sizeof(Bitmap)+((nbits+63)/64)*8,0); b->tag=HT_BITMAP; b->len=nbits; b->esz=8; memset(b->words,0,((nbits+63)/64)*8); return b; }
#define UF_VCOP(NAME,EXPR) \
static void NAME(Ctx*cx){ Cell s=pop(cx),h=pop(cx); Hdr*a=uf_vcheck(h,#NAME); \
  uint64_t n=a->len; Bitmap*r=uf_bm_new(n); UF_PROTECT(&r); \
  double d=uf_f(s); \
  for(uint64_t i=0;i<n;i++) if(EXPR) r->words[i>>6]|=(1ULL<<(i&63)); \
  UF_UNPROTECT(); pushp(cx,r); }
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
  uint64_t n=0; for(uint64_t i=0;i<a->len;i++) if((b->words[i>>6]>>(i&63))&1)n++;
  Hdr*r=uf_arr_like(a,n); UF_PROTECT(&r);
  uint64_t k=0; for(uint64_t i=0;i<a->len;i++) if((b->words[i>>6]>>(i&63))&1)uf_put_el(r,k++,uf_el(a,i));
  UF_UNPROTECT(); pushp(cx,r);
}
/* reductions */
static void op_vsum(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VSUM");
#ifdef UF_GPU
  if(a->ety==1&&a->len>=(uint64_t)uf_gpu_min()){ double _r; if(uf_gpu_reduce((const double*)uf_data(a),a->len,"rsum",&_r)){ pushf(cx,_r); return; } }
#endif
 double s=0; for(uint64_t i=0;i<a->len;i++)s+=uf_el(a,i); if(a->ety==1)pushf(cx,s); else pushi(cx,(int64_t)s); }
static void op_vmean(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VMEAN"); if(!a->len)die("VMEAN: empty arr");
#ifdef UF_GPU
  if(a->ety==1&&a->len>=(uint64_t)uf_gpu_min()){ double _r; if(uf_gpu_reduce((const double*)uf_data(a),a->len,"rsum",&_r)){ pushf(cx,_r/(double)a->len); return; } }
#endif
 double s=0; for(uint64_t i=0;i<a->len;i++)s+=uf_el(a,i); pushf(cx,s/(double)a->len); }
static void op_vmin(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VMIN"); if(!a->len)die("VMIN: empty arr");
#ifdef UF_GPU
  if(a->ety==1&&a->len>=(uint64_t)uf_gpu_min()){ double _r; if(uf_gpu_reduce((const double*)uf_data(a),a->len,"rmin",&_r)){ pushf(cx,_r); return; } }
#endif
 double s=uf_el(a,0); for(uint64_t i=1;i<a->len;i++){double d=uf_el(a,i);if(d<s)s=d;} if(a->ety==1)pushf(cx,s); else pushi(cx,(int64_t)s); }
static void op_vmax(Ctx*cx){ Cell h=pop(cx); Hdr*a=uf_vcheck(h,"VMAX"); if(!a->len)die("VMAX: empty arr");
#ifdef UF_GPU
  if(a->ety==1&&a->len>=(uint64_t)uf_gpu_min()){ double _r; if(uf_gpu_reduce((const double*)uf_data(a),a->len,"rmax",&_r)){ pushf(cx,_r); return; } }
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
  Cell f=pop(cx),acc=pop(cx),h=pop(cx); Hdr*a=uf_vcheck(h,"VFOLD");
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
  Dyn* d=uf_dyn_new((uint64_t)uf_argc); UF_PROTECT(&d);
  char** av=(char**)uf_argv;
  for(int64_t i=0;i<uf_argc;i++){ Cell s=uf_str_new(av[i],strlen(av[i])); uf_dyn_push(&d,s); }
  UF_UNPROTECT(); pushp(cx,d);
}
/* HASARGS: -> int (1 if argv has >1 element, else 0) */
static void op_hasargs(Ctx*cx){
  pushi(cx, uf_argc>1 ? 1 : 0);
}
/* ARGI: index -> int (argv[index] parsed as integer) */
static void op_argi(Ctx*cx){
  int64_t idx=uf_i(pop(cx));
  if(idx<0||idx>=uf_argc) die("ARGI: index out of bounds");
  pushi(cx,(int64_t)strtoll(((char**)uf_argv)[idx],0,10));
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
      char* sp=strstr(cur,E);
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
      Hdr*eh=uf_gc_find((void*)ek.i);
      if(eh&&eh->tag==HT_STR&&eh->len==(uint64_t)flen&&
         memcmp(uf_sbytes((Str*)eh),fk,(size_t)flen)==0){
        m->vals[i]=uf_cadd(m->vals[i],v); return; /* found: add */
      }
    }
    i=(i+1)%m->cap;
  }
  /* not found: create Str key + insert (must alloc for dict storage) */
  Str* sk=(Str*)uf_gc_alloc(sizeof(Str),0);
  sk->tag=HT_STR; sk->esz=1; sk->len=(uint64_t)flen; sk->mlen=0;
  sk->mdata=fk; sk->gc_parent=uf_fsplit_parent; /* view into line (valid during callback) */
  /* For dict storage, copy the key so it survives beyond the callback */
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
      Hdr*eh=uf_gc_find((void*)ek.i);
      if(eh&&eh->tag==HT_STR&&eh->len==(uint64_t)flen&&
         memcmp(uf_sbytes((Str*)eh),fk,(size_t)flen)==0){
        m->vals[i]=uf_cadd(m->vals[i],uf_mki(1)); return;
      }
    }
    i=(i+1)%m->cap;
  }
  Str* sk=(Str*)uf_gc_alloc(sizeof(Str),0);
  sk->tag=HT_STR; sk->esz=1; sk->len=(uint64_t)flen; sk->mlen=0;
  sk->mdata=fk; sk->gc_parent=uf_fsplit_parent; /* view (valid during callback) */
  /* Copy key for persistent dict storage */
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
static void* uf_spawn_worker(void* arg){
  UfSpawn* g=(UfSpawn*)arg;
  Ctx* c=ctx_new(1<<16,1<<12);
  uf_call_addr(c,g->body,0,-1,0);
  Cell r = c->sp>0 ? c->ds[c->sp-1] : uf_mki(0);
  ring_enq(g->r,r);
  ring_close(g->r);
  ctx_free(c);
  free(g);
  return 0;
}
/* SPAWN: body_addr -> chan (cap 1; body's top-of-stack enqueued at end,
   then closed — deq on it is a join) */
static void op_spawn(Ctx*cx){
  Cell a=pop(cx);
  Ring* r=uf_ring_new(1);
  UfSpawn* g=(UfSpawn*)malloc(sizeof(UfSpawn)); if(!g)die("out of memory");
  g->body=(const void*)a.i; g->r=r;
  pthread_t th; if(pthread_create(&th,0,uf_spawn_worker,g)){ ring_close(r); die("SPAWN: thread"); }
  pthread_detach(th);
  pushp(cx,r);
}
/* init-TU worker: fire-and-forget thread for init.uf entry points.
   Same as uf_spawn_worker minus the chan — process exit kills these. */
static void* uf_init_worker(void* arg){
  Ctx* c=ctx_new(1<<16,1<<12);
  uf_call_addr(c,arg,0,-1,0);
  ctx_free(c);
  return 0;
}
static void uf_init_reflection(void){}
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl0 = {0,0,0,9,2,1,0,0,0,"%s"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl1 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[23]; } uf_sl2 = {0,0,0,9,22,1,0,0,0,"parse error: expected "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl3 = {0,0,0,9,5,1,0,0,0," got "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl4 = {0,0,0,9,1,1,0,0,0,"\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl5 = {0,0,0,9,3,1,0,0,0,"int"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl6 = {0,0,0,9,4,1,0,0,0,"char"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl7 = {0,0,0,9,4,1,0,0,0,"void"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl8 = {0,0,0,9,4,1,0,0,0,"long"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl9 = {0,0,0,9,5,1,0,0,0,"short"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[9]; } uf_sl10 = {0,0,0,9,8,1,0,0,0,"unsigned"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl11 = {0,0,0,9,6,1,0,0,0,"signed"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl12 = {0,0,0,9,5,1,0,0,0,"const"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl13 = {0,0,0,9,6,1,0,0,0,"static"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl14 = {0,0,0,9,1,1,0,0,0,"*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl15 = {0,0,0,9,1,1,0,0,0,"v"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl16 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[21]; } uf_sl17 = {0,0,0,9,20,1,0,0,0,"undefined variable: "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl18 = {0,0,0,9,1,1,0,0,0,"\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl19 = {0,0,0,9,16,1,0,0,0,"^\"([^\"\\\\]|\\\\.)*\""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl20 = {0,0,0,9,16,1,0,0,0,"^'([^'\\\\]|\\\\.)*'"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl21 = {0,0,0,9,27,1,0,0,0,"^(0[xX][0-9a-fA-F]+|[0-9]+)"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[24]; } uf_sl22 = {0,0,0,9,23,1,0,0,0,"^[A-Za-z_][A-Za-z0-9_]*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[62]; } uf_sl23 = {0,0,0,9,61,1,0,0,0,"^(<<=|>>=|==|!=|<=|>=|&&|\\|\\||\\+\\+|--|\\+=|-=|\\*=|/=|%=|<<|>>)"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[9]; } uf_sl24 = {0,0,0,9,8,1,0,0,0,"^[\t\n\r ]+"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl25 = {0,0,0,9,2,1,0,0,0,"//"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl26 = {0,0,0,9,2,1,0,0,0,"/*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl27 = {0,0,0,9,1,1,0,0,0,"#"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[9]; } uf_sl28 = {0,0,0,9,8,1,0,0,0,"^[\t\n\r ]+"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl29 = {0,0,0,9,2,1,0,0,0,"//"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl30 = {0,0,0,9,1,1,0,0,0,"\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl31 = {0,0,0,9,2,1,0,0,0,"/*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl32 = {0,0,0,9,2,1,0,0,0,"*/"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl33 = {0,0,0,9,1,1,0,0,0,"#"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl34 = {0,0,0,9,1,1,0,0,0,"\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl35 = {0,0,0,9,1,1,0,0,0,"="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl36 = {0,0,0,9,2,1,0,0,0,"+="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl37 = {0,0,0,9,2,1,0,0,0,"-="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl38 = {0,0,0,9,2,1,0,0,0,"*="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl39 = {0,0,0,9,2,1,0,0,0,"/="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl40 = {0,0,0,9,2,1,0,0,0,"%="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl41 = {0,0,0,9,2,1,0,0,0,"/="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl42 = {0,0,0,9,2,1,0,0,0,"%="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl43 = {0,0,0,9,1,1,0,0,0,"="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl44 = {0,0,0,9,5,1,0,0,0,"^%s!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl45 = {0,0,0,9,5,1,0,0,0,"^%s@ "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl46 = {0,0,0,9,1,1,0,0,0,"+"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl47 = {0,0,0,9,1,1,0,0,0,"-"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl48 = {0,0,0,9,4,1,0,0,0,"add "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl49 = {0,0,0,9,5,1,0,0,0,"^%s!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl50 = {0,0,0,9,4,1,0,0,0,"sub "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl51 = {0,0,0,9,5,1,0,0,0,"^%s!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl52 = {0,0,0,9,4,1,0,0,0,"mul "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl53 = {0,0,0,9,5,1,0,0,0,"^%s!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[22]; } uf_sl54 = {0,0,0,9,21,1,0,0,0,"no division in subset"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl55 = {0,0,0,9,2,1,0,0,0,"||"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[12]; } uf_sl56 = {0,0,0,9,11,1,0,0,0,"or not not "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl57 = {0,0,0,9,2,1,0,0,0,"&&"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[13]; } uf_sl58 = {0,0,0,9,12,1,0,0,0,"mul not not "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl59 = {0,0,0,9,2,1,0,0,0,"=="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl60 = {0,0,0,9,2,1,0,0,0,"!="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl61 = {0,0,0,9,2,1,0,0,0,"=="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl62 = {0,0,0,9,3,1,0,0,0,"eq "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[8]; } uf_sl63 = {0,0,0,9,7,1,0,0,0,"eq not "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl64 = {0,0,0,9,1,1,0,0,0,"<"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl65 = {0,0,0,9,2,1,0,0,0,"<="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl66 = {0,0,0,9,1,1,0,0,0,">"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl67 = {0,0,0,9,2,1,0,0,0,">="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl68 = {0,0,0,9,1,1,0,0,0,"<"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl69 = {0,0,0,9,3,1,0,0,0,"lt "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl70 = {0,0,0,9,2,1,0,0,0,"<="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[8]; } uf_sl71 = {0,0,0,9,7,1,0,0,0,"gt not "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl72 = {0,0,0,9,1,1,0,0,0,">"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl73 = {0,0,0,9,3,1,0,0,0,"gt "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[8]; } uf_sl74 = {0,0,0,9,7,1,0,0,0,"lt not "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl75 = {0,0,0,9,1,1,0,0,0,"+"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl76 = {0,0,0,9,1,1,0,0,0,"-"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl77 = {0,0,0,9,1,1,0,0,0,"+"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl78 = {0,0,0,9,4,1,0,0,0,"add "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl79 = {0,0,0,9,4,1,0,0,0,"sub "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl80 = {0,0,0,9,1,1,0,0,0,"/"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl81 = {0,0,0,9,1,1,0,0,0,"%"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl82 = {0,0,0,9,1,1,0,0,0,"*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl83 = {0,0,0,9,4,1,0,0,0,"mul "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[22]; } uf_sl84 = {0,0,0,9,21,1,0,0,0,"no division in subset"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl85 = {0,0,0,9,1,1,0,0,0,"-"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl86 = {0,0,0,9,1,1,0,0,0,"!"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl87 = {0,0,0,9,1,1,0,0,0,"~"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl88 = {0,0,0,9,1,1,0,0,0,"+"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl89 = {0,0,0,9,2,1,0,0,0,"++"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl90 = {0,0,0,9,2,1,0,0,0,"--"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl91 = {0,0,0,9,2,1,0,0,0,"0 "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl92 = {0,0,0,9,4,1,0,0,0,"sub "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl93 = {0,0,0,9,4,1,0,0,0,"not "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl94 = {0,0,0,9,3,1,0,0,0,"-1 "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl95 = {0,0,0,9,4,1,0,0,0,"sub "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl96 = {0,0,0,9,16,1,0,0,0,"^%s@ 1 add ^%s! "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[20]; } uf_sl97 = {0,0,0,9,19,1,0,0,0,"++ needs a variable"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl98 = {0,0,0,9,16,1,0,0,0,"^%s@ 1 sub ^%s! "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[20]; } uf_sl99 = {0,0,0,9,19,1,0,0,0,"-- needs a variable"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl100 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl101 = {0,0,0,9,2,1,0,0,0,"++"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl102 = {0,0,0,9,2,1,0,0,0,"--"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl103 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl104 = {0,0,0,9,16,1,0,0,0,"^%s@ 1 add ^%s! "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl105 = {0,0,0,9,27,1,0,0,0,"postfix ++ needs a variable"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl106 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl107 = {0,0,0,9,16,1,0,0,0,"^%s@ 1 sub ^%s! "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl108 = {0,0,0,9,27,1,0,0,0,"postfix -- needs a variable"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl109 = {0,0,0,9,6,1,0,0,0,"[0-9]*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl110 = {0,0,0,9,1,1,0,0,0,"'"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl111 = {0,0,0,9,1,1,0,0,0,"\""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl112 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl113 = {0,0,0,9,4,1,0,0,0,"argc"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl114 = {0,0,0,9,4,1,0,0,0,"argv"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl115 = {0,0,0,9,6,1,0,0,0,"__byte"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl116 = {0,0,0,9,4,1,0,0,0,"NULL"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl117 = {0,0,0,9,3,1,0,0,0,"EOF"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl118 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl119 = {0,0,0,9,27,1,0,0,0,"^(0[xX][0-9a-fA-F]+|[0-9]+)"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl120 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl121 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl122 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl123 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl124 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[23]; } uf_sl125 = {0,0,0,9,22,1,0,0,0,"extern \"uf_argc\" load "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl126 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl127 = {0,0,0,9,1,1,0,0,0,"["};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl128 = {0,0,0,9,1,1,0,0,0,"]"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[38]; } uf_sl129 = {0,0,0,9,37,1,0,0,0,"8 mul extern \"uf_argv\" load add load "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl130 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl131 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl132 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[14]; } uf_sl133 = {0,0,0,9,13,1,0,0,0,"load 255 and "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl134 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl135 = {0,0,0,9,2,1,0,0,0,"0 "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl136 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl137 = {0,0,0,9,3,1,0,0,0,"-1 "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl138 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl139 = {0,0,0,9,1,1,0,0,0,"\\"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl140 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl141 = {0,0,0,9,2,1,0,0,0,"\\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl142 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl143 = {0,0,0,9,2,1,0,0,0,"\\t"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl144 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl145 = {0,0,0,9,2,1,0,0,0,"\\r"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl146 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl147 = {0,0,0,9,2,1,0,0,0,"\\0"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl148 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl149 = {0,0,0,9,2,1,0,0,0,"\\\\"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl150 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl151 = {0,0,0,9,2,1,0,0,0,"\\'"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl152 = {0,0,0,9,1,1,0,0,0," "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[16]; } uf_sl153 = {0,0,0,9,15,1,0,0,0,"bad char escape"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl154 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl155 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[10]; } uf_sl156 = {0,0,0,9,9,1,0,0,0,"_call %s "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl157 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl158 = {0,0,0,9,1,1,0,0,0,","};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl159 = {0,0,0,9,1,1,0,0,0,"["};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl160 = {0,0,0,9,1,1,0,0,0,"]"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[39]; } uf_sl161 = {0,0,0,9,38,1,0,0,0,"^%s@ \"\" _call strstr add load 255 and "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl162 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl163 = {0,0,0,9,5,1,0,0,0,"^%s@ "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[23]; } uf_sl164 = {0,0,0,9,22,1,0,0,0,"unknown array variable"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[21]; } uf_sl165 = {0,0,0,9,20,1,0,0,0,"undefined variable: "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl166 = {0,0,0,9,1,1,0,0,0,"\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl167 = {0,0,0,9,6,1,0,0,0,"return"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl168 = {0,0,0,9,2,1,0,0,0,"if"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl169 = {0,0,0,9,5,1,0,0,0,"while"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[4]; } uf_sl170 = {0,0,0,9,3,1,0,0,0,"for"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl171 = {0,0,0,9,2,1,0,0,0,"do"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl172 = {0,0,0,9,5,1,0,0,0,"break"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[9]; } uf_sl173 = {0,0,0,9,8,1,0,0,0,"continue"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl174 = {0,0,0,9,1,1,0,0,0,"{"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl175 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl176 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl177 = {0,0,0,9,1,1,0,0,0,","};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl178 = {0,0,0,9,1,1,0,0,0,"="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl179 = {0,0,0,9,5,1,0,0,0,"^%s!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl180 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[12]; } uf_sl181 = {0,0,0,9,11,1,0,0,0,"_call exit\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl182 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[23]; } uf_sl183 = {0,0,0,9,22,1,0,0,0," ^rv! 1 ^fr! ret ^rv@\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl184 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[20]; } uf_sl185 = {0,0,0,9,19,1,0,0,0," ^rv!\n1 ^fr! 0 ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl186 = {0,0,0,9,6,1,0,0,0,"0 ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl187 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl188 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl189 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl190 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl191 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl192 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl193 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl194 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl195 = {0,0,0,9,1,1,0,0,0,"i"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl196 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl197 = {0,0,0,9,2,1,0,0,0,":\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl198 = {0,0,0,9,4,1,0,0,0,"else"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl199 = {0,0,0,9,1,1,0,0,0,"e"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl200 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl201 = {0,0,0,9,2,1,0,0,0,":\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[19]; } uf_sl202 = {0,0,0,9,18,1,0,0,0,"'i%d 'e%d if_else\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[9]; } uf_sl203 = {0,0,0,9,8,1,0,0,0,"'i%d if\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl204 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl205 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl206 = {0,0,0,9,1,1,0,0,0,"c"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl207 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[12]; } uf_sl208 = {0,0,0,9,11,1,0,0,0,":\n^fr@ not "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl209 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl210 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[10]; } uf_sl211 = {0,0,0,9,9,1,0,0,0," and ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl212 = {0,0,0,9,1,1,0,0,0,"b"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl213 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl214 = {0,0,0,9,2,1,0,0,0,":\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl215 = {0,0,0,9,16,1,0,0,0,"'c%d 'b%d while\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl216 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl217 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl218 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl219 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl220 = {0,0,0,9,1,1,0,0,0,"c"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl221 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[12]; } uf_sl222 = {0,0,0,9,11,1,0,0,0,":\n^fr@ not "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl223 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl224 = {0,0,0,9,4,1,0,0,0,"ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl225 = {0,0,0,9,1,1,0,0,0,"b"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl226 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl227 = {0,0,0,9,2,1,0,0,0,":\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl228 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl229 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl230 = {0,0,0,9,16,1,0,0,0,"'c%d 'b%d while\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl231 = {0,0,0,9,1,1,0,0,0,"="};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl232 = {0,0,0,9,5,1,0,0,0,"^%s!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl233 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl234 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl235 = {0,0,0,9,2,1,0,0,0,"1 "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl236 = {0,0,0,9,5,1,0,0,0," and "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl237 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl238 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl239 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl240 = {0,0,0,9,4,1,0,0,0,"1 df"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl241 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl242 = {0,0,0,9,2,1,0,0,0,"!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl243 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl244 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl245 = {0,0,0,9,1,1,0,0,0,"b"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl246 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl247 = {0,0,0,9,2,1,0,0,0,":\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl248 = {0,0,0,9,4,1,0,0,0,"0 df"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl249 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl250 = {0,0,0,9,2,1,0,0,0,"!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl251 = {0,0,0,9,5,1,0,0,0,"while"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl252 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl253 = {0,0,0,9,1,1,0,0,0,"c"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl254 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[14]; } uf_sl255 = {0,0,0,9,13,1,0,0,0,":\n^fr@ not df"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl256 = {0,0,0,9,2,1,0,0,0,"%d"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl257 = {0,0,0,9,2,1,0,0,0,"@ "};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[13]; } uf_sl258 = {0,0,0,9,12,1,0,0,0," or and ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl259 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl260 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl261 = {0,0,0,9,16,1,0,0,0,"'c%d 'b%d while\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[47]; } uf_sl262 = {0,0,0,9,46,1,0,0,0,"^fr@ ^wdone@ or not 'c%d 'b%d while\n0 ^wdone!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl263 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl264 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl265 = {0,0,0,9,5,1,0,0,0,"b%d:\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl266 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl267 = {0,0,0,9,16,1,0,0,0,"1 ^wdone! 0 ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[30]; } uf_sl268 = {0,0,0,9,29,1,0,0,0,"c%d:\n^fr@ ^wdone@ or not ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl269 = {0,0,0,9,16,1,0,0,0,"1 ^wdone! 0 ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[30]; } uf_sl270 = {0,0,0,9,29,1,0,0,0,"c%d:\n^fr@ ^wdone@ or not ret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl271 = {0,0,0,9,1,1,0,0,0,"}"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl272 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[7]; } uf_sl273 = {0,0,0,9,6,1,0,0,0,"break\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl274 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl275 = {0,0,0,9,5,1,0,0,0,"cont\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl276 = {0,0,0,9,1,1,0,0,0,"}"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl277 = {0,0,0,9,1,1,0,0,0,";"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl278 = {0,0,0,9,1,1,0,0,0,"("};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl279 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl280 = {0,0,0,9,4,1,0,0,0,"main"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl281 = {0,0,0,9,4,1,0,0,0,"main"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl282 = {0,0,0,9,1,1,0,0,0,"{"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl283 = {0,0,0,9,2,1,0,0,0,"%s"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl284 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl285 = {0,0,0,9,2,1,0,0,0,"%s"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl286 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[3]; } uf_sl287 = {0,0,0,9,2,1,0,0,0,"%s"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl288 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[8]; } uf_sl289 = {0,0,0,9,7,1,0,0,0,"entry:\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[5]; } uf_sl290 = {0,0,0,9,4,1,0,0,0,"%s:\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl291 = {0,0,0,9,1,1,0,0,0,")"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl292 = {0,0,0,9,1,1,0,0,0,","};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl293 = {0,0,0,9,1,1,0,0,0,"*"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[6]; } uf_sl294 = {0,0,0,9,5,1,0,0,0,"^%s!\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl295 = {0,0,0,9,1,1,0,0,0,"}"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[18]; } uf_sl296 = {0,0,0,9,17,1,0,0,0,"0 _call exit\nret\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[38]; } uf_sl297 = {0,0,0,9,37,1,0,0,0,"^fr@ ^rv@ mul ^frv! 0 ^fr! ret ^frv@\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl298 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[1]; } uf_sl299 = {0,0,0,9,0,1,0,0,0,""};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[2]; } uf_sl300 = {0,0,0,9,1,1,0,0,0,"r"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[32]; } uf_sl301 = {0,0,0,9,31,1,0,0,0,"import c\"printf\"(ptr,...)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl302 = {0,0,0,9,27,1,0,0,0,"import c\"malloc\"(int)->ptr\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[27]; } uf_sl303 = {0,0,0,9,26,1,0,0,0,"import c\"free\"(ptr)->void\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[26]; } uf_sl304 = {0,0,0,9,25,1,0,0,0,"import c\"puts\"(ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[29]; } uf_sl305 = {0,0,0,9,28,1,0,0,0,"import c\"putchar\"(int)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[26]; } uf_sl306 = {0,0,0,9,25,1,0,0,0,"import c\"getchar\"()->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[31]; } uf_sl307 = {0,0,0,9,30,1,0,0,0,"import c\"fputs\"(ptr,ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[40]; } uf_sl308 = {0,0,0,9,39,1,0,0,0,"import c\"fwrite\"(ptr,int,int,ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl309 = {0,0,0,9,27,1,0,0,0,"import c\"strlen\"(ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[32]; } uf_sl310 = {0,0,0,9,31,1,0,0,0,"import c\"strcmp\"(ptr,ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[37]; } uf_sl311 = {0,0,0,9,36,1,0,0,0,"import c\"strncmp\"(ptr,ptr,int)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[32]; } uf_sl312 = {0,0,0,9,31,1,0,0,0,"import c\"strcpy\"(ptr,ptr)->ptr\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[32]; } uf_sl313 = {0,0,0,9,31,1,0,0,0,"import c\"strcat\"(ptr,ptr)->ptr\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[27]; } uf_sl314 = {0,0,0,9,26,1,0,0,0,"import c\"exit\"(int)->void\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[31]; } uf_sl315 = {0,0,0,9,30,1,0,0,0,"import c\"fopen\"(ptr,ptr)->ptr\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[28]; } uf_sl316 = {0,0,0,9,27,1,0,0,0,"import c\"fclose\"(ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[27]; } uf_sl317 = {0,0,0,9,26,1,0,0,0,"import c\"fgetc\"(ptr)->int\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[32]; } uf_sl318 = {0,0,0,9,31,1,0,0,0,"import c\"strstr\"(ptr,ptr)->ptr\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[17]; } uf_sl319 = {0,0,0,9,16,1,0,0,0,"extern \"stdout\"\n"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[20]; } uf_sl320 = {0,0,0,9,19,1,0,0,0,"usage: trans file.c"};
static struct { void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[18]; } uf_sl321 = {0,0,0,9,17,1,0,0,0,"cannot open input"};
static void* uf_lits[] = {(void*)&uf_sl0,(void*)&uf_sl1,(void*)&uf_sl2,(void*)&uf_sl3,(void*)&uf_sl4,(void*)&uf_sl5,(void*)&uf_sl6,(void*)&uf_sl7,(void*)&uf_sl8,(void*)&uf_sl9,(void*)&uf_sl10,(void*)&uf_sl11,(void*)&uf_sl12,(void*)&uf_sl13,(void*)&uf_sl14,(void*)&uf_sl15,(void*)&uf_sl16,(void*)&uf_sl17,(void*)&uf_sl18,(void*)&uf_sl19,(void*)&uf_sl20,(void*)&uf_sl21,(void*)&uf_sl22,(void*)&uf_sl23,(void*)&uf_sl24,(void*)&uf_sl25,(void*)&uf_sl26,(void*)&uf_sl27,(void*)&uf_sl28,(void*)&uf_sl29,(void*)&uf_sl30,(void*)&uf_sl31,(void*)&uf_sl32,(void*)&uf_sl33,(void*)&uf_sl34,(void*)&uf_sl35,(void*)&uf_sl36,(void*)&uf_sl37,(void*)&uf_sl38,(void*)&uf_sl39,(void*)&uf_sl40,(void*)&uf_sl41,(void*)&uf_sl42,(void*)&uf_sl43,(void*)&uf_sl44,(void*)&uf_sl45,(void*)&uf_sl46,(void*)&uf_sl47,(void*)&uf_sl48,(void*)&uf_sl49,(void*)&uf_sl50,(void*)&uf_sl51,(void*)&uf_sl52,(void*)&uf_sl53,(void*)&uf_sl54,(void*)&uf_sl55,(void*)&uf_sl56,(void*)&uf_sl57,(void*)&uf_sl58,(void*)&uf_sl59,(void*)&uf_sl60,(void*)&uf_sl61,(void*)&uf_sl62,(void*)&uf_sl63,(void*)&uf_sl64,(void*)&uf_sl65,(void*)&uf_sl66,(void*)&uf_sl67,(void*)&uf_sl68,(void*)&uf_sl69,(void*)&uf_sl70,(void*)&uf_sl71,(void*)&uf_sl72,(void*)&uf_sl73,(void*)&uf_sl74,(void*)&uf_sl75,(void*)&uf_sl76,(void*)&uf_sl77,(void*)&uf_sl78,(void*)&uf_sl79,(void*)&uf_sl80,(void*)&uf_sl81,(void*)&uf_sl82,(void*)&uf_sl83,(void*)&uf_sl84,(void*)&uf_sl85,(void*)&uf_sl86,(void*)&uf_sl87,(void*)&uf_sl88,(void*)&uf_sl89,(void*)&uf_sl90,(void*)&uf_sl91,(void*)&uf_sl92,(void*)&uf_sl93,(void*)&uf_sl94,(void*)&uf_sl95,(void*)&uf_sl96,(void*)&uf_sl97,(void*)&uf_sl98,(void*)&uf_sl99,(void*)&uf_sl100,(void*)&uf_sl101,(void*)&uf_sl102,(void*)&uf_sl103,(void*)&uf_sl104,(void*)&uf_sl105,(void*)&uf_sl106,(void*)&uf_sl107,(void*)&uf_sl108,(void*)&uf_sl109,(void*)&uf_sl110,(void*)&uf_sl111,(void*)&uf_sl112,(void*)&uf_sl113,(void*)&uf_sl114,(void*)&uf_sl115,(void*)&uf_sl116,(void*)&uf_sl117,(void*)&uf_sl118,(void*)&uf_sl119,(void*)&uf_sl120,(void*)&uf_sl121,(void*)&uf_sl122,(void*)&uf_sl123,(void*)&uf_sl124,(void*)&uf_sl125,(void*)&uf_sl126,(void*)&uf_sl127,(void*)&uf_sl128,(void*)&uf_sl129,(void*)&uf_sl130,(void*)&uf_sl131,(void*)&uf_sl132,(void*)&uf_sl133,(void*)&uf_sl134,(void*)&uf_sl135,(void*)&uf_sl136,(void*)&uf_sl137,(void*)&uf_sl138,(void*)&uf_sl139,(void*)&uf_sl140,(void*)&uf_sl141,(void*)&uf_sl142,(void*)&uf_sl143,(void*)&uf_sl144,(void*)&uf_sl145,(void*)&uf_sl146,(void*)&uf_sl147,(void*)&uf_sl148,(void*)&uf_sl149,(void*)&uf_sl150,(void*)&uf_sl151,(void*)&uf_sl152,(void*)&uf_sl153,(void*)&uf_sl154,(void*)&uf_sl155,(void*)&uf_sl156,(void*)&uf_sl157,(void*)&uf_sl158,(void*)&uf_sl159,(void*)&uf_sl160,(void*)&uf_sl161,(void*)&uf_sl162,(void*)&uf_sl163,(void*)&uf_sl164,(void*)&uf_sl165,(void*)&uf_sl166,(void*)&uf_sl167,(void*)&uf_sl168,(void*)&uf_sl169,(void*)&uf_sl170,(void*)&uf_sl171,(void*)&uf_sl172,(void*)&uf_sl173,(void*)&uf_sl174,(void*)&uf_sl175,(void*)&uf_sl176,(void*)&uf_sl177,(void*)&uf_sl178,(void*)&uf_sl179,(void*)&uf_sl180,(void*)&uf_sl181,(void*)&uf_sl182,(void*)&uf_sl183,(void*)&uf_sl184,(void*)&uf_sl185,(void*)&uf_sl186,(void*)&uf_sl187,(void*)&uf_sl188,(void*)&uf_sl189,(void*)&uf_sl190,(void*)&uf_sl191,(void*)&uf_sl192,(void*)&uf_sl193,(void*)&uf_sl194,(void*)&uf_sl195,(void*)&uf_sl196,(void*)&uf_sl197,(void*)&uf_sl198,(void*)&uf_sl199,(void*)&uf_sl200,(void*)&uf_sl201,(void*)&uf_sl202,(void*)&uf_sl203,(void*)&uf_sl204,(void*)&uf_sl205,(void*)&uf_sl206,(void*)&uf_sl207,(void*)&uf_sl208,(void*)&uf_sl209,(void*)&uf_sl210,(void*)&uf_sl211,(void*)&uf_sl212,(void*)&uf_sl213,(void*)&uf_sl214,(void*)&uf_sl215,(void*)&uf_sl216,(void*)&uf_sl217,(void*)&uf_sl218,(void*)&uf_sl219,(void*)&uf_sl220,(void*)&uf_sl221,(void*)&uf_sl222,(void*)&uf_sl223,(void*)&uf_sl224,(void*)&uf_sl225,(void*)&uf_sl226,(void*)&uf_sl227,(void*)&uf_sl228,(void*)&uf_sl229,(void*)&uf_sl230,(void*)&uf_sl231,(void*)&uf_sl232,(void*)&uf_sl233,(void*)&uf_sl234,(void*)&uf_sl235,(void*)&uf_sl236,(void*)&uf_sl237,(void*)&uf_sl238,(void*)&uf_sl239,(void*)&uf_sl240,(void*)&uf_sl241,(void*)&uf_sl242,(void*)&uf_sl243,(void*)&uf_sl244,(void*)&uf_sl245,(void*)&uf_sl246,(void*)&uf_sl247,(void*)&uf_sl248,(void*)&uf_sl249,(void*)&uf_sl250,(void*)&uf_sl251,(void*)&uf_sl252,(void*)&uf_sl253,(void*)&uf_sl254,(void*)&uf_sl255,(void*)&uf_sl256,(void*)&uf_sl257,(void*)&uf_sl258,(void*)&uf_sl259,(void*)&uf_sl260,(void*)&uf_sl261,(void*)&uf_sl262,(void*)&uf_sl263,(void*)&uf_sl264,(void*)&uf_sl265,(void*)&uf_sl266,(void*)&uf_sl267,(void*)&uf_sl268,(void*)&uf_sl269,(void*)&uf_sl270,(void*)&uf_sl271,(void*)&uf_sl272,(void*)&uf_sl273,(void*)&uf_sl274,(void*)&uf_sl275,(void*)&uf_sl276,(void*)&uf_sl277,(void*)&uf_sl278,(void*)&uf_sl279,(void*)&uf_sl280,(void*)&uf_sl281,(void*)&uf_sl282,(void*)&uf_sl283,(void*)&uf_sl284,(void*)&uf_sl285,(void*)&uf_sl286,(void*)&uf_sl287,(void*)&uf_sl288,(void*)&uf_sl289,(void*)&uf_sl290,(void*)&uf_sl291,(void*)&uf_sl292,(void*)&uf_sl293,(void*)&uf_sl294,(void*)&uf_sl295,(void*)&uf_sl296,(void*)&uf_sl297,(void*)&uf_sl298,(void*)&uf_sl299,(void*)&uf_sl300,(void*)&uf_sl301,(void*)&uf_sl302,(void*)&uf_sl303,(void*)&uf_sl304,(void*)&uf_sl305,(void*)&uf_sl306,(void*)&uf_sl307,(void*)&uf_sl308,(void*)&uf_sl309,(void*)&uf_sl310,(void*)&uf_sl311,(void*)&uf_sl312,(void*)&uf_sl313,(void*)&uf_sl314,(void*)&uf_sl315,(void*)&uf_sl316,(void*)&uf_sl317,(void*)&uf_sl318,(void*)&uf_sl319,(void*)&uf_sl320,(void*)&uf_sl321};
extern char uf_x0[] __asm__("uf_argc");
extern char uf_x1[] __asm__("uf_argv");
extern int64_t uf_im0() __asm__("printf");
extern void* uf_im1() __asm__("malloc");
extern void* uf_im2() __asm__("fopen");
extern int64_t uf_im3() __asm__("fseek");
extern int64_t uf_im4() __asm__("ftell");
extern int64_t uf_im5() __asm__("fread");
extern int64_t uf_im6() __asm__("fclose");
extern int64_t uf_im7() __asm__("puts");
extern void uf_im8() __asm__("exit");
extern int64_t uf_im9() __asm__("strlen");
extern int64_t uf_im10() __asm__("strcmp");
extern void* uf_im11() __asm__("strcpy");
static Cell var_trans__zt;
static Cell var_trans__ps;
static Cell var_trans__zs;
static Cell var_trans__sp2;
static Cell var_trans__rr;
static Cell var_trans__ls;
static Cell var_trans__s;
static Cell var_trans__emode;
static Cell var_trans__inq;
static Cell var_trans__qout;
static Cell var_trans__douts;
static Cell var_trans__di;
static Cell var_trans__old;
static Cell var_trans__nv;
static Cell var_trans__n;
static Cell var_trans__sn;
static Cell var_trans__a2;
static Cell var_trans__b2;
static Cell var_trans__toks;
static Cell var_trans__pi;
static Cell var_trans__lbl;
static Cell var_trans__it;
static Cell var_trans__its;
static Cell var_trans__fid;
static Cell var_trans__nv2;
static Cell var_trans__vars;
static Cell var_trans__tm_pat;
static Cell var_trans__src;
static Cell var_trans__pos;
static Cell var_trans__srclen;
static Cell var_trans__tmm;
static Cell var_trans__rest;
static Cell var_trans__ln;
static Cell var_trans__fi;
static Cell var_trans__lv;
static Cell var_trans__pc_op;
static Cell var_trans__pc_slot;
static Cell var_trans__pc_o;
static Cell var_trans__lasts;
static Cell var_trans__ptk;
static Cell var_trans__zm;
static Cell var_trans__ci;
static Cell var_trans__didret;
static Cell var_trans__slot;
static Cell var_trans__slot2;
static Cell var_trans__inmain;
static Cell var_trans__cret;
static Cell var_trans__dchunk;
static Cell var_trans__pends;
static Cell var_trans__cp;
static Cell var_trans__psnaps;
static Cell var_trans__sv;
static Cell var_trans__tlbl;
static Cell var_trans__elbl;
static Cell var_trans__clbl;
static Cell var_trans__blbl;
static Cell var_trans__fclbl;
static Cell var_trans__fblbl;
static Cell var_trans__pfpi;
static Cell var_trans__pfd;
static Cell var_trans__pfpi2;
static Cell var_trans__dflbl;
static Cell var_trans__wclbl;
static Cell var_trans__wblbl;
static Cell var_trans__wchunk;
static Cell var_trans__wsv;
static Cell var_trans__wb;
static Cell var_trans__flabels;
static Cell var_trans__fname;
static Cell var_trans__pl;
static Cell var_trans__pfi;
static Cell var_trans__nt;
static Cell var_trans__path;
static Cell var_trans__f;
static Cell* uf_vroots[] = {&var_trans__zt,&var_trans__ps,&var_trans__zs,&var_trans__sp2,&var_trans__rr,&var_trans__ls,&var_trans__s,&var_trans__emode,&var_trans__inq,&var_trans__qout,&var_trans__douts,&var_trans__di,&var_trans__old,&var_trans__nv,&var_trans__n,&var_trans__sn,&var_trans__a2,&var_trans__b2,&var_trans__toks,&var_trans__pi,&var_trans__lbl,&var_trans__it,&var_trans__its,&var_trans__fid,&var_trans__nv2,&var_trans__vars,&var_trans__tm_pat,&var_trans__src,&var_trans__pos,&var_trans__srclen,&var_trans__tmm,&var_trans__rest,&var_trans__ln,&var_trans__fi,&var_trans__lv,&var_trans__pc_op,&var_trans__pc_slot,&var_trans__pc_o,&var_trans__lasts,&var_trans__ptk,&var_trans__zm,&var_trans__ci,&var_trans__didret,&var_trans__slot,&var_trans__slot2,&var_trans__inmain,&var_trans__cret,&var_trans__dchunk,&var_trans__pends,&var_trans__cp,&var_trans__psnaps,&var_trans__sv,&var_trans__tlbl,&var_trans__elbl,&var_trans__clbl,&var_trans__blbl,&var_trans__fclbl,&var_trans__fblbl,&var_trans__pfpi,&var_trans__pfd,&var_trans__pfpi2,&var_trans__dflbl,&var_trans__wclbl,&var_trans__wblbl,&var_trans__wchunk,&var_trans__wsv,&var_trans__wb,&var_trans__flabels,&var_trans__fname,&var_trans__pl,&var_trans__pfi,&var_trans__nt,&var_trans__path,&var_trans__f};
static long uf_lc_v[3141];
static void uf_init_locals(void){uf_lc_v[959]=0;uf_lc_v[22]=0;uf_lc_v[118]=0;uf_lc_v[111]=0;uf_lc_v[571]=0;uf_lc_v[97]=0;uf_lc_v[195]=0;uf_lc_v[89]=0;uf_lc_v[36]=0;uf_lc_v[753]=0;uf_lc_v[42]=0;uf_lc_v[926]=0;uf_lc_v[1889]=0;uf_lc_v[1765]=0;uf_lc_v[715]=0;uf_lc_v[1870]=0;uf_lc_v[248]=1;uf_lc_v[155]=0;uf_lc_v[734]=0;uf_lc_v[796]=0;uf_lc_v[8]=0;uf_lc_v[0]=0;uf_lc_v[30]=0;uf_lc_v[2991]=0;uf_lc_v[135]=2;uf_lc_v[395]=0;uf_lc_v[215]=0;uf_lc_v[566]=0;uf_lc_v[883]=0;uf_lc_v[107]=0;uf_lc_v[127]=0;}
static long uf_lc(long pc){ return (pc>=0&&(unsigned long)pc<(unsigned long)(sizeof(uf_lc_v)/sizeof(uf_lc_v[0])))?uf_lc_v[pc]:0; }

static void uflux_run(Ctx*cx, long pc){
  uf_current_ctx=cx;
  if(pc<0){ goto *(void*)uf_entry_addr; }
  /* v11: set up the entry label's local frame (v13: capacity-checked) */
  cx->local_frames[cx->local_fsp++]=cx->local_base; cx->local_base+=uf_lc(pc); if(cx->local_base>cx->local_cap)die("local frame overflow");
  static const void* labtab[] = {[0]=&&L_0,[50]=&&L_50,[55]=&&L_55,[62]=&&L_62,[69]=&&L_69,[144]=&&L_144,[199]=&&L_199,[209]=&&L_209,[241]=&&L_241,[268]=&&L_268,[276]=&&L_276,[281]=&&L_281,[289]=&&L_289,[306]=&&L_306,[313]=&&L_313,[325]=&&L_325,[332]=&&L_332,[344]=&&L_344,[351]=&&L_351,[363]=&&L_363,[370]=&&L_370,[382]=&&L_382,[389]=&&L_389,[414]=&&L_414,[438]=&&L_438,[450]=&&L_450,[456]=&&L_456,[465]=&&L_465,[478]=&&L_478,[486]=&&L_486,[491]=&&L_491,[500]=&&L_500,[513]=&&L_513,[521]=&&L_521,[526]=&&L_526,[535]=&&L_535,[548]=&&L_548,[556]=&&L_556,[561]=&&L_561,[581]=&&L_581,[585]=&&L_585,[618]=&&L_618,[626]=&&L_626,[644]=&&L_644,[655]=&&L_655,[678]=&&L_678,[685]=&&L_685,[694]=&&L_694,[703]=&&L_703,[712]=&&L_712,[720]=&&L_720,[727]=&&L_727,[739]=&&L_739,[746]=&&L_746,[758]=&&L_758,[769]=&&L_769,[778]=&&L_778,[787]=&&L_787,[801]=&&L_801,[820]=&&L_820,[829]=&&L_829,[838]=&&L_838,[847]=&&L_847,[856]=&&L_856,[865]=&&L_865,[874]=&&L_874,[888]=&&L_888,[899]=&&L_899,[908]=&&L_908,[917]=&&L_917,[940]=&&L_940,[947]=&&L_947,[956]=&&L_956,[966]=&&L_966,[975]=&&L_975,[984]=&&L_984,[993]=&&L_993,[1002]=&&L_1002,[1011]=&&L_1011,[1022]=&&L_1022,[1031]=&&L_1031,[1040]=&&L_1040,[1047]=&&L_1047,[1065]=&&L_1065,[1068]=&&L_1068,[1086]=&&L_1086,[1089]=&&L_1089,[1101]=&&L_1101,[1110]=&&L_1110,[1124]=&&L_1124,[1127]=&&L_1127,[1141]=&&L_1141,[1144]=&&L_1144,[1158]=&&L_1158,[1167]=&&L_1167,[1176]=&&L_1176,[1185]=&&L_1185,[1194]=&&L_1194,[1203]=&&L_1203,[1212]=&&L_1212,[1221]=&&L_1221,[1230]=&&L_1230,[1239]=&&L_1239,[1257]=&&L_1257,[1269]=&&L_1269,[1287]=&&L_1287,[1297]=&&L_1297,[1326]=&&L_1326,[1355]=&&L_1355,[1365]=&&L_1365,[1375]=&&L_1375,[1392]=&&L_1392,[1405]=&&L_1405,[1414]=&&L_1414,[1424]=&&L_1424,[1433]=&&L_1433,[1443]=&&L_1443,[1452]=&&L_1452,[1462]=&&L_1462,[1471]=&&L_1471,[1481]=&&L_1481,[1490]=&&L_1490,[1500]=&&L_1500,[1509]=&&L_1509,[1519]=&&L_1519,[1522]=&&L_1522,[1552]=&&L_1552,[1557]=&&L_1557,[1564]=&&L_1564,[1571]=&&L_1571,[1578]=&&L_1578,[1587]=&&L_1587,[1618]=&&L_1618,[1635]=&&L_1635,[1638]=&&L_1638,[1653]=&&L_1653,[1662]=&&L_1662,[1671]=&&L_1671,[1680]=&&L_1680,[1689]=&&L_1689,[1698]=&&L_1698,[1707]=&&L_1707,[1716]=&&L_1716,[1725]=&&L_1725,[1734]=&&L_1734,[1751]=&&L_1751,[1758]=&&L_1758,[1778]=&&L_1778,[1791]=&&L_1791,[1796]=&&L_1796,[1804]=&&L_1804,[1823]=&&L_1823,[1830]=&&L_1830,[1849]=&&L_1849,[1898]=&&L_1898,[1910]=&&L_1910,[1917]=&&L_1917,[1920]=&&L_1920,[1930]=&&L_1930,[1937]=&&L_1937,[1940]=&&L_1940,[2005]=&&L_2005,[2010]=&&L_2010,[2015]=&&L_2015,[2053]=&&L_2053,[2073]=&&L_2073,[2166]=&&L_2166,[2289]=&&L_2289,[2295]=&&L_2295,[2303]=&&L_2303,[2342]=&&L_2342,[2357]=&&L_2357,[2367]=&&L_2367,[2382]=&&L_2382,[2387]=&&L_2387,[2396]=&&L_2396,[2406]=&&L_2406,[2415]=&&L_2415,[2426]=&&L_2426,[2431]=&&L_2431,[2437]=&&L_2437,[2443]=&&L_2443,[2448]=&&L_2448,[2454]=&&L_2454,[2594]=&&L_2594,[2597]=&&L_2597,[2658]=&&L_2658,[2675]=&&L_2675,[2690]=&&L_2690,[2698]=&&L_2698,[2704]=&&L_2704,[2721]=&&L_2721,[2738]=&&L_2738,[2746]=&&L_2746,[2754]=&&L_2754,[2760]=&&L_2760,[2766]=&&L_2766,[2864]=&&L_2864,[2867]=&&L_2867,[2874]=&&L_2874,[2879]=&&L_2879,[2886]=&&L_2886,[2894]=&&L_2894,[2903]=&&L_2903,[2909]=&&L_2909,[2921]=&&L_2921,[2927]=&&L_2927,[2937]=&&L_2937,[2945]=&&L_2945,[2963]=&&L_2963,[2971]=&&L_2971,[2977]=&&L_2977,[2984]=&&L_2984,[2995]=&&L_2995,[3003]=&&L_3003,[3133]=&&L_3133,[3136]=&&L_3136,};
  if(pc==0) goto L_3009;
  goto *labtab[pc];
  static const struct { int64_t tk; int64_t mh; const void* lab; } uf_mt[] = {{0,0,&&L_0}};
L_0: Cell _pp0=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:pshL': missing parameter 'zt' (1 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_1: Cell t1=var_trans__ps;L_2: L_3: var_trans__zt=_pp0;pushc(cx,t1);pushc(cx,_pp0);uf_cur_op="op_push";op_push(cx);
L_4: Cell t2=pop(cx);L_5: var_trans__ps=t2;pushc(cx,t2);L_6: L_7: Cell _rv3=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv3);return;}cx->csp--;const void*_r4=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv3);if(!_r4)return;goto *_r4;}
L_8: Cell t5=var_trans__ps;L_9: L_10: L_11: var_trans__zs=t5;pushc(cx,t5);pushc(cx,t5);uf_cur_op="op_len";op_len(cx);
L_12: L_13: Cell t6=pop(cx);Cell t7=uf_csub(t6,uf_mki(1LL));L_14: L_15: Cell t8=var_trans__zs;L_16: L_17: var_trans__sp2=t7;pushc(cx,t7);pushc(cx,t8);pushc(cx,t7);uf_cur_op="op_get";op_get(cx);
L_18: Cell t9=pop(cx);L_19: var_trans__rr=t9;pushc(cx,t9);L_20: Cell t10=var_trans__rr;L_21: Cell _rv11=t10;{if(cx->csp==0){pushc(cx,_rv11);return;}cx->csp--;const void*_r12=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv11);if(!_r12)return;goto *_r12;}
L_22: Cell _pp13=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:lpushL': missing parameter 'zt' (1 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_23: Cell t14=var_trans__ls;L_24: L_25: var_trans__zt=_pp13;pushc(cx,t14);pushc(cx,_pp13);uf_cur_op="op_push";op_push(cx);
L_26: Cell t15=pop(cx);L_27: var_trans__ls=t15;pushc(cx,t15);L_28: L_29: Cell _rv16=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv16);return;}cx->csp--;const void*_r17=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv16);if(!_r17)return;goto *_r17;}
L_30: Cell t18=var_trans__ls;L_31: pushc(cx,t18);uf_cur_op="op_lpop";op_lpop(cx);
L_32: Cell t19=pop(cx);L_33: var_trans__rr=t19;pushc(cx,t19);L_34: Cell t20=var_trans__rr;L_35: Cell _rv21=t20;{if(cx->csp==0){pushc(cx,_rv21);return;}cx->csp--;const void*_r22=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv21);if(!_r22)return;goto *_r22;}
L_36: Cell t23=var_trans__ps;L_37: pushc(cx,t23);uf_cur_op="op_lpop";op_lpop(cx);
L_38: Cell t24=pop(cx);L_39: var_trans__rr=t24;pushc(cx,t24);L_40: Cell t25=var_trans__rr;L_41: Cell _rv26=t25;{if(cx->csp==0){pushc(cx,_rv26);return;}cx->csp--;const void*_r27=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv26);if(!_r27)return;goto *_r27;}
L_42: Cell _pp28=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:emit': missing parameter 's' (1 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_43: Cell t29=var_trans__emode;L_44: L_45: Cell t30=uf_ceq(t29,uf_mki(2LL));L_46: var_trans__s=_pp28;pushc(cx,t30);pushp(cx,(void*)&&L_69);
L_47: pushp(cx,(void*)&&L_50);
L_48: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_48,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_48,cx->sp>0?cx->sp-0:0);goto *el;}K_48:;}
L_49: Cell _rv31=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv31);return;}cx->csp--;const void*_r32=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv31);if(!_r32)return;goto *_r32;}
L_50: Cell t33=var_trans__inq;L_51: pushc(cx,t33);pushp(cx,(void*)&&L_62);
L_52: pushp(cx,(void*)&&L_55);
L_53: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_53,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_53,cx->sp>0?cx->sp-0:0);goto *el;}K_53:;}
L_54: Cell _rv34=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv34);return;}cx->csp--;const void*_r35=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv34);if(!_r35)return;goto *_r35;}
L_55: Cell t36=var_trans__s;L_56: Cell t37=uf_mkp((void*)&uf_sl0);L_57: pushc(cx,t36);pushc(cx,t37);uf_cur_op="op_fmt";op_fmt(cx);
L_58: uf_cur_op="op_print";op_print(cx);
L_59: L_60: L_61: Cell _rv38=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv38);return;}cx->csp--;const void*_r39=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv38);if(!_r39)return;goto *_r39;}
L_62: Cell t40=var_trans__qout;L_63: Cell t41=var_trans__s;L_64: pushc(cx,t40);pushc(cx,t41);uf_cur_op="op_cat";op_cat(cx);
L_65: Cell t42=pop(cx);L_66: var_trans__qout=t42;pushc(cx,t42);L_67: L_68: Cell _rv43=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv43);return;}cx->csp--;const void*_r44=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv43);if(!_r44)return;goto *_r44;}
L_69: Cell t45=var_trans__douts;L_70: pushc(cx,t45);uf_cur_op="op_len";op_len(cx);
L_71: L_72: Cell t46=pop(cx);Cell t47=uf_csub(t46,uf_mki(1LL));L_73: L_74: Cell t48=var_trans__douts;L_75: L_76: var_trans__di=t47;pushc(cx,t47);pushc(cx,t48);pushc(cx,t47);uf_cur_op="op_get";op_get(cx);
L_77: Cell t49=pop(cx);L_78: L_79: Cell t50=var_trans__s;L_80: var_trans__old=t49;pushc(cx,t49);pushc(cx,t49);pushc(cx,t50);uf_cur_op="op_cat";op_cat(cx);
L_81: Cell t51=pop(cx);L_82: Cell t52=var_trans__douts;L_83: Cell t53=var_trans__di;L_84: L_85: var_trans__nv=t51;pushc(cx,t51);pushc(cx,t52);pushc(cx,t53);pushc(cx,t51);uf_cur_op="op_set";op_set(cx);
L_86: L_87: L_88: Cell _rv54=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv54);return;}cx->csp--;const void*_r55=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv54);if(!_r55)return;goto *_r55;}
L_89: Cell _pp56=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:emitn': missing parameter 'n' (1 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_90: L_91: Cell t57=uf_mkp((void*)&uf_sl1);L_92: var_trans__n=_pp56;pushc(cx,_pp56);pushc(cx,t57);uf_cur_op="op_fmt";op_fmt(cx);
L_93: Cell t58=pop(cx);L_94: L_95: var_trans__sn=t58;pushc(cx,t58);pushc(cx,t58);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_95,cx->sp>1?cx->sp-1:0);goto L_42;K_95:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_96: Cell _rv59=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv59);return;}cx->csp--;const void*_r60=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv59);if(!_r60)return;goto *_r60;}
L_97: Cell _pp61=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)+1&&cx->sp>0)?pop(cx):(die("label 'trans:streq': missing parameter 'a2' (2 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_98: Cell _pp62=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:streq': missing parameter 'b2' (2 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_99: L_100: L_101: var_trans__a2=_pp61;var_trans__b2=_pp62;pushc(cx,_pp61);pushc(cx,_pp62);uf_cur_op="strcmp";{Cell a1=pop(cx);Cell a0=pop(cx);int r=((int(*)(void*,void*))uf_im10)((void*)uf_sptr(a0),(void*)uf_sptr(a1));pushi(cx,(int64_t)r);}
L_102: Cell t63=pop(cx);Cell t64=uf_cnot(t63);L_103: L_104: var_trans__rr=t64;pushc(cx,t64);L_105: Cell t65=var_trans__rr;L_106: Cell _rv66=t65;{if(cx->csp==0){pushc(cx,_rv66);return;}cx->csp--;const void*_r67=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv66);if(!_r67)return;goto *_r67;}
L_107: uf_cur_op="puts";{Cell a0=pop(cx);int r=((int(*)(void*))uf_im7)((void*)uf_sptr(a0));pushi(cx,(int64_t)r);}
L_108: L_109: pushi(cx,1LL);uf_cur_op="exit";{Cell a0=pop(cx);((void(*)(int64_t))uf_im8)((int64_t)(a0.tag==T_FLOAT?(int64_t)uf_f(a0):a0.i));}
L_110: Cell _rv68=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv68);return;}cx->csp--;const void*_r69=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv68);if(!_r69)return;goto *_r69;}
L_111: Cell t70=var_trans__toks;L_112: Cell t71=var_trans__pi;L_113: pushc(cx,t70);pushc(cx,t71);uf_cur_op="op_get";op_get(cx);
L_114: Cell t72=pop(cx);L_115: var_trans__rr=t72;pushc(cx,t72);L_116: Cell t73=var_trans__rr;L_117: Cell _rv74=t73;{if(cx->csp==0){pushc(cx,_rv74);return;}cx->csp--;const void*_r75=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv74);if(!_r75)return;goto *_r75;}
L_118: Cell t76=var_trans__toks;L_119: Cell t77=var_trans__pi;L_120: L_121: Cell t78=uf_cadd(t77,uf_mki(1LL));L_122: pushc(cx,t76);pushc(cx,t78);uf_cur_op="op_get";op_get(cx);
L_123: Cell t79=pop(cx);L_124: var_trans__rr=t79;pushc(cx,t79);L_125: Cell t80=var_trans__rr;L_126: Cell _rv81=t80;{if(cx->csp==0){pushc(cx,_rv81);return;}cx->csp--;const void*_r82=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv81);if(!_r82)return;goto *_r82;}
L_127: Cell t83=var_trans__pi;L_128: L_129: Cell t84=uf_cadd(t83,uf_mki(1LL));L_130: L_131: L_132: var_trans__pi=t84;var_trans__rr=t84;pushc(cx,t84);L_133: Cell t85=var_trans__rr;L_134: Cell _rv86=t85;{if(cx->csp==0){pushc(cx,_rv86);return;}cx->csp--;const void*_r87=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv86);if(!_r87)return;goto *_r87;}
L_135: Cell t88=var_trans__lbl;L_136: L_137: Cell t89=uf_cadd(t88,uf_mki(1LL));L_138: L_139: L_140: L_141: var_trans__lbl=t89;var_trans__rr=t89;pushc(cx,t89);pushc(cx,t89);L_142: Cell t90=var_trans__rr;L_143: Cell _rv91=t90;{if(cx->csp==0){pushc(cx,_rv91);return;}cx->csp--;const void*_r92=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv91);if(!_r92)return;goto *_r92;}
L_144: Cell t93=uf_mkp((void*)&uf_sl2);L_145: Cell t94=cx->locals[cx->local_base+0];L_146: pushc(cx,t93);pushc(cx,t94);uf_cur_op="op_cat";op_cat(cx);
L_147: Cell t95=uf_mkp((void*)&uf_sl3);L_148: pushc(cx,t95);uf_cur_op="op_cat";op_cat(cx);
L_149: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_149,cx->sp>0?cx->sp-0:0);goto L_111;K_149:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_150: uf_cur_op="op_cat";op_cat(cx);
L_151: Cell t96=uf_mkp((void*)&uf_sl4);L_152: pushc(cx,t96);uf_cur_op="op_cat";op_cat(cx);
L_153: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_153,cx->sp>0?cx->sp-0:0);goto L_107;K_153:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_154: Cell _rv97=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv97);return;}cx->csp--;const void*_r98=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv97);if(!_r98)return;goto *_r98;}
L_155: Cell _pp99=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:istype': missing parameter 'it' (1 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_156: L_157: Cell t100=uf_mkp((void*)&uf_sl5);L_158: var_trans__it=_pp99;pushc(cx,_pp99);pushc(cx,t100);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_158,cx->sp>2?cx->sp-2:0);goto L_97;K_158:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_159: Cell t101=var_trans__it;L_160: Cell t102=uf_mkp((void*)&uf_sl6);L_161: pushc(cx,t101);pushc(cx,t102);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_161,cx->sp>2?cx->sp-2:0);goto L_97;K_161:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_162: Cell t103=pop(cx);Cell t104=pop(cx);Cell t105=uf_cadd(t104,t103);L_163: Cell t106=var_trans__it;L_164: Cell t107=uf_mkp((void*)&uf_sl7);L_165: pushc(cx,t105);pushc(cx,t106);pushc(cx,t107);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_165,cx->sp>2?cx->sp-2:0);goto L_97;K_165:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_166: Cell t108=pop(cx);Cell t109=pop(cx);Cell t110=uf_cadd(t109,t108);L_167: Cell t111=var_trans__it;L_168: Cell t112=uf_mkp((void*)&uf_sl8);L_169: pushc(cx,t110);pushc(cx,t111);pushc(cx,t112);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_169,cx->sp>2?cx->sp-2:0);goto L_97;K_169:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_170: Cell t113=pop(cx);Cell t114=pop(cx);Cell t115=uf_cadd(t114,t113);L_171: Cell t116=var_trans__it;L_172: Cell t117=uf_mkp((void*)&uf_sl9);L_173: pushc(cx,t115);pushc(cx,t116);pushc(cx,t117);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_173,cx->sp>2?cx->sp-2:0);goto L_97;K_173:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_174: Cell t118=pop(cx);Cell t119=pop(cx);Cell t120=uf_cadd(t119,t118);L_175: Cell t121=var_trans__it;L_176: Cell t122=uf_mkp((void*)&uf_sl10);L_177: pushc(cx,t120);pushc(cx,t121);pushc(cx,t122);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_177,cx->sp>2?cx->sp-2:0);goto L_97;K_177:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_178: Cell t123=pop(cx);Cell t124=pop(cx);Cell t125=uf_cadd(t124,t123);L_179: Cell t126=var_trans__it;L_180: Cell t127=uf_mkp((void*)&uf_sl11);L_181: pushc(cx,t125);pushc(cx,t126);pushc(cx,t127);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_181,cx->sp>2?cx->sp-2:0);goto L_97;K_181:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_182: Cell t128=pop(cx);Cell t129=pop(cx);Cell t130=uf_cadd(t129,t128);L_183: Cell t131=var_trans__it;L_184: Cell t132=uf_mkp((void*)&uf_sl12);L_185: pushc(cx,t130);pushc(cx,t131);pushc(cx,t132);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_185,cx->sp>2?cx->sp-2:0);goto L_97;K_185:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_186: Cell t133=pop(cx);Cell t134=pop(cx);Cell t135=uf_cadd(t134,t133);L_187: Cell t136=var_trans__it;L_188: Cell t137=uf_mkp((void*)&uf_sl13);L_189: pushc(cx,t135);pushc(cx,t136);pushc(cx,t137);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_189,cx->sp>2?cx->sp-2:0);goto L_97;K_189:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_190: Cell t138=pop(cx);Cell t139=pop(cx);Cell t140=uf_cadd(t139,t138);L_191: L_192: var_trans__its=t140;pushc(cx,t140);L_193: Cell t141=var_trans__its;L_194: Cell _rv142=t141;{if(cx->csp==0){pushc(cx,_rv142);return;}cx->csp--;const void*_r143=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv142);if(!_r143)return;goto *_r143;}
L_195: pushp(cx,(void*)&&L_199);
L_196: pushp(cx,(void*)&&L_209);
L_197: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_197:;cx->loops[fr].cont=&&K_WT_197;cx->loops[fr].end=&&K_WE_197;
uf_cspush(cx,&&K_WC_197,cx->sp>0?cx->sp-0:0);goto *cnd;K_WC_197:;
if(uf_zero(pop(cx)))goto K_WE_197;
uf_cspush(cx,&&K_WB_197,cx->sp>0?cx->sp-0:0);goto *bod;K_WB_197:;pop(cx);
goto K_WT_197;
K_WE_197:;cx->lsp=fr;}
L_198: Cell _rv144=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv144);return;}cx->csp--;const void*_r145=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv144);if(!_r145)return;goto *_r145;}
L_199: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_199,cx->sp>0?cx->sp-0:0);goto L_111;K_199:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_200: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_200,cx->sp>1?cx->sp-1:0);goto L_155;K_200:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_201: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_201,cx->sp>0?cx->sp-0:0);goto L_111;K_201:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_202: Cell t146=uf_mkp((void*)&uf_sl14);L_203: pushc(cx,t146);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_203,cx->sp>2?cx->sp-2:0);goto L_97;K_203:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_204: Cell t147=pop(cx);Cell t148=pop(cx);Cell t149=uf_cadd(t148,t147);L_205: L_206: var_trans__rr=t149;pushc(cx,t149);L_207: Cell t150=var_trans__rr;L_208: Cell _rv151=t150;{if(cx->csp==0){pushc(cx,_rv151);return;}cx->csp--;const void*_r152=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv151);if(!_r152)return;goto *_r152;}
L_209: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_209,cx->sp>0?cx->sp-0:0);goto L_127;K_209:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_210: L_211: L_212: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_213: Cell t153=var_trans__rr;L_214: Cell _rv154=t153;{if(cx->csp==0){pushc(cx,_rv154);return;}cx->csp--;const void*_r155=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv154);if(!_r155)return;goto *_r155;}
L_215: Cell _pp156=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:newvar': missing parameter 'nv' (1 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_216: Cell t157=uf_mkp((void*)&uf_sl15);L_217: Cell t158=var_trans__fid;L_218: Cell t159=uf_mkp((void*)&uf_sl16);L_219: var_trans__nv=_pp156;pushc(cx,t157);pushc(cx,t158);pushc(cx,t159);uf_cur_op="op_fmt";op_fmt(cx);
L_220: uf_cur_op="op_cat";op_cat(cx);
L_221: Cell t160=pop(cx);L_222: Cell t161=var_trans__fid;L_223: L_224: Cell t162=uf_cadd(t161,uf_mki(1LL));L_225: L_226: Cell t163=var_trans__vars;L_227: Cell t164=var_trans__nv;L_228: L_229: var_trans__nv2=t160;var_trans__fid=t162;pushc(cx,t160);pushc(cx,t162);pushc(cx,t163);pushc(cx,t164);pushc(cx,t160);uf_cur_op="op_set";op_set(cx);
L_230: L_231: Cell t165=var_trans__nv2;L_232: Cell _rv166=t165;{if(cx->csp==0){pushc(cx,_rv166);return;}cx->csp--;const void*_r167=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv166);if(!_r167)return;goto *_r167;}
L_233: Cell _pp168=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:findvar': missing parameter 'nv' (1 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_234: Cell t169=var_trans__vars;L_235: L_236: var_trans__nv=_pp168;pushc(cx,t169);pushc(cx,_pp168);uf_cur_op="op_getq";op_getq(cx);
L_237: Cell t170=pop(cx);Cell t171=uf_cnot(t170);L_238: pushc(cx,t171);pushp(cx,(void*)&&L_241);
L_239: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_239,cx->sp>0?cx->sp-0:0);goto *b;K_239:;}}
L_240: Cell _rv172=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv172);return;}cx->csp--;const void*_r173=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv172);if(!_r173)return;goto *_r173;}
L_241: Cell t174=uf_mkp((void*)&uf_sl17);L_242: Cell t175=var_trans__nv;L_243: pushc(cx,t174);pushc(cx,t175);uf_cur_op="op_cat";op_cat(cx);
L_244: Cell t176=uf_mkp((void*)&uf_sl18);L_245: pushc(cx,t176);uf_cur_op="op_cat";op_cat(cx);
L_246: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_246,cx->sp>0?cx->sp-0:0);goto L_107;K_246:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_247: Cell _rv177=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv177);return;}cx->csp--;const void*_r178=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv177);if(!_r178)return;goto *_r178;}
L_248: Cell _pp179=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:trymatch': missing parameter 'tm_pat' (1 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_249: Cell t180=var_trans__src;L_250: Cell t181=var_trans__pos;L_251: Cell t182=var_trans__srclen;L_252: var_trans__tm_pat=_pp179;pushc(cx,t180);pushc(cx,t181);pushc(cx,t182);uf_cur_op="op_slice";op_slice(cx);
L_253: Cell t183=var_trans__tm_pat;L_254: pushc(cx,t183);uf_cur_op="op_match";op_match(cx);
L_255: Cell t184=pop(cx);cx->locals[cx->local_base+0]=t184;L_256: Cell t185=cx->locals[cx->local_base+0];L_257: L_258: pushc(cx,t184);pushc(cx,t185);pushi(cx,0LL);uf_cur_op="op_getq";op_getq(cx);
L_259: Cell t186=pop(cx);L_260: L_261: var_trans__tmm=t186;pushc(cx,t186);pushc(cx,t186);uf_cur_op="op_len";op_len(cx);
L_262: L_263: Cell t187=pop(cx);Cell t188=uf_cgt(t187,uf_mki(0LL));L_264: pushc(cx,t188);pushp(cx,(void*)&&L_268);
L_265: pushp(cx,(void*)&&L_276);
L_266: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_266,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_266,cx->sp>0?cx->sp-0:0);goto *el;}K_266:;}
L_267: Cell _rv189=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv189);return;}cx->csp--;const void*_r190=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv189);if(!_r190)return;goto *_r190;}
L_268: Cell t191=var_trans__tmm;L_269: L_270: pushc(cx,t191);pushi(cx,0LL);uf_cur_op="op_get";op_get(cx);
L_271: uf_cur_op="strlen";{Cell a0=pop(cx);int r=((int(*)(void*))uf_im9)((void*)uf_sptr(a0));pushi(cx,(int64_t)r);}
L_272: Cell t192=pop(cx);L_273: var_trans__rr=t192;pushc(cx,t192);L_274: Cell t193=var_trans__rr;L_275: Cell _rv194=t193;{if(cx->csp==0){pushc(cx,_rv194);return;}cx->csp--;const void*_r195=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv194);if(!_r195)return;goto *_r195;}
L_276: L_277: L_278: var_trans__rr=uf_mki(-1LL);pushi(cx,-1LL);L_279: Cell t196=var_trans__rr;L_280: Cell _rv197=t196;{if(cx->csp==0){pushc(cx,_rv197);return;}cx->csp--;const void*_r198=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv197);if(!_r198)return;goto *_r198;}
L_281: pushp(cx,(void*)&&L_414);
L_282: pushp(cx,(void*)&&L_438);
L_283: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_283:;cx->loops[fr].cont=&&K_WT_283;cx->loops[fr].end=&&K_WE_283;
uf_cspush(cx,&&K_WC_283,cx->sp>0?cx->sp-0:0);goto *cnd;K_WC_283:;
if(uf_zero(pop(cx)))goto K_WE_283;
uf_cspush(cx,&&K_WB_283,cx->sp>0?cx->sp-0:0);goto *bod;K_WB_283:;pop(cx);
goto K_WT_283;
K_WE_283:;cx->lsp=fr;}
L_284: L_285: Cell t199=var_trans__pos;L_286: Cell t200=var_trans__srclen;L_287: Cell t201=uf_clt(t199,t200);L_288: Cell _rv202=t201;{if(cx->csp==0){pushc(cx,_rv202);return;}cx->csp--;const void*_r203=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv202);if(!_r203)return;goto *_r203;}
L_289: Cell t204=var_trans__src;L_290: Cell t205=var_trans__pos;L_291: Cell t206=var_trans__srclen;L_292: pushc(cx,t204);pushc(cx,t205);pushc(cx,t206);uf_cur_op="op_slice";op_slice(cx);
L_293: Cell t207=pop(cx);L_294: L_295: Cell t208=uf_mkp((void*)&uf_sl19);L_296: var_trans__rest=t207;pushc(cx,t207);pushc(cx,t207);pushc(cx,t208);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=1;uf_cspush(cx,&&K_296,cx->sp>1?cx->sp-1:0);goto L_248;K_296:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_297: Cell t209=pop(cx);L_298: L_299: L_300: Cell t210=uf_ceq(t209,uf_mki(-1LL));L_301: Cell t211=uf_cnot(t210);L_302: var_trans__ln=t209;pushc(cx,t209);pushc(cx,t211);pushp(cx,(void*)&&L_306);
L_303: pushp(cx,(void*)&&L_313);
L_304: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_304,cx->sp>1?cx->sp-1:0);goto *th;}else{uf_cspush(cx,&&K_304,cx->sp>0?cx->sp-0:0);goto *el;}K_304:;}
L_305: Cell _rv212=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv212);return;}cx->csp--;const void*_r213=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv212);if(!_r213)return;goto *_r213;}
L_306: Cell _pp214=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:lex_str': missing parameter 'ln' (1 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_307: var_trans__ln=_pp214;cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_307,cx->sp>0?cx->sp-0:0);goto L_395;K_307:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_308: L_309: L_310: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_311: Cell t215=var_trans__rr;L_312: Cell _rv216=t215;{if(cx->csp==0){pushc(cx,_rv216);return;}cx->csp--;const void*_r217=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv216);if(!_r217)return;goto *_r217;}
L_313: Cell t218=var_trans__rest;L_314: Cell t219=uf_mkp((void*)&uf_sl20);L_315: pushc(cx,t218);pushc(cx,t219);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=1;uf_cspush(cx,&&K_315,cx->sp>1?cx->sp-1:0);goto L_248;K_315:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_316: Cell t220=pop(cx);L_317: L_318: L_319: Cell t221=uf_ceq(t220,uf_mki(-1LL));L_320: Cell t222=uf_cnot(t221);L_321: var_trans__ln=t220;pushc(cx,t220);pushc(cx,t222);pushp(cx,(void*)&&L_325);
L_322: pushp(cx,(void*)&&L_332);
L_323: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_323,cx->sp>1?cx->sp-1:0);goto *th;}else{uf_cspush(cx,&&K_323,cx->sp>0?cx->sp-0:0);goto *el;}K_323:;}
L_324: Cell _rv223=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv223);return;}cx->csp--;const void*_r224=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv223);if(!_r224)return;goto *_r224;}
L_325: Cell _pp225=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:lex_chr': missing parameter 'ln' (1 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_326: var_trans__ln=_pp225;cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_326,cx->sp>0?cx->sp-0:0);goto L_395;K_326:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_327: L_328: L_329: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_330: Cell t226=var_trans__rr;L_331: Cell _rv227=t226;{if(cx->csp==0){pushc(cx,_rv227);return;}cx->csp--;const void*_r228=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv227);if(!_r228)return;goto *_r228;}
L_332: Cell t229=var_trans__rest;L_333: Cell t230=uf_mkp((void*)&uf_sl21);L_334: pushc(cx,t229);pushc(cx,t230);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=1;uf_cspush(cx,&&K_334,cx->sp>1?cx->sp-1:0);goto L_248;K_334:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_335: Cell t231=pop(cx);L_336: L_337: L_338: Cell t232=uf_ceq(t231,uf_mki(-1LL));L_339: Cell t233=uf_cnot(t232);L_340: var_trans__ln=t231;pushc(cx,t231);pushc(cx,t233);pushp(cx,(void*)&&L_344);
L_341: pushp(cx,(void*)&&L_351);
L_342: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_342,cx->sp>1?cx->sp-1:0);goto *th;}else{uf_cspush(cx,&&K_342,cx->sp>0?cx->sp-0:0);goto *el;}K_342:;}
L_343: Cell _rv234=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv234);return;}cx->csp--;const void*_r235=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv234);if(!_r235)return;goto *_r235;}
L_344: Cell _pp236=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:lex_num': missing parameter 'ln' (1 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_345: var_trans__ln=_pp236;cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_345,cx->sp>0?cx->sp-0:0);goto L_395;K_345:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_346: L_347: L_348: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_349: Cell t237=var_trans__rr;L_350: Cell _rv238=t237;{if(cx->csp==0){pushc(cx,_rv238);return;}cx->csp--;const void*_r239=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv238);if(!_r239)return;goto *_r239;}
L_351: Cell t240=var_trans__rest;L_352: Cell t241=uf_mkp((void*)&uf_sl22);L_353: pushc(cx,t240);pushc(cx,t241);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=1;uf_cspush(cx,&&K_353,cx->sp>1?cx->sp-1:0);goto L_248;K_353:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_354: Cell t242=pop(cx);L_355: L_356: L_357: Cell t243=uf_ceq(t242,uf_mki(-1LL));L_358: Cell t244=uf_cnot(t243);L_359: var_trans__ln=t242;pushc(cx,t242);pushc(cx,t244);pushp(cx,(void*)&&L_363);
L_360: pushp(cx,(void*)&&L_370);
L_361: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_361,cx->sp>1?cx->sp-1:0);goto *th;}else{uf_cspush(cx,&&K_361,cx->sp>0?cx->sp-0:0);goto *el;}K_361:;}
L_362: Cell _rv245=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv245);return;}cx->csp--;const void*_r246=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv245);if(!_r246)return;goto *_r246;}
L_363: Cell _pp247=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:lex_id': missing parameter 'ln' (1 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_364: var_trans__ln=_pp247;cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_364,cx->sp>0?cx->sp-0:0);goto L_395;K_364:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_365: L_366: L_367: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_368: Cell t248=var_trans__rr;L_369: Cell _rv249=t248;{if(cx->csp==0){pushc(cx,_rv249);return;}cx->csp--;const void*_r250=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv249);if(!_r250)return;goto *_r250;}
L_370: Cell t251=var_trans__rest;L_371: Cell t252=uf_mkp((void*)&uf_sl23);L_372: pushc(cx,t251);pushc(cx,t252);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=1;uf_cspush(cx,&&K_372,cx->sp>1?cx->sp-1:0);goto L_248;K_372:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_373: Cell t253=pop(cx);L_374: L_375: L_376: Cell t254=uf_ceq(t253,uf_mki(-1LL));L_377: Cell t255=uf_cnot(t254);L_378: var_trans__ln=t253;pushc(cx,t253);pushc(cx,t255);pushp(cx,(void*)&&L_382);
L_379: pushp(cx,(void*)&&L_389);
L_380: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_380,cx->sp>1?cx->sp-1:0);goto *th;}else{uf_cspush(cx,&&K_380,cx->sp>0?cx->sp-0:0);goto *el;}K_380:;}
L_381: Cell _rv256=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv256);return;}cx->csp--;const void*_r257=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv256);if(!_r257)return;goto *_r257;}
L_382: Cell _pp258=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?pop(cx):(die("label 'trans:lex_mop': missing parameter 'ln' (1 parameter(s) declared, fewer present at runtime — null-fill was removed)"),uf_mki(0));L_383: var_trans__ln=_pp258;cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_383,cx->sp>0?cx->sp-0:0);goto L_395;K_383:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_384: L_385: L_386: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_387: Cell t259=var_trans__rr;L_388: Cell _rv260=t259;{if(cx->csp==0){pushc(cx,_rv260);return;}cx->csp--;const void*_r261=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv260);if(!_r261)return;goto *_r261;}
L_389: L_390: L_391: var_trans__ln=uf_mki(1LL);pushi(cx,1LL);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_391,cx->sp>0?cx->sp-0:0);goto L_395;K_391:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_392: L_393: L_394: Cell _rv262=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv262);return;}cx->csp--;const void*_r263=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv262);if(!_r263)return;goto *_r263;}
L_395: Cell t264=var_trans__src;L_396: Cell t265=var_trans__pos;L_397: L_398: Cell t266=var_trans__ln;L_399: Cell t267=uf_cadd(var_trans__pos,t266);L_400: pushc(cx,t264);pushc(cx,t265);pushc(cx,t267);uf_cur_op="op_slice";op_slice(cx);
L_401: Cell t268=pop(cx);L_402: Cell t269=var_trans__toks;L_403: L_404: var_trans__zs=t268;pushc(cx,t268);pushc(cx,t269);pushc(cx,t268);uf_cur_op="op_push";op_push(cx);
L_405: Cell t270=pop(cx);L_406: Cell t271=var_trans__pos;L_407: Cell t272=var_trans__ln;L_408: Cell t273=uf_cadd(t271,t272);L_409: L_410: L_411: var_trans__toks=t270;var_trans__pos=t273;var_trans__rr=t273;pushc(cx,t270);pushc(cx,t273);L_412: Cell t274=var_trans__rr;L_413: Cell _rv275=t274;{if(cx->csp==0){pushc(cx,_rv275);return;}cx->csp--;const void*_r276=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv275);if(!_r276)return;goto *_r276;}
L_414: Cell t277=var_trans__src;L_415: Cell t278=var_trans__pos;L_416: Cell t279=var_trans__srclen;L_417: pushc(cx,t277);pushc(cx,t278);pushc(cx,t279);uf_cur_op="op_slice";op_slice(cx);
L_418: Cell t280=pop(cx);L_419: L_420: Cell t281=uf_mkp((void*)&uf_sl24);L_421: var_trans__rest=t280;pushc(cx,t280);pushc(cx,t280);pushc(cx,t281);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=1;uf_cspush(cx,&&K_421,cx->sp>1?cx->sp-1:0);goto L_248;K_421:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_422: L_423: Cell t282=pop(cx);Cell t283=uf_ceq(t282,uf_mki(-1LL));L_424: Cell t284=uf_cnot(t283);L_425: Cell t285=var_trans__rest;L_426: Cell t286=uf_mkp((void*)&uf_sl25);L_427: pushc(cx,t284);pushc(cx,t285);pushc(cx,t286);uf_cur_op="op_starts";op_starts(cx);
L_428: Cell t287=pop(cx);Cell t288=pop(cx);Cell t289=uf_cadd(t288,t287);L_429: Cell t290=var_trans__rest;L_430: Cell t291=uf_mkp((void*)&uf_sl26);L_431: pushc(cx,t289);pushc(cx,t290);pushc(cx,t291);uf_cur_op="op_starts";op_starts(cx);
L_432: Cell t292=pop(cx);Cell t293=pop(cx);Cell t294=uf_cadd(t293,t292);L_433: Cell t295=var_trans__rest;L_434: Cell t296=uf_mkp((void*)&uf_sl27);L_435: pushc(cx,t294);pushc(cx,t295);pushc(cx,t296);uf_cur_op="op_starts";op_starts(cx);
L_436: Cell t297=pop(cx);Cell t298=pop(cx);Cell t299=uf_cadd(t298,t297);L_437: Cell _rv300=t299;{if(cx->csp==0){pushc(cx,_rv300);return;}cx->csp--;const void*_r301=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv300);if(!_r301)return;goto *_r301;}
L_438: Cell t302=var_trans__rest;L_439: Cell t303=uf_mkp((void*)&uf_sl28);L_440: pushc(cx,t302);pushc(cx,t303);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=1;uf_cspush(cx,&&K_440,cx->sp>1?cx->sp-1:0);goto L_248;K_440:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_441: Cell t304=pop(cx);L_442: L_443: L_444: Cell t305=uf_ceq(t304,uf_mki(-1LL));L_445: Cell t306=uf_cnot(t305);L_446: var_trans__ln=t304;pushc(cx,t304);pushc(cx,t306);pushp(cx,(void*)&&L_450);
L_447: pushp(cx,(void*)&&L_456);
L_448: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_448,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_448,cx->sp>0?cx->sp-0:0);goto *el;}K_448:;}
L_449: Cell _rv307=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv307);return;}cx->csp--;const void*_r308=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv307);if(!_r308)return;goto *_r308;}
L_450: Cell t309=var_trans__pos;L_451: Cell t310=pop(cx);Cell t311=uf_cadd(t310,t309);L_452: L_453: var_trans__pos=t311;pushc(cx,t311);L_454: L_455: Cell _rv312=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv312);return;}cx->csp--;const void*_r313=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv312);if(!_r313)return;goto *_r313;}
L_456: Cell t314=var_trans__rest;L_457: Cell t315=uf_mkp((void*)&uf_sl29);L_458: pushc(cx,t314);pushc(cx,t315);uf_cur_op="op_starts";op_starts(cx);
L_459: pushp(cx,(void*)&&L_465);
L_460: pushp(cx,(void*)&&L_491);
L_461: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_461,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_461,cx->sp>0?cx->sp-0:0);goto *el;}K_461:;}
L_462: L_463: L_464: Cell _rv316=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv316);return;}cx->csp--;const void*_r317=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv316);if(!_r317)return;goto *_r317;}
L_465: Cell t318=var_trans__rest;L_466: Cell t319=uf_mkp((void*)&uf_sl30);L_467: pushc(cx,t318);pushc(cx,t319);uf_cur_op="op_find";op_find(cx);
L_468: Cell t320=pop(cx);L_469: L_470: L_471: Cell t321=uf_ceq(t320,uf_mki(-1LL));L_472: var_trans__fi=t320;pushc(cx,t320);pushc(cx,t321);pushp(cx,(void*)&&L_486);
L_473: pushp(cx,(void*)&&L_478);
L_474: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_474,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_474,cx->sp>0?cx->sp-0:0);goto *el;}K_474:;}
L_475: L_476: L_477: Cell _rv322=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv322);return;}cx->csp--;const void*_r323=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv322);if(!_r323)return;goto *_r323;}
L_478: Cell t324=var_trans__pos;L_479: Cell t325=pop(cx);Cell t326=uf_cadd(t325,t324);L_480: L_481: Cell t327=uf_cadd(t326,uf_mki(1LL));L_482: L_483: var_trans__pos=t327;pushc(cx,t327);L_484: L_485: Cell _rv328=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv328);return;}cx->csp--;const void*_r329=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv328);if(!_r329)return;goto *_r329;}
L_486: Cell t330=var_trans__srclen;L_487: L_488: var_trans__pos=t330;pushc(cx,t330);L_489: L_490: Cell _rv331=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv331);return;}cx->csp--;const void*_r332=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv331);if(!_r332)return;goto *_r332;}
L_491: Cell t333=var_trans__rest;L_492: Cell t334=uf_mkp((void*)&uf_sl31);L_493: pushc(cx,t333);pushc(cx,t334);uf_cur_op="op_starts";op_starts(cx);
L_494: pushp(cx,(void*)&&L_500);
L_495: pushp(cx,(void*)&&L_526);
L_496: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_496,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_496,cx->sp>0?cx->sp-0:0);goto *el;}K_496:;}
L_497: L_498: L_499: Cell _rv335=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv335);return;}cx->csp--;const void*_r336=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv335);if(!_r336)return;goto *_r336;}
L_500: Cell t337=var_trans__rest;L_501: Cell t338=uf_mkp((void*)&uf_sl32);L_502: pushc(cx,t337);pushc(cx,t338);uf_cur_op="op_find";op_find(cx);
L_503: Cell t339=pop(cx);L_504: L_505: L_506: Cell t340=uf_ceq(t339,uf_mki(-1LL));L_507: var_trans__fi=t339;pushc(cx,t339);pushc(cx,t340);pushp(cx,(void*)&&L_521);
L_508: pushp(cx,(void*)&&L_513);
L_509: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_509,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_509,cx->sp>0?cx->sp-0:0);goto *el;}K_509:;}
L_510: L_511: L_512: Cell _rv341=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv341);return;}cx->csp--;const void*_r342=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv341);if(!_r342)return;goto *_r342;}
L_513: Cell t343=var_trans__pos;L_514: Cell t344=pop(cx);Cell t345=uf_cadd(t344,t343);L_515: L_516: Cell t346=uf_cadd(t345,uf_mki(2LL));L_517: L_518: var_trans__pos=t346;pushc(cx,t346);L_519: L_520: Cell _rv347=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv347);return;}cx->csp--;const void*_r348=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv347);if(!_r348)return;goto *_r348;}
L_521: Cell t349=var_trans__srclen;L_522: L_523: var_trans__pos=t349;pushc(cx,t349);L_524: L_525: Cell _rv350=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv350);return;}cx->csp--;const void*_r351=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv350);if(!_r351)return;goto *_r351;}
L_526: Cell t352=var_trans__rest;L_527: Cell t353=uf_mkp((void*)&uf_sl33);L_528: pushc(cx,t352);pushc(cx,t353);uf_cur_op="op_starts";op_starts(cx);
L_529: pushp(cx,(void*)&&L_535);
L_530: pushp(cx,(void*)&&L_561);
L_531: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_531,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_531,cx->sp>0?cx->sp-0:0);goto *el;}K_531:;}
L_532: L_533: L_534: Cell _rv354=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv354);return;}cx->csp--;const void*_r355=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv354);if(!_r355)return;goto *_r355;}
L_535: Cell t356=var_trans__rest;L_536: Cell t357=uf_mkp((void*)&uf_sl34);L_537: pushc(cx,t356);pushc(cx,t357);uf_cur_op="op_find";op_find(cx);
L_538: Cell t358=pop(cx);L_539: L_540: L_541: Cell t359=uf_ceq(t358,uf_mki(-1LL));L_542: var_trans__fi=t358;pushc(cx,t358);pushc(cx,t359);pushp(cx,(void*)&&L_556);
L_543: pushp(cx,(void*)&&L_548);
L_544: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_544,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_544,cx->sp>0?cx->sp-0:0);goto *el;}K_544:;}
L_545: L_546: L_547: Cell _rv360=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv360);return;}cx->csp--;const void*_r361=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv360);if(!_r361)return;goto *_r361;}
L_548: Cell t362=var_trans__pos;L_549: Cell t363=pop(cx);Cell t364=uf_cadd(t363,t362);L_550: L_551: Cell t365=uf_cadd(t364,uf_mki(1LL));L_552: L_553: var_trans__pos=t365;pushc(cx,t365);L_554: L_555: Cell _rv366=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv366);return;}cx->csp--;const void*_r367=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv366);if(!_r367)return;goto *_r367;}
L_556: Cell t368=var_trans__srclen;L_557: L_558: var_trans__pos=t368;pushc(cx,t368);L_559: L_560: Cell _rv369=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv369);return;}cx->csp--;const void*_r370=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv369);if(!_r370)return;goto *_r370;}
L_561: L_562: L_563: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_564: Cell t371=var_trans__rr;L_565: Cell _rv372=t371;{if(cx->csp==0){pushc(cx,_rv372);return;}cx->csp--;const void*_r373=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv372);if(!_r373)return;goto *_r373;}
L_566: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_566,cx->sp>0?cx->sp-0:0);goto L_571;K_566:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_567: Cell t374=pop(cx);L_568: var_trans__rr=t374;pushc(cx,t374);L_569: Cell t375=var_trans__rr;L_570: Cell _rv376=t375;{if(cx->csp==0){pushc(cx,_rv376);return;}cx->csp--;const void*_r377=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv376);if(!_r377)return;goto *_r377;}
L_571: Cell t378=var_trans__vars;L_572: pushc(cx,t378);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_572,cx->sp>0?cx->sp-0:0);goto L_111;K_572:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_573: uf_cur_op="op_getq";op_getq(cx);
L_574: Cell t379=pop(cx);L_575: L_576: Cell t380=uf_cnot(t379);L_577: var_trans__lv=t379;pushc(cx,t379);pushc(cx,t380);pushp(cx,(void*)&&L_581);
L_578: pushp(cx,(void*)&&L_585);
L_579: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_579,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_579,cx->sp>0?cx->sp-0:0);goto *el;}K_579:;}
L_580: Cell _rv381=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv381);return;}cx->csp--;const void*_r382=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv381);if(!_r382)return;goto *_r382;}
L_581: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_581,cx->sp>0?cx->sp-0:0);goto L_715;K_581:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_582: L_583: L_584: Cell _rv383=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv383);return;}cx->csp--;const void*_r384=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv383);if(!_r384)return;goto *_r384;}
L_585: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_585,cx->sp>1?cx->sp-1:0);goto L_0;K_585:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_586: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_586,cx->sp>0?cx->sp-0:0);goto L_118;K_586:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_587: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_587,cx->sp>1?cx->sp-1:0);goto L_0;K_587:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_588: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_588,cx->sp>0?cx->sp-0:0);goto L_8;K_588:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_589: Cell t385=uf_mkp((void*)&uf_sl35);L_590: pushc(cx,t385);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_590,cx->sp>2?cx->sp-2:0);goto L_97;K_590:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_591: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_591,cx->sp>0?cx->sp-0:0);goto L_8;K_591:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_592: Cell t386=uf_mkp((void*)&uf_sl36);L_593: pushc(cx,t386);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_593,cx->sp>2?cx->sp-2:0);goto L_97;K_593:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_594: Cell t387=pop(cx);Cell t388=pop(cx);Cell t389=uf_cadd(t388,t387);L_595: pushc(cx,t389);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_595,cx->sp>0?cx->sp-0:0);goto L_8;K_595:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_596: Cell t390=uf_mkp((void*)&uf_sl37);L_597: pushc(cx,t390);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_597,cx->sp>2?cx->sp-2:0);goto L_97;K_597:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_598: Cell t391=pop(cx);Cell t392=pop(cx);Cell t393=uf_cadd(t392,t391);L_599: pushc(cx,t393);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_599,cx->sp>0?cx->sp-0:0);goto L_8;K_599:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_600: Cell t394=uf_mkp((void*)&uf_sl38);L_601: pushc(cx,t394);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_601,cx->sp>2?cx->sp-2:0);goto L_97;K_601:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_602: Cell t395=pop(cx);Cell t396=pop(cx);Cell t397=uf_cadd(t396,t395);L_603: pushc(cx,t397);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_603,cx->sp>0?cx->sp-0:0);goto L_8;K_603:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_604: Cell t398=uf_mkp((void*)&uf_sl39);L_605: pushc(cx,t398);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_605,cx->sp>2?cx->sp-2:0);goto L_97;K_605:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_606: Cell t399=pop(cx);Cell t400=pop(cx);Cell t401=uf_cadd(t400,t399);L_607: pushc(cx,t401);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_607,cx->sp>0?cx->sp-0:0);goto L_8;K_607:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_608: Cell t402=uf_mkp((void*)&uf_sl40);L_609: pushc(cx,t402);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_609,cx->sp>2?cx->sp-2:0);goto L_97;K_609:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_610: Cell t403=pop(cx);Cell t404=pop(cx);Cell t405=uf_cadd(t404,t403);L_611: Cell t406=uf_cnot(t405);L_612: pushc(cx,t406);pushp(cx,(void*)&&L_618);
L_613: pushp(cx,(void*)&&L_626);
L_614: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_614,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_614,cx->sp>0?cx->sp-0:0);goto *el;}K_614:;}
L_615: L_616: L_617: Cell _rv407=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv407);return;}cx->csp--;const void*_r408=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv407);if(!_r408)return;goto *_r408;}
L_618: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_618,cx->sp>0?cx->sp-0:0);goto L_36;K_618:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_619: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_619,cx->sp>0?cx->sp-0:0);goto L_36;K_619:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_620: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_620,cx->sp>0?cx->sp-0:0);goto L_715;K_620:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_621: L_622: L_623: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_624: Cell t409=var_trans__rr;L_625: Cell _rv410=t409;{if(cx->csp==0){pushc(cx,_rv410);return;}cx->csp--;const void*_r411=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv410);if(!_r411)return;goto *_r411;}
L_626: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_626,cx->sp>0?cx->sp-0:0);goto L_8;K_626:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_627: Cell t412=uf_mkp((void*)&uf_sl41);L_628: pushc(cx,t412);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_628,cx->sp>2?cx->sp-2:0);goto L_97;K_628:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_629: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_629,cx->sp>0?cx->sp-0:0);goto L_8;K_629:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_630: Cell t413=uf_mkp((void*)&uf_sl42);L_631: pushc(cx,t413);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_631,cx->sp>2?cx->sp-2:0);goto L_97;K_631:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_632: Cell t414=pop(cx);Cell t415=pop(cx);Cell t416=uf_cadd(t415,t414);L_633: pushc(cx,t416);pushp(cx,(void*)&&L_712);
L_634: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_634,cx->sp>0?cx->sp-0:0);goto *b;K_634:;}}
L_635: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_635,cx->sp>0?cx->sp-0:0);goto L_8;K_635:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_636: Cell t417=uf_mkp((void*)&uf_sl43);L_637: pushc(cx,t417);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_637,cx->sp>2?cx->sp-2:0);goto L_97;K_637:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_638: pushp(cx,(void*)&&L_644);
L_639: pushp(cx,(void*)&&L_655);
L_640: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_640,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_640,cx->sp>0?cx->sp-0:0);goto *el;}K_640:;}
L_641: L_642: L_643: Cell _rv418=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv418);return;}cx->csp--;const void*_r419=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv418);if(!_r419)return;goto *_r419;}
L_644: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_644,cx->sp>0?cx->sp-0:0);goto L_127;K_644:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_645: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_645,cx->sp>0?cx->sp-0:0);goto L_127;K_645:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_646: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_646,cx->sp>0?cx->sp-0:0);goto L_571;K_646:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_647: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_647,cx->sp>0?cx->sp-0:0);goto L_36;K_647:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_648: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_648,cx->sp>0?cx->sp-0:0);goto L_36;K_648:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_649: Cell t420=uf_mkp((void*)&uf_sl44);L_650: pushc(cx,t420);uf_cur_op="op_fmt";op_fmt(cx);
L_651: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_651,cx->sp>1?cx->sp-1:0);goto L_42;K_651:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_652: L_653: L_654: Cell _rv421=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv421);return;}cx->csp--;const void*_r422=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv421);if(!_r422)return;goto *_r422;}
L_655: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_655,cx->sp>0?cx->sp-0:0);goto L_36;K_655:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_656: Cell t423=pop(cx);L_657: var_trans__pc_op=t423;pushc(cx,t423);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_657,cx->sp>0?cx->sp-0:0);goto L_36;K_657:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_658: Cell t424=pop(cx);L_659: var_trans__pc_slot=t424;pushc(cx,t424);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_659,cx->sp>0?cx->sp-0:0);goto L_127;K_659:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_660: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_660,cx->sp>0?cx->sp-0:0);goto L_127;K_660:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_661: Cell t425=var_trans__pc_slot;L_662: Cell t426=uf_mkp((void*)&uf_sl45);L_663: pushc(cx,t425);pushc(cx,t426);uf_cur_op="op_fmt";op_fmt(cx);
L_664: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_664,cx->sp>1?cx->sp-1:0);goto L_42;K_664:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_665: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_665,cx->sp>0?cx->sp-0:0);goto L_571;K_665:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_666: Cell t427=var_trans__pc_op;L_667: L_668: L_669: pushc(cx,t427);pushi(cx,0LL);pushi(cx,1LL);uf_cur_op="op_slice";op_slice(cx);
L_670: Cell t428=pop(cx);L_671: L_672: Cell t429=uf_mkp((void*)&uf_sl46);L_673: var_trans__pc_o=t428;pushc(cx,t428);pushc(cx,t428);pushc(cx,t429);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_673,cx->sp>2?cx->sp-2:0);goto L_97;K_673:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_674: pushp(cx,(void*)&&L_685);
L_675: pushp(cx,(void*)&&L_678);
L_676: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_676,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_676,cx->sp>0?cx->sp-0:0);goto *el;}K_676:;}
L_677: Cell _rv430=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv430);return;}cx->csp--;const void*_r431=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv430);if(!_r431)return;goto *_r431;}
L_678: Cell t432=var_trans__pc_o;L_679: Cell t433=uf_mkp((void*)&uf_sl47);L_680: pushc(cx,t432);pushc(cx,t433);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_680,cx->sp>2?cx->sp-2:0);goto L_97;K_680:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_681: pushp(cx,(void*)&&L_694);
L_682: pushp(cx,(void*)&&L_703);
L_683: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_683,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_683,cx->sp>0?cx->sp-0:0);goto *el;}K_683:;}
L_684: Cell _rv434=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv434);return;}cx->csp--;const void*_r435=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv434);if(!_r435)return;goto *_r435;}
L_685: Cell t436=uf_mkp((void*)&uf_sl48);L_686: pushc(cx,t436);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_686,cx->sp>1?cx->sp-1:0);goto L_42;K_686:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_687: Cell t437=var_trans__pc_slot;L_688: Cell t438=uf_mkp((void*)&uf_sl49);L_689: pushc(cx,t437);pushc(cx,t438);uf_cur_op="op_fmt";op_fmt(cx);
L_690: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_690,cx->sp>1?cx->sp-1:0);goto L_42;K_690:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_691: L_692: L_693: Cell _rv439=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv439);return;}cx->csp--;const void*_r440=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv439);if(!_r440)return;goto *_r440;}
L_694: Cell t441=uf_mkp((void*)&uf_sl50);L_695: pushc(cx,t441);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_695,cx->sp>1?cx->sp-1:0);goto L_42;K_695:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_696: Cell t442=var_trans__pc_slot;L_697: Cell t443=uf_mkp((void*)&uf_sl51);L_698: pushc(cx,t442);pushc(cx,t443);uf_cur_op="op_fmt";op_fmt(cx);
L_699: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_699,cx->sp>1?cx->sp-1:0);goto L_42;K_699:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_700: L_701: L_702: Cell _rv444=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv444);return;}cx->csp--;const void*_r445=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv444);if(!_r445)return;goto *_r445;}
L_703: Cell t446=uf_mkp((void*)&uf_sl52);L_704: pushc(cx,t446);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_704,cx->sp>1?cx->sp-1:0);goto L_42;K_704:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_705: Cell t447=var_trans__pc_slot;L_706: Cell t448=uf_mkp((void*)&uf_sl53);L_707: pushc(cx,t447);pushc(cx,t448);uf_cur_op="op_fmt";op_fmt(cx);
L_708: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_708,cx->sp>1?cx->sp-1:0);goto L_42;K_708:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_709: L_710: L_711: Cell _rv449=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv449);return;}cx->csp--;const void*_r450=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv449);if(!_r450)return;goto *_r450;}
L_712: Cell t451=uf_mkp((void*)&uf_sl54);L_713: pushc(cx,t451);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_713,cx->sp>0?cx->sp-0:0);goto L_107;K_713:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_714: Cell _rv452=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv452);return;}cx->csp--;const void*_r453=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv452);if(!_r453)return;goto *_r453;}
L_715: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_715,cx->sp>0?cx->sp-0:0);goto L_734;K_715:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_716: L_717: L_718: {long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_718;cx->loops[fr].end=&&K_WE_718;long _sp0=cx->sp;
K_WC_718:;{Cell _wc;{
WC718_L720: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC718_720,cx->sp>0?cx->sp-0:0);goto L_111;K_WC718_720:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC718_L721: Cell t0=uf_mkp((void*)&uf_sl55);WC718_L722: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC718_722,cx->sp>2?cx->sp-2:0);goto L_97;K_WC718_722:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC718_L723: Cell t1=pop(cx);WC718_L724: var_trans__rr=t1;pushc(cx,t1);WC718_L725: Cell t2=var_trans__rr;pushc(cx,t2);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_718;
{
WB718_L727: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB718_727,cx->sp>0?cx->sp-0:0);goto L_127;K_WB718_727:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB718_L728: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB718_728,cx->sp>0?cx->sp-0:0);goto L_734;K_WB718_728:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB718_L729: Cell t0=uf_mkp((void*)&uf_sl56);WB718_L730: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB718_730,cx->sp>1?cx->sp-1:0);goto L_42;K_WB718_730:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB718_L731: }cx->sp=_sp0;goto K_WC_718;}
K_WE_718:;cx->lsp=fr;}
L_719: Cell _rv454=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv454);return;}cx->csp--;const void*_r455=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv454);if(!_r455)return;goto *_r455;}
L_720: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_720,cx->sp>0?cx->sp-0:0);goto L_111;K_720:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_721: Cell t456=uf_mkp((void*)&uf_sl55);L_722: pushc(cx,t456);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_722,cx->sp>2?cx->sp-2:0);goto L_97;K_722:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_723: Cell t457=pop(cx);L_724: var_trans__rr=t457;pushc(cx,t457);L_725: Cell t458=var_trans__rr;L_726: Cell _rv459=t458;{if(cx->csp==0){pushc(cx,_rv459);return;}cx->csp--;const void*_r460=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv459);if(!_r460)return;goto *_r460;}
L_727: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_727,cx->sp>0?cx->sp-0:0);goto L_127;K_727:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_728: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_728,cx->sp>0?cx->sp-0:0);goto L_734;K_728:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_729: Cell t461=uf_mkp((void*)&uf_sl56);L_730: pushc(cx,t461);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_730,cx->sp>1?cx->sp-1:0);goto L_42;K_730:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_731: L_732: L_733: Cell _rv462=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv462);return;}cx->csp--;const void*_r463=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv462);if(!_r463)return;goto *_r463;}
L_734: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_734,cx->sp>0?cx->sp-0:0);goto L_753;K_734:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_735: L_736: L_737: {long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_737;cx->loops[fr].end=&&K_WE_737;long _sp0=cx->sp;
K_WC_737:;{Cell _wc;{
WC737_L739: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC737_739,cx->sp>0?cx->sp-0:0);goto L_111;K_WC737_739:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC737_L740: Cell t0=uf_mkp((void*)&uf_sl57);WC737_L741: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC737_741,cx->sp>2?cx->sp-2:0);goto L_97;K_WC737_741:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC737_L742: Cell t1=pop(cx);WC737_L743: var_trans__rr=t1;pushc(cx,t1);WC737_L744: Cell t2=var_trans__rr;pushc(cx,t2);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_737;
{
WB737_L746: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB737_746,cx->sp>0?cx->sp-0:0);goto L_127;K_WB737_746:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB737_L747: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB737_747,cx->sp>0?cx->sp-0:0);goto L_753;K_WB737_747:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB737_L748: Cell t0=uf_mkp((void*)&uf_sl58);WB737_L749: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB737_749,cx->sp>1?cx->sp-1:0);goto L_42;K_WB737_749:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB737_L750: }cx->sp=_sp0;goto K_WC_737;}
K_WE_737:;cx->lsp=fr;}
L_738: Cell _rv464=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv464);return;}cx->csp--;const void*_r465=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv464);if(!_r465)return;goto *_r465;}
L_739: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_739,cx->sp>0?cx->sp-0:0);goto L_111;K_739:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_740: Cell t466=uf_mkp((void*)&uf_sl57);L_741: pushc(cx,t466);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_741,cx->sp>2?cx->sp-2:0);goto L_97;K_741:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_742: Cell t467=pop(cx);L_743: var_trans__rr=t467;pushc(cx,t467);L_744: Cell t468=var_trans__rr;L_745: Cell _rv469=t468;{if(cx->csp==0){pushc(cx,_rv469);return;}cx->csp--;const void*_r470=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv469);if(!_r470)return;goto *_r470;}
L_746: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_746,cx->sp>0?cx->sp-0:0);goto L_127;K_746:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_747: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_747,cx->sp>0?cx->sp-0:0);goto L_753;K_747:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_748: Cell t471=uf_mkp((void*)&uf_sl58);L_749: pushc(cx,t471);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_749,cx->sp>1?cx->sp-1:0);goto L_42;K_749:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_750: L_751: L_752: Cell _rv472=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv472);return;}cx->csp--;const void*_r473=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv472);if(!_r473)return;goto *_r473;}
L_753: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_753,cx->sp>0?cx->sp-0:0);goto L_796;K_753:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_754: L_755: L_756: {long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_756;cx->loops[fr].end=&&K_WE_756;long _sp0=cx->sp;
K_WC_756:;{Cell _wc;{
WC756_L758: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC756_758,cx->sp>0?cx->sp-0:0);goto L_111;K_WC756_758:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC756_L759: Cell t0=uf_mkp((void*)&uf_sl59);WC756_L760: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC756_760,cx->sp>2?cx->sp-2:0);goto L_97;K_WC756_760:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC756_L761: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC756_761,cx->sp>0?cx->sp-0:0);goto L_111;K_WC756_761:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC756_L762: Cell t1=uf_mkp((void*)&uf_sl60);WC756_L763: pushc(cx,t1);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC756_763,cx->sp>2?cx->sp-2:0);goto L_97;K_WC756_763:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC756_L764: Cell t2=pop(cx);Cell t3=pop(cx);Cell t4=uf_cadd(t3,t2);WC756_L765: WC756_L766: var_trans__rr=t4;pushc(cx,t4);WC756_L767: Cell t5=var_trans__rr;pushc(cx,t5);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_756;
{
WB756_L769: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB756_769,cx->sp>0?cx->sp-0:0);goto L_111;K_WB756_769:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB756_L770: Cell t0=uf_mkp((void*)&uf_sl61);WB756_L771: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB756_771,cx->sp>2?cx->sp-2:0);goto L_97;K_WB756_771:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB756_L772: pushp(cx,(void*)&&L_778);
WB756_L773: pushp(cx,(void*)&&L_787);
WB756_L774: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_WB756_774,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_WB756_774,cx->sp>0?cx->sp-0:0);goto *el;}K_WB756_774:;}
WB756_L775: }cx->sp=_sp0;goto K_WC_756;}
K_WE_756:;cx->lsp=fr;}
L_757: Cell _rv474=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv474);return;}cx->csp--;const void*_r475=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv474);if(!_r475)return;goto *_r475;}
L_758: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_758,cx->sp>0?cx->sp-0:0);goto L_111;K_758:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_759: Cell t476=uf_mkp((void*)&uf_sl59);L_760: pushc(cx,t476);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_760,cx->sp>2?cx->sp-2:0);goto L_97;K_760:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_761: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_761,cx->sp>0?cx->sp-0:0);goto L_111;K_761:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_762: Cell t477=uf_mkp((void*)&uf_sl60);L_763: pushc(cx,t477);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_763,cx->sp>2?cx->sp-2:0);goto L_97;K_763:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_764: Cell t478=pop(cx);Cell t479=pop(cx);Cell t480=uf_cadd(t479,t478);L_765: L_766: var_trans__rr=t480;pushc(cx,t480);L_767: Cell t481=var_trans__rr;L_768: Cell _rv482=t481;{if(cx->csp==0){pushc(cx,_rv482);return;}cx->csp--;const void*_r483=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv482);if(!_r483)return;goto *_r483;}
L_769: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_769,cx->sp>0?cx->sp-0:0);goto L_111;K_769:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_770: Cell t484=uf_mkp((void*)&uf_sl61);L_771: pushc(cx,t484);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_771,cx->sp>2?cx->sp-2:0);goto L_97;K_771:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_772: pushp(cx,(void*)&&L_778);
L_773: pushp(cx,(void*)&&L_787);
L_774: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_774,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_774,cx->sp>0?cx->sp-0:0);goto *el;}K_774:;}
L_775: L_776: L_777: Cell _rv485=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv485);return;}cx->csp--;const void*_r486=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv485);if(!_r486)return;goto *_r486;}
L_778: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_778,cx->sp>0?cx->sp-0:0);goto L_127;K_778:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_779: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_779,cx->sp>0?cx->sp-0:0);goto L_796;K_779:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_780: Cell t487=uf_mkp((void*)&uf_sl62);L_781: pushc(cx,t487);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_781,cx->sp>1?cx->sp-1:0);goto L_42;K_781:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_782: L_783: L_784: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_785: Cell t488=var_trans__rr;L_786: Cell _rv489=t488;{if(cx->csp==0){pushc(cx,_rv489);return;}cx->csp--;const void*_r490=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv489);if(!_r490)return;goto *_r490;}
L_787: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_787,cx->sp>0?cx->sp-0:0);goto L_127;K_787:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_788: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_788,cx->sp>0?cx->sp-0:0);goto L_796;K_788:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_789: Cell t491=uf_mkp((void*)&uf_sl63);L_790: pushc(cx,t491);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_790,cx->sp>1?cx->sp-1:0);goto L_42;K_790:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_791: L_792: L_793: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_794: Cell t492=var_trans__rr;L_795: Cell _rv493=t492;{if(cx->csp==0){pushc(cx,_rv493);return;}cx->csp--;const void*_r494=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv493);if(!_r494)return;goto *_r494;}
L_796: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_796,cx->sp>0?cx->sp-0:0);goto L_883;K_796:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_797: L_798: L_799: {long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_799;cx->loops[fr].end=&&K_WE_799;long _sp0=cx->sp;
K_WC_799:;{Cell _wc;{
WC799_L801: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC799_801,cx->sp>0?cx->sp-0:0);goto L_111;K_WC799_801:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC799_L802: Cell t0=uf_mkp((void*)&uf_sl64);WC799_L803: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC799_803,cx->sp>2?cx->sp-2:0);goto L_97;K_WC799_803:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC799_L804: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC799_804,cx->sp>0?cx->sp-0:0);goto L_111;K_WC799_804:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC799_L805: Cell t1=uf_mkp((void*)&uf_sl65);WC799_L806: pushc(cx,t1);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC799_806,cx->sp>2?cx->sp-2:0);goto L_97;K_WC799_806:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC799_L807: Cell t2=pop(cx);Cell t3=pop(cx);Cell t4=uf_cadd(t3,t2);WC799_L808: pushc(cx,t4);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC799_808,cx->sp>0?cx->sp-0:0);goto L_111;K_WC799_808:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC799_L809: Cell t5=uf_mkp((void*)&uf_sl66);WC799_L810: pushc(cx,t5);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC799_810,cx->sp>2?cx->sp-2:0);goto L_97;K_WC799_810:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC799_L811: Cell t6=pop(cx);Cell t7=pop(cx);Cell t8=uf_cadd(t7,t6);WC799_L812: pushc(cx,t8);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC799_812,cx->sp>0?cx->sp-0:0);goto L_111;K_WC799_812:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC799_L813: Cell t9=uf_mkp((void*)&uf_sl67);WC799_L814: pushc(cx,t9);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC799_814,cx->sp>2?cx->sp-2:0);goto L_97;K_WC799_814:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC799_L815: Cell t10=pop(cx);Cell t11=pop(cx);Cell t12=uf_cadd(t11,t10);WC799_L816: WC799_L817: var_trans__rr=t12;pushc(cx,t12);WC799_L818: Cell t13=var_trans__rr;pushc(cx,t13);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_799;
{
WB799_L820: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB799_820,cx->sp>0?cx->sp-0:0);goto L_111;K_WB799_820:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB799_L821: Cell t0=uf_mkp((void*)&uf_sl68);WB799_L822: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB799_822,cx->sp>2?cx->sp-2:0);goto L_97;K_WB799_822:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB799_L823: pushp(cx,(void*)&&L_829);
WB799_L824: pushp(cx,(void*)&&L_838);
WB799_L825: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_WB799_825,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_WB799_825,cx->sp>0?cx->sp-0:0);goto *el;}K_WB799_825:;}
WB799_L826: }cx->sp=_sp0;goto K_WC_799;}
K_WE_799:;cx->lsp=fr;}
L_800: Cell _rv495=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv495);return;}cx->csp--;const void*_r496=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv495);if(!_r496)return;goto *_r496;}
L_801: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_801,cx->sp>0?cx->sp-0:0);goto L_111;K_801:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_802: Cell t497=uf_mkp((void*)&uf_sl64);L_803: pushc(cx,t497);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_803,cx->sp>2?cx->sp-2:0);goto L_97;K_803:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_804: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_804,cx->sp>0?cx->sp-0:0);goto L_111;K_804:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_805: Cell t498=uf_mkp((void*)&uf_sl65);L_806: pushc(cx,t498);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_806,cx->sp>2?cx->sp-2:0);goto L_97;K_806:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_807: Cell t499=pop(cx);Cell t500=pop(cx);Cell t501=uf_cadd(t500,t499);L_808: pushc(cx,t501);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_808,cx->sp>0?cx->sp-0:0);goto L_111;K_808:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_809: Cell t502=uf_mkp((void*)&uf_sl66);L_810: pushc(cx,t502);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_810,cx->sp>2?cx->sp-2:0);goto L_97;K_810:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_811: Cell t503=pop(cx);Cell t504=pop(cx);Cell t505=uf_cadd(t504,t503);L_812: pushc(cx,t505);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_812,cx->sp>0?cx->sp-0:0);goto L_111;K_812:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_813: Cell t506=uf_mkp((void*)&uf_sl67);L_814: pushc(cx,t506);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_814,cx->sp>2?cx->sp-2:0);goto L_97;K_814:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_815: Cell t507=pop(cx);Cell t508=pop(cx);Cell t509=uf_cadd(t508,t507);L_816: L_817: var_trans__rr=t509;pushc(cx,t509);L_818: Cell t510=var_trans__rr;L_819: Cell _rv511=t510;{if(cx->csp==0){pushc(cx,_rv511);return;}cx->csp--;const void*_r512=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv511);if(!_r512)return;goto *_r512;}
L_820: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_820,cx->sp>0?cx->sp-0:0);goto L_111;K_820:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_821: Cell t513=uf_mkp((void*)&uf_sl68);L_822: pushc(cx,t513);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_822,cx->sp>2?cx->sp-2:0);goto L_97;K_822:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_823: pushp(cx,(void*)&&L_829);
L_824: pushp(cx,(void*)&&L_838);
L_825: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_825,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_825,cx->sp>0?cx->sp-0:0);goto *el;}K_825:;}
L_826: L_827: L_828: Cell _rv514=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv514);return;}cx->csp--;const void*_r515=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv514);if(!_r515)return;goto *_r515;}
L_829: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_829,cx->sp>0?cx->sp-0:0);goto L_127;K_829:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_830: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_830,cx->sp>0?cx->sp-0:0);goto L_883;K_830:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_831: Cell t516=uf_mkp((void*)&uf_sl69);L_832: pushc(cx,t516);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_832,cx->sp>1?cx->sp-1:0);goto L_42;K_832:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_833: L_834: L_835: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_836: Cell t517=var_trans__rr;L_837: Cell _rv518=t517;{if(cx->csp==0){pushc(cx,_rv518);return;}cx->csp--;const void*_r519=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv518);if(!_r519)return;goto *_r519;}
L_838: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_838,cx->sp>0?cx->sp-0:0);goto L_111;K_838:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_839: Cell t520=uf_mkp((void*)&uf_sl70);L_840: pushc(cx,t520);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_840,cx->sp>2?cx->sp-2:0);goto L_97;K_840:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_841: pushp(cx,(void*)&&L_847);
L_842: pushp(cx,(void*)&&L_856);
L_843: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_843,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_843,cx->sp>0?cx->sp-0:0);goto *el;}K_843:;}
L_844: L_845: L_846: Cell _rv521=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv521);return;}cx->csp--;const void*_r522=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv521);if(!_r522)return;goto *_r522;}
L_847: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_847,cx->sp>0?cx->sp-0:0);goto L_127;K_847:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_848: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_848,cx->sp>0?cx->sp-0:0);goto L_883;K_848:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_849: Cell t523=uf_mkp((void*)&uf_sl71);L_850: pushc(cx,t523);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_850,cx->sp>1?cx->sp-1:0);goto L_42;K_850:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_851: L_852: L_853: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_854: Cell t524=var_trans__rr;L_855: Cell _rv525=t524;{if(cx->csp==0){pushc(cx,_rv525);return;}cx->csp--;const void*_r526=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv525);if(!_r526)return;goto *_r526;}
L_856: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_856,cx->sp>0?cx->sp-0:0);goto L_111;K_856:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_857: Cell t527=uf_mkp((void*)&uf_sl72);L_858: pushc(cx,t527);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_858,cx->sp>2?cx->sp-2:0);goto L_97;K_858:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_859: pushp(cx,(void*)&&L_865);
L_860: pushp(cx,(void*)&&L_874);
L_861: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_861,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_861,cx->sp>0?cx->sp-0:0);goto *el;}K_861:;}
L_862: L_863: L_864: Cell _rv528=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv528);return;}cx->csp--;const void*_r529=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv528);if(!_r529)return;goto *_r529;}
L_865: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_865,cx->sp>0?cx->sp-0:0);goto L_127;K_865:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_866: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_866,cx->sp>0?cx->sp-0:0);goto L_883;K_866:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_867: Cell t530=uf_mkp((void*)&uf_sl73);L_868: pushc(cx,t530);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_868,cx->sp>1?cx->sp-1:0);goto L_42;K_868:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_869: L_870: L_871: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_872: Cell t531=var_trans__rr;L_873: Cell _rv532=t531;{if(cx->csp==0){pushc(cx,_rv532);return;}cx->csp--;const void*_r533=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv532);if(!_r533)return;goto *_r533;}
L_874: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_874,cx->sp>0?cx->sp-0:0);goto L_127;K_874:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_875: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_875,cx->sp>0?cx->sp-0:0);goto L_883;K_875:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_876: Cell t534=uf_mkp((void*)&uf_sl74);L_877: pushc(cx,t534);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_877,cx->sp>1?cx->sp-1:0);goto L_42;K_877:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_878: L_879: L_880: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_881: Cell t535=var_trans__rr;L_882: Cell _rv536=t535;{if(cx->csp==0){pushc(cx,_rv536);return;}cx->csp--;const void*_r537=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv536);if(!_r537)return;goto *_r537;}
L_883: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_883,cx->sp>0?cx->sp-0:0);goto L_926;K_883:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_884: L_885: L_886: {long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_886;cx->loops[fr].end=&&K_WE_886;long _sp0=cx->sp;
K_WC_886:;{Cell _wc;{
WC886_L888: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC886_888,cx->sp>0?cx->sp-0:0);goto L_111;K_WC886_888:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC886_L889: Cell t0=uf_mkp((void*)&uf_sl75);WC886_L890: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC886_890,cx->sp>2?cx->sp-2:0);goto L_97;K_WC886_890:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC886_L891: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC886_891,cx->sp>0?cx->sp-0:0);goto L_111;K_WC886_891:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC886_L892: Cell t1=uf_mkp((void*)&uf_sl76);WC886_L893: pushc(cx,t1);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC886_893,cx->sp>2?cx->sp-2:0);goto L_97;K_WC886_893:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC886_L894: Cell t2=pop(cx);Cell t3=pop(cx);Cell t4=uf_cadd(t3,t2);WC886_L895: WC886_L896: var_trans__rr=t4;pushc(cx,t4);WC886_L897: Cell t5=var_trans__rr;pushc(cx,t5);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_886;
{
WB886_L899: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB886_899,cx->sp>0?cx->sp-0:0);goto L_111;K_WB886_899:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB886_L900: Cell t0=uf_mkp((void*)&uf_sl77);WB886_L901: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB886_901,cx->sp>2?cx->sp-2:0);goto L_97;K_WB886_901:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB886_L902: pushp(cx,(void*)&&L_908);
WB886_L903: pushp(cx,(void*)&&L_917);
WB886_L904: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_WB886_904,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_WB886_904,cx->sp>0?cx->sp-0:0);goto *el;}K_WB886_904:;}
WB886_L905: }cx->sp=_sp0;goto K_WC_886;}
K_WE_886:;cx->lsp=fr;}
L_887: Cell _rv538=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv538);return;}cx->csp--;const void*_r539=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv538);if(!_r539)return;goto *_r539;}
L_888: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_888,cx->sp>0?cx->sp-0:0);goto L_111;K_888:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_889: Cell t540=uf_mkp((void*)&uf_sl75);L_890: pushc(cx,t540);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_890,cx->sp>2?cx->sp-2:0);goto L_97;K_890:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_891: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_891,cx->sp>0?cx->sp-0:0);goto L_111;K_891:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_892: Cell t541=uf_mkp((void*)&uf_sl76);L_893: pushc(cx,t541);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_893,cx->sp>2?cx->sp-2:0);goto L_97;K_893:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_894: Cell t542=pop(cx);Cell t543=pop(cx);Cell t544=uf_cadd(t543,t542);L_895: L_896: var_trans__rr=t544;pushc(cx,t544);L_897: Cell t545=var_trans__rr;L_898: Cell _rv546=t545;{if(cx->csp==0){pushc(cx,_rv546);return;}cx->csp--;const void*_r547=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv546);if(!_r547)return;goto *_r547;}
L_899: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_899,cx->sp>0?cx->sp-0:0);goto L_111;K_899:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_900: Cell t548=uf_mkp((void*)&uf_sl77);L_901: pushc(cx,t548);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_901,cx->sp>2?cx->sp-2:0);goto L_97;K_901:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_902: pushp(cx,(void*)&&L_908);
L_903: pushp(cx,(void*)&&L_917);
L_904: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_904,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_904,cx->sp>0?cx->sp-0:0);goto *el;}K_904:;}
L_905: L_906: L_907: Cell _rv549=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv549);return;}cx->csp--;const void*_r550=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv549);if(!_r550)return;goto *_r550;}
L_908: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_908,cx->sp>0?cx->sp-0:0);goto L_127;K_908:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_909: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_909,cx->sp>0?cx->sp-0:0);goto L_926;K_909:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_910: Cell t551=uf_mkp((void*)&uf_sl78);L_911: pushc(cx,t551);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_911,cx->sp>1?cx->sp-1:0);goto L_42;K_911:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_912: L_913: L_914: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_915: Cell t552=var_trans__rr;L_916: Cell _rv553=t552;{if(cx->csp==0){pushc(cx,_rv553);return;}cx->csp--;const void*_r554=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv553);if(!_r554)return;goto *_r554;}
L_917: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_917,cx->sp>0?cx->sp-0:0);goto L_127;K_917:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_918: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_918,cx->sp>0?cx->sp-0:0);goto L_926;K_918:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_919: Cell t555=uf_mkp((void*)&uf_sl79);L_920: pushc(cx,t555);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_920,cx->sp>1?cx->sp-1:0);goto L_42;K_920:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_921: L_922: L_923: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_924: Cell t556=var_trans__rr;L_925: Cell _rv557=t556;{if(cx->csp==0){pushc(cx,_rv557);return;}cx->csp--;const void*_r558=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv557);if(!_r558)return;goto *_r558;}
L_926: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_926,cx->sp>0?cx->sp-0:0);goto L_959;K_926:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_927: L_928: L_929: {long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_929;cx->loops[fr].end=&&K_WE_929;long _sp0=cx->sp;
K_WC_929:;{Cell _wc;{
WC929_L940: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC929_940,cx->sp>0?cx->sp-0:0);goto L_111;K_WC929_940:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC929_L941: Cell t0=uf_mkp((void*)&uf_sl82);WC929_L942: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC929_942,cx->sp>2?cx->sp-2:0);goto L_97;K_WC929_942:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC929_L943: Cell t1=pop(cx);WC929_L944: var_trans__rr=t1;pushc(cx,t1);WC929_L945: Cell t2=var_trans__rr;pushc(cx,t2);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_929;
{
WB929_L947: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB929_947,cx->sp>0?cx->sp-0:0);goto L_127;K_WB929_947:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB929_L948: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB929_948,cx->sp>0?cx->sp-0:0);goto L_959;K_WB929_948:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB929_L949: Cell t0=uf_mkp((void*)&uf_sl83);WB929_L950: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB929_950,cx->sp>1?cx->sp-1:0);goto L_42;K_WB929_950:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB929_L951: WB929_L952: WB929_L953: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);WB929_L954: Cell t1=var_trans__rr;pushc(cx,t1);}cx->sp=_sp0;goto K_WC_929;}
K_WE_929:;cx->lsp=fr;}
L_930: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_930,cx->sp>0?cx->sp-0:0);goto L_111;K_930:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_931: Cell t559=uf_mkp((void*)&uf_sl80);L_932: pushc(cx,t559);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_932,cx->sp>2?cx->sp-2:0);goto L_97;K_932:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_933: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_933,cx->sp>0?cx->sp-0:0);goto L_111;K_933:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_934: Cell t560=uf_mkp((void*)&uf_sl81);L_935: pushc(cx,t560);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_935,cx->sp>2?cx->sp-2:0);goto L_97;K_935:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_936: Cell t561=pop(cx);Cell t562=pop(cx);Cell t563=uf_cadd(t562,t561);L_937: pushc(cx,t563);pushp(cx,(void*)&&L_956);
L_938: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_938,cx->sp>0?cx->sp-0:0);goto *b;K_938:;}}
L_939: Cell _rv564=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv564);return;}cx->csp--;const void*_r565=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv564);if(!_r565)return;goto *_r565;}
L_940: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_940,cx->sp>0?cx->sp-0:0);goto L_111;K_940:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_941: Cell t566=uf_mkp((void*)&uf_sl82);L_942: pushc(cx,t566);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_942,cx->sp>2?cx->sp-2:0);goto L_97;K_942:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_943: Cell t567=pop(cx);L_944: var_trans__rr=t567;pushc(cx,t567);L_945: Cell t568=var_trans__rr;L_946: Cell _rv569=t568;{if(cx->csp==0){pushc(cx,_rv569);return;}cx->csp--;const void*_r570=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv569);if(!_r570)return;goto *_r570;}
L_947: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_947,cx->sp>0?cx->sp-0:0);goto L_127;K_947:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_948: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_948,cx->sp>0?cx->sp-0:0);goto L_959;K_948:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_949: Cell t571=uf_mkp((void*)&uf_sl83);L_950: pushc(cx,t571);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_950,cx->sp>1?cx->sp-1:0);goto L_42;K_950:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_951: L_952: L_953: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_954: Cell t572=var_trans__rr;L_955: Cell _rv573=t572;{if(cx->csp==0){pushc(cx,_rv573);return;}cx->csp--;const void*_r574=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv573);if(!_r574)return;goto *_r574;}
L_956: Cell t575=uf_mkp((void*)&uf_sl84);L_957: pushc(cx,t575);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_957,cx->sp>0?cx->sp-0:0);goto L_107;K_957:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_958: Cell _rv576=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv576);return;}cx->csp--;const void*_r577=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv576);if(!_r577)return;goto *_r577;}
L_959: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_959,cx->sp>0?cx->sp-0:0);goto L_111;K_959:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_960: Cell t578=uf_mkp((void*)&uf_sl85);L_961: pushc(cx,t578);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_961,cx->sp>2?cx->sp-2:0);goto L_97;K_961:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_962: pushp(cx,(void*)&&L_1011);
L_963: pushp(cx,(void*)&&L_966);
L_964: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_964,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_964,cx->sp>0?cx->sp-0:0);goto *el;}K_964:;}
L_965: Cell _rv579=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv579);return;}cx->csp--;const void*_r580=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv579);if(!_r580)return;goto *_r580;}
L_966: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_966,cx->sp>0?cx->sp-0:0);goto L_111;K_966:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_967: Cell t581=uf_mkp((void*)&uf_sl86);L_968: pushc(cx,t581);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_968,cx->sp>2?cx->sp-2:0);goto L_97;K_968:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_969: pushp(cx,(void*)&&L_1022);
L_970: pushp(cx,(void*)&&L_975);
L_971: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_971,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_971,cx->sp>0?cx->sp-0:0);goto *el;}K_971:;}
L_972: L_973: L_974: Cell _rv582=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv582);return;}cx->csp--;const void*_r583=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv582);if(!_r583)return;goto *_r583;}
L_975: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_975,cx->sp>0?cx->sp-0:0);goto L_111;K_975:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_976: Cell t584=uf_mkp((void*)&uf_sl87);L_977: pushc(cx,t584);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_977,cx->sp>2?cx->sp-2:0);goto L_97;K_977:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_978: pushp(cx,(void*)&&L_1031);
L_979: pushp(cx,(void*)&&L_984);
L_980: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_980,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_980,cx->sp>0?cx->sp-0:0);goto *el;}K_980:;}
L_981: L_982: L_983: Cell _rv585=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv585);return;}cx->csp--;const void*_r586=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv585);if(!_r586)return;goto *_r586;}
L_984: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_984,cx->sp>0?cx->sp-0:0);goto L_111;K_984:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_985: Cell t587=uf_mkp((void*)&uf_sl88);L_986: pushc(cx,t587);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_986,cx->sp>2?cx->sp-2:0);goto L_97;K_986:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_987: pushp(cx,(void*)&&L_1040);
L_988: pushp(cx,(void*)&&L_993);
L_989: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_989,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_989,cx->sp>0?cx->sp-0:0);goto *el;}K_989:;}
L_990: L_991: L_992: Cell _rv588=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv588);return;}cx->csp--;const void*_r589=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv588);if(!_r589)return;goto *_r589;}
L_993: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_993,cx->sp>0?cx->sp-0:0);goto L_111;K_993:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_994: Cell t590=uf_mkp((void*)&uf_sl89);L_995: pushc(cx,t590);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_995,cx->sp>2?cx->sp-2:0);goto L_97;K_995:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_996: pushp(cx,(void*)&&L_1047);
L_997: pushp(cx,(void*)&&L_1002);
L_998: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_998,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_998,cx->sp>0?cx->sp-0:0);goto *el;}K_998:;}
L_999: L_1000: L_1001: Cell _rv591=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv591);return;}cx->csp--;const void*_r592=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv591);if(!_r592)return;goto *_r592;}
L_1002: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1002,cx->sp>0?cx->sp-0:0);goto L_111;K_1002:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1003: Cell t593=uf_mkp((void*)&uf_sl90);L_1004: pushc(cx,t593);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1004,cx->sp>2?cx->sp-2:0);goto L_97;K_1004:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1005: pushp(cx,(void*)&&L_1068);
L_1006: pushp(cx,(void*)&&L_1089);
L_1007: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1007,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1007,cx->sp>0?cx->sp-0:0);goto *el;}K_1007:;}
L_1008: L_1009: L_1010: Cell _rv594=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv594);return;}cx->csp--;const void*_r595=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv594);if(!_r595)return;goto *_r595;}
L_1011: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1011,cx->sp>0?cx->sp-0:0);goto L_127;K_1011:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1012: Cell t596=uf_mkp((void*)&uf_sl91);L_1013: pushc(cx,t596);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1013,cx->sp>1?cx->sp-1:0);goto L_42;K_1013:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1014: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1014,cx->sp>0?cx->sp-0:0);goto L_959;K_1014:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1015: Cell t597=uf_mkp((void*)&uf_sl92);L_1016: pushc(cx,t597);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1016,cx->sp>1?cx->sp-1:0);goto L_42;K_1016:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1017: L_1018: L_1019: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1020: Cell t598=var_trans__rr;L_1021: Cell _rv599=t598;{if(cx->csp==0){pushc(cx,_rv599);return;}cx->csp--;const void*_r600=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv599);if(!_r600)return;goto *_r600;}
L_1022: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1022,cx->sp>0?cx->sp-0:0);goto L_127;K_1022:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1023: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1023,cx->sp>0?cx->sp-0:0);goto L_959;K_1023:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1024: Cell t601=uf_mkp((void*)&uf_sl93);L_1025: pushc(cx,t601);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1025,cx->sp>1?cx->sp-1:0);goto L_42;K_1025:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1026: L_1027: L_1028: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1029: Cell t602=var_trans__rr;L_1030: Cell _rv603=t602;{if(cx->csp==0){pushc(cx,_rv603);return;}cx->csp--;const void*_r604=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv603);if(!_r604)return;goto *_r604;}
L_1031: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1031,cx->sp>0?cx->sp-0:0);goto L_127;K_1031:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1032: Cell t605=uf_mkp((void*)&uf_sl94);L_1033: pushc(cx,t605);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1033,cx->sp>1?cx->sp-1:0);goto L_42;K_1033:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1034: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1034,cx->sp>0?cx->sp-0:0);goto L_959;K_1034:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1035: Cell t606=uf_mkp((void*)&uf_sl95);L_1036: pushc(cx,t606);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1036,cx->sp>1?cx->sp-1:0);goto L_42;K_1036:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1037: L_1038: L_1039: Cell _rv607=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv607);return;}cx->csp--;const void*_r608=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv607);if(!_r608)return;goto *_r608;}
L_1040: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1040,cx->sp>0?cx->sp-0:0);goto L_127;K_1040:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1041: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1041,cx->sp>0?cx->sp-0:0);goto L_959;K_1041:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1042: L_1043: L_1044: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1045: Cell t609=var_trans__rr;L_1046: Cell _rv610=t609;{if(cx->csp==0){pushc(cx,_rv610);return;}cx->csp--;const void*_r611=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv610);if(!_r611)return;goto *_r611;}
L_1047: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1047,cx->sp>0?cx->sp-0:0);goto L_127;K_1047:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1048: Cell t612=var_trans__vars;L_1049: pushc(cx,t612);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1049,cx->sp>0?cx->sp-0:0);goto L_111;K_1049:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1050: uf_cur_op="op_getq";op_getq(cx);
L_1051: Cell t613=pop(cx);Cell t614=uf_cnot(t613);L_1052: pushc(cx,t614);pushp(cx,(void*)&&L_1065);
L_1053: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1053,cx->sp>0?cx->sp-0:0);goto *b;K_1053:;}}
L_1054: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1054,cx->sp>0?cx->sp-0:0);goto L_111;K_1054:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1055: Cell t615=pop(cx);L_1056: L_1057: L_1058: Cell t616=uf_mkp((void*)&uf_sl96);L_1059: var_trans__lasts=t615;pushc(cx,t615);pushc(cx,t615);pushc(cx,t615);pushc(cx,t616);uf_cur_op="op_fmt";op_fmt(cx);
L_1060: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1060,cx->sp>1?cx->sp-1:0);goto L_42;K_1060:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1061: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1061,cx->sp>0?cx->sp-0:0);goto L_127;K_1061:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1062: L_1063: L_1064: Cell _rv617=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv617);return;}cx->csp--;const void*_r618=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv617);if(!_r618)return;goto *_r618;}
L_1065: Cell t619=uf_mkp((void*)&uf_sl97);L_1066: pushc(cx,t619);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1066,cx->sp>0?cx->sp-0:0);goto L_107;K_1066:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1067: Cell _rv620=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv620);return;}cx->csp--;const void*_r621=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv620);if(!_r621)return;goto *_r621;}
L_1068: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1068,cx->sp>0?cx->sp-0:0);goto L_127;K_1068:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1069: Cell t622=var_trans__vars;L_1070: pushc(cx,t622);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1070,cx->sp>0?cx->sp-0:0);goto L_111;K_1070:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1071: uf_cur_op="op_getq";op_getq(cx);
L_1072: Cell t623=pop(cx);Cell t624=uf_cnot(t623);L_1073: pushc(cx,t624);pushp(cx,(void*)&&L_1086);
L_1074: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1074,cx->sp>0?cx->sp-0:0);goto *b;K_1074:;}}
L_1075: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1075,cx->sp>0?cx->sp-0:0);goto L_111;K_1075:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1076: Cell t625=pop(cx);L_1077: L_1078: L_1079: Cell t626=uf_mkp((void*)&uf_sl98);L_1080: var_trans__lasts=t625;pushc(cx,t625);pushc(cx,t625);pushc(cx,t625);pushc(cx,t626);uf_cur_op="op_fmt";op_fmt(cx);
L_1081: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1081,cx->sp>1?cx->sp-1:0);goto L_42;K_1081:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1082: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1082,cx->sp>0?cx->sp-0:0);goto L_127;K_1082:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1083: L_1084: L_1085: Cell _rv627=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv627);return;}cx->csp--;const void*_r628=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv627);if(!_r628)return;goto *_r628;}
L_1086: Cell t629=uf_mkp((void*)&uf_sl99);L_1087: pushc(cx,t629);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1087,cx->sp>0?cx->sp-0:0);goto L_107;K_1087:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1088: Cell _rv630=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv630);return;}cx->csp--;const void*_r631=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv630);if(!_r631)return;goto *_r631;}
L_1089: Cell t632=uf_mkp((void*)&uf_sl100);L_1090: L_1091: var_trans__lasts=t632;pushc(cx,t632);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1091,cx->sp>0?cx->sp-0:0);goto L_1149;K_1091:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1092: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1092,cx->sp>0?cx->sp-0:0);goto L_111;K_1092:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1093: Cell t633=uf_mkp((void*)&uf_sl101);L_1094: pushc(cx,t633);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1094,cx->sp>2?cx->sp-2:0);goto L_97;K_1094:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1095: pushp(cx,(void*)&&L_1110);
L_1096: pushp(cx,(void*)&&L_1101);
L_1097: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1097,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1097,cx->sp>0?cx->sp-0:0);goto *el;}K_1097:;}
L_1098: L_1099: L_1100: Cell _rv634=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv634);return;}cx->csp--;const void*_r635=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv634);if(!_r635)return;goto *_r635;}
L_1101: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1101,cx->sp>0?cx->sp-0:0);goto L_111;K_1101:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1102: Cell t636=uf_mkp((void*)&uf_sl102);L_1103: pushc(cx,t636);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1103,cx->sp>2?cx->sp-2:0);goto L_97;K_1103:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1104: pushp(cx,(void*)&&L_1127);
L_1105: pushp(cx,(void*)&&L_1144);
L_1106: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1106,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1106,cx->sp>0?cx->sp-0:0);goto *el;}K_1106:;}
L_1107: L_1108: L_1109: Cell _rv637=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv637);return;}cx->csp--;const void*_r638=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv637);if(!_r638)return;goto *_r638;}
L_1110: Cell t639=var_trans__lasts;L_1111: Cell t640=uf_mkp((void*)&uf_sl103);L_1112: pushc(cx,t639);pushc(cx,t640);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1112,cx->sp>2?cx->sp-2:0);goto L_97;K_1112:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1113: pushp(cx,(void*)&&L_1124);
L_1114: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1114,cx->sp>0?cx->sp-0:0);goto *b;K_1114:;}}
L_1115: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1115,cx->sp>0?cx->sp-0:0);goto L_127;K_1115:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1116: Cell t641=var_trans__lasts;L_1117: L_1118: Cell t642=uf_mkp((void*)&uf_sl104);L_1119: pushc(cx,t641);pushc(cx,var_trans__lasts);pushc(cx,t642);uf_cur_op="op_fmt";op_fmt(cx);
L_1120: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1120,cx->sp>1?cx->sp-1:0);goto L_42;K_1120:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1121: L_1122: L_1123: Cell _rv643=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv643);return;}cx->csp--;const void*_r644=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv643);if(!_r644)return;goto *_r644;}
L_1124: Cell t645=uf_mkp((void*)&uf_sl105);L_1125: pushc(cx,t645);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1125,cx->sp>0?cx->sp-0:0);goto L_107;K_1125:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1126: Cell _rv646=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv646);return;}cx->csp--;const void*_r647=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv646);if(!_r647)return;goto *_r647;}
L_1127: Cell t648=var_trans__lasts;L_1128: Cell t649=uf_mkp((void*)&uf_sl106);L_1129: pushc(cx,t648);pushc(cx,t649);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1129,cx->sp>2?cx->sp-2:0);goto L_97;K_1129:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1130: pushp(cx,(void*)&&L_1141);
L_1131: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1131,cx->sp>0?cx->sp-0:0);goto *b;K_1131:;}}
L_1132: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1132,cx->sp>0?cx->sp-0:0);goto L_127;K_1132:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1133: Cell t650=var_trans__lasts;L_1134: L_1135: Cell t651=uf_mkp((void*)&uf_sl107);L_1136: pushc(cx,t650);pushc(cx,var_trans__lasts);pushc(cx,t651);uf_cur_op="op_fmt";op_fmt(cx);
L_1137: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1137,cx->sp>1?cx->sp-1:0);goto L_42;K_1137:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1138: L_1139: L_1140: Cell _rv652=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv652);return;}cx->csp--;const void*_r653=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv652);if(!_r653)return;goto *_r653;}
L_1141: Cell t654=uf_mkp((void*)&uf_sl108);L_1142: pushc(cx,t654);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1142,cx->sp>0?cx->sp-0:0);goto L_107;K_1142:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1143: Cell _rv655=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv655);return;}cx->csp--;const void*_r656=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv655);if(!_r656)return;goto *_r656;}
L_1144: L_1145: L_1146: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1147: Cell t657=var_trans__rr;L_1148: Cell _rv658=t657;{if(cx->csp==0){pushc(cx,_rv658);return;}cx->csp--;const void*_r659=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv658);if(!_r659)return;goto *_r659;}
L_1149: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1149,cx->sp>0?cx->sp-0:0);goto L_111;K_1149:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1150: Cell t660=pop(cx);L_1151: L_1152: Cell t661=uf_mkp((void*)&uf_sl109);L_1153: var_trans__ptk=t660;pushc(cx,t660);pushc(cx,t660);pushc(cx,t661);uf_cur_op="op_glob";op_glob(cx);
L_1154: pushp(cx,(void*)&&L_1239);
L_1155: pushp(cx,(void*)&&L_1158);
L_1156: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1156,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1156,cx->sp>0?cx->sp-0:0);goto *el;}K_1156:;}
L_1157: Cell _rv662=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv662);return;}cx->csp--;const void*_r663=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv662);if(!_r663)return;goto *_r663;}
L_1158: Cell t664=var_trans__ptk;L_1159: Cell t665=uf_mkp((void*)&uf_sl110);L_1160: pushc(cx,t664);pushc(cx,t665);uf_cur_op="op_starts";op_starts(cx);
L_1161: pushp(cx,(void*)&&L_1375);
L_1162: pushp(cx,(void*)&&L_1167);
L_1163: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1163,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1163,cx->sp>0?cx->sp-0:0);goto *el;}K_1163:;}
L_1164: L_1165: L_1166: Cell _rv666=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv666);return;}cx->csp--;const void*_r667=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv666);if(!_r667)return;goto *_r667;}
L_1167: Cell t668=var_trans__ptk;L_1168: Cell t669=uf_mkp((void*)&uf_sl111);L_1169: pushc(cx,t668);pushc(cx,t669);uf_cur_op="op_starts";op_starts(cx);
L_1170: pushp(cx,(void*)&&L_1257);
L_1171: pushp(cx,(void*)&&L_1176);
L_1172: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1172,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1172,cx->sp>0?cx->sp-0:0);goto *el;}K_1172:;}
L_1173: L_1174: L_1175: Cell _rv670=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv670);return;}cx->csp--;const void*_r671=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv670);if(!_r671)return;goto *_r671;}
L_1176: Cell t672=var_trans__ptk;L_1177: Cell t673=uf_mkp((void*)&uf_sl112);L_1178: pushc(cx,t672);pushc(cx,t673);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1178,cx->sp>2?cx->sp-2:0);goto L_97;K_1178:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1179: pushp(cx,(void*)&&L_1269);
L_1180: pushp(cx,(void*)&&L_1185);
L_1181: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1181,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1181,cx->sp>0?cx->sp-0:0);goto *el;}K_1181:;}
L_1182: L_1183: L_1184: Cell _rv674=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv674);return;}cx->csp--;const void*_r675=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv674);if(!_r675)return;goto *_r675;}
L_1185: Cell t676=var_trans__ptk;L_1186: Cell t677=uf_mkp((void*)&uf_sl113);L_1187: pushc(cx,t676);pushc(cx,t677);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1187,cx->sp>2?cx->sp-2:0);goto L_97;K_1187:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1188: pushp(cx,(void*)&&L_1287);
L_1189: pushp(cx,(void*)&&L_1194);
L_1190: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1190,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1190,cx->sp>0?cx->sp-0:0);goto *el;}K_1190:;}
L_1191: L_1192: L_1193: Cell _rv678=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv678);return;}cx->csp--;const void*_r679=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv678);if(!_r679)return;goto *_r679;}
L_1194: Cell t680=var_trans__ptk;L_1195: Cell t681=uf_mkp((void*)&uf_sl114);L_1196: pushc(cx,t680);pushc(cx,t681);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1196,cx->sp>2?cx->sp-2:0);goto L_97;K_1196:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1197: pushp(cx,(void*)&&L_1297);
L_1198: pushp(cx,(void*)&&L_1203);
L_1199: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1199,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1199,cx->sp>0?cx->sp-0:0);goto *el;}K_1199:;}
L_1200: L_1201: L_1202: Cell _rv682=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv682);return;}cx->csp--;const void*_r683=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv682);if(!_r683)return;goto *_r683;}
L_1203: Cell t684=var_trans__ptk;L_1204: Cell t685=uf_mkp((void*)&uf_sl115);L_1205: pushc(cx,t684);pushc(cx,t685);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1205,cx->sp>2?cx->sp-2:0);goto L_97;K_1205:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1206: pushp(cx,(void*)&&L_1326);
L_1207: pushp(cx,(void*)&&L_1212);
L_1208: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1208,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1208,cx->sp>0?cx->sp-0:0);goto *el;}K_1208:;}
L_1209: L_1210: L_1211: Cell _rv686=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv686);return;}cx->csp--;const void*_r687=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv686);if(!_r687)return;goto *_r687;}
L_1212: Cell t688=var_trans__ptk;L_1213: Cell t689=uf_mkp((void*)&uf_sl116);L_1214: pushc(cx,t688);pushc(cx,t689);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1214,cx->sp>2?cx->sp-2:0);goto L_97;K_1214:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1215: pushp(cx,(void*)&&L_1355);
L_1216: pushp(cx,(void*)&&L_1221);
L_1217: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1217,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1217,cx->sp>0?cx->sp-0:0);goto *el;}K_1217:;}
L_1218: L_1219: L_1220: Cell _rv690=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv690);return;}cx->csp--;const void*_r691=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv690);if(!_r691)return;goto *_r691;}
L_1221: Cell t692=var_trans__ptk;L_1222: Cell t693=uf_mkp((void*)&uf_sl117);L_1223: pushc(cx,t692);pushc(cx,t693);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1223,cx->sp>2?cx->sp-2:0);goto L_97;K_1223:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1224: pushp(cx,(void*)&&L_1365);
L_1225: pushp(cx,(void*)&&L_1230);
L_1226: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1226,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1226,cx->sp>0?cx->sp-0:0);goto *el;}K_1226:;}
L_1227: L_1228: L_1229: Cell _rv694=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv694);return;}cx->csp--;const void*_r695=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv694);if(!_r695)return;goto *_r695;}
L_1230: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1230,cx->sp>0?cx->sp-0:0);goto L_118;K_1230:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1231: Cell t696=uf_mkp((void*)&uf_sl118);L_1232: pushc(cx,t696);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1232,cx->sp>2?cx->sp-2:0);goto L_97;K_1232:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1233: pushp(cx,(void*)&&L_1522);
L_1234: pushp(cx,(void*)&&L_1578);
L_1235: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1235,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1235,cx->sp>0?cx->sp-0:0);goto *el;}K_1235:;}
L_1236: L_1237: L_1238: Cell _rv697=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv697);return;}cx->csp--;const void*_r698=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv697);if(!_r698)return;goto *_r698;}
L_1239: Cell t699=var_trans__ptk;L_1240: Cell t700=uf_mkp((void*)&uf_sl119);L_1241: pushc(cx,t699);pushc(cx,t700);uf_cur_op="op_match";op_match(cx);
L_1242: Cell t701=pop(cx);cx->locals[cx->local_base+1]=t701;L_1243: Cell t702=cx->locals[cx->local_base+1];L_1244: L_1245: pushc(cx,t701);pushc(cx,t702);pushi(cx,0LL);uf_cur_op="op_getq";op_getq(cx);
L_1246: Cell t703=pop(cx);L_1247: L_1248: L_1249: var_trans__zm=t703;pushc(cx,t703);pushc(cx,t703);pushi(cx,0LL);uf_cur_op="op_get";op_get(cx);
L_1250: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1250,cx->sp>1?cx->sp-1:0);goto L_42;K_1250:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1251: Cell t704=uf_mkp((void*)&uf_sl120);L_1252: pushc(cx,t704);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1252,cx->sp>1?cx->sp-1:0);goto L_42;K_1252:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1253: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1253,cx->sp>0?cx->sp-0:0);goto L_127;K_1253:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1254: L_1255: L_1256: Cell _rv705=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv705);return;}cx->csp--;const void*_r706=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv705);if(!_r706)return;goto *_r706;}
L_1257: Cell t707=var_trans__ptk;L_1258: pushc(cx,t707);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1258,cx->sp>1?cx->sp-1:0);goto L_42;K_1258:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1259: Cell t708=uf_mkp((void*)&uf_sl121);L_1260: pushc(cx,t708);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1260,cx->sp>1?cx->sp-1:0);goto L_42;K_1260:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1261: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1261,cx->sp>0?cx->sp-0:0);goto L_127;K_1261:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1262: Cell t709=uf_mkp((void*)&uf_sl122);L_1263: L_1264: L_1265: L_1266: var_trans__lasts=t709;var_trans__rr=uf_mki(0LL);pushc(cx,t709);pushi(cx,0LL);L_1267: Cell t710=var_trans__rr;L_1268: Cell _rv711=t710;{if(cx->csp==0){pushc(cx,_rv711);return;}cx->csp--;const void*_r712=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv711);if(!_r712)return;goto *_r712;}
L_1269: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1269,cx->sp>0?cx->sp-0:0);goto L_127;K_1269:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1270: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1270,cx->sp>0?cx->sp-0:0);goto L_566;K_1270:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1271: Cell t713=uf_mkp((void*)&uf_sl123);L_1272: cx->locals[cx->local_base+0]=t713;L_1273: pushc(cx,t713);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1273,cx->sp>0?cx->sp-0:0);goto L_111;K_1273:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1274: Cell t714=cx->locals[cx->local_base+0];L_1275: pushc(cx,t714);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1275,cx->sp>2?cx->sp-2:0);goto L_97;K_1275:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1276: Cell t715=pop(cx);Cell t716=uf_cnot(t715);L_1277: pushc(cx,t716);pushp(cx,(void*)&&L_144);
L_1278: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1278,cx->sp>0?cx->sp-0:0);goto *b;K_1278:;}}
L_1279: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1279,cx->sp>0?cx->sp-0:0);goto L_127;K_1279:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1280: Cell t717=uf_mkp((void*)&uf_sl124);L_1281: L_1282: L_1283: L_1284: var_trans__lasts=t717;var_trans__rr=uf_mki(0LL);pushc(cx,t717);pushi(cx,0LL);L_1285: Cell t718=var_trans__rr;L_1286: Cell _rv719=t718;{if(cx->csp==0){pushc(cx,_rv719);return;}cx->csp--;const void*_r720=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv719);if(!_r720)return;goto *_r720;}
L_1287: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1287,cx->sp>0?cx->sp-0:0);goto L_127;K_1287:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1288: Cell t721=uf_mkp((void*)&uf_sl125);L_1289: pushc(cx,t721);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1289,cx->sp>1?cx->sp-1:0);goto L_42;K_1289:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1290: Cell t722=uf_mkp((void*)&uf_sl126);L_1291: L_1292: L_1293: L_1294: var_trans__lasts=t722;var_trans__rr=uf_mki(0LL);pushc(cx,t722);pushi(cx,0LL);L_1295: Cell t723=var_trans__rr;L_1296: Cell _rv724=t723;{if(cx->csp==0){pushc(cx,_rv724);return;}cx->csp--;const void*_r725=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv724);if(!_r725)return;goto *_r725;}
L_1297: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1297,cx->sp>0?cx->sp-0:0);goto L_127;K_1297:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1298: Cell t726=uf_mkp((void*)&uf_sl127);L_1299: cx->locals[cx->local_base+0]=t726;L_1300: pushc(cx,t726);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1300,cx->sp>0?cx->sp-0:0);goto L_111;K_1300:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1301: Cell t727=cx->locals[cx->local_base+0];L_1302: pushc(cx,t727);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1302,cx->sp>2?cx->sp-2:0);goto L_97;K_1302:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1303: Cell t728=pop(cx);Cell t729=uf_cnot(t728);L_1304: pushc(cx,t729);pushp(cx,(void*)&&L_144);
L_1305: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1305,cx->sp>0?cx->sp-0:0);goto *b;K_1305:;}}
L_1306: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1306,cx->sp>0?cx->sp-0:0);goto L_127;K_1306:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1307: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1307,cx->sp>0?cx->sp-0:0);goto L_566;K_1307:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1308: Cell t730=uf_mkp((void*)&uf_sl128);L_1309: cx->locals[cx->local_base+0]=t730;L_1310: pushc(cx,t730);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1310,cx->sp>0?cx->sp-0:0);goto L_111;K_1310:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1311: Cell t731=cx->locals[cx->local_base+0];L_1312: pushc(cx,t731);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1312,cx->sp>2?cx->sp-2:0);goto L_97;K_1312:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1313: Cell t732=pop(cx);Cell t733=uf_cnot(t732);L_1314: pushc(cx,t733);pushp(cx,(void*)&&L_144);
L_1315: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1315,cx->sp>0?cx->sp-0:0);goto *b;K_1315:;}}
L_1316: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1316,cx->sp>0?cx->sp-0:0);goto L_127;K_1316:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1317: Cell t734=uf_mkp((void*)&uf_sl129);L_1318: pushc(cx,t734);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1318,cx->sp>1?cx->sp-1:0);goto L_42;K_1318:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1319: Cell t735=uf_mkp((void*)&uf_sl130);L_1320: L_1321: L_1322: L_1323: var_trans__lasts=t735;var_trans__rr=uf_mki(0LL);pushc(cx,t735);pushi(cx,0LL);L_1324: Cell t736=var_trans__rr;L_1325: Cell _rv737=t736;{if(cx->csp==0){pushc(cx,_rv737);return;}cx->csp--;const void*_r738=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv737);if(!_r738)return;goto *_r738;}
L_1326: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1326,cx->sp>0?cx->sp-0:0);goto L_127;K_1326:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1327: Cell t739=uf_mkp((void*)&uf_sl131);L_1328: cx->locals[cx->local_base+0]=t739;L_1329: pushc(cx,t739);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1329,cx->sp>0?cx->sp-0:0);goto L_111;K_1329:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1330: Cell t740=cx->locals[cx->local_base+0];L_1331: pushc(cx,t740);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1331,cx->sp>2?cx->sp-2:0);goto L_97;K_1331:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1332: Cell t741=pop(cx);Cell t742=uf_cnot(t741);L_1333: pushc(cx,t742);pushp(cx,(void*)&&L_144);
L_1334: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1334,cx->sp>0?cx->sp-0:0);goto *b;K_1334:;}}
L_1335: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1335,cx->sp>0?cx->sp-0:0);goto L_127;K_1335:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1336: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1336,cx->sp>0?cx->sp-0:0);goto L_566;K_1336:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1337: Cell t743=uf_mkp((void*)&uf_sl132);L_1338: cx->locals[cx->local_base+0]=t743;L_1339: pushc(cx,t743);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1339,cx->sp>0?cx->sp-0:0);goto L_111;K_1339:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1340: Cell t744=cx->locals[cx->local_base+0];L_1341: pushc(cx,t744);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1341,cx->sp>2?cx->sp-2:0);goto L_97;K_1341:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1342: Cell t745=pop(cx);Cell t746=uf_cnot(t745);L_1343: pushc(cx,t746);pushp(cx,(void*)&&L_144);
L_1344: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1344,cx->sp>0?cx->sp-0:0);goto *b;K_1344:;}}
L_1345: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1345,cx->sp>0?cx->sp-0:0);goto L_127;K_1345:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1346: Cell t747=uf_mkp((void*)&uf_sl133);L_1347: pushc(cx,t747);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1347,cx->sp>1?cx->sp-1:0);goto L_42;K_1347:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1348: Cell t748=uf_mkp((void*)&uf_sl134);L_1349: L_1350: L_1351: L_1352: var_trans__lasts=t748;var_trans__rr=uf_mki(0LL);pushc(cx,t748);pushi(cx,0LL);L_1353: Cell t749=var_trans__rr;L_1354: Cell _rv750=t749;{if(cx->csp==0){pushc(cx,_rv750);return;}cx->csp--;const void*_r751=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv750);if(!_r751)return;goto *_r751;}
L_1355: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1355,cx->sp>0?cx->sp-0:0);goto L_127;K_1355:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1356: Cell t752=uf_mkp((void*)&uf_sl135);L_1357: pushc(cx,t752);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1357,cx->sp>1?cx->sp-1:0);goto L_42;K_1357:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1358: Cell t753=uf_mkp((void*)&uf_sl136);L_1359: L_1360: L_1361: L_1362: var_trans__lasts=t753;var_trans__rr=uf_mki(0LL);pushc(cx,t753);pushi(cx,0LL);L_1363: Cell t754=var_trans__rr;L_1364: Cell _rv755=t754;{if(cx->csp==0){pushc(cx,_rv755);return;}cx->csp--;const void*_r756=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv755);if(!_r756)return;goto *_r756;}
L_1365: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1365,cx->sp>0?cx->sp-0:0);goto L_127;K_1365:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1366: Cell t757=uf_mkp((void*)&uf_sl137);L_1367: pushc(cx,t757);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1367,cx->sp>1?cx->sp-1:0);goto L_42;K_1367:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1368: Cell t758=uf_mkp((void*)&uf_sl138);L_1369: L_1370: L_1371: L_1372: var_trans__lasts=t758;var_trans__rr=uf_mki(0LL);pushc(cx,t758);pushi(cx,0LL);L_1373: Cell t759=var_trans__rr;L_1374: Cell _rv760=t759;{if(cx->csp==0){pushc(cx,_rv760);return;}cx->csp--;const void*_r761=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv760);if(!_r761)return;goto *_r761;}
L_1375: Cell t762=var_trans__ptk;L_1376: L_1377: L_1378: pushc(cx,t762);pushi(cx,1LL);pushc(cx,var_trans__ptk);uf_cur_op="strlen";{Cell a0=pop(cx);int r=((int(*)(void*))uf_im9)((void*)uf_sptr(a0));pushi(cx,(int64_t)r);}
L_1379: L_1380: Cell t763=pop(cx);Cell t764=uf_csub(t763,uf_mki(1LL));L_1381: pushc(cx,t764);uf_cur_op="op_slice";op_slice(cx);
L_1382: Cell t765=pop(cx);L_1383: L_1384: Cell t766=uf_mkp((void*)&uf_sl139);L_1385: var_trans__ci=t765;pushc(cx,t765);pushc(cx,t765);pushc(cx,t766);uf_cur_op="op_starts";op_starts(cx);
L_1386: pushp(cx,(void*)&&L_1405);
L_1387: pushp(cx,(void*)&&L_1392);
L_1388: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1388,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1388,cx->sp>0?cx->sp-0:0);goto *el;}K_1388:;}
L_1389: L_1390: L_1391: Cell _rv767=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv767);return;}cx->csp--;const void*_r768=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv767);if(!_r768)return;goto *_r768;}
L_1392: Cell t769=var_trans__ci;L_1393: pushc(cx,t769);uf_cur_op="op_loadx";op_loadx(cx);
L_1394: L_1395: Cell t770=pop(cx);Cell t771=uf_cand(t770,uf_mki(255LL));L_1396: pushc(cx,t771);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1396,cx->sp>1?cx->sp-1:0);goto L_89;K_1396:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1397: Cell t772=uf_mkp((void*)&uf_sl140);L_1398: pushc(cx,t772);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1398,cx->sp>1?cx->sp-1:0);goto L_42;K_1398:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1399: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1399,cx->sp>0?cx->sp-0:0);goto L_127;K_1399:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1400: L_1401: L_1402: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1403: Cell t773=var_trans__rr;L_1404: Cell _rv774=t773;{if(cx->csp==0){pushc(cx,_rv774);return;}cx->csp--;const void*_r775=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv774);if(!_r775)return;goto *_r775;}
L_1405: Cell t776=var_trans__ci;L_1406: Cell t777=uf_mkp((void*)&uf_sl141);L_1407: pushc(cx,t776);pushc(cx,t777);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1407,cx->sp>2?cx->sp-2:0);goto L_97;K_1407:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1408: pushp(cx,(void*)&&L_1414);
L_1409: pushp(cx,(void*)&&L_1424);
L_1410: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1410,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1410,cx->sp>0?cx->sp-0:0);goto *el;}K_1410:;}
L_1411: L_1412: L_1413: Cell _rv778=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv778);return;}cx->csp--;const void*_r779=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv778);if(!_r779)return;goto *_r779;}
L_1414: L_1415: pushi(cx,10LL);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1415,cx->sp>1?cx->sp-1:0);goto L_89;K_1415:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1416: Cell t780=uf_mkp((void*)&uf_sl142);L_1417: pushc(cx,t780);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1417,cx->sp>1?cx->sp-1:0);goto L_42;K_1417:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1418: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1418,cx->sp>0?cx->sp-0:0);goto L_127;K_1418:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1419: L_1420: L_1421: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1422: Cell t781=var_trans__rr;L_1423: Cell _rv782=t781;{if(cx->csp==0){pushc(cx,_rv782);return;}cx->csp--;const void*_r783=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv782);if(!_r783)return;goto *_r783;}
L_1424: Cell t784=var_trans__ci;L_1425: Cell t785=uf_mkp((void*)&uf_sl143);L_1426: pushc(cx,t784);pushc(cx,t785);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1426,cx->sp>2?cx->sp-2:0);goto L_97;K_1426:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1427: pushp(cx,(void*)&&L_1433);
L_1428: pushp(cx,(void*)&&L_1443);
L_1429: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1429,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1429,cx->sp>0?cx->sp-0:0);goto *el;}K_1429:;}
L_1430: L_1431: L_1432: Cell _rv786=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv786);return;}cx->csp--;const void*_r787=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv786);if(!_r787)return;goto *_r787;}
L_1433: L_1434: pushi(cx,9LL);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1434,cx->sp>1?cx->sp-1:0);goto L_89;K_1434:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1435: Cell t788=uf_mkp((void*)&uf_sl144);L_1436: pushc(cx,t788);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1436,cx->sp>1?cx->sp-1:0);goto L_42;K_1436:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1437: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1437,cx->sp>0?cx->sp-0:0);goto L_127;K_1437:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1438: L_1439: L_1440: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1441: Cell t789=var_trans__rr;L_1442: Cell _rv790=t789;{if(cx->csp==0){pushc(cx,_rv790);return;}cx->csp--;const void*_r791=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv790);if(!_r791)return;goto *_r791;}
L_1443: Cell t792=var_trans__ci;L_1444: Cell t793=uf_mkp((void*)&uf_sl145);L_1445: pushc(cx,t792);pushc(cx,t793);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1445,cx->sp>2?cx->sp-2:0);goto L_97;K_1445:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1446: pushp(cx,(void*)&&L_1452);
L_1447: pushp(cx,(void*)&&L_1462);
L_1448: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1448,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1448,cx->sp>0?cx->sp-0:0);goto *el;}K_1448:;}
L_1449: L_1450: L_1451: Cell _rv794=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv794);return;}cx->csp--;const void*_r795=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv794);if(!_r795)return;goto *_r795;}
L_1452: L_1453: pushi(cx,13LL);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1453,cx->sp>1?cx->sp-1:0);goto L_89;K_1453:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1454: Cell t796=uf_mkp((void*)&uf_sl146);L_1455: pushc(cx,t796);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1455,cx->sp>1?cx->sp-1:0);goto L_42;K_1455:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1456: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1456,cx->sp>0?cx->sp-0:0);goto L_127;K_1456:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1457: L_1458: L_1459: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1460: Cell t797=var_trans__rr;L_1461: Cell _rv798=t797;{if(cx->csp==0){pushc(cx,_rv798);return;}cx->csp--;const void*_r799=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv798);if(!_r799)return;goto *_r799;}
L_1462: Cell t800=var_trans__ci;L_1463: Cell t801=uf_mkp((void*)&uf_sl147);L_1464: pushc(cx,t800);pushc(cx,t801);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1464,cx->sp>2?cx->sp-2:0);goto L_97;K_1464:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1465: pushp(cx,(void*)&&L_1471);
L_1466: pushp(cx,(void*)&&L_1481);
L_1467: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1467,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1467,cx->sp>0?cx->sp-0:0);goto *el;}K_1467:;}
L_1468: L_1469: L_1470: Cell _rv802=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv802);return;}cx->csp--;const void*_r803=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv802);if(!_r803)return;goto *_r803;}
L_1471: L_1472: pushi(cx,0LL);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1472,cx->sp>1?cx->sp-1:0);goto L_89;K_1472:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1473: Cell t804=uf_mkp((void*)&uf_sl148);L_1474: pushc(cx,t804);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1474,cx->sp>1?cx->sp-1:0);goto L_42;K_1474:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1475: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1475,cx->sp>0?cx->sp-0:0);goto L_127;K_1475:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1476: L_1477: L_1478: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1479: Cell t805=var_trans__rr;L_1480: Cell _rv806=t805;{if(cx->csp==0){pushc(cx,_rv806);return;}cx->csp--;const void*_r807=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv806);if(!_r807)return;goto *_r807;}
L_1481: Cell t808=var_trans__ci;L_1482: Cell t809=uf_mkp((void*)&uf_sl149);L_1483: pushc(cx,t808);pushc(cx,t809);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1483,cx->sp>2?cx->sp-2:0);goto L_97;K_1483:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1484: pushp(cx,(void*)&&L_1490);
L_1485: pushp(cx,(void*)&&L_1500);
L_1486: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1486,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1486,cx->sp>0?cx->sp-0:0);goto *el;}K_1486:;}
L_1487: L_1488: L_1489: Cell _rv810=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv810);return;}cx->csp--;const void*_r811=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv810);if(!_r811)return;goto *_r811;}
L_1490: L_1491: pushi(cx,92LL);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1491,cx->sp>1?cx->sp-1:0);goto L_89;K_1491:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1492: Cell t812=uf_mkp((void*)&uf_sl150);L_1493: pushc(cx,t812);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1493,cx->sp>1?cx->sp-1:0);goto L_42;K_1493:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1494: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1494,cx->sp>0?cx->sp-0:0);goto L_127;K_1494:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1495: L_1496: L_1497: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1498: Cell t813=var_trans__rr;L_1499: Cell _rv814=t813;{if(cx->csp==0){pushc(cx,_rv814);return;}cx->csp--;const void*_r815=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv814);if(!_r815)return;goto *_r815;}
L_1500: Cell t816=var_trans__ci;L_1501: Cell t817=uf_mkp((void*)&uf_sl151);L_1502: pushc(cx,t816);pushc(cx,t817);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1502,cx->sp>2?cx->sp-2:0);goto L_97;K_1502:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1503: pushp(cx,(void*)&&L_1509);
L_1504: pushp(cx,(void*)&&L_1519);
L_1505: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1505,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1505,cx->sp>0?cx->sp-0:0);goto *el;}K_1505:;}
L_1506: L_1507: L_1508: Cell _rv818=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv818);return;}cx->csp--;const void*_r819=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv818);if(!_r819)return;goto *_r819;}
L_1509: L_1510: pushi(cx,39LL);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1510,cx->sp>1?cx->sp-1:0);goto L_89;K_1510:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1511: Cell t820=uf_mkp((void*)&uf_sl152);L_1512: pushc(cx,t820);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1512,cx->sp>1?cx->sp-1:0);goto L_42;K_1512:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1513: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1513,cx->sp>0?cx->sp-0:0);goto L_127;K_1513:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1514: L_1515: L_1516: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1517: Cell t821=var_trans__rr;L_1518: Cell _rv822=t821;{if(cx->csp==0){pushc(cx,_rv822);return;}cx->csp--;const void*_r823=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv822);if(!_r823)return;goto *_r823;}
L_1519: Cell t824=uf_mkp((void*)&uf_sl153);L_1520: pushc(cx,t824);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1520,cx->sp>0?cx->sp-0:0);goto L_107;K_1520:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1521: Cell _rv825=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv825);return;}cx->csp--;const void*_r826=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv825);if(!_r826)return;goto *_r826;}
L_1522: Cell t827=var_trans__ptk;L_1523: pushc(cx,t827);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1523,cx->sp>1?cx->sp-1:0);goto L_0;K_1523:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1524: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1524,cx->sp>0?cx->sp-0:0);goto L_127;K_1524:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1525: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1525,cx->sp>0?cx->sp-0:0);goto L_127;K_1525:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1526: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1526,cx->sp>0?cx->sp-0:0);goto L_111;K_1526:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1527: Cell t828=uf_mkp((void*)&uf_sl154);L_1528: pushc(cx,t828);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1528,cx->sp>2?cx->sp-2:0);goto L_97;K_1528:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1529: pushp(cx,(void*)&&L_1552);
L_1530: pushp(cx,(void*)&&L_1557);
L_1531: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1531,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1531,cx->sp>0?cx->sp-0:0);goto *el;}K_1531:;}
L_1532: Cell t829=uf_mkp((void*)&uf_sl155);L_1533: cx->locals[cx->local_base+0]=t829;L_1534: pushc(cx,t829);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1534,cx->sp>0?cx->sp-0:0);goto L_111;K_1534:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1535: Cell t830=cx->locals[cx->local_base+0];L_1536: pushc(cx,t830);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1536,cx->sp>2?cx->sp-2:0);goto L_97;K_1536:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1537: Cell t831=pop(cx);Cell t832=uf_cnot(t831);L_1538: pushc(cx,t832);pushp(cx,(void*)&&L_144);
L_1539: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1539,cx->sp>0?cx->sp-0:0);goto *b;K_1539:;}}
L_1540: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1540,cx->sp>0?cx->sp-0:0);goto L_127;K_1540:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1541: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1541,cx->sp>0?cx->sp-0:0);goto L_36;K_1541:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1542: Cell t833=uf_mkp((void*)&uf_sl156);L_1543: pushc(cx,t833);uf_cur_op="op_fmt";op_fmt(cx);
L_1544: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1544,cx->sp>1?cx->sp-1:0);goto L_42;K_1544:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1545: Cell t834=uf_mkp((void*)&uf_sl157);L_1546: L_1547: L_1548: L_1549: var_trans__lasts=t834;var_trans__rr=uf_mki(0LL);pushc(cx,t834);pushi(cx,0LL);L_1550: Cell t835=var_trans__rr;L_1551: Cell _rv836=t835;{if(cx->csp==0){pushc(cx,_rv836);return;}cx->csp--;const void*_r837=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv836);if(!_r837)return;goto *_r837;}
L_1552: L_1553: L_1554: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1555: Cell t838=var_trans__rr;L_1556: Cell _rv839=t838;{if(cx->csp==0){pushc(cx,_rv839);return;}cx->csp--;const void*_r840=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv839);if(!_r840)return;goto *_r840;}
L_1557: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1557,cx->sp>0?cx->sp-0:0);goto L_566;K_1557:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1558: L_1559: L_1560: {long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_1560;cx->loops[fr].end=&&K_WE_1560;long _sp0=cx->sp;
K_WC_1560:;{Cell _wc;{
WC1560_L1564: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC1560_1564,cx->sp>0?cx->sp-0:0);goto L_111;K_WC1560_1564:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC1560_L1565: Cell t0=uf_mkp((void*)&uf_sl158);WC1560_L1566: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC1560_1566,cx->sp>2?cx->sp-2:0);goto L_97;K_WC1560_1566:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC1560_L1567: Cell t1=pop(cx);WC1560_L1568: var_trans__rr=t1;pushc(cx,t1);WC1560_L1569: Cell t2=var_trans__rr;pushc(cx,t2);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_1560;
{
WB1560_L1571: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB1560_1571,cx->sp>0?cx->sp-0:0);goto L_127;K_WB1560_1571:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB1560_L1572: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB1560_1572,cx->sp>0?cx->sp-0:0);goto L_566;K_WB1560_1572:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB1560_L1573: WB1560_L1574: WB1560_L1575: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);WB1560_L1576: Cell t0=var_trans__rr;pushc(cx,t0);}cx->sp=_sp0;goto K_WC_1560;}
K_WE_1560:;cx->lsp=fr;}
L_1561: L_1562: L_1563: Cell _rv841=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv841);return;}cx->csp--;const void*_r842=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv841);if(!_r842)return;goto *_r842;}
L_1564: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1564,cx->sp>0?cx->sp-0:0);goto L_111;K_1564:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1565: Cell t843=uf_mkp((void*)&uf_sl158);L_1566: pushc(cx,t843);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1566,cx->sp>2?cx->sp-2:0);goto L_97;K_1566:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1567: Cell t844=pop(cx);L_1568: var_trans__rr=t844;pushc(cx,t844);L_1569: Cell t845=var_trans__rr;L_1570: Cell _rv846=t845;{if(cx->csp==0){pushc(cx,_rv846);return;}cx->csp--;const void*_r847=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv846);if(!_r847)return;goto *_r847;}
L_1571: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1571,cx->sp>0?cx->sp-0:0);goto L_127;K_1571:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1572: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1572,cx->sp>0?cx->sp-0:0);goto L_566;K_1572:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1573: L_1574: L_1575: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1576: Cell t848=var_trans__rr;L_1577: Cell _rv849=t848;{if(cx->csp==0){pushc(cx,_rv849);return;}cx->csp--;const void*_r850=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv849);if(!_r850)return;goto *_r850;}
L_1578: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1578,cx->sp>0?cx->sp-0:0);goto L_118;K_1578:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1579: Cell t851=uf_mkp((void*)&uf_sl159);L_1580: pushc(cx,t851);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1580,cx->sp>2?cx->sp-2:0);goto L_97;K_1580:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1581: pushp(cx,(void*)&&L_1587);
L_1582: pushp(cx,(void*)&&L_1618);
L_1583: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1583,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1583,cx->sp>0?cx->sp-0:0);goto *el;}K_1583:;}
L_1584: L_1585: L_1586: Cell _rv852=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv852);return;}cx->csp--;const void*_r853=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv852);if(!_r853)return;goto *_r853;}
L_1587: Cell t854=var_trans__vars;L_1588: Cell t855=var_trans__ptk;L_1589: pushc(cx,t854);pushc(cx,t855);uf_cur_op="op_getq";op_getq(cx);
L_1590: Cell t856=pop(cx);L_1591: L_1592: Cell t857=uf_cnot(t856);L_1593: var_trans__lv=t856;pushc(cx,t856);pushc(cx,t857);pushp(cx,(void*)&&L_1635);
L_1594: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1594,cx->sp>0?cx->sp-0:0);goto *b;K_1594:;}}
L_1595: Cell t858=var_trans__lv;L_1596: pushc(cx,t858);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1596,cx->sp>1?cx->sp-1:0);goto L_0;K_1596:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1597: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1597,cx->sp>0?cx->sp-0:0);goto L_127;K_1597:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1598: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1598,cx->sp>0?cx->sp-0:0);goto L_127;K_1598:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1599: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1599,cx->sp>0?cx->sp-0:0);goto L_566;K_1599:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1600: Cell t859=uf_mkp((void*)&uf_sl160);L_1601: cx->locals[cx->local_base+0]=t859;L_1602: pushc(cx,t859);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1602,cx->sp>0?cx->sp-0:0);goto L_111;K_1602:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1603: Cell t860=cx->locals[cx->local_base+0];L_1604: pushc(cx,t860);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1604,cx->sp>2?cx->sp-2:0);goto L_97;K_1604:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1605: Cell t861=pop(cx);Cell t862=uf_cnot(t861);L_1606: pushc(cx,t862);pushp(cx,(void*)&&L_144);
L_1607: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1607,cx->sp>0?cx->sp-0:0);goto *b;K_1607:;}}
L_1608: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1608,cx->sp>0?cx->sp-0:0);goto L_127;K_1608:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1609: L_1610: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1610,cx->sp>0?cx->sp-0:0);goto L_36;K_1610:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1611: Cell t863=uf_mkp((void*)&uf_sl161);L_1612: pushc(cx,t863);uf_cur_op="op_fmt";op_fmt(cx);
L_1613: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1613,cx->sp>1?cx->sp-1:0);goto L_42;K_1613:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1614: Cell t864=uf_mkp((void*)&uf_sl162);L_1615: L_1616: L_1617: var_trans__lasts=t864;pushc(cx,t864);pushi(cx,0LL);Cell _rv865=uf_list_build(cx,2);{if(cx->csp==0){pushc(cx,_rv865);return;}cx->csp--;const void*_r866=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv865);if(!_r866)return;goto *_r866;}
L_1618: Cell t867=var_trans__vars;L_1619: Cell t868=var_trans__ptk;L_1620: pushc(cx,t867);pushc(cx,t868);uf_cur_op="op_getq";op_getq(cx);
L_1621: Cell t869=pop(cx);L_1622: L_1623: Cell t870=uf_cnot(t869);L_1624: var_trans__lv=t869;pushc(cx,t869);pushc(cx,t870);pushp(cx,(void*)&&L_1638);
L_1625: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1625,cx->sp>0?cx->sp-0:0);goto *b;K_1625:;}}
L_1626: Cell t871=var_trans__lv;L_1627: L_1628: Cell t872=uf_mkp((void*)&uf_sl163);L_1629: var_trans__lasts=t871;pushc(cx,t871);pushc(cx,t872);uf_cur_op="op_fmt";op_fmt(cx);
L_1630: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1630,cx->sp>1?cx->sp-1:0);goto L_42;K_1630:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1631: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1631,cx->sp>0?cx->sp-0:0);goto L_127;K_1631:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1632: L_1633: L_1634: Cell _rv873=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv873);return;}cx->csp--;const void*_r874=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv873);if(!_r874)return;goto *_r874;}
L_1635: Cell t875=uf_mkp((void*)&uf_sl164);L_1636: pushc(cx,t875);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1636,cx->sp>0?cx->sp-0:0);goto L_107;K_1636:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1637: Cell _rv876=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv876);return;}cx->csp--;const void*_r877=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv876);if(!_r877)return;goto *_r877;}
L_1638: Cell t878=uf_mkp((void*)&uf_sl165);L_1639: Cell t879=var_trans__ptk;L_1640: pushc(cx,t878);pushc(cx,t879);uf_cur_op="op_cat";op_cat(cx);
L_1641: Cell t880=uf_mkp((void*)&uf_sl166);L_1642: pushc(cx,t880);uf_cur_op="op_cat";op_cat(cx);
L_1643: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1643,cx->sp>0?cx->sp-0:0);goto L_107;K_1643:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1644: Cell _rv881=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv881);return;}cx->csp--;const void*_r882=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv881);if(!_r882)return;goto *_r882;}
L_1645: L_1646: L_1647: var_trans__didret=uf_mki(0LL);pushi(cx,0LL);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1647,cx->sp>0?cx->sp-0:0);goto L_111;K_1647:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1648: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1648,cx->sp>1?cx->sp-1:0);goto L_155;K_1648:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1649: pushp(cx,(void*)&&L_1734);
L_1650: pushp(cx,(void*)&&L_1653);
L_1651: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1651,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1651,cx->sp>0?cx->sp-0:0);goto *el;}K_1651:;}
L_1652: Cell _rv883=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv883);return;}cx->csp--;const void*_r884=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv883);if(!_r884)return;goto *_r884;}
L_1653: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1653,cx->sp>0?cx->sp-0:0);goto L_111;K_1653:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1654: Cell t885=uf_mkp((void*)&uf_sl167);L_1655: pushc(cx,t885);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1655,cx->sp>2?cx->sp-2:0);goto L_97;K_1655:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1656: pushp(cx,(void*)&&L_1796);
L_1657: pushp(cx,(void*)&&L_1662);
L_1658: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1658,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1658,cx->sp>0?cx->sp-0:0);goto *el;}K_1658:;}
L_1659: L_1660: L_1661: Cell _rv886=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv886);return;}cx->csp--;const void*_r887=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv886);if(!_r887)return;goto *_r887;}
L_1662: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1662,cx->sp>0?cx->sp-0:0);goto L_111;K_1662:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1663: Cell t888=uf_mkp((void*)&uf_sl168);L_1664: pushc(cx,t888);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1664,cx->sp>2?cx->sp-2:0);goto L_97;K_1664:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1665: pushp(cx,(void*)&&L_1940);
L_1666: pushp(cx,(void*)&&L_1671);
L_1667: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1667,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1667,cx->sp>0?cx->sp-0:0);goto *el;}K_1667:;}
L_1668: L_1669: L_1670: Cell _rv889=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv889);return;}cx->csp--;const void*_r890=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv889);if(!_r890)return;goto *_r890;}
L_1671: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1671,cx->sp>0?cx->sp-0:0);goto L_111;K_1671:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1672: Cell t891=uf_mkp((void*)&uf_sl169);L_1673: pushc(cx,t891);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1673,cx->sp>2?cx->sp-2:0);goto L_97;K_1673:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1674: pushp(cx,(void*)&&L_2073);
L_1675: pushp(cx,(void*)&&L_1680);
L_1676: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1676,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1676,cx->sp>0?cx->sp-0:0);goto *el;}K_1676:;}
L_1677: L_1678: L_1679: Cell _rv892=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv892);return;}cx->csp--;const void*_r893=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv892);if(!_r893)return;goto *_r893;}
L_1680: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1680,cx->sp>0?cx->sp-0:0);goto L_111;K_1680:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1681: Cell t894=uf_mkp((void*)&uf_sl170);L_1682: pushc(cx,t894);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1682,cx->sp>2?cx->sp-2:0);goto L_97;K_1682:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1683: pushp(cx,(void*)&&L_2166);
L_1684: pushp(cx,(void*)&&L_1689);
L_1685: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1685,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1685,cx->sp>0?cx->sp-0:0);goto *el;}K_1685:;}
L_1686: L_1687: L_1688: Cell _rv895=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv895);return;}cx->csp--;const void*_r896=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv895);if(!_r896)return;goto *_r896;}
L_1689: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1689,cx->sp>0?cx->sp-0:0);goto L_111;K_1689:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1690: Cell t897=uf_mkp((void*)&uf_sl171);L_1691: pushc(cx,t897);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1691,cx->sp>2?cx->sp-2:0);goto L_97;K_1691:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1692: pushp(cx,(void*)&&L_2454);
L_1693: pushp(cx,(void*)&&L_1698);
L_1694: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1694,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1694,cx->sp>0?cx->sp-0:0);goto *el;}K_1694:;}
L_1695: L_1696: L_1697: Cell _rv898=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv898);return;}cx->csp--;const void*_r899=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv898);if(!_r899)return;goto *_r899;}
L_1698: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1698,cx->sp>0?cx->sp-0:0);goto L_111;K_1698:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1699: Cell t900=uf_mkp((void*)&uf_sl172);L_1700: pushc(cx,t900);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1700,cx->sp>2?cx->sp-2:0);goto L_97;K_1700:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1701: pushp(cx,(void*)&&L_2704);
L_1702: pushp(cx,(void*)&&L_1707);
L_1703: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1703,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1703,cx->sp>0?cx->sp-0:0);goto *el;}K_1703:;}
L_1704: L_1705: L_1706: Cell _rv901=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv901);return;}cx->csp--;const void*_r902=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv901);if(!_r902)return;goto *_r902;}
L_1707: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1707,cx->sp>0?cx->sp-0:0);goto L_111;K_1707:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1708: Cell t903=uf_mkp((void*)&uf_sl173);L_1709: pushc(cx,t903);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1709,cx->sp>2?cx->sp-2:0);goto L_97;K_1709:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1710: pushp(cx,(void*)&&L_2721);
L_1711: pushp(cx,(void*)&&L_1716);
L_1712: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1712,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1712,cx->sp>0?cx->sp-0:0);goto *el;}K_1712:;}
L_1713: L_1714: L_1715: Cell _rv904=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv904);return;}cx->csp--;const void*_r905=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv904);if(!_r905)return;goto *_r905;}
L_1716: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1716,cx->sp>0?cx->sp-0:0);goto L_111;K_1716:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1717: Cell t906=uf_mkp((void*)&uf_sl174);L_1718: pushc(cx,t906);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1718,cx->sp>2?cx->sp-2:0);goto L_97;K_1718:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1719: pushp(cx,(void*)&&L_2738);
L_1720: pushp(cx,(void*)&&L_1725);
L_1721: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1721,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1721,cx->sp>0?cx->sp-0:0);goto *el;}K_1721:;}
L_1722: L_1723: L_1724: Cell _rv907=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv907);return;}cx->csp--;const void*_r908=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv907);if(!_r908)return;goto *_r908;}
L_1725: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1725,cx->sp>0?cx->sp-0:0);goto L_111;K_1725:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1726: Cell t909=uf_mkp((void*)&uf_sl175);L_1727: pushc(cx,t909);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1727,cx->sp>2?cx->sp-2:0);goto L_97;K_1727:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1728: pushp(cx,(void*)&&L_2760);
L_1729: pushp(cx,(void*)&&L_2766);
L_1730: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1730,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1730,cx->sp>0?cx->sp-0:0);goto *el;}K_1730:;}
L_1731: L_1732: L_1733: Cell _rv910=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv910);return;}cx->csp--;const void*_r911=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv910);if(!_r911)return;goto *_r911;}
L_1734: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1734,cx->sp>0?cx->sp-0:0);goto L_195;K_1734:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1735: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1735,cx->sp>0?cx->sp-0:0);goto L_1765;K_1735:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1736: L_1737: L_1738: {long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_1738;cx->loops[fr].end=&&K_WE_1738;long _sp0=cx->sp;
K_WC_1738:;{Cell _wc;{
WC1738_L1751: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC1738_1751,cx->sp>0?cx->sp-0:0);goto L_111;K_WC1738_1751:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC1738_L1752: Cell t0=uf_mkp((void*)&uf_sl177);WC1738_L1753: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC1738_1753,cx->sp>2?cx->sp-2:0);goto L_97;K_WC1738_1753:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC1738_L1754: Cell t1=pop(cx);WC1738_L1755: var_trans__rr=t1;pushc(cx,t1);WC1738_L1756: Cell t2=var_trans__rr;pushc(cx,t2);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_1738;
{
WB1738_L1758: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB1738_1758,cx->sp>0?cx->sp-0:0);goto L_127;K_WB1738_1758:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB1738_L1759: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB1738_1759,cx->sp>0?cx->sp-0:0);goto L_1765;K_WB1738_1759:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB1738_L1760: WB1738_L1761: WB1738_L1762: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);WB1738_L1763: Cell t0=var_trans__rr;pushc(cx,t0);}cx->sp=_sp0;goto K_WC_1738;}
K_WE_1738:;cx->lsp=fr;}
L_1739: L_1740: Cell t912=uf_mkp((void*)&uf_sl176);L_1741: cx->locals[cx->local_base+0]=t912;L_1742: pushc(cx,t912);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1742,cx->sp>0?cx->sp-0:0);goto L_111;K_1742:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1743: Cell t913=cx->locals[cx->local_base+0];L_1744: pushc(cx,t913);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1744,cx->sp>2?cx->sp-2:0);goto L_97;K_1744:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1745: Cell t914=pop(cx);Cell t915=uf_cnot(t914);L_1746: pushc(cx,t915);pushp(cx,(void*)&&L_144);
L_1747: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1747,cx->sp>0?cx->sp-0:0);goto *b;K_1747:;}}
L_1748: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1748,cx->sp>0?cx->sp-0:0);goto L_127;K_1748:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1749: L_1750: Cell _rv916=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv916);return;}cx->csp--;const void*_r917=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv916);if(!_r917)return;goto *_r917;}
L_1751: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1751,cx->sp>0?cx->sp-0:0);goto L_111;K_1751:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1752: Cell t918=uf_mkp((void*)&uf_sl177);L_1753: pushc(cx,t918);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1753,cx->sp>2?cx->sp-2:0);goto L_97;K_1753:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1754: Cell t919=pop(cx);L_1755: var_trans__rr=t919;pushc(cx,t919);L_1756: Cell t920=var_trans__rr;L_1757: Cell _rv921=t920;{if(cx->csp==0){pushc(cx,_rv921);return;}cx->csp--;const void*_r922=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv921);if(!_r922)return;goto *_r922;}
L_1758: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1758,cx->sp>0?cx->sp-0:0);goto L_127;K_1758:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1759: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1759,cx->sp>0?cx->sp-0:0);goto L_1765;K_1759:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1760: L_1761: L_1762: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1763: Cell t923=var_trans__rr;L_1764: Cell _rv924=t923;{if(cx->csp==0){pushc(cx,_rv924);return;}cx->csp--;const void*_r925=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv924);if(!_r925)return;goto *_r925;}
L_1765: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1765,cx->sp>0?cx->sp-0:0);goto L_111;K_1765:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1766: Cell t926=pop(cx);L_1767: var_trans__nv=t926;pushc(cx,t926);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1767,cx->sp>0?cx->sp-0:0);goto L_127;K_1767:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1768: Cell t927=var_trans__nv;L_1769: pushc(cx,t927);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1769,cx->sp>1?cx->sp-1:0);goto L_215;K_1769:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1770: Cell t928=pop(cx);L_1771: var_trans__slot=t928;pushc(cx,t928);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1771,cx->sp>0?cx->sp-0:0);goto L_111;K_1771:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1772: Cell t929=uf_mkp((void*)&uf_sl178);L_1773: pushc(cx,t929);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1773,cx->sp>2?cx->sp-2:0);goto L_97;K_1773:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1774: pushp(cx,(void*)&&L_1778);
L_1775: pushp(cx,(void*)&&L_1791);
L_1776: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1776,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1776,cx->sp>0?cx->sp-0:0);goto *el;}K_1776:;}
L_1777: Cell _rv930=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv930);return;}cx->csp--;const void*_r931=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv930);if(!_r931)return;goto *_r931;}
L_1778: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1778,cx->sp>0?cx->sp-0:0);goto L_127;K_1778:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1779: Cell t932=var_trans__slot;L_1780: pushc(cx,t932);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1780,cx->sp>1?cx->sp-1:0);goto L_0;K_1780:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1781: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1781,cx->sp>0?cx->sp-0:0);goto L_571;K_1781:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1782: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1782,cx->sp>0?cx->sp-0:0);goto L_36;K_1782:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1783: Cell t933=pop(cx);L_1784: L_1785: Cell t934=uf_mkp((void*)&uf_sl179);L_1786: var_trans__slot2=t933;pushc(cx,t933);pushc(cx,t933);pushc(cx,t934);uf_cur_op="op_fmt";op_fmt(cx);
L_1787: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1787,cx->sp>1?cx->sp-1:0);goto L_42;K_1787:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1788: L_1789: L_1790: Cell _rv935=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv935);return;}cx->csp--;const void*_r936=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv935);if(!_r936)return;goto *_r936;}
L_1791: L_1792: L_1793: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_1794: Cell t937=var_trans__rr;L_1795: Cell _rv938=t937;{if(cx->csp==0){pushc(cx,_rv938);return;}cx->csp--;const void*_r939=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv938);if(!_r939)return;goto *_r939;}
L_1796: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1796,cx->sp>0?cx->sp-0:0);goto L_127;K_1796:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1797: Cell t940=var_trans__inmain;L_1798: pushc(cx,t940);pushp(cx,(void*)&&L_1804);
L_1799: pushp(cx,(void*)&&L_1823);
L_1800: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1800,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1800,cx->sp>0?cx->sp-0:0);goto *el;}K_1800:;}
L_1801: L_1802: L_1803: Cell _rv941=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv941);return;}cx->csp--;const void*_r942=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv941);if(!_r942)return;goto *_r942;}
L_1804: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1804,cx->sp>0?cx->sp-0:0);goto L_566;K_1804:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1805: Cell t943=uf_mkp((void*)&uf_sl180);L_1806: cx->locals[cx->local_base+0]=t943;L_1807: pushc(cx,t943);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1807,cx->sp>0?cx->sp-0:0);goto L_111;K_1807:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1808: Cell t944=cx->locals[cx->local_base+0];L_1809: pushc(cx,t944);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1809,cx->sp>2?cx->sp-2:0);goto L_97;K_1809:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1810: Cell t945=pop(cx);Cell t946=uf_cnot(t945);L_1811: pushc(cx,t946);pushp(cx,(void*)&&L_144);
L_1812: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1812,cx->sp>0?cx->sp-0:0);goto *b;K_1812:;}}
L_1813: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1813,cx->sp>0?cx->sp-0:0);goto L_127;K_1813:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1814: Cell t947=uf_mkp((void*)&uf_sl181);L_1815: pushc(cx,t947);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1815,cx->sp>1?cx->sp-1:0);goto L_42;K_1815:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1816: L_1817: L_1818: L_1819: L_1820: var_trans__didret=uf_mki(0LL);var_trans__rr=uf_mki(0LL);pushi(cx,0LL);pushi(cx,0LL);L_1821: Cell t948=var_trans__rr;L_1822: Cell _rv949=t948;{if(cx->csp==0){pushc(cx,_rv949);return;}cx->csp--;const void*_r950=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv949);if(!_r950)return;goto *_r950;}
L_1823: Cell t951=var_trans__inq;L_1824: pushc(cx,t951);pushp(cx,(void*)&&L_1849);
L_1825: pushp(cx,(void*)&&L_1830);
L_1826: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1826,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1826,cx->sp>0?cx->sp-0:0);goto *el;}K_1826:;}
L_1827: L_1828: L_1829: Cell _rv952=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv952);return;}cx->csp--;const void*_r953=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv952);if(!_r953)return;goto *_r953;}
L_1830: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1830,cx->sp>0?cx->sp-0:0);goto L_566;K_1830:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1831: Cell t954=uf_mkp((void*)&uf_sl182);L_1832: cx->locals[cx->local_base+0]=t954;L_1833: pushc(cx,t954);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1833,cx->sp>0?cx->sp-0:0);goto L_111;K_1833:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1834: Cell t955=cx->locals[cx->local_base+0];L_1835: pushc(cx,t955);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1835,cx->sp>2?cx->sp-2:0);goto L_97;K_1835:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1836: Cell t956=pop(cx);Cell t957=uf_cnot(t956);L_1837: pushc(cx,t957);pushp(cx,(void*)&&L_144);
L_1838: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1838,cx->sp>0?cx->sp-0:0);goto *b;K_1838:;}}
L_1839: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1839,cx->sp>0?cx->sp-0:0);goto L_127;K_1839:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1840: Cell t958=uf_mkp((void*)&uf_sl183);L_1841: pushc(cx,t958);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1841,cx->sp>1?cx->sp-1:0);goto L_42;K_1841:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1842: L_1843: L_1844: L_1845: L_1846: var_trans__didret=uf_mki(2LL);var_trans__rr=uf_mki(0LL);pushi(cx,2LL);pushi(cx,0LL);L_1847: Cell t959=var_trans__rr;L_1848: Cell _rv960=t959;{if(cx->csp==0){pushc(cx,_rv960);return;}cx->csp--;const void*_r961=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv960);if(!_r961)return;goto *_r961;}
L_1849: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1849,cx->sp>0?cx->sp-0:0);goto L_566;K_1849:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1850: Cell t962=uf_mkp((void*)&uf_sl184);L_1851: cx->locals[cx->local_base+0]=t962;L_1852: pushc(cx,t962);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1852,cx->sp>0?cx->sp-0:0);goto L_111;K_1852:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1853: Cell t963=cx->locals[cx->local_base+0];L_1854: pushc(cx,t963);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1854,cx->sp>2?cx->sp-2:0);goto L_97;K_1854:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1855: Cell t964=pop(cx);Cell t965=uf_cnot(t964);L_1856: pushc(cx,t965);pushp(cx,(void*)&&L_144);
L_1857: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1857,cx->sp>0?cx->sp-0:0);goto *b;K_1857:;}}
L_1858: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1858,cx->sp>0?cx->sp-0:0);goto L_127;K_1858:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1859: Cell t966=uf_mkp((void*)&uf_sl185);L_1860: pushc(cx,t966);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1860,cx->sp>1?cx->sp-1:0);goto L_42;K_1860:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1861: L_1862: L_1863: L_1864: L_1865: L_1866: L_1867: var_trans__cret=uf_mki(1LL);var_trans__didret=uf_mki(2LL);var_trans__rr=uf_mki(0LL);pushi(cx,1LL);pushi(cx,2LL);pushi(cx,0LL);L_1868: Cell t967=var_trans__rr;L_1869: Cell _rv968=t967;{if(cx->csp==0){pushc(cx,_rv968);return;}cx->csp--;const void*_r969=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv968);if(!_r969)return;goto *_r969;}
L_1870: Cell t970=var_trans__douts;L_1871: pushc(cx,t970);uf_cur_op="op_lpop";op_lpop(cx);
L_1872: Cell t971=pop(cx);L_1873: L_1874: Cell t972=var_trans__pends;L_1875: var_trans__dchunk=t971;pushc(cx,t971);pushc(cx,t971);pushc(cx,t972);uf_cur_op="op_cat";op_cat(cx);
L_1876: Cell t973=pop(cx);L_1877: Cell t974=var_trans__psnaps;L_1878: var_trans__cp=t973;pushc(cx,t973);pushc(cx,t974);uf_cur_op="op_lpop";op_lpop(cx);
L_1879: Cell t975=pop(cx);L_1880: L_1881: Cell t976=var_trans__cp;L_1882: var_trans__sv=t975;pushc(cx,t975);pushc(cx,t975);pushc(cx,t976);uf_cur_op="op_cat";op_cat(cx);
L_1883: Cell t977=var_trans__pends;L_1884: pushc(cx,t977);uf_cur_op="op_cat";op_cat(cx);
L_1885: Cell t978=pop(cx);L_1886: var_trans__pends=t978;pushc(cx,t978);L_1887: L_1888: Cell _rv979=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv979);return;}cx->csp--;const void*_r980=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv979);if(!_r980)return;goto *_r980;}
L_1889: Cell t981=var_trans__didret;L_1890: L_1891: Cell t982=uf_ceq(t981,uf_mki(2LL));L_1892: pushc(cx,t982);pushp(cx,(void*)&&L_1920);
L_1893: pushp(cx,(void*)&&L_1898);
L_1894: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1894,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1894,cx->sp>0?cx->sp-0:0);goto *el;}K_1894:;}
L_1895: L_1896: L_1897: Cell _rv983=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv983);return;}cx->csp--;const void*_r984=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv983);if(!_r984)return;goto *_r984;}
L_1898: Cell t985=uf_mkp((void*)&uf_sl186);L_1899: pushc(cx,t985);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1899,cx->sp>1?cx->sp-1:0);goto L_42;K_1899:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1900: Cell t986=var_trans__pends;L_1901: Cell t987=uf_mkp((void*)&uf_sl187);L_1902: pushc(cx,t986);pushc(cx,t987);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1902,cx->sp>2?cx->sp-2:0);goto L_97;K_1902:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1903: Cell t988=pop(cx);Cell t989=uf_cnot(t988);L_1904: pushc(cx,t989);pushp(cx,(void*)&&L_1910);
L_1905: pushp(cx,(void*)&&L_1917);
L_1906: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1906,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1906,cx->sp>0?cx->sp-0:0);goto *el;}K_1906:;}
L_1907: L_1908: L_1909: Cell _rv990=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv990);return;}cx->csp--;const void*_r991=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv990);if(!_r991)return;goto *_r991;}
L_1910: Cell t992=var_trans__pends;L_1911: pushc(cx,t992);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1911,cx->sp>1?cx->sp-1:0);goto L_42;K_1911:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1912: Cell t993=uf_mkp((void*)&uf_sl188);L_1913: L_1914: var_trans__pends=t993;pushc(cx,t993);L_1915: L_1916: Cell _rv994=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv994);return;}cx->csp--;const void*_r995=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv994);if(!_r995)return;goto *_r995;}
L_1917: L_1918: L_1919: Cell _rv996=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv996);return;}cx->csp--;const void*_r997=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv996);if(!_r997)return;goto *_r997;}
L_1920: Cell t998=var_trans__pends;L_1921: Cell t999=uf_mkp((void*)&uf_sl189);L_1922: pushc(cx,t998);pushc(cx,t999);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1922,cx->sp>2?cx->sp-2:0);goto L_97;K_1922:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1923: Cell t1000=pop(cx);Cell t1001=uf_cnot(t1000);L_1924: pushc(cx,t1001);pushp(cx,(void*)&&L_1930);
L_1925: pushp(cx,(void*)&&L_1937);
L_1926: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1926,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1926,cx->sp>0?cx->sp-0:0);goto *el;}K_1926:;}
L_1927: L_1928: L_1929: Cell _rv1002=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1002);return;}cx->csp--;const void*_r1003=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1002);if(!_r1003)return;goto *_r1003;}
L_1930: Cell t1004=var_trans__pends;L_1931: pushc(cx,t1004);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1931,cx->sp>1?cx->sp-1:0);goto L_42;K_1931:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1932: Cell t1005=uf_mkp((void*)&uf_sl190);L_1933: L_1934: var_trans__pends=t1005;pushc(cx,t1005);L_1935: L_1936: Cell _rv1006=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1006);return;}cx->csp--;const void*_r1007=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1006);if(!_r1007)return;goto *_r1007;}
L_1937: L_1938: L_1939: Cell _rv1008=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1008);return;}cx->csp--;const void*_r1009=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1008);if(!_r1009)return;goto *_r1009;}
L_1940: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1940,cx->sp>0?cx->sp-0:0);goto L_127;K_1940:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1941: Cell t1010=uf_mkp((void*)&uf_sl191);L_1942: cx->locals[cx->local_base+0]=t1010;L_1943: pushc(cx,t1010);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1943,cx->sp>0?cx->sp-0:0);goto L_111;K_1943:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1944: Cell t1011=cx->locals[cx->local_base+0];L_1945: pushc(cx,t1011);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1945,cx->sp>2?cx->sp-2:0);goto L_97;K_1945:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1946: Cell t1012=pop(cx);Cell t1013=uf_cnot(t1012);L_1947: pushc(cx,t1013);pushp(cx,(void*)&&L_144);
L_1948: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1948,cx->sp>0?cx->sp-0:0);goto *b;K_1948:;}}
L_1949: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1949,cx->sp>0?cx->sp-0:0);goto L_127;K_1949:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1950: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1950,cx->sp>0?cx->sp-0:0);goto L_566;K_1950:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1951: Cell t1014=uf_mkp((void*)&uf_sl192);L_1952: cx->locals[cx->local_base+0]=t1014;L_1953: pushc(cx,t1014);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1953,cx->sp>0?cx->sp-0:0);goto L_111;K_1953:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1954: Cell t1015=cx->locals[cx->local_base+0];L_1955: pushc(cx,t1015);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1955,cx->sp>2?cx->sp-2:0);goto L_97;K_1955:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1956: Cell t1016=pop(cx);Cell t1017=uf_cnot(t1016);L_1957: pushc(cx,t1017);pushp(cx,(void*)&&L_144);
L_1958: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1958,cx->sp>0?cx->sp-0:0);goto *b;K_1958:;}}
L_1959: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1959,cx->sp>0?cx->sp-0:0);goto L_127;K_1959:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1960: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=2;uf_cspush(cx,&&K_1960,cx->sp>0?cx->sp-0:0);goto L_135;K_1960:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1961: Cell t1018=pop(cx);L_1962: Cell t1019=var_trans__inq;L_1963: var_trans__tlbl=t1018;pushc(cx,t1018);pushc(cx,t1019);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1963,cx->sp>1?cx->sp-1:0);goto L_22;K_1963:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1964: Cell t1020=var_trans__emode;L_1965: pushc(cx,t1020);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1965,cx->sp>1?cx->sp-1:0);goto L_22;K_1965:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1966: Cell t1021=var_trans__tlbl;L_1967: pushc(cx,t1021);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1967,cx->sp>1?cx->sp-1:0);goto L_22;K_1967:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1968: Cell t1022=var_trans__inq;L_1969: L_1970: Cell t1023=uf_ceq(t1022,uf_mki(1LL));L_1971: pushc(cx,t1023);pushp(cx,(void*)&&L_2005);
L_1972: pushp(cx,(void*)&&L_2010);
L_1973: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_1973,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_1973,cx->sp>0?cx->sp-0:0);goto *el;}K_1973:;}
L_1974: Cell t1024=var_trans__psnaps;L_1975: Cell t1025=var_trans__pends;L_1976: pushc(cx,t1024);pushc(cx,t1025);uf_cur_op="op_push";op_push(cx);
L_1977: Cell t1026=pop(cx);L_1978: Cell t1027=uf_mkp((void*)&uf_sl193);L_1979: L_1980: Cell t1028=var_trans__douts;L_1981: Cell t1029=uf_mkp((void*)&uf_sl194);L_1982: var_trans__psnaps=t1026;var_trans__pends=t1027;pushc(cx,t1026);pushc(cx,t1027);pushc(cx,t1028);pushc(cx,t1029);uf_cur_op="op_push";op_push(cx);
L_1983: Cell t1030=pop(cx);L_1984: L_1985: L_1986: Cell t1031=uf_mkp((void*)&uf_sl195);L_1987: Cell t1032=var_trans__tlbl;L_1988: Cell t1033=uf_mkp((void*)&uf_sl196);L_1989: var_trans__douts=t1030;var_trans__inq=uf_mki(1LL);pushc(cx,t1030);pushi(cx,1LL);pushc(cx,t1031);pushc(cx,t1032);pushc(cx,t1033);uf_cur_op="op_fmt";op_fmt(cx);
L_1990: uf_cur_op="op_cat";op_cat(cx);
L_1991: Cell t1034=uf_mkp((void*)&uf_sl197);L_1992: pushc(cx,t1034);uf_cur_op="op_cat";op_cat(cx);
L_1993: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1993,cx->sp>1?cx->sp-1:0);goto L_42;K_1993:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1994: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1994,cx->sp>0?cx->sp-0:0);goto L_1645;K_1994:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1995: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1995,cx->sp>0?cx->sp-0:0);goto L_1889;K_1995:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1996: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1996,cx->sp>0?cx->sp-0:0);goto L_111;K_1996:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1997: Cell t1035=uf_mkp((void*)&uf_sl198);L_1998: pushc(cx,t1035);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_1998,cx->sp>2?cx->sp-2:0);goto L_97;K_1998:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_1999: pushp(cx,(void*)&&L_2015);
L_2000: pushp(cx,(void*)&&L_2053);
L_2001: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2001,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2001,cx->sp>0?cx->sp-0:0);goto *el;}K_2001:;}
L_2002: L_2003: L_2004: Cell _rv1036=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1036);return;}cx->csp--;const void*_r1037=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1036);if(!_r1037)return;goto *_r1037;}
L_2005: L_2006: L_2007: var_trans__emode=uf_mki(2LL);pushi(cx,2LL);L_2008: L_2009: Cell _rv1038=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1038);return;}cx->csp--;const void*_r1039=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1038);if(!_r1039)return;goto *_r1039;}
L_2010: L_2011: L_2012: var_trans__emode=uf_mki(0LL);pushi(cx,0LL);L_2013: L_2014: Cell _rv1040=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1040);return;}cx->csp--;const void*_r1041=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1040);if(!_r1041)return;goto *_r1041;}
L_2015: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=2;uf_cspush(cx,&&K_2015,cx->sp>0?cx->sp-0:0);goto L_135;K_2015:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2016: Cell t1042=pop(cx);L_2017: L_2018: var_trans__elbl=t1042;pushc(cx,t1042);pushc(cx,t1042);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2018,cx->sp>1?cx->sp-1:0);goto L_22;K_2018:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2019: Cell t1043=uf_mkp((void*)&uf_sl199);L_2020: Cell t1044=var_trans__elbl;L_2021: Cell t1045=uf_mkp((void*)&uf_sl200);L_2022: pushc(cx,t1043);pushc(cx,t1044);pushc(cx,t1045);uf_cur_op="op_fmt";op_fmt(cx);
L_2023: uf_cur_op="op_cat";op_cat(cx);
L_2024: Cell t1046=uf_mkp((void*)&uf_sl201);L_2025: pushc(cx,t1046);uf_cur_op="op_cat";op_cat(cx);
L_2026: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2026,cx->sp>1?cx->sp-1:0);goto L_42;K_2026:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2027: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2027,cx->sp>0?cx->sp-0:0);goto L_127;K_2027:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2028: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2028,cx->sp>0?cx->sp-0:0);goto L_1645;K_2028:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2029: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2029,cx->sp>0?cx->sp-0:0);goto L_1889;K_2029:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2030: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2030,cx->sp>0?cx->sp-0:0);goto L_30;K_2030:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2031: Cell t1047=pop(cx);L_2032: var_trans__elbl=t1047;pushc(cx,t1047);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2032,cx->sp>0?cx->sp-0:0);goto L_30;K_2032:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2033: Cell t1048=pop(cx);L_2034: var_trans__tlbl=t1048;pushc(cx,t1048);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2034,cx->sp>0?cx->sp-0:0);goto L_30;K_2034:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2035: Cell t1049=pop(cx);L_2036: var_trans__emode=t1049;pushc(cx,t1049);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2036,cx->sp>0?cx->sp-0:0);goto L_1870;K_2036:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2037: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2037,cx->sp>0?cx->sp-0:0);goto L_30;K_2037:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2038: Cell t1050=pop(cx);L_2039: Cell t1051=var_trans__tlbl;L_2040: Cell t1052=var_trans__elbl;L_2041: Cell t1053=uf_mkp((void*)&uf_sl202);L_2042: var_trans__inq=t1050;pushc(cx,t1050);pushc(cx,t1051);pushc(cx,t1052);pushc(cx,t1053);uf_cur_op="op_fmt";op_fmt(cx);
L_2043: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2043,cx->sp>1?cx->sp-1:0);goto L_42;K_2043:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2044: L_2045: L_2046: Cell t1054=var_trans__cret;L_2047: var_trans__didret=uf_mki(0LL);pushi(cx,0LL);pushc(cx,t1054);pushp(cx,(void*)&&L_2597);
L_2048: pushp(cx,(void*)&&L_2594);
L_2049: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2049,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2049,cx->sp>0?cx->sp-0:0);goto *el;}K_2049:;}
L_2050: L_2051: L_2052: Cell _rv1055=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1055);return;}cx->csp--;const void*_r1056=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1055);if(!_r1056)return;goto *_r1056;}
L_2053: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2053,cx->sp>0?cx->sp-0:0);goto L_30;K_2053:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2054: Cell t1057=pop(cx);L_2055: var_trans__tlbl=t1057;pushc(cx,t1057);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2055,cx->sp>0?cx->sp-0:0);goto L_30;K_2055:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2056: Cell t1058=pop(cx);L_2057: var_trans__emode=t1058;pushc(cx,t1058);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2057,cx->sp>0?cx->sp-0:0);goto L_1870;K_2057:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2058: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2058,cx->sp>0?cx->sp-0:0);goto L_30;K_2058:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2059: Cell t1059=pop(cx);L_2060: Cell t1060=var_trans__tlbl;L_2061: Cell t1061=uf_mkp((void*)&uf_sl203);L_2062: var_trans__inq=t1059;pushc(cx,t1059);pushc(cx,t1060);pushc(cx,t1061);uf_cur_op="op_fmt";op_fmt(cx);
L_2063: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2063,cx->sp>1?cx->sp-1:0);goto L_42;K_2063:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2064: L_2065: L_2066: Cell t1062=var_trans__cret;L_2067: var_trans__didret=uf_mki(0LL);pushi(cx,0LL);pushc(cx,t1062);pushp(cx,(void*)&&L_2597);
L_2068: pushp(cx,(void*)&&L_2594);
L_2069: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2069,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2069,cx->sp>0?cx->sp-0:0);goto *el;}K_2069:;}
L_2070: L_2071: L_2072: Cell _rv1063=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1063);return;}cx->csp--;const void*_r1064=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1063);if(!_r1064)return;goto *_r1064;}
L_2073: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2073,cx->sp>0?cx->sp-0:0);goto L_127;K_2073:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2074: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=2;uf_cspush(cx,&&K_2074,cx->sp>0?cx->sp-0:0);goto L_135;K_2074:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2075: Cell t1065=pop(cx);L_2076: var_trans__clbl=t1065;pushc(cx,t1065);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=2;uf_cspush(cx,&&K_2076,cx->sp>0?cx->sp-0:0);goto L_135;K_2076:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2077: Cell t1066=pop(cx);L_2078: Cell t1067=var_trans__inq;L_2079: var_trans__blbl=t1066;pushc(cx,t1066);pushc(cx,t1067);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2079,cx->sp>1?cx->sp-1:0);goto L_22;K_2079:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2080: Cell t1068=var_trans__emode;L_2081: pushc(cx,t1068);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2081,cx->sp>1?cx->sp-1:0);goto L_22;K_2081:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2082: Cell t1069=var_trans__clbl;L_2083: pushc(cx,t1069);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2083,cx->sp>1?cx->sp-1:0);goto L_22;K_2083:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2084: Cell t1070=var_trans__blbl;L_2085: pushc(cx,t1070);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2085,cx->sp>1?cx->sp-1:0);goto L_22;K_2085:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2086: Cell t1071=var_trans__inq;L_2087: L_2088: Cell t1072=uf_ceq(t1071,uf_mki(1LL));L_2089: pushc(cx,t1072);pushp(cx,(void*)&&L_2005);
L_2090: pushp(cx,(void*)&&L_2010);
L_2091: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2091,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2091,cx->sp>0?cx->sp-0:0);goto *el;}K_2091:;}
L_2092: Cell t1073=var_trans__psnaps;L_2093: Cell t1074=var_trans__pends;L_2094: pushc(cx,t1073);pushc(cx,t1074);uf_cur_op="op_push";op_push(cx);
L_2095: Cell t1075=pop(cx);L_2096: Cell t1076=uf_mkp((void*)&uf_sl204);L_2097: L_2098: Cell t1077=var_trans__douts;L_2099: Cell t1078=uf_mkp((void*)&uf_sl205);L_2100: var_trans__psnaps=t1075;var_trans__pends=t1076;pushc(cx,t1075);pushc(cx,t1076);pushc(cx,t1077);pushc(cx,t1078);uf_cur_op="op_push";op_push(cx);
L_2101: Cell t1079=pop(cx);L_2102: L_2103: L_2104: Cell t1080=uf_mkp((void*)&uf_sl206);L_2105: Cell t1081=var_trans__clbl;L_2106: Cell t1082=uf_mkp((void*)&uf_sl207);L_2107: var_trans__douts=t1079;var_trans__inq=uf_mki(1LL);pushc(cx,t1079);pushi(cx,1LL);pushc(cx,t1080);pushc(cx,t1081);pushc(cx,t1082);uf_cur_op="op_fmt";op_fmt(cx);
L_2108: uf_cur_op="op_cat";op_cat(cx);
L_2109: Cell t1083=uf_mkp((void*)&uf_sl208);L_2110: pushc(cx,t1083);uf_cur_op="op_cat";op_cat(cx);
L_2111: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2111,cx->sp>1?cx->sp-1:0);goto L_42;K_2111:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2112: Cell t1084=uf_mkp((void*)&uf_sl209);L_2113: cx->locals[cx->local_base+0]=t1084;L_2114: pushc(cx,t1084);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2114,cx->sp>0?cx->sp-0:0);goto L_111;K_2114:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2115: Cell t1085=cx->locals[cx->local_base+0];L_2116: pushc(cx,t1085);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2116,cx->sp>2?cx->sp-2:0);goto L_97;K_2116:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2117: Cell t1086=pop(cx);Cell t1087=uf_cnot(t1086);L_2118: pushc(cx,t1087);pushp(cx,(void*)&&L_144);
L_2119: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2119,cx->sp>0?cx->sp-0:0);goto *b;K_2119:;}}
L_2120: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2120,cx->sp>0?cx->sp-0:0);goto L_127;K_2120:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2121: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2121,cx->sp>0?cx->sp-0:0);goto L_566;K_2121:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2122: Cell t1088=uf_mkp((void*)&uf_sl210);L_2123: cx->locals[cx->local_base+0]=t1088;L_2124: pushc(cx,t1088);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2124,cx->sp>0?cx->sp-0:0);goto L_111;K_2124:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2125: Cell t1089=cx->locals[cx->local_base+0];L_2126: pushc(cx,t1089);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2126,cx->sp>2?cx->sp-2:0);goto L_97;K_2126:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2127: Cell t1090=pop(cx);Cell t1091=uf_cnot(t1090);L_2128: pushc(cx,t1091);pushp(cx,(void*)&&L_144);
L_2129: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2129,cx->sp>0?cx->sp-0:0);goto *b;K_2129:;}}
L_2130: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2130,cx->sp>0?cx->sp-0:0);goto L_127;K_2130:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2131: Cell t1092=uf_mkp((void*)&uf_sl211);L_2132: pushc(cx,t1092);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2132,cx->sp>1?cx->sp-1:0);goto L_42;K_2132:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2133: Cell t1093=uf_mkp((void*)&uf_sl212);L_2134: Cell t1094=var_trans__blbl;L_2135: Cell t1095=uf_mkp((void*)&uf_sl213);L_2136: pushc(cx,t1093);pushc(cx,t1094);pushc(cx,t1095);uf_cur_op="op_fmt";op_fmt(cx);
L_2137: uf_cur_op="op_cat";op_cat(cx);
L_2138: Cell t1096=uf_mkp((void*)&uf_sl214);L_2139: pushc(cx,t1096);uf_cur_op="op_cat";op_cat(cx);
L_2140: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2140,cx->sp>1?cx->sp-1:0);goto L_42;K_2140:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2141: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2141,cx->sp>0?cx->sp-0:0);goto L_1645;K_2141:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2142: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2142,cx->sp>0?cx->sp-0:0);goto L_1889;K_2142:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2143: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2143,cx->sp>0?cx->sp-0:0);goto L_30;K_2143:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2144: Cell t1097=pop(cx);L_2145: var_trans__blbl=t1097;pushc(cx,t1097);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2145,cx->sp>0?cx->sp-0:0);goto L_30;K_2145:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2146: Cell t1098=pop(cx);L_2147: var_trans__clbl=t1098;pushc(cx,t1098);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2147,cx->sp>0?cx->sp-0:0);goto L_30;K_2147:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2148: Cell t1099=pop(cx);L_2149: var_trans__emode=t1099;pushc(cx,t1099);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2149,cx->sp>0?cx->sp-0:0);goto L_1870;K_2149:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2150: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2150,cx->sp>0?cx->sp-0:0);goto L_30;K_2150:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2151: Cell t1100=pop(cx);L_2152: Cell t1101=var_trans__clbl;L_2153: Cell t1102=var_trans__blbl;L_2154: Cell t1103=uf_mkp((void*)&uf_sl215);L_2155: var_trans__inq=t1100;pushc(cx,t1100);pushc(cx,t1101);pushc(cx,t1102);pushc(cx,t1103);uf_cur_op="op_fmt";op_fmt(cx);
L_2156: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2156,cx->sp>1?cx->sp-1:0);goto L_42;K_2156:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2157: L_2158: L_2159: Cell t1104=var_trans__cret;L_2160: var_trans__didret=uf_mki(0LL);pushi(cx,0LL);pushc(cx,t1104);pushp(cx,(void*)&&L_2597);
L_2161: pushp(cx,(void*)&&L_2594);
L_2162: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2162,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2162,cx->sp>0?cx->sp-0:0);goto *el;}K_2162:;}
L_2163: L_2164: L_2165: Cell _rv1105=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1105);return;}cx->csp--;const void*_r1106=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1105);if(!_r1106)return;goto *_r1106;}
L_2166: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2166,cx->sp>0?cx->sp-0:0);goto L_127;K_2166:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2167: Cell t1107=uf_mkp((void*)&uf_sl216);L_2168: cx->locals[cx->local_base+0]=t1107;L_2169: pushc(cx,t1107);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2169,cx->sp>0?cx->sp-0:0);goto L_111;K_2169:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2170: Cell t1108=cx->locals[cx->local_base+0];L_2171: pushc(cx,t1108);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2171,cx->sp>2?cx->sp-2:0);goto L_97;K_2171:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2172: Cell t1109=pop(cx);Cell t1110=uf_cnot(t1109);L_2173: pushc(cx,t1110);pushp(cx,(void*)&&L_144);
L_2174: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2174,cx->sp>0?cx->sp-0:0);goto *b;K_2174:;}}
L_2175: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2175,cx->sp>0?cx->sp-0:0);goto L_127;K_2175:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2176: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=2;uf_cspush(cx,&&K_2176,cx->sp>0?cx->sp-0:0);goto L_135;K_2176:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2177: Cell t1111=pop(cx);L_2178: var_trans__fclbl=t1111;pushc(cx,t1111);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=2;uf_cspush(cx,&&K_2178,cx->sp>0?cx->sp-0:0);goto L_135;K_2178:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2179: Cell t1112=pop(cx);L_2180: var_trans__fblbl=t1112;pushc(cx,t1112);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2180,cx->sp>0?cx->sp-0:0);goto L_111;K_2180:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2181: Cell t1113=uf_mkp((void*)&uf_sl217);L_2182: pushc(cx,t1113);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2182,cx->sp>2?cx->sp-2:0);goto L_97;K_2182:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2183: pushp(cx,(void*)&&L_2289);
L_2184: pushp(cx,(void*)&&L_2295);
L_2185: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2185,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2185,cx->sp>0?cx->sp-0:0);goto *el;}K_2185:;}
L_2186: Cell t1114=var_trans__inq;L_2187: pushc(cx,t1114);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2187,cx->sp>1?cx->sp-1:0);goto L_22;K_2187:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2188: Cell t1115=var_trans__emode;L_2189: pushc(cx,t1115);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2189,cx->sp>1?cx->sp-1:0);goto L_22;K_2189:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2190: Cell t1116=var_trans__fclbl;L_2191: pushc(cx,t1116);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2191,cx->sp>1?cx->sp-1:0);goto L_22;K_2191:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2192: Cell t1117=var_trans__fblbl;L_2193: pushc(cx,t1117);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2193,cx->sp>1?cx->sp-1:0);goto L_22;K_2193:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2194: Cell t1118=var_trans__inq;L_2195: L_2196: Cell t1119=uf_ceq(t1118,uf_mki(1LL));L_2197: pushc(cx,t1119);pushp(cx,(void*)&&L_2005);
L_2198: pushp(cx,(void*)&&L_2010);
L_2199: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2199,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2199,cx->sp>0?cx->sp-0:0);goto *el;}K_2199:;}
L_2200: Cell t1120=var_trans__psnaps;L_2201: Cell t1121=var_trans__pends;L_2202: pushc(cx,t1120);pushc(cx,t1121);uf_cur_op="op_push";op_push(cx);
L_2203: Cell t1122=pop(cx);L_2204: Cell t1123=uf_mkp((void*)&uf_sl218);L_2205: L_2206: Cell t1124=var_trans__douts;L_2207: Cell t1125=uf_mkp((void*)&uf_sl219);L_2208: var_trans__psnaps=t1122;var_trans__pends=t1123;pushc(cx,t1122);pushc(cx,t1123);pushc(cx,t1124);pushc(cx,t1125);uf_cur_op="op_push";op_push(cx);
L_2209: Cell t1126=pop(cx);L_2210: L_2211: L_2212: Cell t1127=uf_mkp((void*)&uf_sl220);L_2213: Cell t1128=var_trans__fclbl;L_2214: Cell t1129=uf_mkp((void*)&uf_sl221);L_2215: var_trans__douts=t1126;var_trans__inq=uf_mki(1LL);pushc(cx,t1126);pushi(cx,1LL);pushc(cx,t1127);pushc(cx,t1128);pushc(cx,t1129);uf_cur_op="op_fmt";op_fmt(cx);
L_2216: uf_cur_op="op_cat";op_cat(cx);
L_2217: Cell t1130=uf_mkp((void*)&uf_sl222);L_2218: pushc(cx,t1130);uf_cur_op="op_cat";op_cat(cx);
L_2219: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2219,cx->sp>1?cx->sp-1:0);goto L_42;K_2219:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2220: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2220,cx->sp>0?cx->sp-0:0);goto L_111;K_2220:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2221: Cell t1131=uf_mkp((void*)&uf_sl223);L_2222: pushc(cx,t1131);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2222,cx->sp>2?cx->sp-2:0);goto L_97;K_2222:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2223: pushp(cx,(void*)&&L_2357);
L_2224: pushp(cx,(void*)&&L_2367);
L_2225: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2225,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2225,cx->sp>0?cx->sp-0:0);goto *el;}K_2225:;}
L_2226: Cell t1132=uf_mkp((void*)&uf_sl224);L_2227: pushc(cx,t1132);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2227,cx->sp>1?cx->sp-1:0);goto L_42;K_2227:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2228: Cell t1133=uf_mkp((void*)&uf_sl225);L_2229: Cell t1134=var_trans__fblbl;L_2230: Cell t1135=uf_mkp((void*)&uf_sl226);L_2231: pushc(cx,t1133);pushc(cx,t1134);pushc(cx,t1135);uf_cur_op="op_fmt";op_fmt(cx);
L_2232: uf_cur_op="op_cat";op_cat(cx);
L_2233: Cell t1136=uf_mkp((void*)&uf_sl227);L_2234: pushc(cx,t1136);uf_cur_op="op_cat";op_cat(cx);
L_2235: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2235,cx->sp>1?cx->sp-1:0);goto L_42;K_2235:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2236: Cell t1137=var_trans__pi;L_2237: L_2238: L_2239: L_2240: L_2241: L_2242: var_trans__pfpi=t1137;var_trans__pfd=uf_mki(1LL);pushc(cx,t1137);pushi(cx,1LL);{long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_2242;cx->loops[fr].end=&&K_WE_2242;long _sp0=cx->sp;
K_WC_2242:;{Cell _wc;{
WC2242_L2382: Cell t0=var_trans__pfd;WC2242_L2383: WC2242_L2384: var_trans__rr=t0;pushc(cx,t0);WC2242_L2385: Cell t1=var_trans__rr;pushc(cx,t1);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_2242;
{
WB2242_L2387: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB2242_2387,cx->sp>0?cx->sp-0:0);goto L_111;K_WB2242_2387:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB2242_L2388: Cell t0=uf_mkp((void*)&uf_sl238);WB2242_L2389: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB2242_2389,cx->sp>2?cx->sp-2:0);goto L_97;K_WB2242_2389:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB2242_L2390: pushp(cx,(void*)&&L_2396);
WB2242_L2391: pushp(cx,(void*)&&L_2406);
WB2242_L2392: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_WB2242_2392,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_WB2242_2392,cx->sp>0?cx->sp-0:0);goto *el;}K_WB2242_2392:;}
WB2242_L2393: }cx->sp=_sp0;goto K_WC_2242;}
K_WE_2242:;cx->lsp=fr;}
L_2243: Cell t1138=uf_mkp((void*)&uf_sl228);L_2244: cx->locals[cx->local_base+0]=t1138;L_2245: pushc(cx,t1138);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2245,cx->sp>0?cx->sp-0:0);goto L_111;K_2245:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2246: Cell t1139=cx->locals[cx->local_base+0];L_2247: pushc(cx,t1139);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2247,cx->sp>2?cx->sp-2:0);goto L_97;K_2247:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2248: Cell t1140=pop(cx);Cell t1141=uf_cnot(t1140);L_2249: pushc(cx,t1141);pushp(cx,(void*)&&L_144);
L_2250: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2250,cx->sp>0?cx->sp-0:0);goto *b;K_2250:;}}
L_2251: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2251,cx->sp>0?cx->sp-0:0);goto L_127;K_2251:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2252: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2252,cx->sp>0?cx->sp-0:0);goto L_1645;K_2252:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2253: Cell t1142=var_trans__pi;L_2254: L_2255: Cell t1143=var_trans__pfpi;L_2256: L_2257: var_trans__pi=t1143;var_trans__pfpi2=t1142;pushc(cx,t1142);pushc(cx,t1143);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2257,cx->sp>0?cx->sp-0:0);goto L_111;K_2257:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2258: Cell t1144=uf_mkp((void*)&uf_sl229);L_2259: pushc(cx,t1144);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2259,cx->sp>2?cx->sp-2:0);goto L_97;K_2259:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2260: pushp(cx,(void*)&&L_2443);
L_2261: pushp(cx,(void*)&&L_2448);
L_2262: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2262,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2262,cx->sp>0?cx->sp-0:0);goto *el;}K_2262:;}
L_2263: Cell t1145=var_trans__pfpi2;L_2264: L_2265: var_trans__pi=t1145;pushc(cx,t1145);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2265,cx->sp>0?cx->sp-0:0);goto L_1889;K_2265:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2266: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2266,cx->sp>0?cx->sp-0:0);goto L_30;K_2266:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2267: Cell t1146=pop(cx);L_2268: var_trans__fblbl=t1146;pushc(cx,t1146);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2268,cx->sp>0?cx->sp-0:0);goto L_30;K_2268:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2269: Cell t1147=pop(cx);L_2270: var_trans__fclbl=t1147;pushc(cx,t1147);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2270,cx->sp>0?cx->sp-0:0);goto L_30;K_2270:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2271: Cell t1148=pop(cx);L_2272: var_trans__emode=t1148;pushc(cx,t1148);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2272,cx->sp>0?cx->sp-0:0);goto L_1870;K_2272:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2273: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2273,cx->sp>0?cx->sp-0:0);goto L_30;K_2273:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2274: Cell t1149=pop(cx);L_2275: Cell t1150=var_trans__fclbl;L_2276: Cell t1151=var_trans__fblbl;L_2277: Cell t1152=uf_mkp((void*)&uf_sl230);L_2278: var_trans__inq=t1149;pushc(cx,t1149);pushc(cx,t1150);pushc(cx,t1151);pushc(cx,t1152);uf_cur_op="op_fmt";op_fmt(cx);
L_2279: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2279,cx->sp>1?cx->sp-1:0);goto L_42;K_2279:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2280: L_2281: L_2282: Cell t1153=var_trans__cret;L_2283: var_trans__didret=uf_mki(0LL);pushi(cx,0LL);pushc(cx,t1153);pushp(cx,(void*)&&L_2597);
L_2284: pushp(cx,(void*)&&L_2594);
L_2285: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2285,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2285,cx->sp>0?cx->sp-0:0);goto *el;}K_2285:;}
L_2286: L_2287: L_2288: Cell _rv1154=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1154);return;}cx->csp--;const void*_r1155=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1154);if(!_r1155)return;goto *_r1155;}
L_2289: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2289,cx->sp>0?cx->sp-0:0);goto L_127;K_2289:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2290: L_2291: L_2292: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2293: Cell t1156=var_trans__rr;L_2294: Cell _rv1157=t1156;{if(cx->csp==0){pushc(cx,_rv1157);return;}cx->csp--;const void*_r1158=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1157);if(!_r1158)return;goto *_r1158;}
L_2295: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2295,cx->sp>0?cx->sp-0:0);goto L_111;K_2295:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2296: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2296,cx->sp>1?cx->sp-1:0);goto L_155;K_2296:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2297: pushp(cx,(void*)&&L_2303);
L_2298: pushp(cx,(void*)&&L_2342);
L_2299: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2299,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2299,cx->sp>0?cx->sp-0:0);goto *el;}K_2299:;}
L_2300: L_2301: L_2302: Cell _rv1159=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1159);return;}cx->csp--;const void*_r1160=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1159);if(!_r1160)return;goto *_r1160;}
L_2303: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2303,cx->sp>0?cx->sp-0:0);goto L_195;K_2303:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2304: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2304,cx->sp>0?cx->sp-0:0);goto L_111;K_2304:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2305: Cell t1161=pop(cx);L_2306: var_trans__nv=t1161;pushc(cx,t1161);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2306,cx->sp>0?cx->sp-0:0);goto L_127;K_2306:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2307: Cell t1162=var_trans__nv;L_2308: pushc(cx,t1162);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2308,cx->sp>1?cx->sp-1:0);goto L_215;K_2308:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2309: Cell t1163=pop(cx);L_2310: Cell t1164=uf_mkp((void*)&uf_sl231);L_2311: cx->locals[cx->local_base+0]=t1164;L_2312: var_trans__slot=t1163;pushc(cx,t1163);pushc(cx,t1164);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2312,cx->sp>0?cx->sp-0:0);goto L_111;K_2312:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2313: Cell t1165=cx->locals[cx->local_base+0];L_2314: pushc(cx,t1165);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2314,cx->sp>2?cx->sp-2:0);goto L_97;K_2314:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2315: Cell t1166=pop(cx);Cell t1167=uf_cnot(t1166);L_2316: pushc(cx,t1167);pushp(cx,(void*)&&L_144);
L_2317: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2317,cx->sp>0?cx->sp-0:0);goto *b;K_2317:;}}
L_2318: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2318,cx->sp>0?cx->sp-0:0);goto L_127;K_2318:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2319: Cell t1168=var_trans__slot;L_2320: pushc(cx,t1168);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2320,cx->sp>1?cx->sp-1:0);goto L_0;K_2320:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2321: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2321,cx->sp>0?cx->sp-0:0);goto L_571;K_2321:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2322: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2322,cx->sp>0?cx->sp-0:0);goto L_36;K_2322:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2323: Cell t1169=pop(cx);L_2324: L_2325: Cell t1170=uf_mkp((void*)&uf_sl232);L_2326: var_trans__slot2=t1169;pushc(cx,t1169);pushc(cx,t1169);pushc(cx,t1170);uf_cur_op="op_fmt";op_fmt(cx);
L_2327: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2327,cx->sp>1?cx->sp-1:0);goto L_42;K_2327:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2328: Cell t1171=uf_mkp((void*)&uf_sl233);L_2329: cx->locals[cx->local_base+0]=t1171;L_2330: pushc(cx,t1171);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2330,cx->sp>0?cx->sp-0:0);goto L_111;K_2330:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2331: Cell t1172=cx->locals[cx->local_base+0];L_2332: pushc(cx,t1172);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2332,cx->sp>2?cx->sp-2:0);goto L_97;K_2332:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2333: Cell t1173=pop(cx);Cell t1174=uf_cnot(t1173);L_2334: pushc(cx,t1174);pushp(cx,(void*)&&L_144);
L_2335: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2335,cx->sp>0?cx->sp-0:0);goto *b;K_2335:;}}
L_2336: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2336,cx->sp>0?cx->sp-0:0);goto L_127;K_2336:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2337: L_2338: L_2339: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2340: Cell t1175=var_trans__rr;L_2341: Cell _rv1176=t1175;{if(cx->csp==0){pushc(cx,_rv1176);return;}cx->csp--;const void*_r1177=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1176);if(!_r1177)return;goto *_r1177;}
L_2342: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2342,cx->sp>0?cx->sp-0:0);goto L_566;K_2342:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2343: Cell t1178=uf_mkp((void*)&uf_sl234);L_2344: cx->locals[cx->local_base+0]=t1178;L_2345: pushc(cx,t1178);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2345,cx->sp>0?cx->sp-0:0);goto L_111;K_2345:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2346: Cell t1179=cx->locals[cx->local_base+0];L_2347: pushc(cx,t1179);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2347,cx->sp>2?cx->sp-2:0);goto L_97;K_2347:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2348: Cell t1180=pop(cx);Cell t1181=uf_cnot(t1180);L_2349: pushc(cx,t1181);pushp(cx,(void*)&&L_144);
L_2350: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2350,cx->sp>0?cx->sp-0:0);goto *b;K_2350:;}}
L_2351: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2351,cx->sp>0?cx->sp-0:0);goto L_127;K_2351:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2352: L_2353: L_2354: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2355: Cell t1182=var_trans__rr;L_2356: Cell _rv1183=t1182;{if(cx->csp==0){pushc(cx,_rv1183);return;}cx->csp--;const void*_r1184=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1183);if(!_r1184)return;goto *_r1184;}
L_2357: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2357,cx->sp>0?cx->sp-0:0);goto L_127;K_2357:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2358: Cell t1185=uf_mkp((void*)&uf_sl235);L_2359: pushc(cx,t1185);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2359,cx->sp>1?cx->sp-1:0);goto L_42;K_2359:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2360: Cell t1186=uf_mkp((void*)&uf_sl236);L_2361: pushc(cx,t1186);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2361,cx->sp>1?cx->sp-1:0);goto L_42;K_2361:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2362: L_2363: L_2364: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2365: Cell t1187=var_trans__rr;L_2366: Cell _rv1188=t1187;{if(cx->csp==0){pushc(cx,_rv1188);return;}cx->csp--;const void*_r1189=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1188);if(!_r1189)return;goto *_r1189;}
L_2367: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2367,cx->sp>0?cx->sp-0:0);goto L_566;K_2367:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2368: Cell t1190=uf_mkp((void*)&uf_sl237);L_2369: cx->locals[cx->local_base+0]=t1190;L_2370: pushc(cx,t1190);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2370,cx->sp>0?cx->sp-0:0);goto L_111;K_2370:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2371: Cell t1191=cx->locals[cx->local_base+0];L_2372: pushc(cx,t1191);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2372,cx->sp>2?cx->sp-2:0);goto L_97;K_2372:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2373: Cell t1192=pop(cx);Cell t1193=uf_cnot(t1192);L_2374: pushc(cx,t1193);pushp(cx,(void*)&&L_144);
L_2375: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2375,cx->sp>0?cx->sp-0:0);goto *b;K_2375:;}}
L_2376: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2376,cx->sp>0?cx->sp-0:0);goto L_127;K_2376:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2377: L_2378: L_2379: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2380: Cell t1194=var_trans__rr;L_2381: Cell _rv1195=t1194;{if(cx->csp==0){pushc(cx,_rv1195);return;}cx->csp--;const void*_r1196=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1195);if(!_r1196)return;goto *_r1196;}
L_2382: Cell t1197=var_trans__pfd;L_2383: L_2384: var_trans__rr=t1197;pushc(cx,t1197);L_2385: Cell t1198=var_trans__rr;L_2386: Cell _rv1199=t1198;{if(cx->csp==0){pushc(cx,_rv1199);return;}cx->csp--;const void*_r1200=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1199);if(!_r1200)return;goto *_r1200;}
L_2387: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2387,cx->sp>0?cx->sp-0:0);goto L_111;K_2387:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2388: Cell t1201=uf_mkp((void*)&uf_sl238);L_2389: pushc(cx,t1201);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2389,cx->sp>2?cx->sp-2:0);goto L_97;K_2389:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2390: pushp(cx,(void*)&&L_2396);
L_2391: pushp(cx,(void*)&&L_2406);
L_2392: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2392,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2392,cx->sp>0?cx->sp-0:0);goto *el;}K_2392:;}
L_2393: L_2394: L_2395: Cell _rv1202=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1202);return;}cx->csp--;const void*_r1203=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1202);if(!_r1203)return;goto *_r1203;}
L_2396: Cell t1204=var_trans__pfd;L_2397: L_2398: Cell t1205=uf_cadd(t1204,uf_mki(1LL));L_2399: L_2400: var_trans__pfd=t1205;pushc(cx,t1205);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2400,cx->sp>0?cx->sp-0:0);goto L_127;K_2400:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2401: L_2402: L_2403: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2404: Cell t1206=var_trans__rr;L_2405: Cell _rv1207=t1206;{if(cx->csp==0){pushc(cx,_rv1207);return;}cx->csp--;const void*_r1208=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1207);if(!_r1208)return;goto *_r1208;}
L_2406: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2406,cx->sp>0?cx->sp-0:0);goto L_111;K_2406:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2407: Cell t1209=uf_mkp((void*)&uf_sl239);L_2408: pushc(cx,t1209);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2408,cx->sp>2?cx->sp-2:0);goto L_97;K_2408:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2409: pushp(cx,(void*)&&L_2415);
L_2410: pushp(cx,(void*)&&L_2437);
L_2411: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2411,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2411,cx->sp>0?cx->sp-0:0);goto *el;}K_2411:;}
L_2412: L_2413: L_2414: Cell _rv1210=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1210);return;}cx->csp--;const void*_r1211=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1210);if(!_r1211)return;goto *_r1211;}
L_2415: Cell t1212=var_trans__pfd;L_2416: L_2417: Cell t1213=uf_csub(t1212,uf_mki(1LL));L_2418: L_2419: L_2420: var_trans__pfd=t1213;pushc(cx,t1213);pushc(cx,t1213);pushp(cx,(void*)&&L_2431);
L_2421: pushp(cx,(void*)&&L_2426);
L_2422: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2422,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2422,cx->sp>0?cx->sp-0:0);goto *el;}K_2422:;}
L_2423: L_2424: L_2425: Cell _rv1214=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1214);return;}cx->csp--;const void*_r1215=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1214);if(!_r1215)return;goto *_r1215;}
L_2426: L_2427: L_2428: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2429: Cell t1216=var_trans__rr;L_2430: Cell _rv1217=t1216;{if(cx->csp==0){pushc(cx,_rv1217);return;}cx->csp--;const void*_r1218=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1217);if(!_r1218)return;goto *_r1218;}
L_2431: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2431,cx->sp>0?cx->sp-0:0);goto L_127;K_2431:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2432: L_2433: L_2434: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2435: Cell t1219=var_trans__rr;L_2436: Cell _rv1220=t1219;{if(cx->csp==0){pushc(cx,_rv1220);return;}cx->csp--;const void*_r1221=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1220);if(!_r1221)return;goto *_r1221;}
L_2437: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2437,cx->sp>0?cx->sp-0:0);goto L_127;K_2437:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2438: L_2439: L_2440: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2441: Cell t1222=var_trans__rr;L_2442: Cell _rv1223=t1222;{if(cx->csp==0){pushc(cx,_rv1223);return;}cx->csp--;const void*_r1224=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1223);if(!_r1224)return;goto *_r1224;}
L_2443: L_2444: L_2445: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2446: Cell t1225=var_trans__rr;L_2447: Cell _rv1226=t1225;{if(cx->csp==0){pushc(cx,_rv1226);return;}cx->csp--;const void*_r1227=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1226);if(!_r1227)return;goto *_r1227;}
L_2448: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2448,cx->sp>0?cx->sp-0:0);goto L_566;K_2448:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2449: L_2450: L_2451: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2452: Cell t1228=var_trans__rr;L_2453: Cell _rv1229=t1228;{if(cx->csp==0){pushc(cx,_rv1229);return;}cx->csp--;const void*_r1230=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1229);if(!_r1230)return;goto *_r1230;}
L_2454: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2454,cx->sp>0?cx->sp-0:0);goto L_127;K_2454:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2455: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=2;uf_cspush(cx,&&K_2455,cx->sp>0?cx->sp-0:0);goto L_135;K_2455:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2456: Cell t1231=pop(cx);L_2457: var_trans__dflbl=t1231;pushc(cx,t1231);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=2;uf_cspush(cx,&&K_2457,cx->sp>0?cx->sp-0:0);goto L_135;K_2457:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2458: Cell t1232=pop(cx);L_2459: var_trans__clbl=t1232;pushc(cx,t1232);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=2;uf_cspush(cx,&&K_2459,cx->sp>0?cx->sp-0:0);goto L_135;K_2459:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2460: Cell t1233=pop(cx);L_2461: Cell t1234=uf_mkp((void*)&uf_sl240);L_2462: Cell t1235=var_trans__dflbl;L_2463: Cell t1236=uf_mkp((void*)&uf_sl241);L_2464: var_trans__blbl=t1233;pushc(cx,t1233);pushc(cx,t1234);pushc(cx,t1235);pushc(cx,t1236);uf_cur_op="op_fmt";op_fmt(cx);
L_2465: uf_cur_op="op_cat";op_cat(cx);
L_2466: Cell t1237=uf_mkp((void*)&uf_sl242);L_2467: pushc(cx,t1237);uf_cur_op="op_cat";op_cat(cx);
L_2468: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2468,cx->sp>1?cx->sp-1:0);goto L_42;K_2468:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2469: Cell t1238=var_trans__inq;L_2470: pushc(cx,t1238);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2470,cx->sp>1?cx->sp-1:0);goto L_22;K_2470:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2471: Cell t1239=var_trans__emode;L_2472: pushc(cx,t1239);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2472,cx->sp>1?cx->sp-1:0);goto L_22;K_2472:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2473: Cell t1240=var_trans__dflbl;L_2474: pushc(cx,t1240);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2474,cx->sp>1?cx->sp-1:0);goto L_22;K_2474:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2475: Cell t1241=var_trans__clbl;L_2476: pushc(cx,t1241);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2476,cx->sp>1?cx->sp-1:0);goto L_22;K_2476:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2477: Cell t1242=var_trans__blbl;L_2478: pushc(cx,t1242);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2478,cx->sp>1?cx->sp-1:0);goto L_22;K_2478:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2479: Cell t1243=var_trans__inq;L_2480: L_2481: Cell t1244=uf_ceq(t1243,uf_mki(1LL));L_2482: pushc(cx,t1244);pushp(cx,(void*)&&L_2005);
L_2483: pushp(cx,(void*)&&L_2010);
L_2484: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2484,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2484,cx->sp>0?cx->sp-0:0);goto *el;}K_2484:;}
L_2485: Cell t1245=var_trans__psnaps;L_2486: Cell t1246=var_trans__pends;L_2487: pushc(cx,t1245);pushc(cx,t1246);uf_cur_op="op_push";op_push(cx);
L_2488: Cell t1247=pop(cx);L_2489: Cell t1248=uf_mkp((void*)&uf_sl243);L_2490: L_2491: Cell t1249=var_trans__douts;L_2492: Cell t1250=uf_mkp((void*)&uf_sl244);L_2493: var_trans__psnaps=t1247;var_trans__pends=t1248;pushc(cx,t1247);pushc(cx,t1248);pushc(cx,t1249);pushc(cx,t1250);uf_cur_op="op_push";op_push(cx);
L_2494: Cell t1251=pop(cx);L_2495: L_2496: L_2497: Cell t1252=uf_mkp((void*)&uf_sl245);L_2498: Cell t1253=var_trans__blbl;L_2499: Cell t1254=uf_mkp((void*)&uf_sl246);L_2500: var_trans__douts=t1251;var_trans__inq=uf_mki(1LL);pushc(cx,t1251);pushi(cx,1LL);pushc(cx,t1252);pushc(cx,t1253);pushc(cx,t1254);uf_cur_op="op_fmt";op_fmt(cx);
L_2501: uf_cur_op="op_cat";op_cat(cx);
L_2502: Cell t1255=uf_mkp((void*)&uf_sl247);L_2503: pushc(cx,t1255);uf_cur_op="op_cat";op_cat(cx);
L_2504: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2504,cx->sp>1?cx->sp-1:0);goto L_42;K_2504:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2505: Cell t1256=uf_mkp((void*)&uf_sl248);L_2506: Cell t1257=var_trans__dflbl;L_2507: Cell t1258=uf_mkp((void*)&uf_sl249);L_2508: pushc(cx,t1256);pushc(cx,t1257);pushc(cx,t1258);uf_cur_op="op_fmt";op_fmt(cx);
L_2509: uf_cur_op="op_cat";op_cat(cx);
L_2510: Cell t1259=uf_mkp((void*)&uf_sl250);L_2511: pushc(cx,t1259);uf_cur_op="op_cat";op_cat(cx);
L_2512: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2512,cx->sp>1?cx->sp-1:0);goto L_42;K_2512:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2513: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2513,cx->sp>0?cx->sp-0:0);goto L_1645;K_2513:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2514: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2514,cx->sp>0?cx->sp-0:0);goto L_1889;K_2514:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2515: Cell t1260=uf_mkp((void*)&uf_sl251);L_2516: cx->locals[cx->local_base+0]=t1260;L_2517: pushc(cx,t1260);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2517,cx->sp>0?cx->sp-0:0);goto L_111;K_2517:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2518: Cell t1261=cx->locals[cx->local_base+0];L_2519: pushc(cx,t1261);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2519,cx->sp>2?cx->sp-2:0);goto L_97;K_2519:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2520: Cell t1262=pop(cx);Cell t1263=uf_cnot(t1262);L_2521: pushc(cx,t1263);pushp(cx,(void*)&&L_144);
L_2522: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2522,cx->sp>0?cx->sp-0:0);goto *b;K_2522:;}}
L_2523: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2523,cx->sp>0?cx->sp-0:0);goto L_127;K_2523:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2524: Cell t1264=uf_mkp((void*)&uf_sl252);L_2525: cx->locals[cx->local_base+0]=t1264;L_2526: pushc(cx,t1264);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2526,cx->sp>0?cx->sp-0:0);goto L_111;K_2526:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2527: Cell t1265=cx->locals[cx->local_base+0];L_2528: pushc(cx,t1265);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2528,cx->sp>2?cx->sp-2:0);goto L_97;K_2528:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2529: Cell t1266=pop(cx);Cell t1267=uf_cnot(t1266);L_2530: pushc(cx,t1267);pushp(cx,(void*)&&L_144);
L_2531: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2531,cx->sp>0?cx->sp-0:0);goto *b;K_2531:;}}
L_2532: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2532,cx->sp>0?cx->sp-0:0);goto L_127;K_2532:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2533: Cell t1268=uf_mkp((void*)&uf_sl253);L_2534: Cell t1269=var_trans__clbl;L_2535: Cell t1270=uf_mkp((void*)&uf_sl254);L_2536: pushc(cx,t1268);pushc(cx,t1269);pushc(cx,t1270);uf_cur_op="op_fmt";op_fmt(cx);
L_2537: uf_cur_op="op_cat";op_cat(cx);
L_2538: Cell t1271=uf_mkp((void*)&uf_sl255);L_2539: pushc(cx,t1271);uf_cur_op="op_cat";op_cat(cx);
L_2540: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2540,cx->sp>1?cx->sp-1:0);goto L_42;K_2540:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2541: Cell t1272=var_trans__dflbl;L_2542: Cell t1273=uf_mkp((void*)&uf_sl256);L_2543: pushc(cx,t1272);pushc(cx,t1273);uf_cur_op="op_fmt";op_fmt(cx);
L_2544: uf_cur_op="op_cat";op_cat(cx);
L_2545: Cell t1274=uf_mkp((void*)&uf_sl257);L_2546: pushc(cx,t1274);uf_cur_op="op_cat";op_cat(cx);
L_2547: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2547,cx->sp>1?cx->sp-1:0);goto L_42;K_2547:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2548: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2548,cx->sp>0?cx->sp-0:0);goto L_566;K_2548:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2549: Cell t1275=uf_mkp((void*)&uf_sl258);L_2550: pushc(cx,t1275);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2550,cx->sp>1?cx->sp-1:0);goto L_42;K_2550:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2551: Cell t1276=uf_mkp((void*)&uf_sl259);L_2552: cx->locals[cx->local_base+0]=t1276;L_2553: pushc(cx,t1276);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2553,cx->sp>0?cx->sp-0:0);goto L_111;K_2553:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2554: Cell t1277=cx->locals[cx->local_base+0];L_2555: pushc(cx,t1277);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2555,cx->sp>2?cx->sp-2:0);goto L_97;K_2555:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2556: Cell t1278=pop(cx);Cell t1279=uf_cnot(t1278);L_2557: pushc(cx,t1279);pushp(cx,(void*)&&L_144);
L_2558: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2558,cx->sp>0?cx->sp-0:0);goto *b;K_2558:;}}
L_2559: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2559,cx->sp>0?cx->sp-0:0);goto L_127;K_2559:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2560: Cell t1280=uf_mkp((void*)&uf_sl260);L_2561: cx->locals[cx->local_base+0]=t1280;L_2562: pushc(cx,t1280);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2562,cx->sp>0?cx->sp-0:0);goto L_111;K_2562:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2563: Cell t1281=cx->locals[cx->local_base+0];L_2564: pushc(cx,t1281);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2564,cx->sp>2?cx->sp-2:0);goto L_97;K_2564:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2565: Cell t1282=pop(cx);Cell t1283=uf_cnot(t1282);L_2566: pushc(cx,t1283);pushp(cx,(void*)&&L_144);
L_2567: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2567,cx->sp>0?cx->sp-0:0);goto *b;K_2567:;}}
L_2568: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2568,cx->sp>0?cx->sp-0:0);goto L_127;K_2568:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2569: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2569,cx->sp>0?cx->sp-0:0);goto L_30;K_2569:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2570: Cell t1284=pop(cx);L_2571: var_trans__blbl=t1284;pushc(cx,t1284);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2571,cx->sp>0?cx->sp-0:0);goto L_30;K_2571:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2572: Cell t1285=pop(cx);L_2573: var_trans__clbl=t1285;pushc(cx,t1285);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2573,cx->sp>0?cx->sp-0:0);goto L_30;K_2573:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2574: Cell t1286=pop(cx);L_2575: var_trans__dflbl=t1286;pushc(cx,t1286);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2575,cx->sp>0?cx->sp-0:0);goto L_30;K_2575:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2576: Cell t1287=pop(cx);L_2577: var_trans__emode=t1287;pushc(cx,t1287);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2577,cx->sp>0?cx->sp-0:0);goto L_1870;K_2577:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2578: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2578,cx->sp>0?cx->sp-0:0);goto L_30;K_2578:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2579: Cell t1288=pop(cx);L_2580: Cell t1289=var_trans__clbl;L_2581: Cell t1290=var_trans__blbl;L_2582: Cell t1291=uf_mkp((void*)&uf_sl261);L_2583: var_trans__inq=t1288;pushc(cx,t1288);pushc(cx,t1289);pushc(cx,t1290);pushc(cx,t1291);uf_cur_op="op_fmt";op_fmt(cx);
L_2584: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2584,cx->sp>1?cx->sp-1:0);goto L_42;K_2584:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2585: L_2586: L_2587: Cell t1292=var_trans__cret;L_2588: var_trans__didret=uf_mki(0LL);pushi(cx,0LL);pushc(cx,t1292);pushp(cx,(void*)&&L_2597);
L_2589: pushp(cx,(void*)&&L_2594);
L_2590: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2590,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2590,cx->sp>0?cx->sp-0:0);goto *el;}K_2590:;}
L_2591: L_2592: L_2593: Cell _rv1293=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1293);return;}cx->csp--;const void*_r1294=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1293);if(!_r1294)return;goto *_r1294;}
L_2594: L_2595: L_2596: Cell _rv1295=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1295);return;}cx->csp--;const void*_r1296=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1295);if(!_r1296)return;goto *_r1296;}
L_2597: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=2;uf_cspush(cx,&&K_2597,cx->sp>0?cx->sp-0:0);goto L_135;K_2597:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2598: Cell t1297=pop(cx);L_2599: var_trans__wclbl=t1297;pushc(cx,t1297);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=2;uf_cspush(cx,&&K_2599,cx->sp>0?cx->sp-0:0);goto L_135;K_2599:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2600: Cell t1298=pop(cx);L_2601: Cell t1299=var_trans__wclbl;L_2602: L_2603: Cell t1300=uf_mkp((void*)&uf_sl262);L_2604: var_trans__wblbl=t1298;pushc(cx,t1298);pushc(cx,t1299);pushc(cx,t1298);pushc(cx,t1300);uf_cur_op="op_fmt";op_fmt(cx);
L_2605: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2605,cx->sp>1?cx->sp-1:0);goto L_42;K_2605:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2606: Cell t1301=var_trans__wclbl;L_2607: pushc(cx,t1301);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2607,cx->sp>1?cx->sp-1:0);goto L_22;K_2607:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2608: Cell t1302=var_trans__wblbl;L_2609: pushc(cx,t1302);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2609,cx->sp>1?cx->sp-1:0);goto L_22;K_2609:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2610: Cell t1303=var_trans__emode;L_2611: pushc(cx,t1303);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2611,cx->sp>1?cx->sp-1:0);goto L_22;K_2611:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2612: Cell t1304=var_trans__inq;L_2613: pushc(cx,t1304);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2613,cx->sp>1?cx->sp-1:0);goto L_22;K_2613:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2614: L_2615: L_2616: Cell t1305=var_trans__psnaps;L_2617: Cell t1306=var_trans__pends;L_2618: var_trans__emode=uf_mki(2LL);pushi(cx,2LL);pushc(cx,t1305);pushc(cx,t1306);uf_cur_op="op_push";op_push(cx);
L_2619: Cell t1307=pop(cx);L_2620: Cell t1308=uf_mkp((void*)&uf_sl263);L_2621: L_2622: Cell t1309=var_trans__douts;L_2623: Cell t1310=uf_mkp((void*)&uf_sl264);L_2624: var_trans__psnaps=t1307;var_trans__pends=t1308;pushc(cx,t1307);pushc(cx,t1308);pushc(cx,t1309);pushc(cx,t1310);uf_cur_op="op_push";op_push(cx);
L_2625: Cell t1311=pop(cx);L_2626: L_2627: L_2628: var_trans__douts=t1311;pushc(cx,t1311);{long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_2628;cx->loops[fr].end=&&K_WE_2628;long _sp0=cx->sp;
K_WC_2628:;{Cell _wc;{
WC2628_L2690: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC2628_2690,cx->sp>0?cx->sp-0:0);goto L_111;K_WC2628_2690:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC2628_L2691: Cell t0=uf_mkp((void*)&uf_sl271);WC2628_L2692: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC2628_2692,cx->sp>2?cx->sp-2:0);goto L_97;K_WC2628_2692:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC2628_L2693: Cell t1=pop(cx);Cell t2=uf_cnot(t1);WC2628_L2694: WC2628_L2695: var_trans__rr=t2;pushc(cx,t2);WC2628_L2696: Cell t3=var_trans__rr;pushc(cx,t3);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_2628;
{
WB2628_L2698: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB2628_2698,cx->sp>0?cx->sp-0:0);goto L_1645;K_WB2628_2698:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB2628_L2699: WB2628_L2700: WB2628_L2701: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);WB2628_L2702: Cell t0=var_trans__rr;pushc(cx,t0);}cx->sp=_sp0;goto K_WC_2628;}
K_WE_2628:;cx->lsp=fr;}
L_2629: Cell t1312=var_trans__douts;L_2630: pushc(cx,t1312);uf_cur_op="op_lpop";op_lpop(cx);
L_2631: Cell t1313=pop(cx);L_2632: Cell t1314=var_trans__psnaps;L_2633: var_trans__wchunk=t1313;pushc(cx,t1313);pushc(cx,t1314);uf_cur_op="op_lpop";op_lpop(cx);
L_2634: Cell t1315=pop(cx);L_2635: L_2636: Cell t1316=var_trans__pends;L_2637: var_trans__wsv=t1315;pushc(cx,t1315);pushc(cx,t1315);pushc(cx,t1316);uf_cur_op="op_cat";op_cat(cx);
L_2638: Cell t1317=pop(cx);L_2639: var_trans__pends=t1317;pushc(cx,t1317);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2639,cx->sp>0?cx->sp-0:0);goto L_30;K_2639:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2640: Cell t1318=pop(cx);L_2641: var_trans__inq=t1318;pushc(cx,t1318);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2641,cx->sp>0?cx->sp-0:0);goto L_30;K_2641:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2642: Cell t1319=pop(cx);L_2643: var_trans__emode=t1319;pushc(cx,t1319);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2643,cx->sp>0?cx->sp-0:0);goto L_30;K_2643:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2644: Cell t1320=pop(cx);L_2645: var_trans__wblbl=t1320;pushc(cx,t1320);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2645,cx->sp>0?cx->sp-0:0);goto L_30;K_2645:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2646: Cell t1321=pop(cx);L_2647: Cell t1322=var_trans__wblbl;L_2648: Cell t1323=uf_mkp((void*)&uf_sl265);L_2649: var_trans__wclbl=t1321;pushc(cx,t1321);pushc(cx,t1322);pushc(cx,t1323);uf_cur_op="op_fmt";op_fmt(cx);
L_2650: Cell t1324=pop(cx);L_2651: Cell t1325=var_trans__wchunk;L_2652: Cell t1326=uf_mkp((void*)&uf_sl266);L_2653: var_trans__wb=t1324;pushc(cx,t1324);pushc(cx,t1325);pushc(cx,t1326);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2653,cx->sp>2?cx->sp-2:0);goto L_97;K_2653:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2654: Cell t1327=pop(cx);Cell t1328=uf_cnot(t1327);L_2655: pushc(cx,t1328);pushp(cx,(void*)&&L_2658);
L_2656: pushp(cx,(void*)&&L_2675);
L_2657: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2657,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2657,cx->sp>0?cx->sp-0:0);goto *el;}K_2657:;Cell _rv1329=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv1329);return;}cx->csp--;const void*_r1330=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1329);if(!_r1330)return;goto *_r1330;}}
L_2658: Cell t1331=var_trans__flabels;L_2659: Cell t1332=var_trans__wb;L_2660: pushc(cx,t1331);pushc(cx,t1332);uf_cur_op="op_cat";op_cat(cx);
L_2661: Cell t1333=var_trans__wchunk;L_2662: pushc(cx,t1333);uf_cur_op="op_cat";op_cat(cx);
L_2663: Cell t1334=uf_mkp((void*)&uf_sl267);L_2664: pushc(cx,t1334);uf_cur_op="op_cat";op_cat(cx);
L_2665: Cell t1335=var_trans__wclbl;L_2666: Cell t1336=uf_mkp((void*)&uf_sl268);L_2667: pushc(cx,t1335);pushc(cx,t1336);uf_cur_op="op_fmt";op_fmt(cx);
L_2668: uf_cur_op="op_cat";op_cat(cx);
L_2669: Cell t1337=pop(cx);L_2670: L_2671: L_2672: var_trans__flabels=t1337;var_trans__didret=uf_mki(0LL);pushc(cx,t1337);pushi(cx,0LL);L_2673: L_2674: Cell _rv1338=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1338);return;}cx->csp--;const void*_r1339=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1338);if(!_r1339)return;goto *_r1339;}
L_2675: Cell t1340=var_trans__flabels;L_2676: Cell t1341=var_trans__wb;L_2677: pushc(cx,t1340);pushc(cx,t1341);uf_cur_op="op_cat";op_cat(cx);
L_2678: Cell t1342=uf_mkp((void*)&uf_sl269);L_2679: pushc(cx,t1342);uf_cur_op="op_cat";op_cat(cx);
L_2680: Cell t1343=var_trans__wclbl;L_2681: Cell t1344=uf_mkp((void*)&uf_sl270);L_2682: pushc(cx,t1343);pushc(cx,t1344);uf_cur_op="op_fmt";op_fmt(cx);
L_2683: uf_cur_op="op_cat";op_cat(cx);
L_2684: Cell t1345=pop(cx);L_2685: L_2686: L_2687: var_trans__flabels=t1345;var_trans__didret=uf_mki(0LL);pushc(cx,t1345);pushi(cx,0LL);L_2688: L_2689: Cell _rv1346=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1346);return;}cx->csp--;const void*_r1347=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1346);if(!_r1347)return;goto *_r1347;}
L_2690: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2690,cx->sp>0?cx->sp-0:0);goto L_111;K_2690:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2691: Cell t1348=uf_mkp((void*)&uf_sl271);L_2692: pushc(cx,t1348);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2692,cx->sp>2?cx->sp-2:0);goto L_97;K_2692:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2693: Cell t1349=pop(cx);Cell t1350=uf_cnot(t1349);L_2694: L_2695: var_trans__rr=t1350;pushc(cx,t1350);L_2696: Cell t1351=var_trans__rr;L_2697: Cell _rv1352=t1351;{if(cx->csp==0){pushc(cx,_rv1352);return;}cx->csp--;const void*_r1353=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1352);if(!_r1353)return;goto *_r1353;}
L_2698: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2698,cx->sp>0?cx->sp-0:0);goto L_1645;K_2698:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2699: L_2700: L_2701: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2702: Cell t1354=var_trans__rr;L_2703: Cell _rv1355=t1354;{if(cx->csp==0){pushc(cx,_rv1355);return;}cx->csp--;const void*_r1356=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1355);if(!_r1356)return;goto *_r1356;}
L_2704: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2704,cx->sp>0?cx->sp-0:0);goto L_127;K_2704:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2705: Cell t1357=uf_mkp((void*)&uf_sl272);L_2706: cx->locals[cx->local_base+0]=t1357;L_2707: pushc(cx,t1357);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2707,cx->sp>0?cx->sp-0:0);goto L_111;K_2707:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2708: Cell t1358=cx->locals[cx->local_base+0];L_2709: pushc(cx,t1358);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2709,cx->sp>2?cx->sp-2:0);goto L_97;K_2709:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2710: Cell t1359=pop(cx);Cell t1360=uf_cnot(t1359);L_2711: pushc(cx,t1360);pushp(cx,(void*)&&L_144);
L_2712: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2712,cx->sp>0?cx->sp-0:0);goto *b;K_2712:;}}
L_2713: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2713,cx->sp>0?cx->sp-0:0);goto L_127;K_2713:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2714: Cell t1361=uf_mkp((void*)&uf_sl273);L_2715: pushc(cx,t1361);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2715,cx->sp>1?cx->sp-1:0);goto L_42;K_2715:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2716: L_2717: L_2718: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2719: Cell t1362=var_trans__rr;L_2720: Cell _rv1363=t1362;{if(cx->csp==0){pushc(cx,_rv1363);return;}cx->csp--;const void*_r1364=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1363);if(!_r1364)return;goto *_r1364;}
L_2721: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2721,cx->sp>0?cx->sp-0:0);goto L_127;K_2721:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2722: Cell t1365=uf_mkp((void*)&uf_sl274);L_2723: cx->locals[cx->local_base+0]=t1365;L_2724: pushc(cx,t1365);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2724,cx->sp>0?cx->sp-0:0);goto L_111;K_2724:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2725: Cell t1366=cx->locals[cx->local_base+0];L_2726: pushc(cx,t1366);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2726,cx->sp>2?cx->sp-2:0);goto L_97;K_2726:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2727: Cell t1367=pop(cx);Cell t1368=uf_cnot(t1367);L_2728: pushc(cx,t1368);pushp(cx,(void*)&&L_144);
L_2729: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2729,cx->sp>0?cx->sp-0:0);goto *b;K_2729:;}}
L_2730: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2730,cx->sp>0?cx->sp-0:0);goto L_127;K_2730:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2731: Cell t1369=uf_mkp((void*)&uf_sl275);L_2732: pushc(cx,t1369);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2732,cx->sp>1?cx->sp-1:0);goto L_42;K_2732:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2733: L_2734: L_2735: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2736: Cell t1370=var_trans__rr;L_2737: Cell _rv1371=t1370;{if(cx->csp==0){pushc(cx,_rv1371);return;}cx->csp--;const void*_r1372=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1371);if(!_r1372)return;goto *_r1372;}
L_2738: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2738,cx->sp>0?cx->sp-0:0);goto L_127;K_2738:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2739: L_2740: L_2741: {long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_2741;cx->loops[fr].end=&&K_WE_2741;long _sp0=cx->sp;
K_WC_2741:;{Cell _wc;{
WC2741_L2746: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC2741_2746,cx->sp>0?cx->sp-0:0);goto L_111;K_WC2741_2746:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC2741_L2747: Cell t0=uf_mkp((void*)&uf_sl276);WC2741_L2748: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC2741_2748,cx->sp>2?cx->sp-2:0);goto L_97;K_WC2741_2748:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC2741_L2749: Cell t1=pop(cx);Cell t2=uf_cnot(t1);WC2741_L2750: WC2741_L2751: var_trans__rr=t2;pushc(cx,t2);WC2741_L2752: Cell t3=var_trans__rr;pushc(cx,t3);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_2741;
{
WB2741_L2754: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB2741_2754,cx->sp>0?cx->sp-0:0);goto L_1645;K_WB2741_2754:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB2741_L2755: WB2741_L2756: WB2741_L2757: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);WB2741_L2758: Cell t0=var_trans__rr;pushc(cx,t0);}cx->sp=_sp0;goto K_WC_2741;}
K_WE_2741:;cx->lsp=fr;}
L_2742: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2742,cx->sp>0?cx->sp-0:0);goto L_127;K_2742:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2743: L_2744: L_2745: Cell _rv1373=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1373);return;}cx->csp--;const void*_r1374=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1373);if(!_r1374)return;goto *_r1374;}
L_2746: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2746,cx->sp>0?cx->sp-0:0);goto L_111;K_2746:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2747: Cell t1375=uf_mkp((void*)&uf_sl276);L_2748: pushc(cx,t1375);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2748,cx->sp>2?cx->sp-2:0);goto L_97;K_2748:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2749: Cell t1376=pop(cx);Cell t1377=uf_cnot(t1376);L_2750: L_2751: var_trans__rr=t1377;pushc(cx,t1377);L_2752: Cell t1378=var_trans__rr;L_2753: Cell _rv1379=t1378;{if(cx->csp==0){pushc(cx,_rv1379);return;}cx->csp--;const void*_r1380=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1379);if(!_r1380)return;goto *_r1380;}
L_2754: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2754,cx->sp>0?cx->sp-0:0);goto L_1645;K_2754:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2755: L_2756: L_2757: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2758: Cell t1381=var_trans__rr;L_2759: Cell _rv1382=t1381;{if(cx->csp==0){pushc(cx,_rv1382);return;}cx->csp--;const void*_r1383=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1382);if(!_r1383)return;goto *_r1383;}
L_2760: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2760,cx->sp>0?cx->sp-0:0);goto L_127;K_2760:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2761: L_2762: L_2763: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2764: Cell t1384=var_trans__rr;L_2765: Cell _rv1385=t1384;{if(cx->csp==0){pushc(cx,_rv1385);return;}cx->csp--;const void*_r1386=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1385);if(!_r1386)return;goto *_r1386;}
L_2766: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2766,cx->sp>0?cx->sp-0:0);goto L_566;K_2766:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2767: Cell t1387=uf_mkp((void*)&uf_sl277);L_2768: cx->locals[cx->local_base+0]=t1387;L_2769: pushc(cx,t1387);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2769,cx->sp>0?cx->sp-0:0);goto L_111;K_2769:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2770: Cell t1388=cx->locals[cx->local_base+0];L_2771: pushc(cx,t1388);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2771,cx->sp>2?cx->sp-2:0);goto L_97;K_2771:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2772: Cell t1389=pop(cx);Cell t1390=uf_cnot(t1389);L_2773: pushc(cx,t1390);pushp(cx,(void*)&&L_144);
L_2774: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2774,cx->sp>0?cx->sp-0:0);goto *b;K_2774:;}}
L_2775: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2775,cx->sp>0?cx->sp-0:0);goto L_127;K_2775:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2776: L_2777: L_2778: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2779: Cell t1391=var_trans__rr;L_2780: Cell _rv1392=t1391;{if(cx->csp==0){pushc(cx,_rv1392);return;}cx->csp--;const void*_r1393=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1392);if(!_r1393)return;goto *_r1393;}
L_2781: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2781,cx->sp>0?cx->sp-0:0);goto L_195;K_2781:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2782: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2782,cx->sp>0?cx->sp-0:0);goto L_111;K_2782:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2783: Cell t1394=pop(cx);L_2784: var_trans__fname=t1394;pushc(cx,t1394);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2784,cx->sp>0?cx->sp-0:0);goto L_127;K_2784:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2785: Cell t1395=uf_mkp((void*)&uf_sl278);L_2786: cx->locals[cx->local_base+0]=t1395;L_2787: pushc(cx,t1395);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2787,cx->sp>0?cx->sp-0:0);goto L_111;K_2787:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2788: Cell t1396=cx->locals[cx->local_base+0];L_2789: pushc(cx,t1396);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2789,cx->sp>2?cx->sp-2:0);goto L_97;K_2789:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2790: Cell t1397=pop(cx);Cell t1398=uf_cnot(t1397);L_2791: pushc(cx,t1398);pushp(cx,(void*)&&L_144);
L_2792: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2792,cx->sp>0?cx->sp-0:0);goto *b;K_2792:;}}
L_2793: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2793,cx->sp>0?cx->sp-0:0);goto L_127;K_2793:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2794: uf_cur_op="op_list";op_list(cx);
L_2795: Cell t1399=pop(cx);L_2796: L_2797: L_2798: var_trans__pl=t1399;pushc(cx,t1399);{long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_2798;cx->loops[fr].end=&&K_WE_2798;long _sp0=cx->sp;
K_WC_2798:;{Cell _wc;{
WC2798_L2886: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC2798_2886,cx->sp>0?cx->sp-0:0);goto L_111;K_WC2798_2886:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC2798_L2887: Cell t0=uf_mkp((void*)&uf_sl291);WC2798_L2888: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC2798_2888,cx->sp>2?cx->sp-2:0);goto L_97;K_WC2798_2888:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC2798_L2889: Cell t1=pop(cx);Cell t2=uf_cnot(t1);WC2798_L2890: WC2798_L2891: var_trans__rr=t2;pushc(cx,t2);WC2798_L2892: Cell t3=var_trans__rr;pushc(cx,t3);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_2798;
{
WB2798_L2894: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB2798_2894,cx->sp>0?cx->sp-0:0);goto L_111;K_WB2798_2894:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB2798_L2895: Cell t0=uf_mkp((void*)&uf_sl292);WB2798_L2896: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB2798_2896,cx->sp>2?cx->sp-2:0);goto L_97;K_WB2798_2896:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB2798_L2897: pushp(cx,(void*)&&L_2903);
WB2798_L2898: pushp(cx,(void*)&&L_2909);
WB2798_L2899: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_WB2798_2899,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_WB2798_2899,cx->sp>0?cx->sp-0:0);goto *el;}K_WB2798_2899:;}
WB2798_L2900: }cx->sp=_sp0;goto K_WC_2798;}
K_WE_2798:;cx->lsp=fr;}
L_2799: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2799,cx->sp>0?cx->sp-0:0);goto L_127;K_2799:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2800: uf_cur_op="op_dict";op_dict(cx);
L_2801: Cell t1400=pop(cx);L_2802: L_2803: L_2804: Cell t1401=uf_mkp((void*)&uf_sl279);L_2805: L_2806: Cell t1402=var_trans__fname;L_2807: Cell t1403=uf_mkp((void*)&uf_sl280);L_2808: var_trans__vars=t1400;var_trans__cret=uf_mki(0LL);var_trans__qout=t1401;pushc(cx,t1400);pushi(cx,0LL);pushc(cx,t1401);pushc(cx,t1402);pushc(cx,t1403);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2808,cx->sp>2?cx->sp-2:0);goto L_97;K_2808:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2809: pushp(cx,(void*)&&L_2874);
L_2810: pushp(cx,(void*)&&L_2879);
L_2811: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2811,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2811,cx->sp>0?cx->sp-0:0);goto *el;}K_2811:;}
L_2812: Cell t1404=var_trans__fname;L_2813: Cell t1405=uf_mkp((void*)&uf_sl281);L_2814: pushc(cx,t1404);pushc(cx,t1405);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2814,cx->sp>2?cx->sp-2:0);goto L_97;K_2814:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2815: Cell t1406=pop(cx);L_2816: Cell t1407=var_trans__pl;L_2817: var_trans__inmain=t1406;pushc(cx,t1406);pushc(cx,t1407);uf_cur_op="op_len";op_len(cx);
L_2818: L_2819: Cell t1408=pop(cx);Cell t1409=uf_csub(t1408,uf_mki(1LL));L_2820: L_2821: Cell t1410=var_trans__inmain;L_2822: Cell t1411=uf_cnot(t1410);L_2823: L_2824: L_2825: var_trans__pfi=t1409;pushc(cx,t1409);pushc(cx,t1411);{long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_2825;cx->loops[fr].end=&&K_WE_2825;long _sp0=cx->sp;
K_WC_2825:;{Cell _wc;{
WC2825_L2937: Cell t0=var_trans__pfi;WC2825_L2938: WC2825_L2939: Cell t1=uf_ceq(t0,uf_mki(-1LL));WC2825_L2940: Cell t2=uf_cnot(t1);WC2825_L2941: WC2825_L2942: var_trans__rr=t2;pushc(cx,t2);WC2825_L2943: Cell t3=var_trans__rr;pushc(cx,t3);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_2825;
{
WB2825_L2945: Cell t0=var_trans__pl;WB2825_L2946: Cell t1=var_trans__pfi;WB2825_L2947: pushc(cx,t0);pushc(cx,t1);uf_cur_op="op_get";op_get(cx);
WB2825_L2948: Cell t2=pop(cx);WB2825_L2949: WB2825_L2950: var_trans__nv=t2;pushc(cx,t2);pushc(cx,t2);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB2825_2950,cx->sp>1?cx->sp-1:0);goto L_215;K_WB2825_2950:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB2825_L2951: Cell t3=uf_mkp((void*)&uf_sl294);WB2825_L2952: pushc(cx,t3);uf_cur_op="op_fmt";op_fmt(cx);
WB2825_L2953: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB2825_2953,cx->sp>1?cx->sp-1:0);goto L_42;K_WB2825_2953:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB2825_L2954: Cell t4=var_trans__pfi;WB2825_L2955: WB2825_L2956: Cell t5=uf_csub(t4,uf_mki(1LL));WB2825_L2957: WB2825_L2958: WB2825_L2959: WB2825_L2960: var_trans__pfi=t5;var_trans__rr=uf_mki(0LL);pushc(cx,t5);pushi(cx,0LL);WB2825_L2961: Cell t6=var_trans__rr;pushc(cx,t6);}cx->sp=_sp0;goto K_WC_2825;}
K_WE_2825:;cx->lsp=fr;}
L_2826: Cell t1412=uf_mkp((void*)&uf_sl282);L_2827: cx->locals[cx->local_base+0]=t1412;L_2828: pushc(cx,t1412);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2828,cx->sp>0?cx->sp-0:0);goto L_111;K_2828:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2829: Cell t1413=cx->locals[cx->local_base+0];L_2830: pushc(cx,t1413);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2830,cx->sp>2?cx->sp-2:0);goto L_97;K_2830:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2831: Cell t1414=pop(cx);Cell t1415=uf_cnot(t1414);L_2832: pushc(cx,t1415);pushp(cx,(void*)&&L_144);
L_2833: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2833,cx->sp>0?cx->sp-0:0);goto *b;K_2833:;}}
L_2834: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2834,cx->sp>0?cx->sp-0:0);goto L_127;K_2834:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2835: L_2836: L_2837: {long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_2837;cx->loops[fr].end=&&K_WE_2837;long _sp0=cx->sp;
K_WC_2837:;{Cell _wc;{
WC2837_L2963: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC2837_2963,cx->sp>0?cx->sp-0:0);goto L_111;K_WC2837_2963:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC2837_L2964: Cell t0=uf_mkp((void*)&uf_sl295);WC2837_L2965: pushc(cx,t0);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WC2837_2965,cx->sp>2?cx->sp-2:0);goto L_97;K_WC2837_2965:;cx->local_base=cx->local_frames[--cx->local_fsp];
WC2837_L2966: Cell t1=pop(cx);Cell t2=uf_cnot(t1);WC2837_L2967: WC2837_L2968: var_trans__rr=t2;pushc(cx,t2);WC2837_L2969: Cell t3=var_trans__rr;pushc(cx,t3);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_2837;
{
WB2837_L2971: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_WB2837_2971,cx->sp>0?cx->sp-0:0);goto L_1645;K_WB2837_2971:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB2837_L2972: WB2837_L2973: WB2837_L2974: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);WB2837_L2975: Cell t0=var_trans__rr;pushc(cx,t0);}cx->sp=_sp0;goto K_WC_2837;}
K_WE_2837:;cx->lsp=fr;}
L_2838: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2838,cx->sp>0?cx->sp-0:0);goto L_127;K_2838:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2839: Cell t1416=var_trans__didret;L_2840: L_2841: Cell t1417=uf_ceq(t1416,uf_mki(2LL));L_2842: pushc(cx,t1417);pushp(cx,(void*)&&L_2864);
L_2843: pushp(cx,(void*)&&L_2867);
L_2844: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2844,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2844,cx->sp>0?cx->sp-0:0);goto *el;}K_2844:;}
L_2845: Cell t1418=var_trans__qout;L_2846: Cell t1419=uf_mkp((void*)&uf_sl283);L_2847: pushc(cx,t1418);pushc(cx,t1419);uf_cur_op="op_fmt";op_fmt(cx);
L_2848: uf_cur_op="op_print";op_print(cx);
L_2849: Cell t1420=uf_mkp((void*)&uf_sl284);L_2850: L_2851: Cell t1421=var_trans__pends;L_2852: Cell t1422=uf_mkp((void*)&uf_sl285);L_2853: var_trans__qout=t1420;pushc(cx,t1420);pushc(cx,t1421);pushc(cx,t1422);uf_cur_op="op_fmt";op_fmt(cx);
L_2854: uf_cur_op="op_print";op_print(cx);
L_2855: Cell t1423=uf_mkp((void*)&uf_sl286);L_2856: L_2857: Cell t1424=var_trans__flabels;L_2858: Cell t1425=uf_mkp((void*)&uf_sl287);L_2859: var_trans__pends=t1423;pushc(cx,t1423);pushc(cx,t1424);pushc(cx,t1425);uf_cur_op="op_fmt";op_fmt(cx);
L_2860: uf_cur_op="op_print";op_print(cx);
L_2861: Cell t1426=uf_mkp((void*)&uf_sl288);L_2862: L_2863: var_trans__flabels=t1426;Cell _rv1427=t1426;{if(cx->csp==0){pushc(cx,_rv1427);return;}cx->csp--;const void*_r1428=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1427);if(!_r1428)return;goto *_r1428;}
L_2864: L_2865: L_2866: Cell _rv1429=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1429);return;}cx->csp--;const void*_r1430=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1429);if(!_r1430)return;goto *_r1430;}
L_2867: Cell t1431=var_trans__inmain;L_2868: pushc(cx,t1431);pushp(cx,(void*)&&L_2977);
L_2869: pushp(cx,(void*)&&L_2984);
L_2870: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2870,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2870,cx->sp>0?cx->sp-0:0);goto *el;}K_2870:;}
L_2871: L_2872: L_2873: Cell _rv1432=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1432);return;}cx->csp--;const void*_r1433=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1432);if(!_r1433)return;goto *_r1433;}
L_2874: Cell t1434=uf_mkp((void*)&uf_sl289);L_2875: pushc(cx,t1434);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2875,cx->sp>1?cx->sp-1:0);goto L_42;K_2875:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2876: L_2877: L_2878: Cell _rv1435=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1435);return;}cx->csp--;const void*_r1436=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1435);if(!_r1436)return;goto *_r1436;}
L_2879: Cell t1437=var_trans__fname;L_2880: Cell t1438=uf_mkp((void*)&uf_sl290);L_2881: pushc(cx,t1437);pushc(cx,t1438);uf_cur_op="op_fmt";op_fmt(cx);
L_2882: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2882,cx->sp>1?cx->sp-1:0);goto L_42;K_2882:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2883: L_2884: L_2885: Cell _rv1439=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1439);return;}cx->csp--;const void*_r1440=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1439);if(!_r1440)return;goto *_r1440;}
L_2886: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2886,cx->sp>0?cx->sp-0:0);goto L_111;K_2886:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2887: Cell t1441=uf_mkp((void*)&uf_sl291);L_2888: pushc(cx,t1441);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2888,cx->sp>2?cx->sp-2:0);goto L_97;K_2888:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2889: Cell t1442=pop(cx);Cell t1443=uf_cnot(t1442);L_2890: L_2891: var_trans__rr=t1443;pushc(cx,t1443);L_2892: Cell t1444=var_trans__rr;L_2893: Cell _rv1445=t1444;{if(cx->csp==0){pushc(cx,_rv1445);return;}cx->csp--;const void*_r1446=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1445);if(!_r1446)return;goto *_r1446;}
L_2894: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2894,cx->sp>0?cx->sp-0:0);goto L_111;K_2894:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2895: Cell t1447=uf_mkp((void*)&uf_sl292);L_2896: pushc(cx,t1447);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2896,cx->sp>2?cx->sp-2:0);goto L_97;K_2896:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2897: pushp(cx,(void*)&&L_2903);
L_2898: pushp(cx,(void*)&&L_2909);
L_2899: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2899,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2899,cx->sp>0?cx->sp-0:0);goto *el;}K_2899:;}
L_2900: L_2901: L_2902: Cell _rv1448=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1448);return;}cx->csp--;const void*_r1449=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1448);if(!_r1449)return;goto *_r1449;}
L_2903: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2903,cx->sp>0?cx->sp-0:0);goto L_127;K_2903:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2904: L_2905: L_2906: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2907: Cell t1450=var_trans__rr;L_2908: Cell _rv1451=t1450;{if(cx->csp==0){pushc(cx,_rv1451);return;}cx->csp--;const void*_r1452=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1451);if(!_r1452)return;goto *_r1452;}
L_2909: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2909,cx->sp>0?cx->sp-0:0);goto L_111;K_2909:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2910: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2910,cx->sp>1?cx->sp-1:0);goto L_155;K_2910:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2911: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2911,cx->sp>0?cx->sp-0:0);goto L_111;K_2911:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2912: Cell t1453=uf_mkp((void*)&uf_sl293);L_2913: pushc(cx,t1453);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2913,cx->sp>2?cx->sp-2:0);goto L_97;K_2913:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2914: Cell t1454=pop(cx);Cell t1455=pop(cx);Cell t1456=uf_cadd(t1455,t1454);L_2915: pushc(cx,t1456);pushp(cx,(void*)&&L_2921);
L_2916: pushp(cx,(void*)&&L_2927);
L_2917: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_2917,cx->sp>0?cx->sp-0:0);goto *th;}else{uf_cspush(cx,&&K_2917,cx->sp>0?cx->sp-0:0);goto *el;}K_2917:;}
L_2918: L_2919: L_2920: Cell _rv1457=uf_mki(0LL);{if(cx->csp==0){pushc(cx,_rv1457);return;}cx->csp--;const void*_r1458=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1457);if(!_r1458)return;goto *_r1458;}
L_2921: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2921,cx->sp>0?cx->sp-0:0);goto L_127;K_2921:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2922: L_2923: L_2924: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2925: Cell t1459=var_trans__rr;L_2926: Cell _rv1460=t1459;{if(cx->csp==0){pushc(cx,_rv1460);return;}cx->csp--;const void*_r1461=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1460);if(!_r1461)return;goto *_r1461;}
L_2927: Cell t1462=var_trans__pl;L_2928: pushc(cx,t1462);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2928,cx->sp>0?cx->sp-0:0);goto L_111;K_2928:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2929: uf_cur_op="op_push";op_push(cx);
L_2930: Cell t1463=pop(cx);L_2931: var_trans__pl=t1463;pushc(cx,t1463);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2931,cx->sp>0?cx->sp-0:0);goto L_127;K_2931:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2932: L_2933: L_2934: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2935: Cell t1464=var_trans__rr;L_2936: Cell _rv1465=t1464;{if(cx->csp==0){pushc(cx,_rv1465);return;}cx->csp--;const void*_r1466=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1465);if(!_r1466)return;goto *_r1466;}
L_2937: Cell t1467=var_trans__pfi;L_2938: L_2939: Cell t1468=uf_ceq(t1467,uf_mki(-1LL));L_2940: Cell t1469=uf_cnot(t1468);L_2941: L_2942: var_trans__rr=t1469;pushc(cx,t1469);L_2943: Cell t1470=var_trans__rr;L_2944: Cell _rv1471=t1470;{if(cx->csp==0){pushc(cx,_rv1471);return;}cx->csp--;const void*_r1472=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1471);if(!_r1472)return;goto *_r1472;}
L_2945: Cell t1473=var_trans__pl;L_2946: Cell t1474=var_trans__pfi;L_2947: pushc(cx,t1473);pushc(cx,t1474);uf_cur_op="op_get";op_get(cx);
L_2948: Cell t1475=pop(cx);L_2949: L_2950: var_trans__nv=t1475;pushc(cx,t1475);pushc(cx,t1475);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2950,cx->sp>1?cx->sp-1:0);goto L_215;K_2950:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2951: Cell t1476=uf_mkp((void*)&uf_sl294);L_2952: pushc(cx,t1476);uf_cur_op="op_fmt";op_fmt(cx);
L_2953: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2953,cx->sp>1?cx->sp-1:0);goto L_42;K_2953:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2954: Cell t1477=var_trans__pfi;L_2955: L_2956: Cell t1478=uf_csub(t1477,uf_mki(1LL));L_2957: L_2958: L_2959: L_2960: var_trans__pfi=t1478;var_trans__rr=uf_mki(0LL);pushc(cx,t1478);pushi(cx,0LL);L_2961: Cell t1479=var_trans__rr;L_2962: Cell _rv1480=t1479;{if(cx->csp==0){pushc(cx,_rv1480);return;}cx->csp--;const void*_r1481=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1480);if(!_r1481)return;goto *_r1481;}
L_2963: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2963,cx->sp>0?cx->sp-0:0);goto L_111;K_2963:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2964: Cell t1482=uf_mkp((void*)&uf_sl295);L_2965: pushc(cx,t1482);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2965,cx->sp>2?cx->sp-2:0);goto L_97;K_2965:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2966: Cell t1483=pop(cx);Cell t1484=uf_cnot(t1483);L_2967: L_2968: var_trans__rr=t1484;pushc(cx,t1484);L_2969: Cell t1485=var_trans__rr;L_2970: Cell _rv1486=t1485;{if(cx->csp==0){pushc(cx,_rv1486);return;}cx->csp--;const void*_r1487=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1486);if(!_r1487)return;goto *_r1487;}
L_2971: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2971,cx->sp>0?cx->sp-0:0);goto L_1645;K_2971:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2972: L_2973: L_2974: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2975: Cell t1488=var_trans__rr;L_2976: Cell _rv1489=t1488;{if(cx->csp==0){pushc(cx,_rv1489);return;}cx->csp--;const void*_r1490=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1489);if(!_r1490)return;goto *_r1490;}
L_2977: Cell t1491=uf_mkp((void*)&uf_sl296);L_2978: pushc(cx,t1491);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2978,cx->sp>1?cx->sp-1:0);goto L_42;K_2978:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2979: L_2980: L_2981: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2982: Cell t1492=var_trans__rr;L_2983: Cell _rv1493=t1492;{if(cx->csp==0){pushc(cx,_rv1493);return;}cx->csp--;const void*_r1494=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1493);if(!_r1494)return;goto *_r1494;}
L_2984: Cell t1495=uf_mkp((void*)&uf_sl297);L_2985: pushc(cx,t1495);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_2985,cx->sp>1?cx->sp-1:0);goto L_42;K_2985:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_2986: L_2987: L_2988: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_2989: Cell t1496=var_trans__rr;L_2990: Cell _rv1497=t1496;{if(cx->csp==0){pushc(cx,_rv1497);return;}cx->csp--;const void*_r1498=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1497);if(!_r1498)return;goto *_r1498;}
L_2991: pushp(cx,(void*)&&L_2995);
L_2992: pushp(cx,(void*)&&L_3003);
L_2993: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_2993:;cx->loops[fr].cont=&&K_WT_2993;cx->loops[fr].end=&&K_WE_2993;
uf_cspush(cx,&&K_WC_2993,cx->sp>0?cx->sp-0:0);goto *cnd;K_WC_2993:;
if(uf_zero(pop(cx)))goto K_WE_2993;
uf_cspush(cx,&&K_WB_2993,cx->sp>0?cx->sp-0:0);goto *bod;K_WB_2993:;pop(cx);
goto K_WT_2993;
K_WE_2993:;cx->lsp=fr;}
L_2994: Cell _rv1499=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv1499);return;}cx->csp--;const void*_r1500=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1499);if(!_r1500)return;goto *_r1500;}
L_2995: Cell t1501=var_trans__pi;L_2996: Cell t1502=var_trans__nt;L_2997: Cell t1503=uf_ceq(t1501,t1502);L_2998: Cell t1504=uf_cnot(t1503);L_2999: L_3000: var_trans__rr=t1504;pushc(cx,t1504);L_3001: Cell t1505=var_trans__rr;L_3002: Cell _rv1506=t1505;{if(cx->csp==0){pushc(cx,_rv1506);return;}cx->csp--;const void*_r1507=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1506);if(!_r1507)return;goto *_r1507;}
L_3003: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3003,cx->sp>0?cx->sp-0:0);goto L_2781;K_3003:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3004: L_3005: L_3006: var_trans__rr=uf_mki(0LL);pushi(cx,0LL);L_3007: Cell t1508=var_trans__rr;L_3008: Cell _rv1509=t1508;{if(cx->csp==0){pushc(cx,_rv1509);return;}cx->csp--;const void*_r1510=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1509);if(!_r1510)return;goto *_r1510;}
L_3009: L_3010: L_3011: L_3012: L_3013: L_3014: L_3015: L_3016: L_3017: var_trans__inq=uf_mki(0LL);var_trans__emode=uf_mki(0LL);var_trans__didret=uf_mki(0LL);var_trans__cret=uf_mki(0LL);pushi(cx,0LL);pushi(cx,0LL);pushi(cx,0LL);pushi(cx,0LL);uf_cur_op="op_list";op_list(cx);
L_3018: Cell t1511=pop(cx);L_3019: Cell t1512=uf_mkp((void*)&uf_sl298);L_3020: L_3021: var_trans__douts=t1511;var_trans__pends=t1512;pushc(cx,t1511);pushc(cx,t1512);uf_cur_op="op_list";op_list(cx);
L_3022: Cell t1513=pop(cx);L_3023: Cell t1514=uf_mkp((void*)&uf_sl299);L_3024: L_3025: var_trans__psnaps=t1513;var_trans__flabels=t1514;pushc(cx,t1513);pushc(cx,t1514);pushp(cx,(void*)&uf_x0);
L_3026: uf_cur_op="op_loadx";op_loadx(cx);
L_3027: L_3028: Cell t1515=pop(cx);Cell t1516=uf_ceq(t1515,uf_mki(2LL));L_3029: Cell t1517=uf_cnot(t1516);L_3030: pushc(cx,t1517);pushp(cx,(void*)&&L_3133);
L_3031: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_3031,cx->sp>0?cx->sp-0:0);goto *b;K_3031:;}}
L_3032: pushp(cx,(void*)&uf_x1);
L_3033: uf_cur_op="op_loadx";op_loadx(cx);
L_3034: L_3035: Cell t1518=pop(cx);Cell t1519=uf_cadd(t1518,uf_mki(8LL));L_3036: pushc(cx,t1519);uf_cur_op="op_loadx";op_loadx(cx);
L_3037: Cell t1520=pop(cx);L_3038: L_3039: Cell t1521=uf_mkp((void*)&uf_sl300);L_3040: var_trans__path=t1520;pushc(cx,t1520);pushc(cx,t1520);pushc(cx,t1521);uf_cur_op="fopen";{Cell a1=pop(cx);Cell a0=pop(cx);void* r=((void*(*)(void*,void*))uf_im2)((void*)uf_sptr(a0),(void*)uf_sptr(a1));pushp(cx,r);}
L_3041: Cell t1522=pop(cx);L_3042: L_3043: Cell t1523=uf_cnot(t1522);L_3044: var_trans__f=t1522;pushc(cx,t1522);pushc(cx,t1523);pushp(cx,(void*)&&L_3136);
L_3045: {const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_3045,cx->sp>0?cx->sp-0:0);goto *b;K_3045:;}}
L_3046: Cell t1524=var_trans__f;L_3047: L_3048: L_3049: pushc(cx,t1524);pushi(cx,0LL);pushi(cx,2LL);uf_cur_op="fseek";{Cell a2=pop(cx);Cell a1=pop(cx);Cell a0=pop(cx);int r=((int(*)(void*,int64_t,int64_t))uf_im3)((void*)uf_sptr(a0),(int64_t)(a1.tag==T_FLOAT?(int64_t)uf_f(a1):a1.i),(int64_t)(a2.tag==T_FLOAT?(int64_t)uf_f(a2):a2.i));pushi(cx,(int64_t)r);}
L_3050: Cell t1525=var_trans__f;L_3051: pushc(cx,t1525);uf_cur_op="ftell";{Cell a0=pop(cx);int r=((int(*)(void*))uf_im4)((void*)uf_sptr(a0));pushi(cx,(int64_t)r);}
L_3052: Cell t1526=pop(cx);L_3053: Cell t1527=var_trans__f;L_3054: L_3055: L_3056: var_trans__srclen=t1526;pushc(cx,t1526);pushc(cx,t1527);pushi(cx,0LL);pushi(cx,0LL);uf_cur_op="fseek";{Cell a2=pop(cx);Cell a1=pop(cx);Cell a0=pop(cx);int r=((int(*)(void*,int64_t,int64_t))uf_im3)((void*)uf_sptr(a0),(int64_t)(a1.tag==T_FLOAT?(int64_t)uf_f(a1):a1.i),(int64_t)(a2.tag==T_FLOAT?(int64_t)uf_f(a2):a2.i));pushi(cx,(int64_t)r);}
L_3057: Cell t1528=var_trans__srclen;L_3058: L_3059: Cell t1529=uf_cadd(t1528,uf_mki(16LL));L_3060: pushc(cx,t1529);uf_cur_op="op_buf";op_buf(cx);
L_3061: Cell t1530=pop(cx);L_3062: L_3063: L_3064: Cell t1531=var_trans__srclen;L_3065: Cell t1532=var_trans__f;L_3066: var_trans__src=t1530;pushc(cx,t1530);pushc(cx,t1530);pushi(cx,1LL);pushc(cx,t1531);pushc(cx,t1532);uf_cur_op="fread";{Cell a3=pop(cx);Cell a2=pop(cx);Cell a1=pop(cx);Cell a0=pop(cx);int r=((int(*)(void*,int64_t,int64_t,void*))uf_im5)((void*)uf_sptr(a0),(int64_t)(a1.tag==T_FLOAT?(int64_t)uf_f(a1):a1.i),(int64_t)(a2.tag==T_FLOAT?(int64_t)uf_f(a2):a2.i),(void*)uf_sptr(a3));pushi(cx,(int64_t)r);}
L_3067: Cell t1533=var_trans__f;L_3068: pushc(cx,t1533);uf_cur_op="fclose";{Cell a0=pop(cx);int r=((int(*)(void*))uf_im6)((void*)uf_sptr(a0));pushi(cx,(int64_t)r);}
L_3069: uf_cur_op="op_list";op_list(cx);
L_3070: Cell t1534=pop(cx);L_3071: L_3072: L_3073: L_3074: L_3075: var_trans__toks=t1534;var_trans__pos=uf_mki(0LL);pushc(cx,t1534);pushi(cx,0LL);{long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_3075;cx->loops[fr].end=&&K_WE_3075;long _sp0=cx->sp;
K_WC_3075:;{Cell _wc;{
WC3075_L281: pushp(cx,(void*)&&L_414);
WC3075_L282: pushp(cx,(void*)&&L_438);
WC3075_L283: {const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die("loops nested too deep");cx->loops[fr].cspl=cx->csp;
K_WT_WC3075_283:;cx->loops[fr].cont=&&K_WT_WC3075_283;cx->loops[fr].end=&&K_WE_WC3075_283;
uf_cspush(cx,&&K_WC_WC3075_283,cx->sp>0?cx->sp-0:0);goto *cnd;K_WC_WC3075_283:;
if(uf_zero(pop(cx)))goto K_WE_WC3075_283;
uf_cspush(cx,&&K_WB_WC3075_283,cx->sp>0?cx->sp-0:0);goto *bod;K_WB_WC3075_283:;pop(cx);
goto K_WT_WC3075_283;
K_WE_WC3075_283:;cx->lsp=fr;}
WC3075_L284: WC3075_L285: Cell t0=var_trans__pos;WC3075_L286: Cell t1=var_trans__srclen;WC3075_L287: Cell t2=uf_clt(t0,t1);pushc(cx,t2);}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_3075;
{
WB3075_L289: Cell t0=var_trans__src;WB3075_L290: Cell t1=var_trans__pos;WB3075_L291: Cell t2=var_trans__srclen;WB3075_L292: pushc(cx,t0);pushc(cx,t1);pushc(cx,t2);uf_cur_op="op_slice";op_slice(cx);
WB3075_L293: Cell t3=pop(cx);WB3075_L294: WB3075_L295: Cell t4=uf_mkp((void*)&uf_sl19);WB3075_L296: var_trans__rest=t3;pushc(cx,t3);pushc(cx,t3);pushc(cx,t4);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=1;uf_cspush(cx,&&K_WB3075_296,cx->sp>1?cx->sp-1:0);goto L_248;K_WB3075_296:;cx->local_base=cx->local_frames[--cx->local_fsp];
WB3075_L297: Cell t5=pop(cx);WB3075_L298: WB3075_L299: WB3075_L300: Cell t6=uf_ceq(t5,uf_mki(-1LL));WB3075_L301: Cell t7=uf_cnot(t6);WB3075_L302: var_trans__ln=t5;pushc(cx,t5);pushc(cx,t7);pushp(cx,(void*)&&L_306);
WB3075_L303: pushp(cx,(void*)&&L_313);
WB3075_L304: {const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);if(uf_truthy(c)){uf_cspush(cx,&&K_WB3075_304,cx->sp>1?cx->sp-1:0);goto *th;}else{uf_cspush(cx,&&K_WB3075_304,cx->sp>0?cx->sp-0:0);goto *el;}K_WB3075_304:;}
}cx->sp=_sp0;goto K_WC_3075;}
K_WE_3075:;cx->lsp=fr;}
L_3076: Cell t1535=var_trans__toks;L_3077: pushc(cx,t1535);uf_cur_op="op_len";op_len(cx);
L_3078: Cell t1536=pop(cx);L_3079: L_3080: L_3081: L_3082: L_3083: L_3084: L_3085: L_3086: L_3087: var_trans__nt=t1536;var_trans__pi=uf_mki(0LL);var_trans__lbl=uf_mki(0LL);var_trans__fid=uf_mki(0LL);var_trans__inmain=uf_mki(0LL);pushc(cx,t1536);pushi(cx,0LL);pushi(cx,0LL);pushi(cx,0LL);pushi(cx,0LL);uf_cur_op="op_list";op_list(cx);
L_3088: Cell t1537=pop(cx);L_3089: var_trans__ps=t1537;pushc(cx,t1537);uf_cur_op="op_list";op_list(cx);
L_3090: Cell t1538=pop(cx);L_3091: Cell t1539=uf_mkp((void*)&uf_sl301);L_3092: var_trans__ls=t1538;pushc(cx,t1538);pushc(cx,t1539);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3092,cx->sp>1?cx->sp-1:0);goto L_42;K_3092:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3093: Cell t1540=uf_mkp((void*)&uf_sl302);L_3094: pushc(cx,t1540);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3094,cx->sp>1?cx->sp-1:0);goto L_42;K_3094:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3095: Cell t1541=uf_mkp((void*)&uf_sl303);L_3096: pushc(cx,t1541);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3096,cx->sp>1?cx->sp-1:0);goto L_42;K_3096:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3097: Cell t1542=uf_mkp((void*)&uf_sl304);L_3098: pushc(cx,t1542);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3098,cx->sp>1?cx->sp-1:0);goto L_42;K_3098:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3099: Cell t1543=uf_mkp((void*)&uf_sl305);L_3100: pushc(cx,t1543);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3100,cx->sp>1?cx->sp-1:0);goto L_42;K_3100:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3101: Cell t1544=uf_mkp((void*)&uf_sl306);L_3102: pushc(cx,t1544);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3102,cx->sp>1?cx->sp-1:0);goto L_42;K_3102:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3103: Cell t1545=uf_mkp((void*)&uf_sl307);L_3104: pushc(cx,t1545);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3104,cx->sp>1?cx->sp-1:0);goto L_42;K_3104:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3105: Cell t1546=uf_mkp((void*)&uf_sl308);L_3106: pushc(cx,t1546);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3106,cx->sp>1?cx->sp-1:0);goto L_42;K_3106:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3107: Cell t1547=uf_mkp((void*)&uf_sl309);L_3108: pushc(cx,t1547);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3108,cx->sp>1?cx->sp-1:0);goto L_42;K_3108:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3109: Cell t1548=uf_mkp((void*)&uf_sl310);L_3110: pushc(cx,t1548);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3110,cx->sp>1?cx->sp-1:0);goto L_42;K_3110:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3111: Cell t1549=uf_mkp((void*)&uf_sl311);L_3112: pushc(cx,t1549);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3112,cx->sp>1?cx->sp-1:0);goto L_42;K_3112:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3113: Cell t1550=uf_mkp((void*)&uf_sl312);L_3114: pushc(cx,t1550);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3114,cx->sp>1?cx->sp-1:0);goto L_42;K_3114:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3115: Cell t1551=uf_mkp((void*)&uf_sl313);L_3116: pushc(cx,t1551);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3116,cx->sp>1?cx->sp-1:0);goto L_42;K_3116:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3117: Cell t1552=uf_mkp((void*)&uf_sl314);L_3118: pushc(cx,t1552);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3118,cx->sp>1?cx->sp-1:0);goto L_42;K_3118:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3119: Cell t1553=uf_mkp((void*)&uf_sl315);L_3120: pushc(cx,t1553);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3120,cx->sp>1?cx->sp-1:0);goto L_42;K_3120:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3121: Cell t1554=uf_mkp((void*)&uf_sl316);L_3122: pushc(cx,t1554);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3122,cx->sp>1?cx->sp-1:0);goto L_42;K_3122:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3123: Cell t1555=uf_mkp((void*)&uf_sl317);L_3124: pushc(cx,t1555);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3124,cx->sp>1?cx->sp-1:0);goto L_42;K_3124:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3125: Cell t1556=uf_mkp((void*)&uf_sl318);L_3126: pushc(cx,t1556);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3126,cx->sp>1?cx->sp-1:0);goto L_42;K_3126:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3127: Cell t1557=uf_mkp((void*)&uf_sl319);L_3128: pushc(cx,t1557);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3128,cx->sp>1?cx->sp-1:0);goto L_42;K_3128:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3129: cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3129,cx->sp>0?cx->sp-0:0);goto L_2991;K_3129:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3130: L_3131: pushi(cx,0LL);uf_cur_op="exit";{Cell a0=pop(cx);((void(*)(int64_t))uf_im8)((int64_t)(a0.tag==T_FLOAT?(int64_t)uf_f(a0):a0.i));}
L_3132: Cell _rv1558=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv1558);return;}cx->csp--;const void*_r1559=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1558);if(!_r1559)return;goto *_r1559;}
L_3133: Cell t1560=uf_mkp((void*)&uf_sl320);L_3134: pushc(cx,t1560);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3134,cx->sp>0?cx->sp-0:0);goto L_107;K_3134:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3135: Cell _rv1561=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv1561);return;}cx->csp--;const void*_r1562=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1561);if(!_r1562)return;goto *_r1562;}
L_3136: Cell t1563=uf_mkp((void*)&uf_sl321);L_3137: pushc(cx,t1563);cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+=0;uf_cspush(cx,&&K_3137,cx->sp>0?cx->sp-0:0);goto L_107;K_3137:;cx->local_base=cx->local_frames[--cx->local_fsp];
L_3138: Cell _rv1564=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv1564);return;}cx->csp--;const void*_r1565=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1564);if(!_r1565)return;goto *_r1565;}
L_3139: Cell _rv1566=(cx->sp>(cx->csp>0?cx->rsps[cx->csp-1]:0)&&cx->sp>0)?cx->ds[cx->sp-1]:uf_mki(0);{if(cx->csp==0){pushc(cx,_rv1566);return;}cx->csp--;const void*_r1567=cx->cs[cx->csp];cx->sp=cx->rsps[cx->csp];pushc(cx,_rv1566);if(!_r1567)return;goto *_r1567;}
L_3140: return;
}
int main(int argc,char**argv){uf_argc=argc;uf_argv=(void*)argv;uf_init_reflection();uf_init_locals();uf_init_lits(uf_lits,322);uf_gc_setroots(uf_vroots,74);uf_sb_on=0;
uf_sb_policy="none";
uf_device="cpu";
{int _i;for(_i=0;_i<8;_i++)uf_sb_caps[_i]=1;}
uf_sb_caps[0]=1;
uf_sb_caps[1]=1;
uf_sb_caps[2]=1;
uf_sb_caps[3]=1;
uf_sb_caps[4]=1;
uf_sb_caps[5]=1;
uf_sb_caps[6]=1;
uf_sb_caps[7]=1;
uf_ws_nroots=0;
uf_mod_allow_n=-1;
uf_mod_deny_n=0;
uf_gc_init();uflux_run(main_cx,0);return 0;}
