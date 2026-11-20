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


class _SplitLayout(NamedTuple):
    """The split's shape: one entry per produced module (its classes and the
    package name, or None for a flat sibling file), the snake names the flat
    targets use, and the decline when the shape cannot be named."""

    members: list[tuple[list[cst.ClassDef], str | None]]
    any_package: bool
    snake: dict[str, str]
    decline: str | None = None


class _SplitOutcome(NamedTuple):
    """What a split produced: the origin's new source ("" when the origin is
    deleted), the other files to write, the files to delete, and the reason it
    refused (None on success)."""

    origin: str | None
    writes: list[tuple[str, str]]
    deletes: list[str]
    decline: str | None = None


class _SplitCtx(NamedTuple):
    """Everything a class-file build needs to route the class's free names to
    the right imports: the origin identity, the other subjects, the residual
    members, the constants moving in — and the layout's import forms."""

    stem: str

    subjects: set[str]
    residual_def_names: set[str]
    moved: set[str]
    package: str | None  # None = flat sibling; else the package directory
    origin_dots: int  # relative dots for `from <origin> import X`
    member_dots: int  # relative dots for `from .<Class> import <Class>`


