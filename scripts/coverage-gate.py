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


class Lines(NamedTuple):
    """One report element's line accounting: covered out of valid."""

    covered: int
    valid: int


class Subject(NamedTuple):
    """One package and the two bars its coverage is held to, in percent."""

    package: str
    minimum: float
    target: float


class CoverageGate:
    """The gate run: the merged report, the subjects with their bars, and what
    the run owes the caller — the rows, the failures, the exit status."""

    def __init__(self, root: ET.Element, subjects: list[Subject], output: Path | None):
        self.root = root
        self.subjects = subjects
        self.output = output
        self.rows: list[str] = []
        self.failures: list[str] = []
        self.covered_total = 0
        self.valid_total = 0

    @staticmethod
    def percent(fraction: float) -> float:
        """A coverage fraction as the percentage the bars are stated in."""
        return fraction * 100  # lucidlint: ignore magic-number 100 is the definition of percent

    @staticmethod
    def line_rate(cls: ET.Element) -> Lines:
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
        return Lines(sum(1 for count in hits.values() if count > 0), len(hits))

    def package_rate(self, package: str) -> Lines | None:
        """The package's lines, or None when the report does not name it — a
        subject missing from the report is a measurement gap, not 0%."""
        covered = 0
        valid = 0
        found = False
        for element in self.root.findall("packages/package"):
            if element.get("name") != package:
                continue
            found = True
            for cls in element.findall("classes/class"):
                lines = self.line_rate(cls)
                covered += lines.covered
                valid += lines.valid
        return Lines(covered, valid) if found else None

    @staticmethod
    def status(measured: float, minimum: float, target: float) -> str:
        """The row's word: the two bars name different promises."""
        if measured < minimum:
            return "below minimum"
        if measured < target:
            return "below target"
        return "target met"

    def row(self, subject: Subject, covered: int, valid: int, measured: float) -> str:
        verdict = self.status(measured, subject.minimum, subject.target)
        return (
            f"| {subject.package} | {covered}/{valid} | {measured:.1f}% | "
            f"{subject.minimum:.0f}% | {subject.target:.0f}% | {verdict} |"
        )

    def measure(self, subject: Subject) -> Lines | None:
        """Measure one subject, or None when the report cannot answer for it
        (the diagnostic is printed here — the caller only sees the gap)."""
        rate = self.package_rate(subject.package)
        if rate is None:
            print(
                f"coverage-gate: {subject.package} is missing from the report — "
                "the merged report has no measurement for it",
                file=sys.stderr,
            )
            return None
        if rate.valid == 0:
            print(
                f"coverage-gate: {subject.package} measures no line at all — "
                "the coverage run produced nothing (a broken report, not 0%)",
                file=sys.stderr,
            )
            return None
        return rate

    def run(self) -> int:
        for subject in self.subjects:
            rate = self.measure(subject)
            if rate is None:
                return 2
            covered, valid = rate.covered, rate.valid
            self.covered_total += covered
            self.valid_total += valid
            measured = self.percent(covered / valid)
            verdict = self.status(measured, subject.minimum, subject.target)
            print(
                f"coverage-gate: {subject.package} {measured:.1f}% "
                f"(minimum {subject.minimum:.0f}%, target {subject.target:.0f}%) — {verdict}"
            )
            if verdict != "target met":
                self.failures.append(f"{subject.package} {measured:.1f}% < target {subject.target:.0f}%")
            self.rows.append(self.row(subject, covered, valid, measured))

        if self.output is not None:
            try:
                self.output.write_text(self.table() + "\n", encoding="utf-8")
            except OSError as exc:
                print(f"coverage-gate: cannot write {self.output}: {exc}", file=sys.stderr)
                return 2
        if self.failures:
            print(
                "coverage-gate: below target — " + "; ".join(self.failures)
                + ". Add the missing tests, or move the bar deliberately with its new measurement.",
                file=sys.stderr,
            )
            return 1
        return 0

    def table(self) -> str:
        """The sticky-comment markdown: one row per subject, then the total."""
        total = self.percent(self.covered_total / self.valid_total) if self.valid_total else 0.0
        return "\n".join(
            [
                "| Subject | Lines | Coverage | Minimum | Target | Status |",
                "|---|---|---|---|---|---|",
                *self.rows,
                f"| **total** | **{self.covered_total}/{self.valid_total}** | **{total:.1f}%** | — | — | — |",
            ]
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

    subjects = [Subject(package, float(minimum), float(target)) for package, minimum, target in args.subject]
    return CoverageGate(root, subjects, args.output).run()


if __name__ == "__main__":
    sys.exit(main())
