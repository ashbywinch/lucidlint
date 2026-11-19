"""The whole JSON report document: provenance, the run's suppression facts,
and the actions in priority order.

A record, so the renderer builds it once and serialises it with `asdict` — a
key cannot drift from the field that produces it.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from orchestrator.report_meta import ReportMeta
from orchestrator.seam_group import SeamGroup


@dataclass(frozen=True)
class ReportDocument:
    """The whole JSON report: provenance, the run's suppression facts, and the
    actions in priority order."""

    meta: ReportMeta
    baseline: str
    config_ignored: dict[str, int]
    header: str
    suppressions: dict[str, int]
    actions: list[dict[str, Any]]
    naming_notice: str | None
    groups: list[SeamGroup]
