# lucidlint: ignore-file fakefs the coverage scripts ARE subprocess CLIs — the
# test's subject is their real-FS interop (write XML, read it back), the named
# exception in the fakefs rule's own message
"""The coverage reporting scripts: one merged Cobertura file, one gate.

`make coverage` measures three shipped subjects and must never report fewer
lines than it measured, or gate a subject it did not measure — these tests pin
the merge (package names, repo-relative filenames, totals) and the floor check
(pass, fail, unreadable input).
"""

import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MERGE = ROOT / "scripts" / "merge-coverage.py"
GATE = ROOT / "scripts" / "coverage-gate.py"


def cobertura(path: Path, classes: list[tuple[str, list[int]]]) -> Path:
    """A minimal Cobertura report: one class per (filename, hit counts) entry."""
    body = []
    for filename, hits in classes:
        lines = "".join(
            f'<line number="{i + 1}" hits="{hit}"/>' for i, hit in enumerate(hits)
        )
        body.append(
            f'<class name="{Path(filename).stem}" filename="{filename}" '
            f'line-rate="0" branch-rate="0"><lines>{lines}</lines></class>'
        )
    path.write_text(
        '<?xml version="1.0"?><coverage version="test" line-rate="0" branch-rate="0">'
        "<packages><package name=\"src\" line-rate=\"0\"><classes>"
        + "".join(body)
        + "</classes></package></packages></coverage>"
    )
    return path


def run(script: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(script), *args], capture_output=True, text=True
    )


def test_merge_keeps_every_subject_with_its_tree_path(tmp_path):
    python = cobertura(
        tmp_path / "coverage.xml",
        [("lucidlint.py", [1, 1, 0, 1]), ("fix_engine.py", [1, 0])],
    )
    rust = cobertura(tmp_path / "coverage-rust.xml", [("src/checks.rs", [1, 1, 1])])
    merged = tmp_path / "coverage-all.xml"

    result = run(
        MERGE,
        "--report", "python", str(python), "",
        "--report", "scanner", str(rust), "scanner/",
        "-o", str(merged),
    )

    assert result.returncode == 0, result.stderr
    root = ET.parse(merged).getroot()
    packages = root.findall("packages/package")
    assert [p.get("name") for p in packages] == ["python", "scanner"]
    filenames = [c.get("filename") for p in packages for c in p.findall("classes/class")]
    assert filenames == ["lucidlint.py", "fix_engine.py", "scanner/src/checks.rs"]
    # 7 covered of 9 lines — the merged total spans every package
    assert (root.get("lines-covered"), root.get("lines-valid")) == ("7", "9")
    assert root.get("line-rate") == "0.7778"


def test_duplicate_line_records_collapse_to_one_line(tmp_path):
    """llvm-cov emits one record per instantiation of a line (generics, macro
    expansion); coverage is per line NUMBER, covered when any record hit it.
    Counting records understates the rate and inflates the totals."""
    rust = tmp_path / "rust.xml"
    rust.write_text(
        '<?xml version="1.0"?><coverage version="t"><packages><package name="src">'
        '<classes><class name="a" filename="src/a.rs"><lines>'
        '<line number="10" hits="0"/><line number="10" hits="4"/>'
        '<line number="11" hits="1"/>'
        "</lines></class></classes></package></packages></coverage>"
    )
    merged = tmp_path / "coverage-all.xml"

    result = run(MERGE, "--report", "scanner", str(rust), "scanner/", "-o", str(merged))

    assert result.returncode == 0, result.stderr
    root = ET.parse(merged).getroot()
    assert (root.get("lines-covered"), root.get("lines-valid")) == ("2", "2")
    assert run(GATE, str(rust), "100", "scanner").returncode == 0


def test_merge_skips_a_record_without_a_line_number(tmp_path):
    """Line 0 is not a source line: a malformed record is skipped, so it can
    neither inflate the totals nor dilute the rate."""
    rust = tmp_path / "rust.xml"
    rust.write_text(
        '<?xml version="1.0"?><coverage version="t"><packages><package name="src">'
        '<classes><class name="a" filename="src/a.rs"><lines>'
        '<line hits="1"/><line number="7" hits="1"/>'
        "</lines></class></classes></package></packages></coverage>"
    )
    merged = tmp_path / "coverage-all.xml"
    assert run(MERGE, "--report", "scanner", str(rust), "scanner/", "-o", str(merged)).returncode == 0
    root = ET.parse(merged).getroot()
    assert (root.get("lines-covered"), root.get("lines-valid")) == ("1", "1")


def test_gate_treats_an_empty_report_as_broken_not_uncovered(tmp_path):
    """A report with no measurable line is a broken coverage run, not 0% — the
    gate names it instead of pretending the code is untested."""
    empty = tmp_path / "coverage.xml"
    empty.write_text(
        '<?xml version="1.0"?><coverage version="t"><packages><package name="src">'
        '</package></packages></coverage>'
    )
    result = run(GATE, str(empty), "80", "Python")
    assert result.returncode == 2
    assert "measures no line at all" in result.stderr


def test_merge_names_a_missing_report(tmp_path):
    missing = tmp_path / "absent.xml"
    result = run(
        MERGE,
        "--report", "python", str(missing), "",
        "-o", str(tmp_path / "all.xml"),
    )
    assert result.returncode == 2
    assert "no report" in result.stderr


def test_gate_passes_at_the_floor_and_fails_below_it(tmp_path):
    report = cobertura(tmp_path / "coverage.xml", [("a.py", [1] * 8 + [0] * 2)])
    assert run(GATE, str(report), "80", "Python").returncode == 0
    below = run(GATE, str(report), "85", "Python")
    assert below.returncode == 1
    assert "80.0% is below the 85% floor" in below.stderr


def test_gate_refuses_an_unreadable_report(tmp_path):
    unreadable = tmp_path / "coverage.xml"
    unreadable.write_text("not xml at all")
    assert run(GATE, str(unreadable), "80").returncode == 2
    assert run(GATE, str(tmp_path / "absent.xml"), "80").returncode == 2
