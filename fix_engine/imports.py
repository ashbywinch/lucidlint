"""The origin's imports, and which of them a moved name needs.

A definition moving into a new module keeps working only when the imports that
bind the names it reads travel with it. `Imports(module)` is that question
asked of one parsed module, so the fixers that build a new file (split-module,
extract-module) answer it the same way.
"""

from __future__ import annotations

import libcst as cst

from fix_engine.bindings import Bindings


class Imports:
    """One module's module-level imports, as sources for a moved definition."""

    def __init__(self, module: cst.Module) -> None:
        self.module = module

    def needed(self, referenced: set[str]) -> list:
        """The origin's module-level imports binding a referenced name — what the
        new module needs (libcst wraps imports in SimpleStatementLine)."""
        moved: list = []
        for stmt in self.module.body:
            if not isinstance(stmt, cst.SimpleStatementLine):
                continue
            # a line can hold several imports (`import os; import sys`) — emit
            # each matching import as its own statement, or the new module
            # misses a name the moved def needs and NameErrors (review bot)
            for inner in stmt.body:
                if isinstance(inner, cst.ImportFrom) and isinstance(inner.names, cst.ImportStar):
                    continue  # `from x import *` binds no importable name
                if not isinstance(inner, (cst.Import, cst.ImportFrom)):
                    continue
                if Imports._binds_referenced(inner, referenced):
                    moved.append(cst.SimpleStatementLine(body=[inner]))
        return moved

    @staticmethod
    def _binds_referenced(importer: cst.Import | cst.ImportFrom, referenced: set[str]) -> bool:
        """Does any alias of this import statement bind a referenced name?"""
        if isinstance(importer.names, cst.ImportStar):
            return False
        return any(Imports._alias_bound(alias, importer) & referenced for alias in importer.names)

    @staticmethod
    def _alias_bound(alias: cst.ImportAlias, importer) -> set[str]:
        """The names one import alias binds: its asname, else `a.b.c`'s base for
        `import`, else the imported name for `from ... import`."""
        if alias.asname is not None:
            return Bindings.hoist_target_names(alias.asname.name)
        if isinstance(importer, cst.Import):
            return {Bindings.import_base_name(alias.name)}
        return {alias.name.value} if isinstance(alias.name, cst.Name) else set()
