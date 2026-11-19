"""One scanned source file: its path and its repo-relative name."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class SourceFile:
    """One scanned .py file: its path and repo-relative name."""

    py: Path
    rel: str
