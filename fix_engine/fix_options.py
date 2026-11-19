"""The agent-supplied bits a fix may need.

`params` is the callee signature for positional-literals; `name` is the
semantic name a structural fix needs (a class, a method, a constant); `callee`
binds those params to one call when the callee is external.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class FixOptions:
    """The agent-supplied bits a fix may need: the callee signature for
    positional-literals, the class name for extract-class."""

    params: list[str] | None = None
    name: str | None = None
    # the callee whose --params these are (external callees): binds the fix to
    # THAT call on nested lines instead of the innermost spanning one
    callee: str | None = None
