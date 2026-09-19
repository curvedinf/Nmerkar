#!/usr/bin/env python3
import os,sys,json,subprocess,time,platform
from pathlib import Path
PROJ=Path("/home/chase/Projects/uflux")
BENCH=PROJ/"bench"
DATA=BENCH/"data"
RESULTS=BENCH/"results"
UF=PROJ/"comp"/"target"/"release"/"uf"
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
    le_uf_src=BENCH/"src"/"logextract"/"logextract.uft"
    le_uf_bin=BENCH/"src"/"logextract"/"logextract_uf"
    rc,t,out,err=run_cmd([str(UF),"-c",str(le_uf_src),"-o",str(le_uf_bin)])
    results["uf_logextract"]=(rc==0,t)
    print(f"  uFlux logextract: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    an_uf_src=BENCH/"src"/"analytics"/"analytics.uft"
    an_uf_bin=BENCH/"src"/"analytics"/"analytics_uf"
    rc,t,out,err=run_cmd([str(UF),"-c",str(an_uf_src),"-o",str(an_uf_bin)])
    results["uf_analytics"]=(rc==0,t)
    print(f"  uFlux analytics: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    mb_uf_src=BENCH/"src"/"mandelbrot"/"mandelbrot.uft"
    mb_uf_bin=BENCH/"src"/"mandelbrot"/"mandelbrot_uf"
    rc,t,out,err=run_cmd([str(UF),"-c",str(mb_uf_src),"-o",str(mb_uf_bin)])
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
    sn_uf_src=BENCH/"src"/"spectralnorm"/"spectralnorm.uft"
    sn_uf_bin=BENCH/"src"/"spectralnorm"/"spectralnorm_uf"
    rc,t,out,err=run_cmd([str(UF),"-c",str(sn_uf_src),"-o",str(sn_uf_bin)])
    results["uf_spectralnorm"]=(rc==0,t)
    print(f"  uFlux spectralnorm: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
    sn_cpp_src=BENCH/"src"/"spectralnorm"/"spectralnorm.cpp"
    sn_cpp_bin=BENCH/"src"/"spectralnorm"/"spectralnorm_cpp"
    rc,t,out,err=run_cmd(["g++","-std=c++17","-O2","-o",str(sn_cpp_bin),str(sn_cpp_src)])
    print(f"  C++ spectralnorm: {'OK' if rc==0 else 'FAIL'} ({t:.1f}s)")
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
    # Generate dense .uf files from .uft for µFlux token counting
    print("=== Generating dense .uf files ===\n")
    bench_names=["logextract","analytics","mandelbrot","spectralnorm","matmul","blackscholes"]
    uf_paths={}
    for bn in bench_names:
        uft=BENCH/"src"/bn/f"{bn}.uft"
        uf_out=BENCH/"src"/bn/f"{bn}.uf"
        rc,t,out,err=run_cmd([str(UF),"--to-dense",str(uft)])
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
        },
        "Node.js":{
            "logextract":BENCH/"src"/"logextract"/"logextract.js",
            "analytics":BENCH/"src"/"analytics"/"analytics.js",
            "mandelbrot":BENCH/"src"/"mandelbrot"/"mandelbrot.js",
            "spectralnorm":BENCH/"src"/"spectralnorm"/"spectralnorm.js",
        },
        "C++":{
            "logextract":BENCH/"src"/"logextract"/"logextract.cpp",
            "analytics":BENCH/"src"/"analytics"/"analytics.cpp",
            "mandelbrot":BENCH/"src"/"mandelbrot"/"mandelbrot.cpp",
            "spectralnorm":BENCH/"src"/"spectralnorm"/"spectralnorm.cpp",
        },
        "Rust":{
            "logextract":BENCH/"src"/"logextract"/"logextract.rs",
            "analytics":BENCH/"src"/"analytics"/"analytics.rs",
            "mandelbrot":BENCH/"src"/"mandelbrot"/"mandelbrot.rs",
            "spectralnorm":BENCH/"src"/"spectralnorm"/"spectralnorm.rs",
        },
        "µFlux":{bn:uf_paths[bn] for bn in bench_names if bn in uf_paths},
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
    le_uf_src=BENCH/"src"/"logextract"/"logextract.uft"
    rc,t,out,err=run_cmd([str(UF),"--device","cpu","--gc-threshold","1000000000","-c",str(le_uf_src),"-o",str(BENCH/"src"/"logextract"/"logextract_uf")])
    le_results.append(run_benchmark("µFlux",[str(BENCH/"src"/"logextract"/"logextract_uf"),log_path],timeout=60))
    le_gpu=run_benchmark("µFlux(GPU-on)",[str(UF),"--gc-threshold","1000000000",str(le_uf_src),"--",log_path],timeout=60)
    le_results.append(le_gpu)
    print("\n=== Performance: Data Analytics (500MB sales.csv) ===\n")
    an_results=[]
    an_bin=BENCH/"src"/"analytics"/"analytics_cpp"
    an_results.append(run_benchmark("C++",[str(an_bin),csv_path]))
    an_bin=BENCH/"src"/"analytics"/"analytics_rs"
    an_results.append(run_benchmark("Rust",[str(an_bin),csv_path]))
    an_results.append(run_benchmark("Python",["python3",str(BENCH/"src"/"analytics"/"analytics.py"),csv_path]))
    an_results.append(run_benchmark("Node.js",["node",str(BENCH/"src"/"analytics"/"analytics.js"),csv_path]))
    an_uf_src=BENCH/"src"/"analytics"/"analytics.uft"
    an_results.append(run_benchmark("µFlux",[str(UF),"--device","cpu","--gc-threshold","1000000000",str(an_uf_src),"--",csv_path],timeout=60))
    an_results.append(run_benchmark("µFlux(GPU-on)",[str(UF),"--gc-threshold","1000000000",str(an_uf_src),"--",csv_path],timeout=60))

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
    mb_uf_src=BENCH/"src"/"mandelbrot"/"mandelbrot.uft"
    mb_results.append(run_benchmark("µFlux",[str(UF),"--device","cpu","--gc-threshold","1000000000",str(mb_uf_src),"--",str(N_MANDEL)],timeout=120))
    mb_results.append(run_benchmark("µFlux(GPU-on)",[str(UF),"--gc-threshold","1000000000",str(mb_uf_src),"--",str(N_MANDEL)],timeout=120))

    print(f"\n=== Performance: Spectral Norm (n={N_SPECTRAL}) ===\n")
    sn_results=[]
    sn_cpp_bin=BENCH/"src"/"spectralnorm"/"spectralnorm_cpp"
    sn_results.append(run_benchmark("C++",[str(sn_cpp_bin),str(N_SPECTRAL)]))
    sn_rs_bin=BENCH/"src"/"spectralnorm"/"spectralnorm_rs"
    sn_results.append(run_benchmark("Rust",[str(sn_rs_bin),str(N_SPECTRAL)]))
    sn_results.append(run_benchmark("Python",["python3",str(BENCH/"src"/"spectralnorm"/"spectralnorm.py"),str(N_SPECTRAL)],timeout=300))
    sn_results.append(run_benchmark("Node.js",["node",str(BENCH/"src"/"spectralnorm"/"spectralnorm.js"),str(N_SPECTRAL)],timeout=300))
    sn_uf_src=BENCH/"src"/"spectralnorm"/"spectralnorm.uft"
    sn_results.append(run_benchmark("µFlux",[str(UF),"--device","cpu","--gc-threshold","1000000000",str(sn_uf_src),"--",str(N_SPECTRAL)],timeout=300))
    sn_results.append(run_benchmark("µFlux(GPU-on)",[str(UF),"--gc-threshold","1000000000",str(sn_uf_src),"--",str(N_SPECTRAL)],timeout=300))

    # ---- GPU-oriented benchmarks (v13.1): CPU column + GPU-on column ----
    # µFlux GPU-on = default auto device (Vulkan). The non-µFlux GPU variants
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
    mm_results.append(run_benchmark("µFlux",[str(UF),"--device","cpu","--gc-threshold","500000000",str(mm/"matmul.uft"),"--",str(N_MATMUL)],timeout=120))
    mm_results.append(run_benchmark("µFlux(GPU-on)",[str(UF),"--gc-threshold","500000000",str(mm/"matmul.uft"),"--",str(N_MATMUL)],timeout=120))
    if (mm/"matmul_cpp_gpu").exists():
        mm_results.append(run_benchmark("C++(GPU-on)",[f"UF_SPV_DIR={BENCH/'src'/'_gpu'}",str(mm/"matmul_cpp_gpu"),str(N_MATMUL)],timeout=60,env=True))

    print(f"\n=== Performance: Black-Scholes N={N_BS} (double, GPU-oriented) ===\n")
    bs_results=[]
    bs=BENCH/"src"/"blackscholes"
    bs_results.append(run_benchmark("C++",[str(bs/"blackscholes_cpp"),str(N_BS)]))
    bs_results.append(run_benchmark("Rust",[str(bs/"blackscholes_rs"),str(N_BS)]))
    bs_results.append(run_benchmark("Python",["python3",str(bs/"blackscholes.py"),str(N_BS)],timeout=120))
    bs_results.append(run_benchmark("Node.js",["node",str(bs/"blackscholes.js"),str(N_BS)],timeout=120))
    bs_results.append(run_benchmark("µFlux",[str(UF),"--device","cpu","--gc-threshold","2000000000",str(bs/"blackscholes.uft"),"--",str(N_BS)],timeout=240))
    bs_results.append(run_benchmark("µFlux(GPU-on)",[str(UF),"--gc-threshold","2000000000",str(bs/"blackscholes.uft"),"--",str(N_BS)],timeout=240))
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
                        "matmul":mm_results,"blackscholes":bs_results},
    }
    with open(RESULTS/"benchmark.json","w") as f:
        json.dump(report,f,indent=2)
    print(f"\nResults saved to {RESULTS/'benchmark.json'}")
    print("\n=== SUMMARY ===\n")
    perf_mb={r["name"]:r for r in mb_results}
    perf_sn={r["name"]:r for r in sn_results}
    print(f"{'Language':<10} {'LogExtract':>11} {'Analytics':>11} {'Mandelbrot':>11} {'SpectralNorm':>13} {'Matmul':>11} {'BlackSch':>11}")
    print("-"*82)
    perf_le={r["name"]:r for r in le_results}
    perf_an={r["name"]:r for r in an_results}
    perf_mm={r["name"]:r for r in mm_results}
    perf_bs={r["name"]:r for r in bs_results}
    def fmt(d,k):
        t=d.get(k,{}).get("time_sec","—")
        return f"{t:.3f}s" if isinstance(t,float) else str(t)
    for lang in["µFlux","Rust","C++","Python","Node.js"]:
        print(f"{lang:<10} {fmt(perf_le,lang):>11} {fmt(perf_an,lang):>11} {fmt(perf_mb,lang):>11} {fmt(perf_sn,lang):>13} {fmt(perf_mm,lang):>11} {fmt(perf_bs,lang):>11}")
    print()
    print(f"{'GPU-on':<10} {'LogExtract':>11} {'Analytics':>11} {'Mandelbrot':>11} {'SpectralNorm':>13} {'Matmul':>11} {'BlackSch':>11}")
    print("-"*82)
    print(f"{'µFlux':<10} {fmt(perf_le,'µFlux(GPU-on)'):>11} {fmt(perf_an,'µFlux(GPU-on)'):>11} {fmt(perf_mb,'µFlux(GPU-on)'):>11} {fmt(perf_sn,'µFlux(GPU-on)'):>13} {fmt(perf_mm,'µFlux(GPU-on)'):>11} {fmt(perf_bs,'µFlux(GPU-on)'):>11}")
    print(f"{'C++':<10} {'—':>11} {'—':>11} {'—':>11} {'—':>13} {fmt(perf_mm,'C++(GPU-on)'):>11} {fmt(perf_bs,'C++(GPU-on)'):>11}")
    print(f"\n{'Token counts':<10} {'LogExtract':>12} {'Analytics':>12} {'Mandelbrot':>12} {'SpectralNorm':>14} {'Matmul':>12} {'BlackSch':>12}")
    print("-"*88)
    for lang in["µFlux","Rust","C++","Python","Node.js"]:
        vals=[]
        for bn in["logextract","analytics","mandelbrot","spectralnorm","matmul","blackscholes"]:
            t=token_data.get(lang,{}).get(bn,{}).get("tokens")
            vals.append(f"{t:>12}" if t else f"{'—':>12}")
        print(f"{lang:<10} {vals[0]} {vals[1]} {vals[2]} {vals[3]:>14} {vals[4]} {vals[5]}")
if __name__=="__main__":
    main()
