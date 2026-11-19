"""The coverage snapshot plus its staleness verdict, as one scan input."""

from __future__ import annotations

from dataclasses import dataclass

from orchestrator.coverage_result import CoverageResult


@dataclass(frozen=True)
class CoverageInput:
    """The repo's own snapshot plus its staleness verdict — the one coverage
    input the scan core needs to decide whether it can claim a file is
    unverified (R6): a stale snapshot downgrades the claim to verify."""

    result: CoverageResult
    stale: bool
