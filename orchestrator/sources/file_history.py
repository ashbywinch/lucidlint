"""One repo's git history: churn per file and the last-touched epoch.

The record holds the two facts the report needs; the scoring and the date
rendering are its own, because the score IS the churn-weighted metric and the
date IS the commit epoch.
"""

from __future__ import annotations

import datetime
import math
from collections import Counter
from dataclasses import dataclass


@dataclass
class FileHistory:
    """Per-file change counts and last-touched dates from git history."""

    # the weight an unlisted family takes: neutral, half of the scale
    _NEUTRAL_WEIGHT = 0.5

    churn: Counter[str]
    last_modified: dict[str, str]

    # how much history the churn signal needs: 200 commits reaches ~2 weeks on
    # an active repo and years on a quiet one — the percentile ranking is
    # relative within the window, so the bound adapts to the repo's activity;
    # the age cap is the hard floor for the "is this still live?" judgement
    MAX_COMMITS = 200
    MAX_AGE_DAYS = 730

    @staticmethod
    def raw_score(kind: str, metric: float, churn: int, callers: int | None = None) -> float:
        """Continuous risk score: normalized metric x root-weighted churn factor
        (x fan-in for high-risk).

        Normalized to a 1-99 percentile ranking in main, so the list spreads
        instead of saturating at 99. The churn factor is the SQUARE ROOT of the
        linear one: a 10x churn gap is ~3.2x raw risk, not 10x, so complexity
        and size keep equal weight with churn (F5 — the top line names the code
        whose change costs the most, and a big untouched function costs more
        than a small heavily-churned one).
        """
        # family -> normalizer: the values SELECT the normalization (a size
        # metric saturates at the fail bar it is measured against, a
        # whole-number family keeps its fixed 0.5-0.7 weight), so this is an
        # ordered lookup: first match wins, and an unknown family takes the
        # neutral weight
        # each metric saturates at its own fail bar; the churn and caller
        # boosts are capped so no single fact dominates the score
        saturation = {
            "complexity": 40,
            "large-function": 200,
            "hub-file": 400,
            "hotspot": 40,
        }
        churn_cap = 30
        caller_cap = 5
        churn_boost_cap = 1.5
        caller_boost_cap = 1.0
        table = (
            ("latent-class", 0.7),
            ("standard", 0.6),
            ("docs", 0.5),
            ("folder-mix", 0.5),
            ("layer-mix", 0.5),
            ("vague-name", 0.7),
            ("record-shape", 0.7),
            ("complexity", min(metric / saturation["complexity"], 1.0)),
            ("large-function", min(metric / saturation["large-function"], 1.0)),
            ("hub-file", min(metric / saturation["hub-file"], 1.0)),
            ("hotspot", min(metric / saturation["hotspot"], 1.0)),
            ("high-risk", min(metric, 1.0)),
        )
        # an unknown family takes the neutral weight
        norm = next((weight for family, weight in table if family == kind), FileHistory._NEUTRAL_WEIGHT)
        score = norm * math.sqrt(1 + min(churn / churn_cap, churn_boost_cap))
        if kind == "high-risk" and callers:
            score *= 1 + min(callers / caller_cap, caller_boost_cap)
        return score

    @staticmethod
    def iso_day(value: str) -> str:
        """The epoch git hands us, as a date a reader can place in time."""
        try:
            return datetime.date.fromtimestamp(int(value)).isoformat()
        except (ValueError, OSError, OverflowError):
            return value

    @staticmethod
    def tracked_churn_path(path: str) -> bool:
        """A source/doc path the churn scan counts."""
        return path.endswith((".py", ".rs", ".md")) and not path.startswith((".venv/", "node_modules/"))
