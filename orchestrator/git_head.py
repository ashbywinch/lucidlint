"""Branch and short commit for report provenance."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class GitHead:
    """Branch and short commit for report provenance."""

    branch: str
    commit: str
