"""One finding, and the keys that identify and merge it.

The action carries the identity its consumers need: the unique key a baseline
acknowledges, the per-position key that keeps two record-shape findings on one
signature line distinct, and the merge key that folds echo carriers into one
action. They are the action's own, so no module restates them.
"""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class Action:
    """One finding. Kind families: complexity/large-function merge per target;
    hotspot/hub-file/high-risk/record-shape are separate problems."""

    kind: str
    severity: str
    file: str
    line: int
    function: str
    message: str
    metric: float
    churn: int
    last_modified: str
    tested: str
    signal: str = ""  # suppression identity — the raw family kind from the scanner
    note: str = ""
    raw: float = 0.0
    priority: int = 0
    kinds: list[str] = field(default_factory=list)
    callers: list[str] = field(default_factory=list)
    col: int = 0  # schema-3 anchor column; 0 = line-level
    seam_members: tuple[str, ...] = ()  # clump identity (#34) — the seam
    # grouping keys on this; the three carriers carry it, others empty
    fix_kind: str = ""  # the structured fix kind (schema 4) — the directive's
    # kind when present, else the rule's default link (Rule.fix); empty =
    # fix-less finding, never constructed into a Fix

    def key(self) -> str:
        """The action's identity: one finding per (kind, file, line, function)."""
        return f"{self.kind}:{self.file}:{self.line}:{self.function}"

    def position_key(self) -> str:
        """record-shape reports per annotated position; its message names the
        parameter or return, so two positions on one signature line stay
        distinct. Other kinds are per-line — their duplicates are echoes."""
        return self.message if self.kind == "record-shape" else ""

    def merge_key(self) -> tuple:
        """complexity + large-function on the same function are ONE fix; every
        other kind is per-LINE — two positional-literals calls in one function
        are two different fixes at two lines, and merging them would hide one
        finding behind the other (the fix loop would thrash). record-shape is
        per-POSITION: its message names the parameter or return, so two
        annotated positions on one signature line stay distinct."""
        if self.kind in ("complexity", "large-function"):
            return (self.file, self.function, "fn")
        return (self.file, self.function, self.line, self.kind, self.position_key())
