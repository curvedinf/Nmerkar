#!/usr/bin/env python3
import os,sys,json,subprocess,time,platform
from pathlib import Path
PROJ=Path("/home/chase/Projects/uflux")  # TODO: update to the nmerkar repo path after the move
BENCH=PROJ/"bench"
DATA=BENCH/"data"
RESULTS=BENCH/"results"
NK=PROJ/"comp"/"target"/"release"/"nk"
RESULTS.mkdir(exist_ok=True)
from transformers import AutoTokenizer
tok=AutoTokenizer.from_pretrained("Qwen/Qwen3-0.6B")
def count_tokens(path):
    with open(path,"r",encoding="utf-8",errors="replace") as f:
        text=f.read()
    return len(tok.encode(text)),len(text)
def count_lines(path):
    with open(path,"rb") as f:
        return sum(1 for _ in f)
def run_cmd(cmd,cwd=PROJ,timeout=120):
    t0=time.perf_counter()
    try:
        r=subprocess.run(cmd,cwd=cwd,capture_output=True,text=True,timeout=timeout)
        elapsed=time.perf_counter()-t0
        return r.returncode,elapsed,r.stdout.strip(),r.stderr.strip()
    except subprocess.TimeoutExpired:
        return -1,timeout,"","TIMEOUT"
    except Exception as e:
        return -2,time.perf_counter()-t0,"",str(e)
def compile_all():
    print("=== Compiling ===")
    results={}
    le_cpp_src=BENCH/"src"/"logextract"/"logextract.cpp"
    le_cpp_bin=BENCH/"src"/"logextract"/"logextract_cpp"
    rc,t,out,err=run_cmd(["g++","-std=c++17","-O2","-o",str(le_cpp_bin),str(le_cpp_src)])
    results["cpp_logextract"]=(rc==0,t)
    print(f"  C++ logextract: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    an_cpp_src=BENCH/"src"/"analytics"/"analytics.cpp"
    an_cpp_bin=BENCH/"src"/"analytics"/"analytics_cpp"
    rc,t,out,err=run_cmd(["g++","-std=c++17","-O2","-o",str(an_cpp_bin),str(an_cpp_src)])
    results["cpp_analytics"]=(rc==0,t)
    print(f"  C++ analytics: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    le_rs_src=BENCH/"src"/"logextract"/"logextract.rs"
    le_rs_bin=BENCH/"src"/"logextract"/"logextract_rs"
    rc,t,out,err=run_cmd(["rustc","-O","-o",str(le_rs_bin),str(le_rs_src)])
    results["rs_logextract"]=(rc==0,t)
    print(f"  Rust logextract: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    an_rs_src=BENCH/"src"/"analytics"/"analytics.rs"
    an_rs_bin=BENCH/"src"/"analytics"/"analytics_rs"
    rc,t,out,err=run_cmd(["rustc","-O","-o",str(an_rs_bin),str(an_rs_src)])
    results["rs_analytics"]=(rc==0,t)
    print(f"  Rust analytics: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    le_nk_src=BENCH/"src"/"logextract"/"logextract.n"
    le_nk_bin=BENCH/"src"/"logextract"/"logextract_nk"
    rc,t,out,err=run_cmd([str(NK),"-c",str(le_nk_src),"-o",str(le_nk_bin)])
    results["uf_logextract"]=(rc==0,t)
    print(f"  uFlux logextract: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    an_nk_src=BENCH/"src"/"analytics"/"analytics.n"
    an_nk_bin=BENCH/"src"/"analytics"/"analytics_nk"
    rc,t,out,err=run_cmd([str(NK),"-c",str(an_nk_src),"-o",str(an_nk_bin)])
    results["uf_analytics"]=(rc==0,t)
    print(f"  uFlux analytics: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    mb_nk_src=BENCH/"src"/"mandelbrot"/"mandelbrot.n"
    mb_nk_bin=BENCH/"src"/"mandelbrot"/"mandelbrot_nk"
    rc,t,out,err=run_cmd([str(NK),"-c",str(mb_nk_src),"-o",str(mb_nk_bin)])
    results["uf_mandelbrot"]=(rc==0,t)
    print(f"  uFlux mandelbrot: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    mb_cpp_src=BENCH/"src"/"mandelbrot"/"mandelbrot.cpp"
    mb_cpp_bin=BENCH/"src"/"mandelbrot"/"mandelbrot_cpp"
    rc,t,out,err=run_cmd(["g++","-std=c++17","-O2","-o",str(mb_cpp_bin),str(mb_cpp_src)])
    print(f"  C++ mandelbrot: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    mb_rs_src=BENCH/"src"/"mandelbrot"/"mandelbrot.rs"
    mb_rs_bin=BENCH/"src"/"mandelbrot"/"mandelbrot_rs"
    rc,t,out,err=run_cmd(["rustc","-O","-o",str(mb_rs_bin),str(mb_rs_src)])
    print(f"  Rust mandelbrot: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    sn_nk_src=BENCH/"src"/"spectralnorm"/"spectralnorm.n"
    sn_nk_bin=BENCH/"src"/"spectralnorm"/"spectralnorm_nk"
    rc,t,out,err=run_cmd([str(NK),"-c",str(sn_nk_src),"-o",str(sn_nk_bin)])
    results["uf_spectralnorm"]=(rc==0,t)
    print(f"  uFlux spectralnorm: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    sn_cpp_src=BENCH/"src"/"spectralnorm"/"spectralnorm.cpp"
    sn_cpp_bin=BENCH/"src"/"spectralnorm"/"spectralnorm_cpp"
    rc,t,out,err=run_cmd(["g++","-std=c++17","-O2","-o",str(sn_cpp_bin),str(sn_cpp_src)])
    print(f"  C++ spectralnorm: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    # ---- pattern-diverse benchmarks ----
    for bn in ("nqueens","bfs","dynamicgraph"):
        d=BENCH/"src"/bn
        rc,t,out,err=run_cmd(["g++","-std=c++17","-O2","-o",str(d/f"{bn}_cpp"),str(d/f"{bn}.cpp")])
        results[f"cpp_{bn}"]=(rc==0,t)
        print(f"  C++ {bn}: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
        rc,t,out,err=run_cmd(["rustc","-O","-o",str(d/f"{bn}_rs"),str(d/f"{bn}.rs")])
        results[f"rs_{bn}"]=(rc==0,t)
        print(f"  Rust {bn}: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    # ---- v13.1 GPU-oriented benchmarks (matmul, blackscholes) ----
    for bn in ("matmul","blackscholes"):
        d=BENCH/"src"/bn
        rc,t,out,err=run_cmd(["g++","-std=c++17","-O2","-o",str(d/f"{bn}_cpp"),str(d/f"{bn}.cpp")])
        results[f"cpp_{bn}"]=(rc==0,t)
        print(f"  C++ {bn}: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
        rc,t,out,err=run_cmd(["rustc","-O","-o",str(d/f"{bn}_rs"),str(d/f"{bn}.rs")])
        results[f"rs_{bn}"]=(rc==0,t)
        print(f"  Rust {bn}: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    # shared Vulkan launcher artifacts (Python ctypes lib + Node addon; the
    # C++/Rust GPU binaries stay disabled pending the launcher fix — see SPEC)
    gpu=BENCH/"src"/"_gpu"
    for sh in (gpu/"matmul.spv",gpu/"bs.spv"):
        spv=sh.with_suffix(".comp")
        rc,t,out,err=run_cmd(["glslc","-O","--target-env=vulkan1.2","-o",str(sh),str(spv)])
        results[f"spv_{sh.stem}"]=(rc==0,t)
        print(f"  shader {sh.stem}: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    rc,t,out,err=run_cmd(["cc","-O2","-shared","-fPIC",str(gpu/"gpucomp.c"),"-o",str(gpu/"libgpucomp.so"),"-lvulkan"])
    results["gpucomp_so"]=(rc==0,t)
    print(f"  gpucomp.so: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    node_inc="/usr/include/node"
    if (Path(node_inc)/"node_api.h").exists():
        rc,t,out,err=run_cmd(["cc","-O2","-shared","-fPIC",f"-I{node_inc}",str(gpu/"gpucomp_node.c"),str(gpu/"gpucomp.c"),"-o",str(gpu/"gpucomp_node.node"),"-lvulkan"])
        results["gpucomp_node"]=(rc==0,t)
        print(f"  gpucomp.node: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    sn_rs_src=BENCH/"src"/"spectralnorm"/"spectralnorm.rs"
    sn_rs_bin=BENCH/"src"/"spectralnorm"/"spectralnorm_rs"
    rc,t,out,err=run_cmd(["rustc","-O","-o",str(sn_rs_bin),str(sn_rs_src)])
    print(f"  Rust spectralnorm: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    return results
def run_benchmark(name,cmd,cwd=PROJ,timeout=120,env=False):
    print(f"  Running {name}...",end="",flush=True)
    if env:
        cmd=["env"]+cmd
    rc,t,out,err=run_cmd(cmd,cwd=cwd,timeout=timeout)
    status="OK" if rc==0 else("TIMEOUT" if rc==-1 else f"FAIL({rc})")
    print(f" {status} {t:.2f}s")
    return{"name":name,"exit_code":rc,"time_sec":round(t,3),"status":status,
           "stdout":out[:500]if out else"","stderr":err[:200]if err else""}
def main():
    print("=== Token Counting (Qwen3-0.6B tokenizer) ===\n")
    # Generate dense .nd files from .n for Nmerkar token counting
    print("=== Generating dense .nd files ===\n")
    bench_names=["logextract","analytics","mandelbrot","spectralnorm","matmul","blackscholes","nqueens","bfs","dynamicgraph"]
    uf_paths={}
    for bn in bench_names:
        uft=BENCH/"src"/bn/f"{bn}.n"
        uf_out=BENCH/"src"/bn/f"{bn}.nd"
        rc,t,out,err=run_cmd([str(NK),"--to-dense",str(uft)])
        if rc==0 and uf_out.exists():
            uf_paths[bn]=uf_out
            print(f"  {bn:14s}: generated ({t:.1f}s)")
        else:
            print(f"  {bn:14s}: FAILED ({err})")
    print()

    sources={
        "Python":{
            "logextract":BENCH/"src"/"logextract"/"logextract.py",
            "analytics":BENCH/"src"/"analytics"/"analytics.py",
            "mandelbrot":BENCH/"src"/"mandelbrot"/"mandelbrot.py",
            "spectralnorm":BENCH/"src"/"spectralnorm"/"spectralnorm.py",
            "nqueens":BENCH/"src"/"nqueens"/"nqueens.py",
            "bfs":BENCH/"src"/"bfs"/"bfs.py",
            "dynamicgraph":BENCH/"src"/"dynamicgraph"/"dynamicgraph.py",
        },
        "Node.js":{
            "logextract":BENCH/"src"/"logextract"/"logextract.js",
            "analytics":BENCH/"src"/"analytics"/"analytics.js",
            "mandelbrot":BENCH/"src"/"mandelbrot"/"mandelbrot.js",
            "spectralnorm":BENCH/"src"/"spectralnorm"/"spectralnorm.js",
            "nqueens":BENCH/"src"/"nqueens"/"nqueens.js",
            "bfs":BENCH/"src"/"bfs"/"bfs.js",
            "dynamicgraph":BENCH/"src"/"dynamicgraph"/"dynamicgraph.js",
        },
        "C++":{
            "logextract":BENCH/"src"/"logextract"/"logextract.cpp",
            "analytics":BENCH/"src"/"analytics"/"analytics.cpp",
            "mandelbrot":BENCH/"src"/"mandelbrot"/"mandelbrot.cpp",
            "spectralnorm":BENCH/"src"/"spectralnorm"/"spectralnorm.cpp",
            "nqueens":BENCH/"src"/"nqueens"/"nqueens.cpp",
            "bfs":BENCH/"src"/"bfs"/"bfs.cpp",
            "dynamicgraph":BENCH/"src"/"dynamicgraph"/"dynamicgraph.cpp",
        },
        "Rust":{
            "logextract":BENCH/"src"/"logextract"/"logextract.rs",
            "analytics":BENCH/"src"/"analytics"/"analytics.rs",
            "mandelbrot":BENCH/"src"/"mandelbrot"/"mandelbrot.rs",
            "spectralnorm":BENCH/"src"/"spectralnorm"/"spectralnorm.rs",
            "nqueens":BENCH/"src"/"nqueens"/"nqueens.rs",
            "bfs":BENCH/"src"/"bfs"/"bfs.rs",
            "dynamicgraph":BENCH/"src"/"dynamicgraph"/"dynamicgraph.rs",
        },
        "Nmerkar":{bn:uf_paths[bn] for bn in bench_names if bn in uf_paths},
    }
    for lang in ("Python","Node.js","C++","Rust"):
        sources[lang]["matmul"]=BENCH/"src"/"matmul"/("matmul."+{"Python":"py","Node.js":"js","C++":"cpp","Rust":"rs"}[lang])
        sources[lang]["blackscholes"]=BENCH/"src"/"blackscholes"/("blackscholes."+{"Python":"py","Node.js":"js","C++":"cpp","Rust":"rs"}[lang])
    print("=== Token Counting (Qwen3-0.6B tokenizer) ===\n")
    token_data={}
    for lang,files in sources.items():
        token_data[lang]={}
        for bench_name,path in files.items():
            tc,chars=count_tokens(path)
            lines=count_lines(path)
            token_data[lang][bench_name]={"tokens":tc,"chars":chars,"lines":lines,"path":str(path)}
            print(f"  {lang:8s} {bench_name:12s}: {tc:5d} tokens  {chars:6d} chars  {lines:3d} lines")
    compile_all()
    log_path=str(DATA/"access.log")
    csv_path=str(DATA/"sales.csv")
    print("\n=== Performance: Log Extraction (500MB access.log) ===\n")
    le_results=[]
    le_bin=BENCH/"src"/"logextract"/"logextract_cpp"
    le_results.append(run_benchmark("C++",[str(le_bin),log_path]))
    le_bin=BENCH/"src"/"logextract"/"logextract_rs"
    le_results.append(run_benchmark("Rust",[str(le_bin),log_path]))
    le_results.append(run_benchmark("Python",["python3",str(BENCH/"src"/"logextract"/"logextract.py"),log_path]))
    le_results.append(run_benchmark("Node.js",["node",str(BENCH/"src"/"logextract"/"logextract.js"),log_path]))
    le_nk_src=BENCH/"src"/"logextract"/"logextract.n"
    rc,t,out,err=run_cmd([str(NK),"--device","cpu","--gc-threshold","1000000000","-c",str(le_nk_src),"-o",str(BENCH/"src"/"logextract"/"logextract_nk")])
    le_results.append(run_benchmark("Nmerkar",[str(BENCH/"src"/"logextract"/"logextract_nk"),log_path],timeout=60))
    le_gpu=run_benchmark("Nmerkar(GPU-on)",[str(NK),"--gc-threshold","1000000000",str(le_nk_src),"--",log_path],timeout=60)
    le_results.append(le_gpu)
    print("\n=== Performance: Data Analytics (500MB sales.csv) ===\n")
    an_results=[]
    an_bin=BENCH/"src"/"analytics"/"analytics_cpp"
    an_results.append(run_benchmark("C++",[str(an_bin),csv_path]))
    an_bin=BENCH/"src"/"analytics"/"analytics_rs"
    an_results.append(run_benchmark("Rust",[str(an_bin),csv_path]))
    an_results.append(run_benchmark("Python",["python3",str(BENCH/"src"/"analytics"/"analytics.py"),csv_path]))
    an_results.append(run_benchmark("Node.js",["node",str(BENCH/"src"/"analytics"/"analytics.js"),csv_path]))
    an_nk_src=BENCH/"src"/"analytics"/"analytics.n"
    an_results.append(run_benchmark("Nmerkar",[str(NK),"--device","cpu","--gc-threshold","1000000000",str(an_nk_src),"--",csv_path],timeout=60))
    an_results.append(run_benchmark("Nmerkar(GPU-on)",[str(NK),"--gc-threshold","1000000000",str(an_nk_src),"--",csv_path],timeout=60))

    # Compute-only benchmarks (no data files; n passed as argv[1])
    N_MANDEL=1000
    N_SPECTRAL=5500
    print(f"\n=== Performance: Mandelbrot (n={N_MANDEL}) ===\n")
    mb_results=[]
    mb_cpp_bin=BENCH/"src"/"mandelbrot"/"mandelbrot_cpp"
    mb_results.append(run_benchmark("C++",[str(mb_cpp_bin),str(N_MANDEL)]))
    mb_rs_bin=BENCH/"src"/"mandelbrot"/"mandelbrot_rs"
    mb_results.append(run_benchmark("Rust",[str(mb_rs_bin),str(N_MANDEL)]))
    mb_results.append(run_benchmark("Python",["python3",str(BENCH/"src"/"mandelbrot"/"mandelbrot.py"),str(N_MANDEL)],timeout=120))
    mb_results.append(run_benchmark("Node.js",["node",str(BENCH/"src"/"mandelbrot"/"mandelbrot.js"),str(N_MANDEL)],timeout=120))
    mb_nk_src=BENCH/"src"/"mandelbrot"/"mandelbrot.n"
    mb_results.append(run_benchmark("Nmerkar",[str(NK),"--device","cpu","--gc-threshold","1000000000",str(mb_nk_src),"--",str(N_MANDEL)],timeout=120))
    mb_results.append(run_benchmark("Nmerkar(GPU-on)",[str(NK),"--gc-threshold","1000000000",str(mb_nk_src),"--",str(N_MANDEL)],timeout=120))

    print(f"\n=== Performance: Spectral Norm (n={N_SPECTRAL}) ===\n")
    sn_results=[]
    sn_cpp_bin=BENCH/"src"/"spectralnorm"/"spectralnorm_cpp"
    sn_results.append(run_benchmark("C++",[str(sn_cpp_bin),str(N_SPECTRAL)]))
    sn_rs_bin=BENCH/"src"/"spectralnorm"/"spectralnorm_rs"
    sn_results.append(run_benchmark("Rust",[str(sn_rs_bin),str(N_SPECTRAL)]))
    sn_results.append(run_benchmark("Python",["python3",str(BENCH/"src"/"spectralnorm"/"spectralnorm.py"),str(N_SPECTRAL)],timeout=300))
    sn_results.append(run_benchmark("Node.js",["node",str(BENCH/"src"/"spectralnorm"/"spectralnorm.js"),str(N_SPECTRAL)],timeout=300))
    sn_nk_src=BENCH/"src"/"spectralnorm"/"spectralnorm.n"
    sn_results.append(run_benchmark("Nmerkar",[str(NK),"--device","cpu","--gc-threshold","1000000000",str(sn_nk_src),"--",str(N_SPECTRAL)],timeout=300))
    sn_results.append(run_benchmark("Nmerkar(GPU-on)",[str(NK),"--gc-threshold","1000000000",str(sn_nk_src),"--",str(N_SPECTRAL)],timeout=300))

    # ---- pattern-diverse benchmarks (v13.2) ----
    N_QUEENS=11
    print(f"\n=== Performance: N-Queens N={N_QUEENS} (iterative backtracking) ===\n")
    nq=BENCH/"src"/"nqueens"
    nq_results=[]
    nq_results.append(run_benchmark("C++",[str(nq/"nqueens_cpp"),str(N_QUEENS)]))
    nq_results.append(run_benchmark("Rust",[str(nq/"nqueens_rs"),str(N_QUEENS)]))
    nq_results.append(run_benchmark("Python",["python3",str(nq/"nqueens.py"),str(N_QUEENS)],timeout=120))
    nq_results.append(run_benchmark("Node.js",["node",str(nq/"nqueens.js"),str(N_QUEENS)]))
    nq_results.append(run_benchmark("Nmerkar",[str(NK),"--device","cpu","--gc-threshold","1000000000",str(nq/"nqueens.n"),"--",str(N_QUEENS)],timeout=120))
    nq_results.append(run_benchmark("Nmerkar(GPU-on)",[str(NK),"--gc-threshold","1000000000",str(nq/"nqueens.n"),"--",str(N_QUEENS)],timeout=120))
    N_BFS=1000000
    print(f"\n=== Performance: BFS n={N_BFS} (packed CSR) ===\n")
    bf=BENCH/"src"/"bfs"
    bf_nk=bf/"bfs_nk"
    bf_nk_auto=bf/"bfs_nk_auto"
    run_cmd([str(NK),"--device","cpu","--gc-threshold","2000000000","-c",str(bf/"bfs.n"),"-o",str(bf_nk)])
    run_cmd([str(NK),"--device","auto","--gc-threshold","2000000000","-c",str(bf/"bfs.n"),"-o",str(bf_nk_auto)])
    bf_results=[]
    bf_results.append(run_benchmark("C++",[str(bf/"bfs_cpp"),str(N_BFS)],timeout=120))
    bf_results.append(run_benchmark("Rust",[str(bf/"bfs_rs"),str(N_BFS)],timeout=120))
    bf_results.append(run_benchmark("Python",["python3",str(bf/"bfs.py"),str(N_BFS)],timeout=120))
    bf_results.append(run_benchmark("Node.js",["node",str(bf/"bfs.js"),str(N_BFS)],timeout=120))
    bf_results.append(run_benchmark("Nmerkar",[str(bf_nk),str(N_BFS)],timeout=240))
    bf_results.append(run_benchmark("Nmerkar(GPU-on)",[str(bf_nk_auto),str(N_BFS)],timeout=240))

    N_DYNAMICGRAPH=1000000
    print(f"\n=== Performance: Dynamic Graph n={N_DYNAMICGRAPH} (hash map + lists) ===\n")
    dg=BENCH/"src"/"dynamicgraph"
    dg_nk=dg/"dynamicgraph_nk"
    dg_nk_auto=dg/"dynamicgraph_nk_auto"
    run_cmd([str(NK),"--device","cpu","--gc-threshold","2000000000","-c",str(dg/"dynamicgraph.n"),"-o",str(dg_nk)])
    run_cmd([str(NK),"--device","auto","--gc-threshold","2000000000","-c",str(dg/"dynamicgraph.n"),"-o",str(dg_nk_auto)])
    dg_results=[]
    dg_results.append(run_benchmark("C++",[str(dg/"dynamicgraph_cpp"),str(N_DYNAMICGRAPH)],timeout=120))
    dg_results.append(run_benchmark("Rust",[str(dg/"dynamicgraph_rs"),str(N_DYNAMICGRAPH)],timeout=120))
    dg_results.append(run_benchmark("Python",["python3",str(dg/"dynamicgraph.py"),str(N_DYNAMICGRAPH)],timeout=120))
    dg_results.append(run_benchmark("Node.js",["node",str(dg/"dynamicgraph.js"),str(N_DYNAMICGRAPH)],timeout=120))
    dg_results.append(run_benchmark("Nmerkar",[str(dg_nk),str(N_DYNAMICGRAPH)],timeout=240))
    dg_results.append(run_benchmark("Nmerkar(GPU-on)",[str(dg_nk_auto),str(N_DYNAMICGRAPH)],timeout=240))

    # ---- GPU-oriented benchmarks (v13.1): CPU column + GPU-on column ----
    # Nmerkar GPU-on = default auto device (Vulkan). The non-Nmerkar GPU variants
    # use the shared launcher in src/_gpu (see bench/SPEC.md; when its binaries
    # are absent their GPU-on entries are skipped).
    N_MATMUL=512
    N_BS=2000000
    print(f"\n=== Performance: Matmul N={N_MATMUL} (double, GPU-oriented) ===\n")
    mm_results=[]
    mm=BENCH/"src"/"matmul"
    mm_results.append(run_benchmark("C++",[str(mm/"matmul_cpp"),str(N_MATMUL)]))
    mm_results.append(run_benchmark("Rust",[str(mm/"matmul_rs"),str(N_MATMUL)]))
    mm_results.append(run_benchmark("Python",["python3",str(mm/"matmul.py"),str(N_MATMUL)]))
    mm_results.append(run_benchmark("Node.js",["node",str(mm/"matmul.js"),str(N_MATMUL)]))
    mm_results.append(run_benchmark("Nmerkar",[str(NK),"--device","cpu","--gc-threshold","500000000",str(mm/"matmul.n"),"--",str(N_MATMUL)],timeout=120))
    mm_results.append(run_benchmark("Nmerkar(GPU-on)",[str(NK),"--gc-threshold","500000000",str(mm/"matmul.n"),"--",str(N_MATMUL)],timeout=120))
    if (mm/"matmul_cpp_gpu").exists():
        mm_results.append(run_benchmark("C++(GPU-on)",[f"UF_SPV_DIR={BENCH/'src'/'_gpu'}",str(mm/"matmul_cpp_gpu"),str(N_MATMUL)],timeout=60,env=True))

    print(f"\n=== Performance: Black-Scholes N={N_BS} (double, GPU-oriented) ===\n")
    bs_results=[]
    bs=BENCH/"src"/"blackscholes"
    bs_results.append(run_benchmark("C++",[str(bs/"blackscholes_cpp"),str(N_BS)]))
    bs_results.append(run_benchmark("Rust",[str(bs/"blackscholes_rs"),str(N_BS)]))
    bs_results.append(run_benchmark("Python",["python3",str(bs/"blackscholes.py"),str(N_BS)],timeout=120))
    bs_results.append(run_benchmark("Node.js",["node",str(bs/"blackscholes.js"),str(N_BS)],timeout=120))
    bs_results.append(run_benchmark("Nmerkar",[str(NK),"--device","cpu","--gc-threshold","2000000000",str(bs/"blackscholes.n"),"--",str(N_BS)],timeout=240))
    bs_results.append(run_benchmark("Nmerkar(GPU-on)",[str(NK),"--gc-threshold","2000000000",str(bs/"blackscholes.n"),"--",str(N_BS)],timeout=240))
    if (bs/"blackscholes_cpp_gpu").exists():
        bs_results.append(run_benchmark("C++(GPU-on)",[f"UF_SPV_DIR={BENCH/'src'/'_gpu'}",str(bs/"blackscholes_cpp_gpu"),str(N_BS)],timeout=60,env=True))

    report={
        "date":time.strftime("%Y-%m-%d %H:%M:%S"),
        "data":{
            "access_log":{"size_mb":round(os.path.getsize(log_path)/1048576,1),"lines":count_lines(log_path)},
            "sales_csv":{"size_mb":round(os.path.getsize(csv_path)/1048576,1),"lines":count_lines(csv_path)},
        },
        "tokenizer":"Qwen/Qwen3-0.6B (vocab=151643)",
        "tokens":token_data,
        "performance":{"logextract":le_results,"analytics":an_results,
                        "mandelbrot":mb_results,"spectralnorm":sn_results,
                        "matmul":mm_results,"blackscholes":bs_results,"nqueens":nq_results,
                        "bfs":bf_results,"dynamicgraph":dg_results},
    }
    with open(RESULTS/"benchmark.json","w") as f:
        json.dump(report,f,indent=2)
    print(f"\nResults saved to {RESULTS/'benchmark.json'}")
    print("\n=== SUMMARY ===\n")
    perf_mb={r["name"]:r for r in mb_results}
    perf_sn={r["name"]:r for r in sn_results}
    print(f"{'Language':<10} {'LogExtract':>11} {'Analytics':>11} {'Mandelbrot':>11} {'SpectralNorm':>13} {'Matmul':>11} {'BlackSch':>11} {'NQueens':>11} {'BFS':>11} {'DynGraph':>11}")
    print("-"*118)
    perf_le={r["name"]:r for r in le_results}
    perf_nq={r["name"]:r for r in nq_results}
    perf_bf={r["name"]:r for r in bf_results}
    perf_dg={r["name"]:r for r in dg_results}
    perf_an={r["name"]:r for r in an_results}
    perf_mm={r["name"]:r for r in mm_results}
    perf_bs={r["name"]:r for r in bs_results}
    def fmt(d,k):
        t=d.get(k,{}).get("time_sec","—")
        return f"{t:.3f}s" if isinstance(t,float) else str(t)
    for lang in["Nmerkar","Rust","C++","Python","Node.js"]:
        print(f"{lang:<10} {fmt(perf_le,lang):>11} {fmt(perf_an,lang):>11} {fmt(perf_mb,lang):>11} {fmt(perf_sn,lang):>13} {fmt(perf_mm,lang):>11} {fmt(perf_bs,lang):>11} {fmt(perf_nq,lang):>11} {fmt(perf_bf,lang):>11} {fmt(perf_dg,lang):>11}")
    print()
    print(f"{'GPU-on':<10} {'LogExtract':>11} {'Analytics':>11} {'Mandelbrot':>11} {'SpectralNorm':>13} {'Matmul':>11} {'BlackSch':>11} {'NQueens':>11} {'BFS':>11} {'DynGraph':>11}")
    print("-"*118)
    print(f"{'Nmerkar':<10} {fmt(perf_le,'Nmerkar(GPU-on)'):>11} {fmt(perf_an,'Nmerkar(GPU-on)'):>11} {fmt(perf_mb,'Nmerkar(GPU-on)'):>11} {fmt(perf_sn,'Nmerkar(GPU-on)'):>13} {fmt(perf_mm,'Nmerkar(GPU-on)'):>11} {fmt(perf_bs,'Nmerkar(GPU-on)'):>11} {fmt(perf_nq,'Nmerkar(GPU-on)'):>11} {fmt(perf_bf,'Nmerkar(GPU-on)'):>11} {fmt(perf_dg,'Nmerkar(GPU-on)'):>11}")
    print(f"{'C++':<10} {'—':>11} {'—':>11} {'—':>11} {'—':>13} {fmt(perf_mm,'C++(GPU-on)'):>11} {fmt(perf_bs,'C++(GPU-on)'):>11}")
    print(f"\n{'Token counts':<10} {'LogExtract':>12} {'Analytics':>12} {'Mandelbrot':>12} {'SpectralNorm':>14} {'Matmul':>12} {'BlackSch':>12} {'NQueens':>12} {'BFS':>12} {'DynGraph':>12}")
    print("-"*128)
    for lang in["Nmerkar","Rust","C++","Python","Node.js"]:
        vals=[]
        for bn in["logextract","analytics","mandelbrot","spectralnorm","matmul","blackscholes","nqueens","bfs","dynamicgraph"]:
            t=token_data.get(lang,{}).get(bn,{}).get("tokens")
            vals.append(f"{t:>12}" if t else f"{'—':>12}")
        print(f"{lang:<10} {vals[0]} {vals[1]} {vals[2]} {vals[3]:>14} {vals[4]} {vals[5]} {vals[6]} {vals[7]} {vals[8]}")
if __name__=="__main__":
    main()
