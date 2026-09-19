#!/bin/sh
# trans test suite — two pathways, all gated.
#
#  1. tests/*.c        round-trip: C -> trans -> uf -> run, gated against the
#                      system binary's stdout and exit code (echo/true/false/wc/yes)
#  2. tests/ops/*.c    per-operation: transpile, compile with uf, run, gate on
#                      exit code 0, empty stderr, and stdout == ops/<name>.out
#                      (optional ops/<name>.args supplies program arguments)
#
# Every gate must pass: transpile rc 0 + empty stderr, compile rc 0 + empty
# stderr, run rc (0 or reference), stdout match, stderr empty.
UF=${UF:-../comp/target/release/uf}
T=$(mktemp -d)
PASS=0; FAIL=0

gate_fail() { echo "FAIL   $1: $2"; FAIL=$((FAIL+1)); }

# ---- pathway 1: system-binary round-trips ----
for c in tests/*.c; do
  name=$(basename "$c" .c)
  [ "$name" = README ] && continue
  SYS=$(command -pv "$name" 2>/dev/null | tail -1)
  [ -z "$SYS" ] && SYS=/usr/bin/$name
  ./trans "$c" > "$T/$name.uft" 2>"$T/$name.terr"
  [ $? -ne 0 ] && { gate_fail "$name" "transpile rc!=0: $(head -c 120 "$T/$name.terr")"; continue; }
  [ -s "$T/$name.terr" ] && { gate_fail "$name" "transpiler stderr not empty"; continue; }
  $UF --device cpu -c "$T/$name.uft" -o "$T/$name.bin" 2>"$T/$name.cerr"
  [ $? -ne 0 ] && { gate_fail "$name" "compile rc!=0: $(head -c 120 "$T/$name.cerr")"; continue; }
  [ -s "$T/$name.cerr" ] && { gate_fail "$name" "compile stderr not empty"; continue; }
  if [ -x "$SYS" ]; then
    timeout 10 "$SYS" hello world > "$T/$name.ref" 2>/dev/null; SRC=$?
    timeout 10 "$T/$name.bin" hello world > "$T/$name.out" 2>"$T/$name.rerr"; RC=$?
    [ -s "$T/$name.rerr" ] && { gate_fail "$name" "runtime stderr not empty"; continue; }
    if [ "$RC" = "$SRC" ] && diff -q "$T/$name.out" "$T/$name.ref" >/dev/null 2>&1; then
      echo "MATCH  $name (rc=$RC)"; PASS=$((PASS+1))
    else
      gate_fail "$name" "rc=$RC want=$SRC; out=$(head -c 60 "$T/$name.out"|tr '\n' '|') ref=$(head -c 60 "$T/$name.ref"|tr '\n' '|')"
    fi
  else
    timeout 10 "$T/$name.bin" > "$T/$name.out" 2>"$T/$name.rerr"; RC=$?
    [ -s "$T/$name.rerr" ] && { gate_fail "$name" "runtime stderr not empty (no ref binary)"; continue; }
    [ "$RC" = 0 ] && { echo "NOREF  $name (ran rc=0, stderr clean)"; PASS=$((PASS+1)); } || gate_fail "$name" "rc=$RC (no ref binary)"
  fi
done

# ---- pathway 2: per-operation gates ----
for c in tests/ops/*.c; do
  name=ops/$(basename "$c" .c)
  exp="tests/ops/$(basename "$c" .c).out"
  [ -f "$exp" ] || { gate_fail "$name" "missing .out file"; continue; }
  ARGS=""
  [ -f "tests/ops/$(basename "$c" .c).args" ] && ARGS=$(cat "tests/ops/$(basename "$c" .c).args")
  ./trans "$c" > "$T/op.uft" 2>"$T/op.terr"
  [ $? -ne 0 ] && { gate_fail "$name" "transpile rc!=0: $(head -c 120 "$T/op.terr")"; continue; }
  [ -s "$T/op.terr" ] && { gate_fail "$name" "transpiler stderr not empty"; continue; }
  $UF --device cpu -c "$T/op.uft" -o "$T/op.bin" 2>"$T/op.cerr"
  [ $? -ne 0 ] && { gate_fail "$name" "compile rc!=0: $(head -c 120 "$T/op.cerr")"; continue; }
  [ -s "$T/op.cerr" ] && { gate_fail "$name" "compile stderr not empty"; continue; }
  timeout 10 "$T/op.bin" $ARGS > "$T/op.out" 2>"$T/op.rerr"
  RC=$?
  [ "$RC" = 0 ] || { gate_fail "$name" "run rc=$RC: $(head -c 80 "$T/op.rerr")"; continue; }
  [ -s "$T/op.rerr" ] && { gate_fail "$name" "runtime stderr not empty"; continue; }
  if diff -q "$T/op.out" "$exp" >/dev/null 2>&1; then
    echo "OPS-OK $name"; PASS=$((PASS+1))
  else
    gate_fail "$name" "stdout mismatch: got=$(head -c 60 "$T/op.out"|tr '\n' '|') want=$(head -c 60 "$exp"|tr '\n' '|')"
  fi
done

rm -rf "$T"
echo "pass=$PASS fail=$FAIL"
[ "$FAIL" = 0 ]
