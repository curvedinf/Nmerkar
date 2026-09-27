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
            assert "count: 100000" in result.stdout and "count2: 25000" in result.stdout
        if number == 13:
            assert "139998" in result.stdout and "69999" in result.stdout
    print(f"PASS: {'quick' if QUICK else 'full'} Unix QC on {os.uname().sysname} {os.uname().machine}")


if __name__ == "__main__":
    main()
