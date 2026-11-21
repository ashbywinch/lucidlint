"""One seam cluster as the report renders it.

A dict subclass (a document fragment): the text surface indexes it, the JSON
serialises it, and the shape has a name, so the two cannot drift.
"""

from __future__ import annotations

from typing import Any


class SeamGroup(dict[str, Any]):
    """One seam cluster as the report renders it: its heading, the seam's name,
    and the finding positions. A document fragment — the text surface and the
    JSON read the same fields."""
