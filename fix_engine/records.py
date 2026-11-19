"""The fix engine's named records.

Each one replaces a bare tuple or a constant-key dict whose positions a call
site had to remember: the loop plan, the chain facts, the split layout, the
partition plans, the record-build facts. A module of records is one concept —
the shapes the fixers pass — not a set of concerns.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import NamedTuple

import libcst as cst


class _ModuleProposal(NamedTuple):
    """The extract-module split result: the new origin source and the new
    module source — a named record instead of a bare tuple."""

    origin: str
    module: str

@dataclass
class _RecordBuild:
    """The record's dict names + the first build's elements (for the field
    names) — a named return instead of a bare tuple."""

    names: set[str]
    elements: list[cst.BaseExpression] | None

class _RecordDictHit(NamedTuple):
    """One candidate literal: anchor position + the node + its const keys."""

    line: int
    col: int
    node: cst.Dict
    keys: list[str]

class _Collaborator(NamedTuple):
    """One `graph = self.graph` alias: the local name and the owner field it
    aliases — a named return instead of a bare tuple."""

    alias: str
    attr: str

class _StmtReads(NamedTuple):
    """One top-level statement's analysis for the feature-envy move: the
    names it reads (bound names excluded), the names it binds, and whether
    it touches self — a named return instead of a bare tuple."""

    reads: set[str]
    bound: set[str]
    has_self: bool

@dataclass
class _LoopPipelinePlan:
    """One loop's accumulation shape for the comprehension rewrite: the
    accumulator, the collection kind, and the element/key/flatten exprs."""

    acc: str
    collection: str  # "list" | "set" | "dict"
    element: cst.BaseExpression | None  # the appended/added value or the dict value
    key: cst.BaseExpression | None  # the dict key (subscript store) or None
    flatten: cst.BaseExpression | None  # the extend/`+= [a, b]` value whose ITEMS join
    if_test: cst.BaseExpression | None  # the one-filter test or None

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

class _WideTupleSite(NamedTuple):
    """One fixed-arity tuple annotation on the anchor line: where it lives,
    the annotated name, its arity, and the build site's element expressions
    (None when the annotation has no matching tuple literal)."""

    where: str
    var: str
    arity: int
    annotation: cst.BaseExpression
    elements: tuple[cst.BaseExpression, ...] | None

class _Clump(NamedTuple):
    """The recovered clump: the shared pair (in the anchor's signature order)
    and the module-level defs carrying it, in source order."""

    pair: tuple[str, str]
    fns: tuple[cst.FunctionDef, ...]

class _PartitionGroup(NamedTuple):
    """One field-disjoint method group: its methods in source order and the
    fields only they touch."""

    methods: tuple[cst.FunctionDef, ...]
    fields: tuple[str, ...]

class _GroupInit(NamedTuple):
    """One group's synthesized __init__: the field assignments it keeps and
    the parameters those assignments need (name, annotation, default)."""

    statements: list[cst.BaseStatement]
    params: list[cst.Param]

class _PartitionPlan(NamedTuple):
    groups: tuple[_PartitionGroup, ...]
    init_fields: tuple[tuple[str, cst.BaseExpression], ...]
    init_docstring: cst.BaseStatement | None
    init_params: tuple[tuple[str, cst.Annotation | None, cst.BaseExpression | None], ...]

class _PartitionAttempt(NamedTuple):
    """The plan, or the specific reason it cannot be computed."""

    plan: _PartitionPlan | None
    decline: str
