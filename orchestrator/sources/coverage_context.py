"""The provenance and staleness verdict for one run's coverage snapshot."""

from __future__ import annotations

import time
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path


@dataclass
class CoverageContext:
    """Coverage provenance + staleness verdict for the run."""

    label: str
    graph_preferred: bool
    stale_note: str


def coverage_context(
    repo: Path, covered: Mapping[str, set[int]] | None, coverage_source: str
) -> CoverageContext:
    """Coverage provenance label + staleness verdict. The snapshot compared is
    the one the loader actually read (coverage.xml, else `.coverage`) — using
    the other file's mtime either hides a stale snapshot or invents one."""
    snapshot = repo / "coverage.xml" if (repo / "coverage.xml").exists() else repo / ".coverage"
    if coverage_source == ".coverage" and (repo / ".coverage").exists():
        coverage_source += (
            " (mtime " + time.strftime("%Y-%m-%d %H:%M", time.localtime((repo / ".coverage").stat().st_mtime)) + ")"
        )
    graph_preferred = False
    stale_note = ""
    if covered is not None and snapshot.exists():
        cov_mtime = snapshot.stat().st_mtime
        newest_test = max((p.stat().st_mtime for p in (repo / "tests").rglob("*.py")), default=0.0)
        if newest_test > cov_mtime:
            graph_preferred = True
            stale_note = (
                " (coverage snapshot older than the repo's tests — graph verdict used; "
                "verify against htmlcov/ if present)"
            )
    return CoverageContext(label=coverage_source, graph_preferred=graph_preferred, stale_note=stale_note)
