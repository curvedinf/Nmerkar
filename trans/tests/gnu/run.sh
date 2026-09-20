#!/bin/bash
# GNU tool test suite — transpiles each multi-file tool adaptation,
# compiles with nk, and gates behavior against the system binaries.
# The transpiled .n of every tool is exported to examples/gnu/ so the
# generated Nmerkar code is visible to web indexers.
#
# Tool sources live in tests/gnu/<tool>/*.c (concatenated in ls order,
# gnu_main.c excluded — that one is a gcc-reference-only wrapper).
# Transpilation gates: rc 0 + empty stderr at every stage, plus the
# behavior comparison. gcc reference builds are also checked first so a
# bad adaptation is caught independently of the transpiler.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
TRANSROOT=$(cd "$HERE/../.." && pwd)
UF=${UF:-$TRANSROOT/../comp/target/release/nk}
TRANS=${TRANS:-$TRANSROOT/target/release/trans}
[ -x "$TRANS" ] || { echo "trans binary missing; run: (cd trans && cargo build --release)"; exit 1; }
[ -x "$UF" ] || { echo "nk binary missing; run: (cd comp && cargo build --release)"; exit 1; }
TOOLS="cat head nl tee cksum base64"
EXDIR=$HERE/../../../examples/gnu
T=$(mktemp -d)
PASS=0; FAIL=0

gate_fail() { echo "FAIL   $1: $2"; FAIL=$((FAIL+1)); }

# fixtures
printf 'hello\nworld\n\n\n\ttab here\nlast line no nl' > "$T/text.txt"
printf 'aaaa\nbbbbbbbb\ncc\n' > "$T/lines.txt"
head -c 300 /dev/urandom > "$T/bin.dat"
printf 'The quick brown fox jumps over the lazy dog\npack my box with five dozen liquor jugs\n' > "$T/b64.txt"

for tool in $TOOLS; do
  dir=$HERE/$tool
  [ -d "$dir" ] || continue

  # 1. gcc reference build (sanity of the adaptation itself)
  gcc -w -I "$HERE" -o "$T/${tool}_gcc" "$dir"/*.c 2>/dev/null \
    || { gate_fail "$tool" "gcc reference build failed"; continue; }

  # 2. transpile (multi-file, gnu_main.c excluded)
  srcs=$(ls "$dir"/*.c | grep -v gnu_main.c | tr '\n' ' ')
  "$TRANS" $srcs > "$T/$tool.n" 2>"$T/$tool.terr"
  [ $? -eq 0 ] || { gate_fail "$tool" "transpile rc!=0: $(head -c 120 "$T/$tool.terr")"; continue; }
  [ -s "$T/$tool.terr" ] && { gate_fail "$tool" "transpiler stderr not empty"; continue; }

  # 3. compile
  "$UF" --device cpu -c "$T/$tool.n" -o "$T/${tool}_bin" 2>"$T/$tool.cerr"
  [ $? -eq 0 ] || { gate_fail "$tool" "compile rc!=0: $(head -c 120 "$T/$tool.cerr")"; continue; }
  [ -s "$T/$tool.cerr" ] && { gate_fail "$tool" "compile stderr not empty"; continue; }

  # 4. export transpiled code for indexers
  cp "$T/$tool.n" "$EXDIR/$tool.n"

  # 5. behavior gates: (label|args|stdin-fixture...) — compare
  #    transpiled binary vs the SYSTEM binary
  pass_t=0; fail_t=0
  check() { # label args... -- stdin-file
    local label="$1"; shift
    local args=""
    local stdin_f=/dev/null
    local collecting=0
    for a in "$@"; do
      if [ "$a" = "--" ]; then collecting=1; continue; fi
      if [ $collecting = 1 ]; then stdin_f="$a"; else args="$args $a"; fi
    done
    timeout 10 "$T/${tool}_bin" $args > "$T/out" 2>"$T/rerr" < "$stdin_f"
    local rc=$?
    timeout 10 "$tool" $args > "$T/ref" 2>/dev/null < "$stdin_f"
    local rrc=$?
    if [ $rc = $rrc ] && diff -q "$T/out" "$T/ref" >/dev/null 2>&1 && [ ! -s "$T/rerr" ]; then
      pass_t=$((pass_t+1))
    else
      fail_t=$((fail_t+1))
      echo "  DIFF $tool [$label] rc=$rc want=$rrc err=$(head -c 60 "$T/rerr")"
      diff "$T/out" "$T/ref" 2>/dev/null | head -3 | sed 's/^/    /'
    fi
  }

  case $tool in
    cat)
      check plain -- "$T/text.txt"
      for f in -n -b -E -T -A -v -s -nE -e -t -ns -bs -bn; do
        check "$f" $f -- "$T/text.txt"
      done
      check binary-v -v -- "$T/bin.dat"
      check two-files "$T/lines.txt" "$T/text.txt"
      ;;
    head)
      check default -- "$T/lines.txt"
      check n3 -n 3 -- "$T/lines.txt"
      check n1 -n 1 -- "$T/text.txt"
      check n15 -n 15 -- "$T/lines.txt"
      check attached -n2 -- "$T/lines.txt"
      check oldstyle -2 -- "$T/lines.txt"
      check c8 -c 8 -- "$T/lines.txt"
      check cK -c 1K -- "$T/bin.dat"
      check quiet -q "$T/lines.txt" "$T/text.txt"
      check verbose -v -- "$T/lines.txt"
      check two-files "$T/lines.txt" "$T/text.txt"
      ;;
    nl)
      check default -- "$T/lines.txt"
      check ba -b a -- "$T/lines.txt"
      check bn -b n -- "$T/lines.txt"
      check ln -n ln -- "$T/lines.txt"
      check rz -n rz -- "$T/lines.txt"
      check w2 -w 2 -- "$T/lines.txt"
      check v10 -v 10 -- "$T/lines.txt"
      check i0 -i 0 -- "$T/lines.txt"
      check sdash -s"::" -- "$T/lines.txt"
      check nonl -- "$T/text.txt"
      ;;
    tee)
    rm -f "$T/tee1.out" "$T/tee2.out"
      timeout 10 "$T/tee_bin" "$T/tee1.out" "$T/tee2.out" > "$T/out" < "$T/lines.txt"
      cat "$T/tee1.out" > "$T/t1"; cat "$T/tee2.out" > "$T/t2"
      if diff -q "$T/out" "$T/lines.txt" >/dev/null && diff -q "$T/t1" "$T/lines.txt" >/dev/null && diff -q "$T/t2" "$T/lines.txt" >/dev/null; then
        pass_t=$((pass_t+1))
      else
        fail_t=$((fail_t+1)); echo "  DIFF tee [two-files]"
      fi
      rm -f "$T/tee3.out"
      timeout 10 "$T/tee_bin" -a "$T/tee3.out" > /dev/null < "$T/lines.txt"
      timeout 10 "$T/tee_bin" -a "$T/tee3.out" > /dev/null < "$T/text.txt"
      cat "$T/tee3.out" > "$T/t3"
      cat "$T/lines.txt" "$T/text.txt" > "$T/want3"
      diff -q "$T/t3" "$T/want3" >/dev/null && pass_t=$((pass_t+1)) || { fail_t=$((fail_t+1)); echo "  DIFF tee [append]"; }
      ;;
    cksum)
      check empty -- /dev/null
      check text -- "$T/lines.txt"
      check binary -- "$T/bin.dat"
      check bigfile -- /etc/passwd
      ;;
    base64)
      check default -- "$T/b64.txt"
      check w0 -w 0 -- "$T/b64.txt"
      check w20 -w 20 -- "$T/b64.txt"
      check binary -- "$T/bin.dat"
      base64 "$T/b64.txt" | timeout 10 "$T/base64_bin" -d > "$T/out" 2>"$T/rerr"
      diff -q "$T/out" "$T/b64.txt" >/dev/null && [ ! -s "$T/rerr" ] && pass_t=$((pass_t+1)) || { fail_t=$((fail_t+1)); echo "  DIFF base64 [roundtrip]"; }
      base64 -w 7 "$T/b64.txt" | timeout 10 "$T/base64_bin" -d > "$T/out" 2>/dev/null
      diff -q "$T/out" "$T/b64.txt" >/dev/null && pass_t=$((pass_t+1)) || { fail_t=$((fail_t+1)); echo "  DIFF base64 [wrapped roundtrip]"; }
      ;;
  esac

  if [ $fail_t = 0 ]; then
    echo "GNU-OK $tool ($pass_t gates, .n exported)"
    PASS=$((PASS+1))
  else
    gate_fail "$tool" "$fail_t of $((pass_t+fail_t)) behavior gates"
  fi
done

rm -rf "$T"
echo "gnu pass=$PASS fail=$FAIL"
[ "$FAIL" = 0 ]
