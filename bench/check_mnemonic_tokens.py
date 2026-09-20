#!/usr/bin/env python3
"""Check which Nmerkar text op mnemonics are single Qwen3-0.6B tokens, in both
the trailing-space form ('lit ') and the leading-space form (' lit') that the
GPT-2-style BPE actually uses to represent a word mid-document.

The leading-space form ('Ġlit') is the token that actually appears when the
benchmark tokenizes a .n source, because in this BPE scheme a space attaches
to the FOLLOWING token as a leading marker."""
from transformers import AutoTokenizer

tok = AutoTokenizer.from_pretrained("Qwen/Qwen3-0.6B")

MNEMONICS = [
    "lit","dup","ovr","drop","swp","pick","add","sub","mul","and","shr","inc","dec",
    "for","call","ret","obj","get","set","arr","clone","cast","macro","tensor",
    "setv","getv","str","cat","fmt","buf","bufcopy","addr","loadx","storex",
    "sizeof","offset","struct","malloc","free","sys","gc","import","export",
    "extern","print","scan","dict","list","push","pop","chan","enq","deq","close",
    "atom","aget","aset","aadd","cas","typeof","len","use","mod","pub","weave",
    "task","endt","wrun","sh","shp","exec","match","replace","rsplit","glob",
    "split","join","slice","find","repl","trim","up","down","starts","ends",
    "div","rem","eq","lt","gt","not","or","xor","shl","bnot",
    "if","ifelse","while","break","cont","getq","has","orelse","keys","range",
    "sort","filter","some","every","vadd","vsub","vmul","vdiv","veadd","vesub",
    "vemul","vediv","vemax","vemin","veq","vlt","vgt","vge","vle","vand","vor",
    "vnot","vcount","vgather","vsum","vmean","vmin","vmax","del","vmap","vfold",
    "now","time","timef","bloom","badd","btest","slurp","spit","argv",
    "group","agg","unique","flat","chunk","vargsort","vsearchsorted","vwhere",
    "mmap","feach","ffold","fsplit","fget","fatoi","fatof","fsget","fbyte",
    "vget","vset","addto","faddto","finc","fmatch","bfs","dfs","wfind",
    "json","unjson","iter","next","collect","imap","ifilter","femit",
    "try","retry","spawn","atoi","atof","itoa","ftoa","entry",
]
assert len(MNEMONICS) == len(set(MNEMONICS))

def ntok(s):
    return tok.encode(s, add_special_tokens=False)

print(f"{len(MNEMONICS)} mnemonics, vocab size {tok.vocab_size}\n")

# --- Trailing-space form: mnemonic + " " ---
print("=== Form 1: mnemonic + trailing space ('lit ') ===")
trail_fail = [m for m in MNEMONICS if len(ntok(m + " ")) != 1]
print(f"single-token: {len(MNEMONICS)-len(trail_fail)}, not-single: {len(trail_fail)}")
print("(GPT-2-style BPE: the space is always a separate id 220 trailing the word,")
print(" so the trailing-space form can essentially never be a single token.)\n")

# --- Leading-space form: " " + mnemonic ---  (how it appears mid-document)
print("=== Form 2: leading space + mnemonic (' lit')  [how it appears in a .n] ===")
lead_fail = []
for m in MNEMONICS:
    ids = ntok(" " + m)
    pieces = [tok.decode([i]) for i in ids]
    if len(ids) != 1:
        lead_fail.append((m, ids, pieces))

print(f"single-token: {len(MNEMONICS)-len(lead_fail)}, not-single: {len(lead_fail)}\n")
if lead_fail:
    w = max(len(m) for m, _, _ in lead_fail)
    for m, ids, pieces in lead_fail:
        show = " ".join(repr(p) for p in pieces)
        print(f"  {m:<{w}}  {len(ids)} tok  [{','.join(str(i) for i in ids)}]  {show}")
