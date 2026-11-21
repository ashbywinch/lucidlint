"""One scan's output: findings per repo-relative path, complexity, and the
run's header and suppression census."""

from __future__ import annotations

from typing import NamedTuple

from orchestrator.rust_finding import RustFinding


class RustFindings(NamedTuple):
    """The Rust scan output for one file set, keyed by repo-relative path —
    a named object, not a bare map (the record-shape rule's escape hatch)."""

    by_rel: dict[str, list[RustFinding]]
    cc_by_rel: dict[str, list[tuple[str, int, int]]]
    header: str = ""
    suppressions: dict[str, int] | None = None

    def for_rel(self, rel: str) -> list[RustFinding]:
        return self.by_rel.get(rel, [])
