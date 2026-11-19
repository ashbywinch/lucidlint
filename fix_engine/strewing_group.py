"""The strewing seam: the functions that share a leading parameter, and the
shared parameter itself."""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class StrewingGroup:
    """The free-function group extract-class moves — shared leading type,
    the fn names in source order, the anchor line."""

    shared: str
    fns: list[str] = field(default_factory=list)
    anchor: int = 0
