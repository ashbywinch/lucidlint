"""The fix engine's named records.

Each one replaces a bare tuple or a constant-key dict whose positions a call
site had to remember: the split's origin, layout, context and outcome, and the
extract-module proposal. A module of records is one concept — the shapes the
splitter passes — not a set of concerns.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import NamedTuple

import libcst as cst


class _ModuleProposal(NamedTuple):
    """The extract-module split result: the new origin source and the new
    module source — a named record instead of a bare tuple."""

    origin: str
    module: str


