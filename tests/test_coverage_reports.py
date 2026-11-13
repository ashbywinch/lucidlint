# lucidlint: ignore-file fakefs the coverage scripts ARE subprocess CLIs — the
# test's subject is their real-FS interop (write XML, read it back), the named
# exception in the fakefs rule's own message
"""The coverage reporting scripts: one merged Cobertura file, one gated comment.

`make coverage` measures three shipped subjects and must never report fewer
lines than it measured, or gate a subject it did not measure. The merger names
each subject and prefixes its filenames with the tree path; the gate reads the
merged report by package, compares each subject with its minimum AND its
target (failing below the target), and writes the comment table.
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


def merged_report(path: Path, packages: dict[str, list[tuple[str, list[tuple[int, int]]]]]) -> Path:
    """A merged-style report: package name -> [(filename, [(line, hits), ...])]."""
    package_xml = []
    for name, classes in packages.items():
        class_xml = []
        for filename, lines in classes:
            records = "".join(f'<line number="{number}" hits="{hits}"/>' for number, hits in lines)
            class_xml.append(
                f'<class name="{Path(filename).stem}" filename="{filename}" line-rate="0">'
                f"<lines>{records}</lines></class>"
            )
        package_xml.append(f'<package name="{name}"><classes>{"".join(class_xml)}</classes></package>')
    path.write_text(
        '<?xml version="1.0"?><coverage version="test"><packages>'
        + "".join(package_xml)
        + "</packages></coverage>"
    )
    return path


def lines(covered: int, total: int) -> list[tuple[int, int]]:
    """Unique line numbers: the first `covered` hit, the rest not — repeated
    numbers would collapse in the merger's per-line accounting."""
    return [(number, 1 if number <= covered else 0) for number in range(1, total + 1)]


def run(script: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(script), *args], capture_output=True, text=True
    )


def gate(report: Path, *subjects: tuple[str, int, int], out: Path | None = None) -> subprocess.CompletedProcess:
    args = [str(report)]
    for name, minimum, target in subjects:
        args += ["--subject", name, str(minimum), str(target)]
    if out is not None:
        args += ["-o", str(out)]
    return run(GATE, *args)


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


def test_merge_skips_a_record_without_a_line_number(tmp_path):
    """Line 0 is not a source line: a malformed record is skipped, so it can
    neither inflate the totals nor dilute the rate."""
    rust = tmp_path / "rust.xml"
    rust.write_text(
        '<?xml version="1.0"?><coverage version="t"><packages><package name="src">'
        '<classes><class name="a" filename="src/a.rs"><lines>'
        '<line hits="1"/><line number="0" hits="1"/><line number="7" hits="1"/>'
        "</lines></class></classes></package></packages></coverage>"
    )
    merged = tmp_path / "coverage-all.xml"
    assert run(MERGE, "--report", "scanner", str(rust), "scanner/", "-o", str(merged)).returncode == 0
    root = ET.parse(merged).getroot()
    assert (root.get("lines-covered"), root.get("lines-valid")) == ("1", "1")


def test_merge_names_a_missing_report(tmp_path):
    missing = tmp_path / "absent.xml"
    result = run(
        MERGE,
        "--report", "python", str(missing), "",
        "-o", str(tmp_path / "all.xml"),
    )
    assert result.returncode == 2
    assert "no report" in result.stderr


def test_gate_passes_at_target_and_writes_the_comment(tmp_path):
    report = merged_report(
        tmp_path / "coverage-all.xml",
        {
            "python": [("lucidlint.py", lines(8, 10))],  # 80%
            "scanner": [("scanner/src/a.rs", lines(9, 10))],  # 90%
        },
    )
    summary = tmp_path / "coverage-summary.md"

    result = gate(
        report,
        ("python", 70, 80),
        ("scanner", 80, 90),
        out=summary,
    )

    assert result.returncode == 0, result.stderr
    table = summary.read_text()
    assert "| python | 8/10 | 80.0% | 70% | 80% | target met |" in table
    assert "| scanner | 9/10 | 90.0% | 80% | 90% | target met |" in table
    assert "| **total** | **17/20** | **85.0%** |" in table


def test_gate_fails_between_minimum_and_target(tmp_path):
    report = merged_report(
        tmp_path / "coverage-all.xml",
        {"python": [("a.py", lines(86, 100))]},  # 86%
    )
    result = gate(report, ("python", 85, 90))
    assert result.returncode == 1
    assert "below target" in result.stdout
    assert "python 86.0% < target 90%" in result.stderr


def test_gate_fails_below_minimum(tmp_path):
    report = merged_report(
        tmp_path / "coverage-all.xml",
        {"python": [("a.py", lines(80, 100))]},  # 80%
    )
    result = gate(report, ("python", 85, 90))
    assert result.returncode == 1
    assert "below minimum" in result.stdout


def test_gate_skips_line_zero_and_malformed_records(tmp_path):
    """The gate and the merger agree: line 0 and a missing number are not
    source lines, so they cannot drag the rate down (2/2, not 2/4)."""
    report = merged_report(
        tmp_path / "coverage-all.xml",
        {"python": [("a.py", [(0, 0), (13, 5), (7, 1)])]},
    )
    report.write_text(report.read_text().replace("<line number=\"13\"", "<line hits=\"1\"/><line number=\"13\""))
    assert gate(report, ("python", 90, 100)).returncode == 0


def test_gate_refuses_a_subject_missing_from_the_report(tmp_path):
    report = merged_report(tmp_path / "coverage-all.xml", {"python": [("a.py", [(1, 1)])]})
    result = gate(report, ("scanner", 80, 85))
    assert result.returncode == 2
    assert "missing from" in result.stderr


def test_gate_treats_a_package_with_no_lines_as_broken(tmp_path):
    """A subject with no measurable line is a broken coverage run, not 0% —
    the gate names it instead of pretending the code is untested."""
    report = md = tmp_path / "coverage-all.xml"
    md.write_text(
        '<?xml version="1.0"?><coverage version="t"><packages>'
        '<package name="python"><classes/></package></packages></coverage>'
    )
    result = gate(report, ("python", 80, 90))
    assert result.returncode == 2
    assert "measures no line at all" in result.stderr


def test_gate_refuses_an_unreadable_report(tmp_path):
    unreadable = tmp_path / "coverage-all.xml"
    unreadable.write_text("not xml at all")
    assert gate(unreadable, ("python", 80, 90)).returncode == 2
    assert run(GATE, str(tmp_path / "absent.xml"), "--subject", "python", "80", "90").returncode == 2
