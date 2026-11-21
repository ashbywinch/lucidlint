"""One contract finding — the scan core's wire record for an action."""

from __future__ import annotations

from typing import NamedTuple


class RustFinding(NamedTuple):
    """One contract finding — the final action model: kind (action kind,
    owned by the Rust core), signal (suppression identity), severity,
    location, message, and the raw metric the priority percentile runs on
    (cc for complexity; 1.0 for families with fixed norms)."""

    kind: str
    signal: str
    severity: str
    file: str
    line: int
    function: str
    message: str
    metric: float = 1.0
    col: int = 0  # 1-based anchor column; 0 = line-level (schema 3)
    seam_members: tuple[str, ...] = ()  # clump identity (#34) — data-clump/
    # partition/strewing only, empty elsewhere (schema 4)
    fix_kind: str = ""  # the structured fix kind (schema 4) — the
    # directive's kind when present, else the rule's default link
