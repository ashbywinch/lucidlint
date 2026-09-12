# lucidlint: ignore-file fakefs the drift guard reads the REAL scanner sources
# as its test data — a faked filesystem would make the guard vacuous.
"""Every `— fix: <kind>` directive a finding advertises must name a fix that
actually exists for that finding's language: running the advertised command
must change the file — never a silent no-op, and never the wrong fix (the
Rust --fix dispatcher once treated every unknown kind as extract-method).
Where no fixer exists, the finding's message itself carries the instruction.

User ruling (2026-09-05): never suggest that someone run fix unless it is
actually going to fix something; if we cannot fix it, the suggestion belongs
in the original error message.
"""

import re
from pathlib import Path

import fix_engine
import lucidlint as ch

SCANNER_SRC = Path(__file__).resolve().parent.parent / "scanner" / "src"
TAIL = re.compile(r"— fix: ([a-z-]+)")


def rust_fixable_from_dispatch() -> set[str]:
    """The kinds main.rs's --fix dispatcher actually routes — parsed from the
    match arms so the orchestrator's constant and the dispatcher cannot
    drift apart."""
    src = (SCANNER_SRC / "main.rs").read_text()
    return {m.replace("_", "-") for m in re.findall(r"fix::fix_([a-z_]+)\(", src)}


def tails_of(src: str) -> list[str]:
    """`— fix: <kind>` admissions. `— fix: lucidlint fix --kind …` is the
    full_fix_command TEMPLATE (the plumbing that appends the command to a
    message), not an admission — the binary name leads it, so skip it."""
    return [k for k in TAIL.findall(src) if k != "lucidlint"]




def test_python_finding_tails_name_python_fixable_kinds():
    offenders = []
    for rs in sorted(SCANNER_SRC.glob("*.rs")):
        if rs.name == "rustscan.rs":  # findings about .rs files: guarded below
            continue
        for kind in tails_of(rs.read_text()):
            if kind not in fix_engine.FIXABLE_KINDS:
                offenders.append(f"{rs.name}: {kind}")
    assert offenders == [], f"fix directives with no Python fixer: {offenders}"


def test_rust_finding_tails_name_rust_fixable_kinds():
    src = (SCANNER_SRC / "rustscan.rs").read_text() + (SCANNER_SRC / "rustloops.rs").read_text()
    fixable = rust_fixable_from_dispatch()
    offenders = [k for k in tails_of(src) if k not in fixable]
    assert offenders == [], f"fix directives with no Rust fixer: {offenders}"


def test_fix_refuses_a_check_without_an_auto_fix(tmp_path, capsys):
    """`fix --kind data-clump` says the check can't be fixed by this command
    and points at the report — not a traceback, not a silent success, and
    not a bare 'no auto-fix exists' (R31: the message addresses the
    mistake)."""
    repo = tmp_path / "repo"
    (repo / ".git").mkdir(parents=True)
    (repo / "houses").mkdir(parents=True)
    (repo / "houses" / "app.py").write_text("x = 1\n")
    rc = run_fix(repo, "fix", "--kind", "data-clump", "--file", "houses/app.py", "--line", "1")
    assert rc == 1
    out = capsys.readouterr().out
    assert "data-clump" in out and "cannot be fixed by this command" in out, out
    assert "report line" in out, out
    assert "Traceback" not in out


def test_fix_refuses_an_unknown_kind_with_a_hint(tmp_path, capsys):
    """`fix --kind magic-nubmer` says the kind is not fixable and suggests
    the closest real name — the message is designed around the typo."""
    repo = tmp_path / "repo"
    (repo / ".git").mkdir(parents=True)
    (repo / "houses").mkdir(parents=True)
    (repo / "houses" / "app.py").write_text("x = 1\n")
    rc = run_fix(repo, "fix", "--kind", "magic-nubmer", "--file", "houses/app.py", "--line", "1")
    assert rc == 1
    out = capsys.readouterr().out
    assert "magic-nubmer" in out and "not a fixable kind" in out, out
    assert "Did you mean 'magic-number'" in out, out


def test_fix_refuses_unfixable_kind_on_rust_file(tmp_path, capsys):
    """`fix --kind vague-name --file x.rs` refuses — the Rust engine names
    exactly which fixes exist; nothing is written, and the previous
    dispatcher fallback (running extract-method at that line) never fires.
    (vague-name is name-required, so the naming guidance fires first with
    rc 0 — the no-engine refusal is covered by a non-name kind below.)"""
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / "lib.rs").write_text("fn main() {}\n")
    rc = run_fix(repo, "fix", "--kind", "vague-name", "--file", "lib.rs", "--line", "1")
    assert rc == 0
    out = capsys.readouterr().out
    assert "vague-name" in out and "needs a semantic name" in out, out
    assert (repo / "lib.rs").read_text() == "fn main() {}\n"


def test_fix_refuses_engine_less_kind_on_rust_file(tmp_path, capsys):
    """The scanner's --fix gate refuses a kind with no engine (rc 2, the
    engine list) — exercised at the binary itself, where no orchestrator
    pre-dispatch can silence it. The orchestrator path (no finding at the
    line) correctly stays R28-silent and writes nothing."""
    import json
    import subprocess

    from test_lucidlint import make_repo

    repo = make_repo(tmp_path, app_src="def alpha(a):\n    return a\n")
    target = repo / "houses" / "lib.rs"
    target.write_text("fn main() {}\n")
    binary = ch.RUST_SCAN.binary(repo)
    assert binary is not None
    spec = json.dumps({"kind": "boolean-arg", "file": str(target), "line": 1, "name": ""})
    proc = subprocess.run([str(binary), "--fix", spec], capture_output=True, text=True, cwd=str(repo))
    assert proc.returncode == 2, proc.stdout + proc.stderr
    assert "boolean-arg" in proc.stdout and "no Rust auto-fix" in proc.stdout, proc.stdout
    assert target.read_text() == "fn main() {}\n"


def test_fix_names_the_params_prerequisite_when_unresolvable(tmp_path, capsys):
    """A positional-literals fix whose callee cannot be resolved refuses
    naming the exact missing input and the exact rerun — never a bare
    'nothing to change' (which reads as already-fixed)."""
    repo = tmp_path / "repo"
    (repo / "houses").mkdir(parents=True)
    (repo / "houses" / "app.py").write_text("def g():\n    mystery(10, 20)\n")
    rc = run_fix(repo, "fix", "--kind", "positional-literals", "--file", "houses/app.py", "--line", "2")
    assert rc == 0
    out = capsys.readouterr().out
    assert "--params" in out and "nothing to change" not in out, out


def run_fix(repo, *extra):
    """Run `lucidlint fix` against a bare repo — the fixerless guard must
    fire before any scan or git access, so no fakes are needed."""
    import sys

    saved = sys.argv
    sys.argv = ["lucidlint.py", "--repo", str(repo), *extra]
    try:
        return ch.main()
    finally:
        sys.argv = saved
