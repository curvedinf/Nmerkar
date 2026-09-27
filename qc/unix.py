"""Compile and run Nmerkar on the current Unix host, including generated C.

Usage: python3 qc/unix.py /path/to/nk /path/to/nks [--quick]
Run from any directory. --quick excludes the large compute and thread cases.
"""

import os
from pathlib import Path
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
COMP = ROOT / "comp"
NK = Path(sys.argv[1]).resolve()
NKS = Path(sys.argv[2]).resolve()
QUICK = "--quick" in sys.argv[3:]


def run(*args, cwd=COMP, ok=True, timeout=180):
    print("+", *map(str, args), flush=True)
    p = subprocess.run(args, cwd=cwd, text=True, capture_output=True, timeout=timeout)
    if (p.returncode == 0) != ok:
        raise AssertionError(
            f"exit={p.returncode}, expected {'success' if ok else 'failure'}\n"
            f"stdout:\n{p.stdout[-4000:]}\nstderr:\n{p.stderr[-4000:]}"
        )
    return p


def main():
    assert NK.is_file() and NKS.is_file(), "both nk and nks must be installed"
    run("cc", "--version")
    hello = run(NK, "--device", "cpu", '"unix smoke" print')
    assert "unix smoke" in hello.stdout, hello.stdout

    json_cases = '''
"[true,false,null,1,0]" parse_json values!
values to_json print
values 0 get type_of print
values 1 get type_of print
values 2 get type_of print
values 3 get type_of print
values 0 get 1 structural_equal print
values 0 get 1 eq print
values 0 get 2 add print
2 _cast byte to_json print
values unique to_json print
"{\\"flag\\":true,\\"empty\\":null}" parse_json record!
record "flag" get to_json print
record "empty" get to_json print
'''
    json_result = run(NK, "--device", "cpu", json_cases)
    assert json_result.stdout.splitlines() == [
        '[true,false,null,1,0]', '21', '21', '2', '0',
        '0', '1', '3', '2', '[true,false,null,1,0]', 'true', 'null',
    ], json_result.stdout

    with tempfile.TemporaryDirectory(prefix="nk-qc-") as tmp:
        binary = Path(tmp) / "program"
        run(NK, "--device", "cpu", "-c", '"compiled" print', "-o", binary)
        assert "compiled" in run(binary).stdout
        run(NK, "--emit-c", '"c" print', "-o", Path(tmp) / "source")
        assert (Path(tmp) / "source.c").is_file()
        file = Path(tmp) / "main.n"
        file.write_text('"source file" print\n')
        assert "source file" in run(NK, "--device", "cpu", file).stdout

    run(NKS, "--caps")
    run(NKS, "--policy", "pure", '"/etc/passwd" read_file print', ok=False)

    cases = [1, 2, 3, 4, 5, 6, 7, 8, 9, 12, 17]
    if not QUICK:
        cases += [13, 14, 15, 16]
    for number in cases:
        path, = (COMP / "tests").glob(f"t{number:02d}_*.n")
        args = [str(NK), "--device", "cpu"]
        if number == 13:
            args += ["--gc-threshold", "200000000"]
        result = run(*args, path, timeout=300)
        if number == 15:
            assert result.stdout.split() == ["count:", "100000", "count2:", "25000"], result.stdout
        if number == 13:
            assert "139998" in result.stdout and "69999" in result.stdout
        if number == 14:
            assert "premium_sum:" in result.stdout and "sample[N-1]:" in result.stdout
    print(f"PASS: {'quick' if QUICK else 'full'} Unix QC on {os.uname().sysname} {os.uname().machine}")


if __name__ == "__main__":
    main()
