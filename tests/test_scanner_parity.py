# code-health: ignore-file fakefs the parity gate shells out to the Rust binary (subprocess
# interop, a named real-FS exception) and reads the repo's own sources — pyfakefs would fake
# away exactly what this test inspects (same class as test_tool_imports.py).
"""Parity gate: the Rust scan core must match the Python implementation.

Runs the scanner binary over the fixture corpus + the tool's own Python
files and diffs, per file:
- cyclomatic complexity vs radon (normalized for the decorated-function
  line offset — ruff excludes decorators, radon includes them; the
  identity is (file, function, cc), line-insensitive for decorated fns);
- the five ported signals (magic-number, noop-statement, inline-import,
  private-import, unreachable) on (kind, file, line).

The houses corpus stays an optional deep check (make scanner-parity-houses)
— houses is not part of this repo, so it cannot be a CI gate.
"""

import json
import subprocess
from pathlib import Path

import pytest
from radon.visitors import ComplexityVisitor

import code_health as ch

ROOT = Path(__file__).resolve().parent.parent
SCANNER = ROOT / "scanner"
BINARY = SCANNER / "target" / "release" / "code-health-scan"
CORPUS = sorted((ROOT / "tests" / "fixtures").rglob("*.py")) + [
    ROOT / "code_health.py",
    ROOT / "check_records.py",
    ROOT / "check_review_posted.py",
]

PORTED_SIGNALS = ("magic-number", "noop-statement", "inline-import", "private-import", "unreachable")


def _signal_of(message: str) -> str | None:
    if "magic number" in message:
        return "magic-number"
    if "no-op statement" in message:
        return "noop-statement"
    if "import inside function body" in message:
        return "inline-import"
    if "imports private" in message:
        return "private-import"
    if "unreachable statement" in message:
        return "unreachable"
    return None


@pytest.fixture(scope="module")
def binary() -> Path:
    if not BINARY.exists():
        subprocess.run(["cargo", "build", "--release", "--manifest-path", str(SCANNER / "Cargo.toml")], check=True)
    return BINARY


def _rust_output(binary: Path) -> dict:
    proc = subprocess.run([str(binary)] + [str(p) for p in CORPUS], capture_output=True, text=True, check=True)
    return json.loads(proc.stdout)


def _python_side() -> tuple[set, set]:
    """(cc entries, findings) from the Python implementation."""
    cc: set[tuple[str, str, int]] = set()
    findings: set[tuple[str, str, int]] = set()
    visitor = ch._radon_visitor()
    for py in CORPUS:
        rel = py.relative_to(ROOT).as_posix()
        try:
            src = py.read_text(encoding="utf-8", errors="replace")
        except UnicodeDecodeError:
            continue
        try:
            for f in ComplexityVisitor.from_code(src).functions:
                cc.add((rel, f.name, f.complexity))
        except SyntaxError:
            pass
        for a in ch._scan_file(py, rel, False, visitor, ROOT, {}, {}):
            sig = _signal_of(a.message)
            if sig:
                findings.add((sig, rel, a.line))
    return cc, findings


def test_cc_parity(binary: Path):
    rust = _rust_output(binary)
    rust_cc = {(e["file"].replace(str(ROOT) + "/", ""), e["function"], e["cc"]) for e in rust["cc"]}
    py_cc, _ = _python_side()
    # the decorated-function line offset is not part of the identity — a
    # function present on both sides must have identical CC
    both = {(f, n) for f, n, _ in rust_cc} & {(f, n) for f, n, _ in py_cc}
    mismatches = [(f, n, c1, c2) for f, n in both
                  for c1 in [next(c for ff, nn, c in rust_cc if ff == f and nn == n)]
                  for c2 in [next(c for ff, nn, c in py_cc if ff == f and nn == n)]
                  if c1 != c2]
    assert not mismatches, f"CC mismatches: {mismatches[:5]}"
    # every Python function must exist in Rust
    missing = {(f, n) for f, n, _ in py_cc} - {(f, n) for f, n, _ in rust_cc}
    assert not missing, f"functions missing from Rust: {sorted(missing)[:5]}"


def test_findings_parity(binary: Path):
    rust = _rust_output(binary)
    rust_findings = {
        (f["kind"], f["file"].replace(str(ROOT) + "/", ""), f["line"]) for f in rust["findings"]
    }
    _, py_findings = _python_side()
    only_py = py_findings - rust_findings
    only_rust = rust_findings - py_findings
    assert not only_py, f"Python findings missing from Rust: {sorted(only_py)[:5]}"
    assert not only_rust, f"Rust findings missing from Python: {sorted(only_rust)[:5]}"


def test_scanner_parses_every_corpus_file(binary: Path):
    """broken.py is intentionally unparseable (fixture) — exactly 1 error,
    and error tolerance is the point: everything else must parse clean."""
    rust = _rust_output(binary)
    assert rust["parse_errors"] == 1, f"unexpected parse errors: {rust['parse_errors']}"
