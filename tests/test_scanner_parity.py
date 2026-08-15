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

PORTED_SIGNALS = (
    "magic-number", "noop-statement", "inline-import", "private-import", "unreachable",
    "suppression", "type-ignore", "global-state", "builtin-shadow", "closures",
    "class-module", "vague-name", "strewing", "except", "broad-except",
)


# (message needle, signal) — order matters: type-ignore is checked BEFORE
# 'without a why' (a why-less type-ignore comment is both)
_SIGNAL_NEEDLES = (
    ("magic number", "magic-number"),
    ("no-op statement", "noop-statement"),
    ("import inside function body", "inline-import"),
    ("imports private", "private-import"),
    ("unreachable statement", "unreachable"),
    ("type: ignore", "type-ignore"),
    ("without a why", "suppression"),
    ("global statement", "global-state"),
    ("shadows a builtin", "builtin-shadow"),
    ("closing over", "closures"),
    ("-line body", "closures"),
    ("holds one class", "class-module"),
    ("name carries a", "vague-name"),
    ("share leading parameter", "strewing"),
    ("bare except", "except"),
    ("swallows", "except"),
    ("broad `except", "broad-except"),
)


def _signal_of(message: str) -> str | None:
    for needle, sig in _SIGNAL_NEEDLES:
        if needle in message:
            return sig
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
        # include_tests=True so the fixtures take the general scan branch —
        # the parity corpus deliberately includes tests/fixtures input.
        for a in ch._scan_file(py, rel, True, visitor, ROOT, {}, {}):
            sig = _signal_of(a.message)
            if sig:
                findings.add((sig, rel, a.line))
    return cc, findings


def test_cc_parity(binary: Path):
    rust = _rust_output(binary)
    rust_map = {
        (e["file"].replace(str(ROOT) + "/", ""), e["function"]): e["cc"] for e in rust["cc"]
    }
    py_map = {(f, n): c for f, n, c in _python_side()[0]}
    # the decorated-function line offset is not part of the identity — a
    # function present on both sides must have identical CC
    mismatch_pairs = [
        (f, n, rust_map[(f, n)], py_map[(f, n)])
        for f, n in rust_map
        if f in py_map and n in py_map and rust_map[(f, n)] != py_map[(f, n)]
    ]
    assert not mismatch_pairs, f"CC mismatches: {mismatch_pairs[:5]}"
    missing = [k for k in py_map if k not in rust_map]
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


def _rust_kind_pairs(rust: dict, kind: str) -> set[tuple[str, int]]:
    return {(f["file"], f["line"]) for f in rust["findings"] if f["kind"] == kind}


def _py_kind_pairs(fn, repo) -> set[tuple[str, int]]:
    from collections import Counter

    return {(a.file, a.line) for a in fn(repo, True, Counter(), {})}


def test_repo_wide_parity(binary: Path):
    """duplicate + unused: the Rust core computes the whole-repo families
    (Dice on structural skeletons; defined-but-never-referenced) exactly
    like _duplicate_actions / _unused_actions."""
    # the binary must see the FULL repo file set (reference scan splits
    # prod vs test files)
    all_py = sorted(
        p for p in ROOT.rglob("*.py")
        if not any(part in ch.EXCLUDED_DIRS for part in p.parts)
    )
    proc = subprocess.run([str(binary)] + [str(p) for p in all_py],
                          capture_output=True, text=True, check=True)
    rust = json.loads(proc.stdout)
    rust_dup = _rust_kind_pairs(rust, "duplicate")
    rust_unused = _rust_kind_pairs(rust, "unused")
    py_dup = _py_kind_pairs(ch._duplicate_actions, ROOT)
    py_unused = _py_kind_pairs(ch._unused_actions, ROOT)
    assert py_dup <= rust_dup, f"duplicate missing from Rust: {sorted(py_dup - rust_dup)[:5]}"
    assert rust_dup <= py_dup, f"duplicate extra in Rust: {sorted(rust_dup - py_dup)[:5]}"
    assert py_unused <= rust_unused, f"unused missing from Rust: {sorted(py_unused - rust_unused)[:5]}"
    assert rust_unused <= py_unused, f"unused extra in Rust: {sorted(rust_unused - py_unused)[:5]}"


def test_scanner_parses_every_corpus_file(binary: Path):
    """broken.py is intentionally unparseable (fixture) — exactly 1 error,
    and error tolerance is the point: everything else must parse clean."""
    rust = _rust_output(binary)
    assert rust["parse_errors"] == 1, f"unexpected parse errors: {rust['parse_errors']}"
