#!/usr/bin/env python3
"""Fail when a Cobertura report drops below its floor — the coverage gate.

`make coverage` runs this once per shipped subject, with the floor beside the
measured number in the Makefile. A floor is a promise the suite keeps until
someone raises it deliberately; the measured number it protects is written
next to it, so raising a floor is a one-line change with its evidence.

Usage:
    coverage-gate.py coverage.xml 85 Python
"""

import argparse
import sys
import xml.etree.ElementTree as ET
from pathlib import Path


def percent(fraction: float) -> float:
    """A coverage fraction as the percentage a floor is stated in."""
    return fraction * 100  # lucidlint: ignore magic-number 100 is the definition of percent


def line_rate(report: Path) -> float | None:
    """The report's line rate as a percentage, recomputed per CLASS and per
    line NUMBER: llvm-cov emits one record per instantiation of a line, and
    counting records inflates the total (a duplicated line is still one line).
    A root attribute that disagrees with the lines cannot hide a drop. None
    when the report measures NO line — that is a broken coverage run, not 0%."""
    root = ET.parse(report).getroot()
    valid = 0
    covered = 0
    for cls in root.iter("class"):
        hits: dict[str, int] = {}
        for line in cls.findall("lines/line"):
            number = line.get("number") or ""
            if not number.isdigit():
                continue  # line 0 is not a source line — a malformed record adds no data
            count = int(line.get("hits", "0") or 0)
            hits[number] = max(hits.get(number, 0), count)
        valid += len(hits)
        covered += sum(1 for count in hits.values() if count > 0)
    return percent(covered / valid) if valid else None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Gate one Cobertura report against its floor.")
    parser.add_argument("report", type=Path, help="the Cobertura XML to check")
    parser.add_argument("floor", type=float, help="minimum line coverage, in percent")
    parser.add_argument("label", nargs="?", default="", help="the subject's name in the message")
    args = parser.parse_args(argv)

    if not args.report.is_file():
        print(f"coverage-gate: no report at {args.report}", file=sys.stderr)
        return 2
    try:
        measured = line_rate(args.report)
    except ET.ParseError as exc:
        print(f"coverage-gate: {args.report} is not parseable: {exc}", file=sys.stderr)
        return 2

    subject = f"{args.label}: " if args.label else ""
    if measured is None:
        print(
            f"coverage-gate: {subject}{args.report} measures no line at all — "
            "the coverage run produced nothing (a broken report, not 0%)",
            file=sys.stderr,
        )
        return 2
    print(f"coverage-gate: {subject}{measured:.1f}% (floor {args.floor:.0f}%)")
    if round(measured, 1) < round(args.floor, 1):
        print(
            f"coverage-gate: {subject}{measured:.1f}% is below the {args.floor:.0f}% floor — "
            "add the missing tests, or lower the floor deliberately with its new measurement",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
