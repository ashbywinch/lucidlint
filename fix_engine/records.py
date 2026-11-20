"""The fix engine's named records.

Each one replaces a bare tuple or a constant-key dict whose positions a call
site had to remember: the loop plan, the chain facts, the split layout, the
partition plans, the record-build facts. A module of records is one concept —
the shapes the fixers pass — not a set of concerns.
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


@dataclass(frozen=True)
class _ForwardTarget:
    """F/M's forwarder callee: a module fn (is_name) or a method on another
    object ((recv-param, method))."""

    is_name: bool
    name: str = ""
    attr_method: tuple[str, str] | None = None


class _ChainHead(NamedTuple):
    """A forwarding chain's head: the method, its class, the parameters it
    passes through, and the call it forwards to."""

    class_name: str
    class_node: cst.ClassDef
    method: cst.FunctionDef
    params: list[str]
    callee: _ForwardTarget


class _ChainMiddle(NamedTuple):
    """The module function the head forwards into: where it lives, the module
    it belongs to, its parameters, and where that forwards."""

    rel: str
    module: cst.Module
    params: list[str]
    callee: _ForwardTarget


class _ModuleFn(NamedTuple):
    """One module-level function resolved repo-wide: the file it lives in, the
    parsed module, and its node — the three facts a caller needs together."""

    rel: str
    module: cst.Module
    node: cst.FunctionDef


class _ChainTerminal(NamedTuple):
    """The method the chain reaches through its middle: D.N, plus the
    receiver parameter the head passes for it."""

    recv: str
    name: str
    rel: str
    cls: cst.ClassDef
    method: cst.FunctionDef


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


class _ModuleRef(NamedTuple):
    """One module-level name in its repo: the thing the split and husk lookups
    ask about, as one parameter instead of a (repo, name) pair everywhere."""

    repo: Path
    name: str
