#!/bin/sh
# pkg_gate.sh — package-system integration gate (SPEC.md "Packages").
#
# Phase 1: install + approval, compile-from-store, grant algebra (allow /
# starved-required / undeclared / no-grant gate), lock discipline (bare-
# unpinned, drift under --locked, reconcile, prune), glued-directive guard.
# Phase 2: use aliases + `:` namespacing (alias twins, struct
# qualification, deps-of-deps, two versions under aliases).
#
# Uses a throwaway NKPKG_HOME and a temp project dir; leaves nothing
# behind in the repo. Run: sh comp/tests/pkg_gate.sh [path-to-nk]

set -u
REPO=$(cd "$(dirname "$0")/../.." && pwd)
NK="${1:-$REPO/comp/target/release/nk}"
if [ ! -x "$NK" ]; then
    if command -v nk >/dev/null 2>&1; then NK=nk; else echo "no nk binary (build comp/ or pass a path)"; exit 1; fi
fi
NK=$(command -v "$NK" || echo "$NK")
NKS_BIN="$REPO/comp/target/release/nks"
[ -x "$NKS_BIN" ] || NKS_BIN="$NK"
FIX="$REPO/comp/tests/pkgs"
WORK=$(mktemp -d /tmp/nkpkg-gate.XXXXXX)
PROJ="$WORK/proj"
mkdir -p "$PROJ"
export NKPKG_HOME="$WORK/store"
fails=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1"; fails=$((fails+1)); }
expect_rc() { if [ "$2" = "$3" ]; then pass "$1"; else fail "$1 (want rc=$2 got rc=$3)"; fi }
expect_out() { if grep -q "$2"; then pass "$1"; else fail "$1 (output missing: $2)"; fi }

# ---- happy path: install + grant-gated compile + run (qualified refs) ----
cat > "$PROJ/main.n" <<EOF
use demo "path:$FIX/libdemo"
use files "path:$FIX/filelib" allow fs.read
use needy "path:$FIX/needy" allow proc
entry:
  21 'demo:double print
  "$PROJ/main.n" 'files:slurp length 0 gt check
  "true" 'needy:shcmd 2 get 0 structural_equal check
  "pkg gate ok" print
  ret
EOF
out=$("$NK" install "$PROJ" --yes 2>&1); rc=$?
expect_rc "install . --yes" 0 "$rc"
echo "$out" | expect_out "install reports resolved pins" 'resolved[[:space:]]*path:.*libdemo'
out=$("$NK" install "$PROJ" --locked 2>&1); rc=$?
expect_rc "install . --locked verifies" 0 "$rc"
echo "$out" | expect_out "locked reports verified" 'verified[[:space:]]*path:.*libdemo'
out=$("$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "compile+run from store" 0 "$rc"
echo "$out" | expect_out "package call works" '^42$'
echo "$out" | expect_out "gate marker" 'pkg gate ok'
[ -f "$PROJ/.nk/approvals" ] && pass "approvals written" || fail "approvals written"
grep -q "source=path:$FIX/libdemo" "$PROJ/.nk/lock" && pass "pin recorded" || fail "pin recorded"

# ---- no grant -> package op is a compile error ----
sed -i 's/ allow fs.read//' "$PROJ/main.n"
out=$("$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "no-grant gate fires" 1 "$rc"
echo "$out" | expect_out "gate names capability" 'fs.read'
sed -i "s|path:$FIX/filelib|path:$FIX/filelib allow fs.read|" "$PROJ/main.n"

# ---- undeclared grant is an error ----
printf 'use "path:%s/libdemo" allow proc\nentry:\n  ret\n' "$FIX" > "$PROJ/main.n"
out=$("$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "undeclared grant rejected" 1 "$rc"
echo "$out" | expect_out "names the declaration gap" 'declares no such capability'

# ---- starved required cap ----
printf 'use "path:%s/needy"\nentry:\n  ret\n' "$FIX" > "$PROJ/main.n"
out=$("$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "starved required rejected" 1 "$rc"
echo "$out" | expect_out "starve message" 'starves required capability .proc.'

# ---- bare unpinned use ----
rm -rf "$PROJ/.nk"
out=$("$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "bare use without lock rejected" 1 "$rc"
echo "$out" | expect_out "remedy names install" 'nk install'
"$NK" install "$PROJ" --yes >/dev/null 2>&1

# ---- drift: locked fails, reconcile updates ----
printf 'use "path:%s/libdemo"\nentry:\n  ret\n' "$FIX" > "$PROJ/main.n"
"$NK" install "$PROJ" --yes >/dev/null 2>&1
cp "$FIX/libdemo/main.n" "$WORK/libdemo-orig.n"
printf 'pub\ndouble: n!\n  ret n 3 mul\n' > "$FIX/libdemo/main.n"
out=$("$NK" install "$PROJ" --locked 2>&1); rc=$?
expect_rc "locked detects drift" 1 "$rc"
echo "$out" | expect_out "drift names both roots" 'content drifted'
out=$("$NK" install "$PROJ" --yes 2>&1); rc=$?
expect_rc "reconcile adopts new content" 0 "$rc"
echo "$out" | expect_out "reconcile reports updated" 'updated'
cp "$WORK/libdemo-orig.n" "$FIX/libdemo/main.n"
"$NK" install "$PROJ" --yes >/dev/null 2>&1

# ---- phase 2: aliases + `:` namespacing ----
cat > "$PROJ/main.n" <<EOF
use lib1 "path:$FIX/structlib"
use lib2 "path:$FIX/structlib"
entry:
  'lib1:ptsize print
  'lib2:ptsize print
  _offset lib1:Pt.y print
  'lib1:ptyoff print
  ret
EOF
out=$("$NK" install "$PROJ" --yes 2>&1); rc=$?
expect_rc "alias install" 0 "$rc"
out=$("$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "alias twins compile+run" 0 "$rc"
echo "$out" | tr '\n' ' ' | expect_out "struct size via both aliases" '32 32'
echo "$out" | tr '\n' ' ' | expect_out "caller-side _offset + package-internal offset" '16 16'

# ---- phase 2: deps of deps install and resolve transitively ----
mkdir -p "$WORK/pkgdep"
printf 'use "path:%s/libdemo"\npub\nquad: n!\n  ret n '\''libdemo:double\n' "$FIX" > "$WORK/pkgdep/main.n"
cat > "$PROJ/main.n" <<EOF
use "path:$WORK/pkgdep"
entry:
  5 'pkgdep:quad print
  ret
EOF
rm -rf "$PROJ/.nk"
out=$("$NK" install "$PROJ" --yes 2>&1); rc=$?
expect_rc "dep-of-dep install" 0 "$rc"
[ "$(grep -c 'pkg ' "$PROJ/.nk/lock")" = "2" ] && pass "both pins recorded" || fail "both pins recorded"
out=$("$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "dep chain runs" 0 "$rc"
echo "$out" | expect_out "dep chain result" '^10$'

# ---- phase 2: two versions of one package under distinct aliases ----
mkdir -p "$WORK/libv"
printf 'pub\ndouble: n!\n  ret n 2 mul\n' > "$WORK/libv/main.n"
printf 'use "path:%s/libv"\nentry:\n  ret\n' "$WORK" > "$PROJ/main.n"
rm -rf "$PROJ/.nk"
"$NK" install "$PROJ" --yes >/dev/null 2>&1
ROOTA=$(grep 'libv' "$PROJ/.nk/lock" | sed 's/.*root=//')
printf 'pub\ndouble: n!\n  ret n 3 mul\n' > "$WORK/libv/main.n"
"$NK" install "$PROJ" --yes >/dev/null 2>&1
ROOTB=$(grep 'libv' "$PROJ/.nk/lock" | sed 's/.*root=//')
cat > "$PROJ/.nk/lock" <<EOF
pkg source=path:$WORK/libv ref=v1 sha=- root=$ROOTA
pkg source=path:$WORK/libv ref=v2 sha=- root=$ROOTB
EOF
cat > "$PROJ/main.n" <<EOF
use v1 "path:$WORK/libv@v1"
use v2 "path:$WORK/libv@v2"
entry:
  4 'v1:double print
  4 'v2:double print
  ret
EOF
out=$("$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "two versions under aliases" 0 "$rc"
echo "$out" | tr '\n' ' ' | expect_out "both versions isolated" '8 12'

# ---- alias uniqueness ----
printf 'use "path:%s/libdemo"\nuse "path:%s/structlib"\nentry:\n  ret\n' "$FIX" "$FIX" > "$PROJ/main.n"
rm -rf "$PROJ/.nk"
"$NK" install "$PROJ" --yes >/dev/null 2>&1
cat > "$PROJ/main.n" <<EOF
use same "path:$FIX/libdemo"
use same "path:$FIX/structlib"
entry:
  ret
EOF
out=$("$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "duplicate alias rejected" 1 "$rc"
echo "$out" | expect_out "alias collision message" 'alias .same. collides'

# ---- glued directive spelling guard ----
printf 'entry:\n  use"x" ret\n' > "$PROJ/glued.n"
out=$("$NK" "$PROJ/glued.n" 2>&1); rc=$?
expect_rc "glued use rejected" 1 "$rc"
echo "$out" | expect_out "glued remedy" 'write a space'
rm -f "$PROJ/glued.n"

# ---- local mods + plain projects still work ----
printf 'entry:\n  ret 7\n' > "$PROJ/main.n"
rm -rf "$PROJ/.nk"
out=$("$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "lockless plain project still compiles" 0 "$rc"

# ---- phase 3: runtime ceiling (NK_CAP_CEILING) ----
cat > "$PROJ/main.n" <<EOF
use files "path:$FIX/filelib" allow fs.read
use needy "path:$FIX/needy" allow proc
entry:
  "proc" cap print
  "$FIX/libdemo/main.n" 'files:slurp length 0 gt check
  "ceiling ok" print
  ret
EOF
"$NK" install "$PROJ" --yes >/dev/null 2>&1
out=$("$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "no ceiling runs granted caps" 0 "$rc"
echo "$out" | expect_out "cap reflects grant" '^1$'
out=$(NK_CAP_CEILING=fs.read,proc "$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "ceiling within grants runs" 0 "$rc"
out=$(NK_CAP_CEILING=fs.read "$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "ceiling starves required at startup" 1 "$rc"
echo "$out" | expect_out "startup names package+cap" 'package needy requires capability proc'
cat > "$PROJ/main.n" <<EOF
use files "path:$FIX/filelib" allow fs.read
entry:
  "fs.read" cap print
  "$FIX/libdemo/main.n" 'files:slurp length print
  ret
EOF
"$NK" install "$PROJ" --yes >/dev/null 2>&1
out=$(NK_CAP_CEILING= "$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "unchecked op dies above ceiling" 1 "$rc"
echo "$out" | expect_out "op gate names cap+op" 'cap denied: fs.read .runtime ceiling. in op_slurp'
echo "$out" | expect_out "cap reflects ceiling" '^0$'

# ---- phase 5/6: tools (bins) + .nk/tools toolchain pinning ----
TOOLSRC="$WORK/tooldemo"
mkdir -p "$TOOLSRC"
printf 'name tooldemo\nbin hello = main.n\ncaps host.argv optional\n' > "$TOOLSRC/nk.pkg"
cat > "$TOOLSRC/main.n" <<'EOF'
entry:
  argv 1 get nm!
  "hello " nm concat print
  "fs.read" cap print
  ret
EOF
out=$("$NK" install "path:$TOOLSRC" --yes --grant host.argv 2>&1); rc=$?
expect_rc "tool install (compile-at-install)" 0 "$rc"
echo "$out" | expect_out "tool reported" '^tool[[:space:]]*hello'
[ -x "$NKPKG_HOME/bin/hello" ] && pass "shim installed" || fail "shim installed"
out=$("$NKPKG_HOME/bin/hello" world 2>&1); rc=$?
expect_rc "shim executes tool with argv" 0 "$rc"
echo "$out" | expect_out "tool output" '^hello world$'
out=$("$NK" run hello world 2>&1); rc=$?
expect_rc "nk run executes tool" 0 "$rc"
echo "$out" | expect_out "run output" '^hello world$'
out=$("$NKS_BIN" run hello --policy data world 2>&1); rc=$?
expect_rc "policy ceiling restricts run" 1 "$rc"
echo "$out" | expect_out "run ceiling names cap" 'cap denied: host.argv'
# .nk/tools toolchain pinning
rm -rf "$PROJ/.nk"
printf 'entry:\n  ret 7\n' > "$PROJ/main.n"
mkdir -p "$PROJ/.nk"
printf 'tool hello = path:%s grant host.argv\n' "$TOOLSRC" > "$PROJ/.nk/tools"
rm -rf "$NKPKG_HOME/tools" "$NKPKG_HOME/bin"
out=$("$NK" install "$PROJ" --yes 2>&1); rc=$?
expect_rc ".nk/tools installs pinned tool" 0 "$rc"
echo "$out" | expect_out "tool from tools file" '^tool[[:space:]]*hello'
out=$("$NK" install "$PROJ" --yes 2>&1); rc=$?
expect_rc ".nk/tools reconcile idempotent" 0 "$rc"
echo "$out" | expect_out "tool-ok reported" 'tool-ok'
out=$("$NK" tools rm hello 2>&1); rc=$?
expect_rc "tools rm" 0 "$rc"
"$NK" tools list | grep -q hello && fail "tool removed from registry" || pass "tool removed from registry"

# ---- phase 4: signing (ed25519 over the nkph1 root) ----
SIGNLIB="$WORK/siglib"
mkdir -p "$SIGNLIB"
printf 'pub\nval:\n  ret 7\n' > "$SIGNLIB/main.n"
printf 'use "path:%s"\nentry:\n  '\''siglib:val print\n  ret\n' "$SIGNLIB" > "$PROJ/main.n"
rm -rf "$PROJ/.nk"
"$NK" install "$PROJ" --yes >/dev/null 2>&1
SROOT=$(grep '/siglib' "$PROJ/.nk/lock" | sed 's/.*root=//' | cut -d' ' -f1)
SEED=$("$NK" pkg keygen 2>/dev/null | grep seed= | cut -d= -f2)
"$NK" pkg pub "$SEED" 2>/dev/null > "$SIGNLIB/nk.pub"
printf '%s' "$SROOT" | "$NK" pkg sign "$SEED" 2>/dev/null > "$SIGNLIB/nk.sig"
rm -rf "$PROJ/.nk"
out=$("$NK" install "$PROJ" --yes 2>&1); rc=$?
expect_rc "signed install verifies" 0 "$rc"
echo "$out" | expect_out "signed reported" 'signed$'
grep -q 'sig=ok' "$PROJ/.nk/lock" && pass "lock records sig=ok" || fail "lock records sig=ok"
grep -q 'key=' "$PROJ/.nk/approvals" && pass "approval pins publisher key" || fail "approval pins publisher key"
out=$("$NK" "$PROJ/main.n" 2>&1); rc=$?
expect_rc "signed package runs" 0 "$rc"
echo "$out" | expect_out "signed package output" '^7$'
printf 'pub\nval:\n  ret 8\n' > "$SIGNLIB/main.n"
rm -rf "$PROJ/.nk"
out=$("$NK" install "$PROJ" --yes 2>&1); rc=$?
expect_rc "tampered content rejected" 1 "$rc"
echo "$out" | expect_out "tamper message" 'signature verification FAILED'

rm -rf "$WORK"
echo "----"
if [ "$fails" = 0 ]; then echo "pkg gate: all pass"; else echo "pkg gate: $fails FAILURES"; exit 1; fi
