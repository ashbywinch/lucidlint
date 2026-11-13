#!/usr/bin/env python3
"""Gate every shipped subject against its minimum and its target, and write
the PR comment.

`make coverage` merges the subject reports into one Cobertura file; this script
reads that file by PACKAGE (python, scanner, radonc), measures each package's
line coverage from its line records, and compares it with the two bars the
Makefile states beside the measured number:

- **minimum** — below this the subject is in trouble;
- **target** — the high bar the suite is expected to hit. The gate FAILS when
  a subject is below its target, so "coverage went down a bit" cannot land
  quietly just because it stayed above the minimum.

It also writes the sticky-comment table: one row per subject with measured,
minimum, target and status (target met / below target / below minimum).

Usage:
    coverage-gate.py coverage-all.xml \\
        --subject python 85 90 --subject scanner 82 85 --subject radonc 62 70 \\
        -o coverage-summary.md
"""

import argparse
import sys
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import NamedTuple


def percent(fraction: float) -> float:
    """A coverage fraction as the percentage the bars are stated in."""
    return fraction * 100  # lucidlint: ignore magic-number 100 is the definition of percent


class _Lines(NamedTuple):
    """One report element's line accounting: covered out of valid."""

    covered: int
    valid: int


def line_rate(cls: ET.Element) -> _Lines:
    """Covered/valid lines for one class, counted per line NUMBER.

    llvm-cov emits one record per instantiation of a line (a generic or
    macro-expanded line appears more than once); coverage is per line, so the
    records collapse to the highest hit count. A record without a numeric
    `number`, or line 0, is malformed and adds no data."""
    hits: dict[str, int] = {}
    for line in cls.findall("lines/line"):
        number = line.get("number") or ""
        if not number.isdigit() or int(number) == 0:
            continue  # a missing number or line 0 is not a source line — malformed record
        count = int(line.get("hits", "0") or 0)
        hits[number] = max(hits.get(number, 0), count)
    return _Lines(sum(1 for count in hits.values() if count > 0), len(hits))


def package_rate(root: ET.Element, package: str) -> _Lines | None:
    """The package's lines, or None when the report does not name it — a
    subject missing from the report is a measurement gap, not 0%."""
    covered = 0
    valid = 0
    found = False
    for element in root.findall("packages/package"):
        if element.get("name") != package:
            continue
        found = True
        for cls in element.findall("classes/class"):
            lines = line_rate(cls)
            covered += lines.covered
            valid += lines.valid
    return _Lines(covered, valid) if found else None


def status(measured: float, minimum: float, target: float) -> str:
    """The row's word: the two bars name different promises."""
    if measured < minimum:
        return "below minimum"
    if measured < target:
        return "below target"
    return "target met"


def _row(name: str, covered: int, valid: int, minimum: float, target: float) -> str:
    measured = percent(covered / valid) if valid else 0.0
    verdict = status(measured, minimum, target)
    return (
        f"| {name} | {covered}/{valid} | {measured:.1f}% | "
        f"{minimum:.0f}% | {target:.0f}% | {verdict} |"
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Gate the merged Cobertura report and write the summary.")
    parser.add_argument("report", type=Path, help="the merged Cobertura XML")
    parser.add_argument(
        "--subject",
        nargs=3,
        action="append",
        required=True,
        metavar=("PACKAGE", "MINIMUM", "TARGET"),
        help="a subject: its package name in the report, its minimum, its target (both in percent)",
    )
    parser.add_argument("-o", "--output", type=Path, help="write the markdown comment here")
    args = parser.parse_args(argv)

    if not args.report.is_file():
        print(f"coverage-gate: no report at {args.report}", file=sys.stderr)
        return 2
    try:
        root = ET.parse(args.report).getroot()
    except ET.ParseError as exc:
        print(f"coverage-gate: {args.report} is not parseable: {exc}", file=sys.stderr)
        return 2

    rows: list[str] = []
    failures: list[str] = []
    covered_total = 0
    valid_total = 0
    for package, minimum_text, target_text in args.subject:
        minimum = float(minimum_text)
        target = float(target_text)
        rate = package_rate(root, package)
        if rate is None:
            print(
                f"coverage-gate: {package} is missing from {args.report} — "
                "the merged report has no measurement for it",
                file=sys.stderr,
            )
            return 2
        covered, valid = rate.covered, rate.valid
        if valid == 0:
            print(
                f"coverage-gate: {package} measures no line at all — "
                "the coverage run produced nothing (a broken report, not 0%)",
                file=sys.stderr,
            )
            return 2
        covered_total += covered
        valid_total += valid
        measured = percent(covered / valid)
        verdict = status(measured, minimum, target)
        print(
            f"coverage-gate: {package} {measured:.1f}% "
            f"(minimum {minimum:.0f}%, target {target:.0f}%) — {verdict}"
        )
        if verdict != "target met":
            failures.append(f"{package} {measured:.1f}% < target {target:.0f}%")
        rows.append(_row(package, covered, valid, minimum, target))

    total = percent(covered_total / valid_total) if valid_total else 0.0
    table = "\n".join(
        [
            "| Subject | Lines | Coverage | Minimum | Target | Status |",
            "|---|---|---|---|---|---|",
            *rows,
            f"| **total** | **{covered_total}/{valid_total}** | **{total:.1f}%** | — | — | — |",
        ]
    )
    if args.output is not None:
        try:
            args.output.write_text(table + "\n", encoding="utf-8")
        except OSError as exc:
            print(f"coverage-gate: cannot write {args.output}: {exc}", file=sys.stderr)
            return 2
    if failures:
        print(
            "coverage-gate: below target — " + "; ".join(failures)
            + ". Add the missing tests, or move the bar deliberately with its new measurement.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
