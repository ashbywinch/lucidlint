"""The optional scan inputs — one object instead of a parameter tail."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path


@dataclass
class ScanFlags:
    """The optional scan inputs the binary takes as paths or switches."""

    graph: Path | None = None
    churn_json: Path | None = None
    coverage: Path | None = None
    include_tests: bool = False
    docs_root: str | None = None
    gitignored: tuple[str, ...] = ()
