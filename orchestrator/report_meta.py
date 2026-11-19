"""The JSON report's provenance block and the thresholds it was produced under
(the wire format, PRD R18)."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class ReportMeta:
    """The report document's provenance and the thresholds it was produced
    under (the JSON wire format, PRD R18)."""

    repo: str
    tool_version: str
    branch: str
    commit: str
    generated_at: str
    coverage_source: str
    thresholds: dict[str, float]
