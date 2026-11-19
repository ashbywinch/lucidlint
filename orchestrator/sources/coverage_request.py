"""The coverage snapshot as the scan core's JSON input.

The core reads which files the repo's own snapshot names and which lines it
saw executed; a file the snapshot omits stays UNKNOWN (R6). The shape is the
core's wire contract, so it has a name here rather than a literal at the call
site.
"""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass


@dataclass(frozen=True)
class CoverageRequest:
    """One coverage snapshot for the binary: the loader's source label, the
    staleness verdict, and the covered lines per repo-relative file."""

    source: str
    stale: bool
    covered: dict[str, list[int]]

    def to_json(self) -> str:
        return json.dumps(asdict(self))
