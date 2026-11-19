"""The `--fix` payload the orchestrator hands the scan core."""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass


@dataclass(frozen=True)
class RustFixRequest:
    """The --fix payload for the Rust scan core. A record with a to_json() —
    a wire contract is still a record (the boundary doctrine)."""

    kind: str
    file: str
    line: int
    name: str

    def to_json(self) -> str:
        return json.dumps(asdict(self))
