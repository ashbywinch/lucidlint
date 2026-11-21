#!/usr/bin/env python3
"""Merge Cobertura reports into one — every shipped subject, one report.

`make coverage` measures three subjects: the shipped Python modules, the scan
core crate, and the radon-parity crate. CI posts ONE sticky PR comment from a
single Cobertura file, and CodeCoverageSummary renders a row per package. Each
input keeps its own package name, and its class filenames are prefixed with the
subject's place in the tree (`scanner/src/checks.rs`), so the comment reads as
the repo layout instead of three anonymous `src/` rows.

Usage:
    merge-coverage.py \\
        --report python coverage.xml "" \\
        --report scanner coverage-rust.xml scanner/ \\
        --report radonc coverage-radonc.xml scanner/radonc/ \\
        -o coverage-all.xml
"""

# lucidlint: ignore-file record-shape this script SERIALIZES the Cobertura wire
# format — the attribute maps are ElementTree's, not domain records; the two
# records below ARE named (Report, ClassCoverage), the XML stays XML.

import argparse
import sys
import time
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import NamedTuple


class Report(NamedTuple):
    """One Cobertura input: the package name it gets, its file, and the
    repo-relative prefix its class filenames take."""

    name: str
    path: Path
    prefix: str


class ClassCoverage(NamedTuple):
    """One merged class element and the line counts its package and the root
    add up — the element plus the two numbers, named."""

    element: ET.Element
    covered: int
    valid: int


class CoberturaMerge:
    """The merge itself: one package per input report, in the order given, each
    class prefixed and its rates re-derived from the line records."""

    def __init__(self, reports: list[Report]):
        self.reports = reports
        self.covered_total = 0
        self.valid_total = 0

    @staticmethod
    def rate(covered: int, valid: int) -> str:
        """Cobertura stores rates as fractions with four decimals."""
        return f"{(covered / valid) if valid else 0.0:.4f}"

    def set_rates(self, element: ET.Element, covered: int, valid: int) -> None:
        """The three rate attributes every Cobertura element carries."""
        element.set("line-rate", self.rate(covered, valid))
        element.set("branch-rate", "0")
        element.set("complexity", "0")

    @staticmethod
    def line_hits(source: ET.Element) -> dict[int, int]:
        """Hits per line NUMBER. A Cobertura file from llvm-cov carries one record
        per INSTANTIATION of a line (a generic or macro-expanded line appears more
        than once); coverage is per line, so the records collapse to the highest
        hit count — counting records instead of lines inflates both totals and
        understates the rate."""
        hits: dict[int, int] = {}
        for line in source.findall("lines/line"):
            number = line.get("number") or ""
            if not number.isdigit() or int(number) == 0:
                continue  # a missing number or line 0 is not a source line — malformed record
            count = int(line.get("hits", "0") or 0)
            hits[int(number)] = max(hits.get(int(number), 0), count)
        return hits

    def class_element(self, source: ET.Element, prefix: str) -> ClassCoverage:
        """A class element with the prefixed filename and one line record per
        number, plus its covered/valid line counts."""
        hits = self.line_hits(source)
        covered = sum(1 for count in hits.values() if count > 0)
        filename = prefix + (source.get("filename") or "")
        merged = ET.Element(
            "class",
            {
                "name": source.get("name") or Path(filename).name,
                "filename": filename,
                "line-rate": self.rate(covered, len(hits)),
                "branch-rate": "0",
                "complexity": "0",
            },
        )
        merged_lines = ET.SubElement(merged, "lines")
        for number in sorted(hits):
            ET.SubElement(merged_lines, "line", {"number": str(number), "hits": str(hits[number])})
        return ClassCoverage(merged, covered, len(hits))

    def add_package(self, packages: ET.Element, report: Report) -> None:
        """One input's package: its merged classes, its rates, its totals."""
        source = ET.parse(report.path).getroot()
        package = ET.SubElement(packages, "package", {"name": report.name})
        classes = ET.SubElement(package, "classes")
        package_covered = 0
        package_valid = 0
        for source_class in source.iter("class"):
            merged = self.class_element(source_class, report.prefix)
            if merged.valid == 0:
                continue  # a class with no line records adds no coverage data
            package_covered += merged.covered
            package_valid += merged.valid
            classes.append(merged.element)
        self.set_rates(package, package_covered, package_valid)
        self.covered_total += package_covered
        self.valid_total += package_valid

    def merged(self) -> ET.Element:
        """The merged Cobertura root: one package per subject, then the totals."""
        root = ET.Element("coverage", {"version": "merged", "timestamp": str(int(time.time()))})
        packages = ET.SubElement(root, "packages")
        for report in self.reports:
            self.add_package(packages, report)
        sources = ET.SubElement(root, "sources")
        ET.SubElement(sources, "source").text = "."
        self.set_rates(root, self.covered_total, self.valid_total)
        root.set("lines-covered", str(self.covered_total))
        root.set("lines-valid", str(self.valid_total))
        root.set("branches-covered", "0")
        root.set("branches-valid", "0")
        return root


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Merge Cobertura reports into one.")
    parser.add_argument(
        "--report",
        nargs=3,
        action="append",
        required=True,
        metavar=("NAME", "PATH", "PREFIX"),
        help="a Cobertura report: the package name it gets, its path, and the "
        "repo-relative prefix its filenames take",
    )
    parser.add_argument("-o", "--output", required=True, type=Path, help="the merged XML path")
    args = parser.parse_args(argv)

    reports: list[Report] = []
    for name, path, prefix in args.report:
        report = Path(path)
        if not report.is_file():
            print(f"merge-coverage: no report at {report}", file=sys.stderr)
            return 2
        reports.append(Report(name, report, prefix))

    merged = CoberturaMerge(reports).merged()
    try:
        ET.indent(merged)
        ET.ElementTree(merged).write(args.output, encoding="utf-8", xml_declaration=True)
    except OSError as exc:
        print(f"merge-coverage: cannot write {args.output}: {exc}", file=sys.stderr)
        return 2
    print(
        f"merge-coverage: {args.output} — {len(reports)} subjects, "
        f"{merged.get('lines-covered')}/{merged.get('lines-valid')} lines "
        f"({float(merged.get('line-rate') or 0):.1%})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
