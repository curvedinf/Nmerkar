# Benchmark Specification

Programs print results to stdout and exit 0. The first two benchmarks read an
input file path from argv[1]; the remaining generated-data workloads take their
problem size N from argv[1] and read no files. Output format must match exactly
for cross-validation (tolerances for reductions are noted per benchmark).

## 1. Log Extraction (`logextract`)

**Input:** Apache combined access log (~500MB, 3.7M lines)

**Line format:**
```
IP - - [DD/Mon/YYYY:HH:MM:SS +0000] "METHOD PATH HTTP/1.1" STATUS BYTES "REF" "UA"
```

**Parsing approach:** Split line by space. Relevant tokens:
- `[0]` = IP address
- `[3]` = timestamp starting with `[DD/Mon/YYYY:HH` — split by `:`, index 3 = hour
- `[8]` = HTTP status code (integer)
- `[9]` = bytes transferred (integer)

**Compute:**
1. Total lines processed
2. Total bytes transferred (sum of bytes field)
3. HTTP status distribution: 2xx, 3xx, 4xx, 5xx counts
4. Requests per hour (24 integer counts, index 0-23)
5. Top 10 IP addresses by request count

**Output format (exact):**
```
=== LOG EXTRACTION RESULTS ===
total_lines: <int>
total_bytes: <int>
status_2xx: <int>
status_3xx: <int>
status_4xx: <int>
status_5xx: <int>
hour_00: <int>
hour_01: <int>
...
hour_23: <int>
top_ip_1: <ip> (<count>)
top_ip_2: <ip> (<count>)
...
top_ip_10: <ip> (<count>)
```

## 2. Data Analytics (`analytics`)

**Input:** Sales CSV (~500MB, 5.8M rows)

**Header:** `order_id,date,region,country,product,category,channel,units,unit_price,discount,total,reps`

**Parsing approach:** Skip first line (header). Split each row by comma. Relevant columns:
- `[2]` = region
- `[3]` = country
- `[4]` = product
- `[7]` = units (integer)
- `[10]` = total (float, 2 decimal places)

**Compute:**
1. Total rows processed
2. Total revenue (sum of total)
3. Average order value (total_revenue / rows)
4. Total units sold (sum of units)
5. Revenue by region (6 regions, sorted by revenue descending)
6. Top 5 products by revenue
7. Top 3 countries by revenue

**Output format (exact):**
```
=== DATA ANALYTICS RESULTS ===
total_rows: <int>
total_revenue: <float 2dp>
avg_order: <float 2dp>
total_units: <int>
region_1: <name> $<float 2dp>
region_2: <name> $<float 2dp>
region_3: <name> $<float 2dp>
region_4: <name> $<float 2dp>
region_5: <name> $<float 2dp>
region_6: <name> $<float 2dp>
product_1: <name> $<float 2dp>
product_2: <name> $<float 2dp>
product_3: <name> $<float 2dp>
product_4: <name> $<float 2dp>
product_5: <name> $<float 2dp>
country_1: <name> $<float 2dp>
country_2: <name> $<float 2dp>
country_3: <name> $<float 2dp>
```

## 3. Mandelbrot (`mandelbrot`)

**Input:** n from argv[1] (default 1000). Grid n×n over the standard
Mandelbrot view; per cell, iterate z ← z² + c up to the escape limit;
count cells that never escape. Register-cached iteration, no array traffic.

**Output format (exact):** one line — the count of cells that never escape, printed as `%d`.
(Reference: n=100 → 3959.)

## 4. Spectral Norm (`spectralnorm`)

**Input:** n from argv[1] (default 5500). Computes the 2-norm of the n×n
matrix A where A(i,j) = 1/(i+j+1) (Hilbert matrix) via the classic
power-iteration method.

**Output format (exact):** the norm printed with `%.9f`.
(Reference: n=100 → 1.274219991.)

## Tie-breaking

- For top-N rankings, sort by value descending. Ties broken by name ascending.

## GPU-oriented benchmarks (v13.1)

### 5. Matmul (`matmul`)

**Input:** N from argv[1] (default 512). No data files.

**Compute:** three N×N row-major float64 matrices. Flat index k = row·N + col:

```
A[k] = 1.0 + 1e-6·k
B[k] = 1.0 - 1e-6·k
C = A·B          (plain triple loop / BLAS / GPU kernel — any order)
```

**Output format (exact labels; values compared across languages):**
```
c[0,0]: <%.6f>
c[N-1,N-1]: <%.6f>
checksum: <%.2f>
```
Samples are bit-identical across sequential implementations; `checksum` (a
reduction) may reassociate — compare within relative 1e-6 (GPU/numpy columns).

### 6. Black-Scholes-style pricing (`blackscholes`)

**Input:** N from argv[1] (default 2000000).

Per option k (float64 throughout; `log(S/K)` is pinned as the ratio-minus-one
form below so the whole formula is pure +−*/ arithmetic — identical doubles in
every language and on the GPU):

```
S[k]   = 100.0 + 1e-6·k
K[k]   = 95.0 + 5e-7·k
sigT[k]= 0.2 + 1e-7·k
D      = 0.98511193960306265           (e^(-rT), r=0.02, T=0.75, pinned)
d1     = ((S/K) - 1 + 0.5·sigT²) / sigT
d2     = d1 - sigT
price  = S·N(d1) - K·D·N(d2)
N(x)   = 0.5 + x·S(x²)                 (pinned 19-coefficient polynomial)
```

Pinned `S` coefficients (Horner, ascending order — copy verbatim):
```
 0.39894225252808718, -0.066489940262615232, 0.009972402563565597,
-0.0011861254737728477, 0.00011477352256159136, -0.0000092236015244880478,
 0.00000061709290701241611, -0.000000033683815695296368,
 0.0000000014305101232167135, -0.0000000000429103246845425126,
 0.00000000000067639314645906401, 0.0000000000000067395099116704979,
-0.00000000000000059769612419950018, 0.0000000000000000083488823309250727,
 0.00000000000000000025294417305671556, -0.0000000000000000000130500997464350326,
 0.00000000000000000000025200131520055619, -0.0000000000000000000000024409281764270801,
 0.0000000000000000000000000098374183081777466
```

**Output format:**
```
premium_sum: <%.2f>
sample[0]: <%.6f>
sample[N-1]: <%.6f>
```
Sequential CPU implementations are bit-identical; GPU columns may differ on
`premium_sum` within ±0.01% (reduction order).

**Structure**: the formula, coefficients and outputs are pinned; the code
shape is not. The Nmerkar implementation evaluates the polynomial once over
the concatenated `[d1 d2]` vector and splits the result (identical doubles,
same op order per element — outputs stay bit-identical while the 19
coefficients appear once in source).

### 7. N-Queens (`nqueens`)

**Input:** N from argv[1] (default 11). Pinned algorithm: iterative
backtracking with an explicit stack — `cols[r]` is the placed column of row
r, `nxt[r]` the next column to try after backtracking into row r. Candidate
(r, c) conflicts with row i < r when `cols[i] == c` or `|cols[i]-c| == |r-i|`
(expressed as the two signed tests `(ci-c)==(i-r)` / `(ci-c)==(r-i)` so no
`abs` op is needed). Reaching depth N counts one solution.

**Output format:**
```
solutions: <count>
```
Known counts for verification: N=8 → 92, N=10 → 724, N=11 → 2680, N=12 → 14200.

Pattern coverage: search/backtracking, nested loops, flag-driven early exit,
int-array state — no tensors, no GPU offloading (CPU-only group).

### 8. Packed-CSR graph BFS (`bfs`)

**Input:** n from argv[1] (default 1000000). Deterministic graph: node u has
edges to `(7u+3) mod n` and `(13u+11) mod n`, plus `(u+1) mod n` when
`u mod 3 == 0`. All implementations first materialize the graph in compressed
sparse row form: `offsets` has n+1 integer entries and `edges` has
`2n + ceil(n/3)` packed integer entries. BFS starts at node 0; `dist` is an
integer array initialized to -1 and the frontier is a preallocated integer
queue with head/tail indices.

**Output format (exact integers, identical across languages):**
```
reached: <r>
unreached: <n-r>
sum_dist: <sum of finite distances>
max_dist: <max finite distance>
```
At n=1M: reached 1000000, sum_dist 15758576, max_dist 21.

Pattern coverage: packed graph construction, indirect typed-array access,
nested variable-bound loops, and queue mutation. This is the traversal/codegen
benchmark; dynamic-container overhead is isolated below.

### 9. Dynamic graph stress (`dynamicgraph`)

**Input and graph:** identical to `bfs`, including default n=1000000 and BFS
from node 0. Instead of CSR, every implementation stores adjacency in a hash
map keyed by node id, with a separately allocated dynamic neighbor list as each
value. The queue remains preallocated so this benchmark isolates hash-map,
small-list allocation, dynamic lookup, and pointer-chasing costs.

**Output format and reference values:** identical to `bfs`. At n=1M: reached
1000000, sum_dist 15758576, max_dist 21.

Pattern coverage: one million hash-map entries, one million small dynamic
lists, irregular lookup, GC/allocation pressure, and mixed dynamic/typed-array
access. It has no elementwise structure to fuse or offload (CPU-only group).

### CPU-only policy and GPU-on column (v13.1)

The four legacy benchmarks (logextract, analytics, mandelbrot, spectralnorm)
run **CPU-only**: Nmerkar entries are compiled with `--device cpu` so automatic
GPU offloading never contaminates the historical numbers. The **GPU-on column**
shows the same Nmerkar source under the default `auto` device; other languages
show `—` (no GPU builds of those programs).

Nmerkar GPU-on entries also pass `--gc-threshold` — large multi-MB arrays
collect constantly at the default 1MB threshold, which costs time (the v13.2
GC fixes made small-threshold runs *correct*; the big thresholds are now
pure performance).

The non-Nmerkar GPU variants (`*_gpu.{cpp,rs,py,js}`) share `src/_gpu/gpucomp.c`,
a Vulkan launcher mirroring the nk runtime (auto device = hardware-first, most
free VRAM). **Status: pending** — the launcher currently hangs on pipeline
setup outside gdb and its binaries are disabled (`.broken`); sources are kept
and the Nmerkar GPU-on columns (which exercise the same shaders through the
compiler's own runtime) are fully functional.
