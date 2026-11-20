"""The module under repair: its text parsed into one metadata tree.

Every node a fixer holds must come from the wrapper's tree, or the parent and
position lookups miss — so the parse boundary is one value, and a fixer reads
`source.module` and `source.wrapper` instead of remembering two positions.
"""

from __future__ import annotations

from typing import NamedTuple

import libcst as cst


class ModuleSource(NamedTuple):
    """A parsed module and the metadata wrapper whose tree it is."""

    module: cst.Module
    wrapper: cst.MetadataWrapper

    @classmethod
    def parse(cls, source: str) -> ModuleSource:
        """Parse `source` into the wrapper's own tree."""
        wrapper = cst.MetadataWrapper(cst.parse_module(source))
        return cls(module=wrapper.module, wrapper=wrapper)
