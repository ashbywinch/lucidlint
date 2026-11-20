"""Extracting named defs out of a module into a new one.

One fix family, one class; the class holds the request, the proposal is pure
and the apply writes both files.
"""

from __future__ import annotations

from typing import NamedTuple

import libcst as cst

from fix_engine.bindings import Bindings
from fix_engine.imports import Imports


class ModuleExtraction:
    """The extract-module fix for one request: the moved defs go to a new
    module, the origin re-exports them."""

    class _ModuleProposal(NamedTuple):
        """The extract-module split result: the new origin source and the new
        module source — a named record instead of a bare tuple."""

        origin: str
        module: str

    def __init__(self, request) -> None:
        self.request = request

    def proposal(self) -> ModuleExtraction._ModuleProposal | None:
        source = self.request._loaded_source()
        opts = self.request.opts
        """(new origin source, new module source) for the extract-module split, or
        None when the split is not safe. Pure — no writes; the apply path writes
        both files. The moved defs are the --params names; the module is --name
        (same directory as the origin). Refuses when the moved code needs a
        non-moved origin def (a from-origin import in the new module would create
        the very cycle the review log §3.4 fixed) or a decorated def (registration
        semantics)."""
        if not opts.name or not opts.params:
            return None
        module = cst.parse_module(source)
        move = set(opts.params)
        top_defs = {stmt.name.value: stmt for stmt in module.body if isinstance(stmt, (cst.FunctionDef, cst.ClassDef))}
        if not move <= set(top_defs):
            return None  # a named member is not module-scope here
        for name in move:
            if top_defs[name].decorators:
                return None
        # free reads only: a name bound inside the moved function (parameter,
        # local, for/comprehension target) cannot reference a module-level
        # binding — counting it would falsely refuse safe splits (review finding)
        referenced: set[str] = set()
        for name in move:
            referenced |= Bindings(top_defs[name]).free_names()
        if referenced & (set(top_defs) - move):
            return None  # the moved code needs an origin def — cycle risk
        # module-level assignments/constants the moved code reads would be
        # missing from the new module (a runtime NameError) — refuse; moving
        # them would break the origin's remaining code (review finding)
        if referenced & Bindings.module_bindings(module):
            return None
        new_module = cst.Module(
            body=[*Imports(module).needed(referenced), *(top_defs[n] for n in opts.params)]
        )
        return ModuleExtraction._ModuleProposal(
            origin=cst.Module(body=ModuleExtraction._origin_after_move(module, move, opts, self.request.rel)).code,
            module=new_module.code,
        )

    def run(self) -> str | None:
        """The apply side of extract-module — writes the new module and returns
        the origin's new source (fix_finding writes the origin)."""
        result = self.proposal()
        if result is None:
            return None
        origin_source, new_module_source = result
        repo, rel, module_name = self.request.repo, self.request.rel, self.request.opts.name
        new_path = repo / (rel.rsplit("/", 1)[0] if "/" in rel else "") / f"{module_name}.py"
        if new_path.exists():
            return None  # never clobber an existing module
        new_path.parent.mkdir(parents=True, exist_ok=True)
        new_path.write_text(new_module_source, encoding="utf-8")
        return origin_source

    @staticmethod
    def _origin_after_move(module: cst.Module, move: set[str], opts, rel: str) -> list:
        """The origin's body after the split: the moved defs dropped, the
        re-export import inserted after the last import — every other file's
        `from origin import x` keeps working (the origin re-exports). The
        re-export is RELATIVE when the origin sits in a package (`houses/text.py`
        is not on sys.path as a top-level module) — review finding."""
        remaining = [
            s for s in module.body if not (isinstance(s, (cst.FunctionDef, cst.ClassDef)) and s.name.value in move)
        ]
        in_package = "/" in (rel or "")
        reexport = cst.SimpleStatementLine(
            body=[
                cst.ImportFrom(
                    module=cst.Name(opts.name),
                    names=[cst.ImportAlias(name=cst.Name(n)) for n in opts.params],
                    relative=[cst.Dot()] if in_package else (),
                    lpar=None,
                    rpar=None,
                )
            ]
        )
        insert_at = 0
        for i, s in enumerate(remaining):
            if ModuleExtraction._is_import_line(s):
                insert_at = i + 1
        remaining.insert(insert_at, reexport)
        return remaining


    @staticmethod
    def _is_import_line(stmt: cst.BaseStatement) -> bool:
        """A line holding exactly one import."""
        return (
            isinstance(stmt, cst.SimpleStatementLine)
            and len(stmt.body) == 1
            and isinstance(stmt.body[0], (cst.Import, cst.ImportFrom))
        )
