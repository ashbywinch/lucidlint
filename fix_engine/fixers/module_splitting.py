"""Splitting a multi-class module so every class lives in its own file.

One fix family, one class; the class holds the request, the split itself is a
nested operation (its phases read in order), and its libcst visitors are
nested classes.
"""

from __future__ import annotations

import re
from collections.abc import Sequence
from pathlib import Path
from typing import NamedTuple, cast, override

import libcst as cst
from libcst.metadata import ParentNodeProvider

from fix_engine.bindings import Bindings
from fix_engine.imports import Imports
from fix_engine.module_outline import ModuleOutline
from fix_engine.module_source import ModuleSource
from fix_engine.source_files import SourceFiles

# lucidlint: ignore-file record-shape the libcst layer's tuple/dict shorthands ARE its wire records —
# lucidlint: ignore-file record-shape a class per helper hop is ceremony


class ModuleSplitting:
    """The split-module fix for one request: the misplaced classes move out."""

    class _SplitOrigin(NamedTuple):
        """Where the module being split lives: its repo-relative path, the
        directory segments that decide flat-vs-package, its stem (the file name
        without `.py`), and the dotted path the split's imports resolve through."""

        rel: str
        directory: str
        segments: list[str]
        stem: str

        @property
        def in_package(self) -> bool:
            return bool(self.segments)

        @property
        def origin_path(self) -> list[str]:
            return self.segments + [self.stem]

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

    def __init__(self, request) -> None:
        self.request = request

    def run(self) -> str | None:
        """Split a multi-class module (#31): the misplaced public classes move
        to files named after them — flat siblings when independent, a package
        when cohesive; the residual (module functions, ownerless constants,
        private classes, the stem-matching class) stays in the origin (FLAT)
        or in `mod/__init__.py` (PACKAGE); an origin that becomes empty is
        deleted. REFUSES (self.request.decline) on target collisions, import cycles
        in the produced layout, and executable statements between the
        classes; package layouts are refused when a module-level constant
        cannot move into a class (the __init__ holds no constants, BQ6), and
        relative imports in the origin refuse the package layout and fall
        the split back to flat."""
        if not self.request.rel.endswith(".py") or self.request.rel.endswith("__init__.py"):
            return None  # the scanner never flags __init__ — nothing to split
        module, wrapper = ModuleSource.parse(self.request.loaded_source())
        origin = ModuleSplitting._split_origin(self.request.rel)
        subjects = ModuleSplitting._split_subjects(module.body, origin.stem)
        if not subjects:
            return None  # stale — no misplaced public class remains
        refusal = ModuleSplitting._refuse_split_reordering(module.body, subjects)
        if refusal is not None:
            self.request.decline = refusal
            return None
        outcome = ModuleSplitting._Split(self.request.repo, origin, module, subjects).execute(
            wrapper, self.request.opts.name
        )
        if outcome.decline is not None:
            self.request.decline = outcome.decline
            return None
        self.request.extra_writes = outcome.writes
        self.request.deletes.extend(outcome.deletes)
        return outcome.origin

    @staticmethod
    def _module_parts(rel: str) -> list[str]:
        """A module's import path: its directory parts plus the stem."""
        return rel.split("/")[:-1] + [rel.rsplit("/", 1)[-1][:-3]]

    @staticmethod
    def _class_matches_stem(name: str, stem: str) -> bool:
        """A class name matches the file stem case-insensitively, with the
        plural-underscore form tolerated (`APIKey` in api_key.py)."""
        n = name.lower()
        return n == stem or n == stem.replace("_", "")

    @staticmethod
    def _snake_name(name: str) -> str:
        """CamelCase -> snake_case, acronym-aware (`APIKey` -> api_key, not
        a_p_i_key) — the flat sibling file's stem for a moved class."""
        s = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", name)
        s = re.sub(r"(?<=[A-Z])(?=[A-Z][a-z])", "_", s)
        return s.lower()

    @staticmethod
    def _class_free_names(cls: cst.ClassDef) -> set[str]:
        """The names a class READS from the enclosing module: every Name in its
        subtree minus the names bound inside it and minus its own name."""
        return Bindings(cls).free_names() - {cls.name.value}

    @staticmethod
    def _free_names_of(stmt) -> set[str]:
        """The names one module-level statement reads from the module scope
        (bindings inside it excluded) — the residual's references to moved
        classes and constants."""
        return Bindings(stmt).free_names()

    @staticmethod
    def _class_attr_names(cls: cst.ClassDef) -> set[str]:
        """The class-level attribute names a class binds directly in its body —
        the shared-class-level-attribute cohesion edge."""
        names: set[str] = set()
        for stmt in cls.body.body:
            if not isinstance(stmt, cst.SimpleStatementLine):
                continue
            for inner in stmt.body:
                if isinstance(inner, cst.Assign):
                    for t in inner.targets:
                        if isinstance(t.target, cst.Name):
                            names.add(t.target.value)
                elif isinstance(inner, cst.AnnAssign) and isinstance(inner.target, cst.Name):
                    names.add(inner.target.value)
        return names

    @staticmethod
    def _stmt_is_plain_assign(stmt) -> bool:
        """An import / pass / plain-name assignment (a constant) — the module
        statements allowed between the classes of a module being split."""
        if not isinstance(stmt, cst.SimpleStatementLine):
            return False
        for inner in stmt.body:
            if isinstance(inner, (cst.Import, cst.ImportFrom, cst.Pass)):
                continue
            if isinstance(inner, cst.Assign) and all(isinstance(t.target, cst.Name) for t in inner.targets):
                continue
            if isinstance(inner, cst.AnnAssign) and isinstance(inner.target, cst.Name):
                continue
            return False
        return True

    @staticmethod
    def _module_constants(top: Sequence) -> list[tuple[str, cst.SimpleStatementLine]]:
        """The module-level constants in source order: single-target Assign /
        AnnAssign to plain names (the BQ6 subject set)."""
        out: list[tuple[str, cst.SimpleStatementLine]] = []
        for stmt in top:
            if not isinstance(stmt, cst.SimpleStatementLine) or len(stmt.body) != 1:
                continue
            inner = stmt.body[0]
            if ModuleSplitting._is_single_name_assign(inner):
                assert isinstance(inner, cst.Assign) and isinstance(inner.targets[0].target, cst.Name)
                out.append((inner.targets[0].target.value, stmt))
            elif isinstance(inner, cst.AnnAssign) and inner.value is not None and isinstance(inner.target, cst.Name):
                out.append((inner.target.value, stmt))
        return out

    @staticmethod
    def _const_value(stmt: cst.SimpleStatementLine) -> cst.BaseExpression:
        inner = stmt.body[0]
        if isinstance(inner, cst.Assign):
            return inner.value
        if isinstance(inner, cst.AnnAssign) and inner.value is not None:
            return inner.value
        return cst.Name("")

    @staticmethod
    def _module_has_relative_imports(module: cst.Module) -> bool:
        return any(
            isinstance(s, cst.SimpleStatementLine) and any(isinstance(i, cst.ImportFrom) and i.relative for i in s.body)
            for s in module.body
        )

    @staticmethod
    def _name_ref_kind(wrapper: cst.MetadataWrapper, node: cst.Name) -> str:
        """A Name reference's execution context: 'skip' (a binding or a
        non-reference position), 'deferred' (evaluates at call time — inside a
        function body), or 'immediate' (evaluates at import/class-definition
        time — module statements, class bodies, defaults, decorators)."""
        parents = wrapper.resolve(ParentNodeProvider)
        parent = parents.get(node)
        if parent is None:
            return "skip"
        if isinstance(parent, cst.AssignTarget):
            return "skip"
        if isinstance(parent, cst.AnnAssign) and parent.target is node:
            return "skip"
        if isinstance(parent, cst.Param) and parent.name is node:
            return "skip"
        if isinstance(parent, cst.For) and parent.target is node:
            return "skip"
        if isinstance(parent, cst.CompFor) and parent.target is node:
            return "skip"
        if isinstance(parent, cst.AsName):
            return "skip"
        if isinstance(parent, cst.ImportAlias):
            return "skip"
        if isinstance(parent, cst.NamedExpr) and parent.target is node:
            return "skip"
        if isinstance(parent, cst.Arg) and parent.keyword is node:
            return "skip"
        if isinstance(parent, cst.Attribute) and parent.attr is node:
            return "skip"
        if isinstance(parent, cst.ClassDef) and parent.name is node:
            return "skip"
        if isinstance(parent, cst.FunctionDef) and parent.name is node:
            return "skip"
        cur = parent
        while cur is not None:
            if isinstance(cur, cst.IndentedBlock):
                p = parents.get(cur)
                if isinstance(p, cst.FunctionDef) and p.body is cur:
                    return "deferred"
            if isinstance(cur, cst.FunctionDef):
                return "immediate"
            cur = parents.get(cur)
        return "immediate"

    @staticmethod
    def _exec_time_names(wrapper: cst.MetadataWrapper, node) -> set[str]:
        """The names `node` reads at evaluation time (function bodies deferred) —
        the residual check that refuses package layouts whose import-time code
        touches a moving class."""

        class _ExecNames(cst.CSTVisitor):
            def __init__(self) -> None:
                self.names: set[str] = set()

            @override
            def visit_Name(self, node) -> None:
                if ModuleSplitting._name_ref_kind(wrapper, node) == "immediate":
                    self.names.add(node.value)

        probe = _ExecNames()
        node.visit(probe)
        return probe.names

    @staticmethod
    def _consts_moving_into(
        const_stmts: list[tuple[str, cst.SimpleStatementLine]],
        class_refs: dict[int, set[str]],
        residual_refs: set[str],
        external_consts: set[str],
        cls: cst.ClassDef,
    ) -> set[str]:
        """The constants that may move INTO this class as class attributes (BQ6):
        every reader is inside the class's own subtree (a residual or foreign
        reader, or an external `from mod import CONST`, keeps it in the origin),
        the class does not already bind the name, and the value references only
        literals, builtins, or earlier co-moving constants (a class body
        evaluates sequentially — a later constant is not yet bound, and a
        constant that STAYS in the origin must not read a moved one)."""
        cls_id = id(cls)
        own = class_refs.get(cls_id, set())
        others = {n for cid, refs in class_refs.items() if cid != cls_id for n in refs}
        order = {n: i for i, (n, _s) in enumerate(const_stmts)}
        value_of = {n: ModuleSplitting._const_value(s) for n, s in const_stmts}
        value_refs_of = {n: Bindings.expr_names(v) - Bindings.BUILTINS for n, v in value_of.items()}
        readers_via_value: dict[str, set[str]] = {}
        for reader, vrefs in value_refs_of.items():
            for k in vrefs:
                readers_via_value.setdefault(k, set()).add(reader)
        candidates = {
            n
            for n, _s in const_stmts
            if n in own
            and n not in residual_refs
            and n not in others
            and n not in external_consts
            and not Bindings.class_body_binds(cls, n)
        }
        while True:
            changed = False
            for n in list(candidates):
                # every constant that READS n through its value must co-move
                if any(m not in candidates for m in readers_via_value.get(n, ())):
                    candidates.discard(n)
                    changed = True
                    continue
                vrefs = value_refs_of[n]
                if not vrefs <= candidates or any(order.get(m, -1) >= order[n] for m in vrefs):
                    candidates.discard(n)
                    changed = True
            if not changed:
                break
        if not (candidates & own):
            return set()  # nothing the class directly reads — ownerless constants stay
        return candidates

    class _RewriteMovedConstRefs(cst.CSTTransformer):
        """Method-body references to a constant that moved into the class become
        `Cls.CONST` (a bare name does not resolve through the method scope);
        class-body references stay bare (the class scope resolves them), and
        binding positions are never touched."""

        def __init__(self, wrapper: cst.MetadataWrapper, moved: set[str], cls_name: str) -> None:
            self.wrapper = wrapper
            self.moved = moved
            self.cls_name = cls_name

        @override
        def leave_Name(self, original_node, updated_node) -> cst.BaseExpression:
            if updated_node.value not in self.moved:
                return updated_node
            try:
                kind = ModuleSplitting._name_ref_kind(self.wrapper, original_node)
            except KeyError:
                return updated_node  # a freshly inserted node — not a pre-split reference
            if kind != "deferred":
                return updated_node
            return cst.Attribute(value=cst.Name(self.cls_name), attr=updated_node)

    @staticmethod
    def _class_with_moved_consts(
        wrapper: cst.MetadataWrapper, cls: cst.ClassDef, moved: set[str], cls_name: str
    ) -> cst.ClassDef:
        """Insert the moved constants as class attributes (in their original
        source order, after the docstring) and rewrite the method-body references
        to them as `Cls.CONST`."""
        if not moved:
            return cls
        attrs = [s for n, s in ModuleSplitting._module_constants(wrapper.module.body) if n in moved]
        body = list(cls.body.body)
        insert = 1 if body and ModuleOutline.is_docstring_stmt(body[0]) else 0
        new_body = body[:insert] + attrs + body[insert:]
        cls2 = cls.with_changes(body=cls.body.with_changes(body=new_body))
        return cast(cst.ClassDef, cls2.visit(ModuleSplitting._RewriteMovedConstRefs(wrapper, moved, cls_name)))

    @staticmethod
    def _split_origin(rel: str) -> ModuleSplitting._SplitOrigin:
        """The module's location facts, derived once from its repo-relative path."""
        directory = rel.rsplit("/", 1)[0] if "/" in rel else ""
        return ModuleSplitting._SplitOrigin(
            rel=rel,
            directory=directory,
            segments=directory.split("/") if directory else [],
            stem=rel.rsplit("/", 1)[-1].removesuffix(".py"),
        )

    @staticmethod
    def _split_subjects(top: Sequence, stem: str) -> list[cst.ClassDef]:
        """The misplaced public classes: not private, not the stem-matching class
        (that one belongs to the module's own name)."""
        return [
            stmt
            for stmt in top
            if isinstance(stmt, cst.ClassDef)
            and not stmt.name.value.startswith("_")
            and not ModuleSplitting._class_matches_stem(stmt.name.value, stem)
        ]

    @staticmethod
    def _refuse_split_reordering(top: Sequence, subjects: list[cst.ClassDef]) -> str | None:
        """Module-level executable statements BETWEEN the first and last subject
        class would be reordered by the split — decline, never reorder."""
        subject_ids = {id(s) for s in subjects}
        bounds = [i for i, stmt in enumerate(top) if id(stmt) in subject_ids]
        for stmt in top[bounds[0] : bounds[-1] + 1]:
            if id(stmt) in subject_ids or isinstance(stmt, cst.EmptyLine):
                continue
            if isinstance(stmt, (cst.ClassDef, cst.FunctionDef)):
                continue
            if ModuleSplitting._stmt_is_plain_assign(stmt):
                continue
            return "module-level executable statements between classes would be reordered by the split — split by hand"
        return None

    @staticmethod
    def _union_find(parent: dict[int, int], node: int) -> int:
        """One union-find root lookup — the clustering's only shared state."""
        while parent[node] != node:
            parent[node] = parent[parent[node]]
            node = parent[node]
        return node

    class _Split:
        """One module split (#31), as the operation it is: the module's misplaced
        classes, the constants that move with them, the residual that stays, and
        the layout the split produces. Each step is named after what it decides —
        plan the constant moves, cluster, name the layout, refuse the unsafe
        shapes, build the sources — so the phases read in order."""

        def __init__(
            self, repo: Path, origin: ModuleSplitting._SplitOrigin, module: cst.Module, subjects: list[cst.ClassDef]
        ) -> None:
            self.repo = repo
            self.origin = origin
            self.module = module
            self.subjects = subjects
            self.subject_names = {subject.name.value for subject in subjects}
            self.const_stmts = ModuleSplitting._module_constants(module.body)
            self.const_names = {name for name, _stmt in self.const_stmts}
            self.moved_consts = self._moved_constants()
            self.moved_all = set().union(*self.moved_consts.values()) if self.moved_consts else set()
            self.residual = [
                stmt
                for stmt in module.body
                if id(stmt) not in {id(s) for s in subjects}
                and id(stmt) not in {id(s) for _name, s in self.const_stmts}
            ]
            self.residual_def_names = {
                stmt.name.value for stmt in self.residual if isinstance(stmt, (cst.FunctionDef, cst.ClassDef))
            }
            self.residual_def_names |= self.const_names - self.moved_all
            self.residual_subject_refs = self._residual_subject_refs()
            self.free_of = {id(c): ModuleSplitting._class_free_names(c) for c in subjects}
            self.attrs_of = {id(c): ModuleSplitting._class_attr_names(c) for c in subjects}

        def _moved_constants(self) -> dict[int, set[str]]:
            """Which module constants each subject class reads, and which of those
            no residual statement and no other module needs — only those move."""
            const_def_ids = {id(stmt) for _name, stmt in self.const_stmts}
            subject_ids = {id(subject) for subject in self.subjects}
            class_refs: dict[int, set[str]] = {}
            residual_refs: set[str] = set()
            for stmt in self.module.body:
                if id(stmt) in const_def_ids:
                    continue
                names = ModuleSplitting._free_names_of(stmt) & self.const_names
                if isinstance(stmt, cst.ClassDef) and id(stmt) in subject_ids:
                    class_refs[id(stmt)] = names
                else:
                    residual_refs |= names
            external = {name for name in self.const_names if self._const_imported_elsewhere(name)}
            return {
                id(subject): ModuleSplitting._consts_moving_into(
                    self.const_stmts, class_refs, residual_refs, external, subject
                )
                for subject in self.subjects
            }

        def _const_imported_elsewhere(self, name: str) -> bool:
            """Is the constant imported or attribute-read from another file? A moved
            constant must be sole-owned by its class (BQ6) — an external reader would
            break."""
            origin_path = ModuleSplitting._module_parts(self.origin.rel)
            for path in SourceFiles(self.repo).py_files():
                if path.relative_to(self.repo).as_posix() == self.origin.rel:
                    continue
                try:
                    mod = cst.parse_module(path.read_text(encoding="utf-8"))
                except Exception:
                    continue
                importer_dir = path.relative_to(self.repo).as_posix().split("/")[:-1]
                alias_map = ModuleSplitting._module_alias_map(mod, origin_path)

                probe = ModuleSplitting._ConstImportScan(importer_dir, alias_map, origin_path, name)
                mod.visit(probe)
                if probe.hit:
                    return True
            return False

        def _residual_subject_refs(self) -> set[str]:
            """The moved classes the residual statements need — the origin imports
            them back. A constant's VALUE may reference one (`CACHE = {User: []}`):
            the residual keeps the constant, so the origin imports the class too."""
            refs: set[str] = set()
            for stmt in self.residual:
                if isinstance(stmt, cst.SimpleStatementLine) and any(
                    isinstance(item, (cst.Import, cst.ImportFrom)) for item in stmt.body
                ):
                    continue  # imports bind their own names — not class references
                refs |= ModuleSplitting._free_names_of(stmt) & self.subject_names
            for _name, stmt in self.const_stmts:
                refs |= Bindings.expr_names(ModuleSplitting._const_value(stmt)) & self.subject_names
            return refs

        def components(self) -> list[list[cst.ClassDef]]:
            """The subjects clustered by cohesion edges: cross-class member
            references OR shared class-level attributes. Components keep source
            order."""
            parent = {id(subject): id(subject) for subject in self.subjects}
            for i, first in enumerate(self.subjects):
                for second in self.subjects[i + 1 :]:
                    if ModuleSplitting._functions_collide(first, second, self.free_of, self.attrs_of):
                        parent[ModuleSplitting._union_find(parent, id(first))] = ModuleSplitting._union_find(
                            parent, id(second)
                        )
            class_index = {id(stmt): i for i, stmt in enumerate(self.module.body)}
            components: dict[int, list[cst.ClassDef]] = {}
            for subject in self.subjects:
                components.setdefault(ModuleSplitting._union_find(parent, id(subject)), []).append(subject)
            return sorted(components.values(), key=lambda group: min(class_index[id(c)] for c in group))

        def layout(self, package_name: str | None) -> ModuleSplitting._SplitLayout:
            """Name each component's target: a lone class goes flat, a cohesive
            whole takes the stem (or --name), a sub-cluster needs --name. Relative
            imports in the origin refuse the package layout and fall back to flat."""
            total = len(self.subjects)
            members: list[tuple[list[cst.ClassDef], str | None]] = []
            for component in self.components():
                if len(component) == 1:
                    members.append((component, None))
                elif len(component) == total:
                    members.append((component, package_name or self.origin.stem))
                elif package_name:
                    members.append((component, package_name))
                else:
                    return ModuleSplitting._SplitLayout(
                        members=[],
                        any_package=False,
                        snake={},
                        decline=(
                            "the cohesive sub-cluster needs a package name — name the package "
                            "with a domain noun (--name)"
                        ),
                    )
            any_package = any(name is not None for _component, name in members)
            if any_package and ModuleSplitting._module_has_relative_imports(self.module):
                members = [(component, None) for component, _name in members]
                any_package = False
            snake = {subject.name.value: ModuleSplitting._snake_name(subject.name.value) for subject in self.subjects}
            return ModuleSplitting._SplitLayout(members=members, any_package=any_package, snake=snake)

        def is_whole_package(self, layout: ModuleSplitting._SplitLayout) -> bool:
            """One package holding every subject class — the origin is replaced."""
            return (
                len(layout.members) == 1
                and layout.members[0][1] is not None
                and len(layout.members[0][0]) == len(self.subjects)
            )

        def refuse_package(
            self, layout: ModuleSplitting._SplitLayout, wrapper: cst.MetadataWrapper, whole_pkg: bool
        ) -> str | None:
            """The package-layout refusals: no module-level constant may survive
            (BQ6), and no residual code may evaluate a moved class at import time
            (the __init__ evaluates before its re-exports)."""
            if layout.any_package and (self.const_names - self.moved_all):
                names = ", ".join(sorted(self.const_names - self.moved_all))
                return (
                    f"the constant(s) {names} cannot move into a class; a package "
                    "__init__ may hold no constants — split by hand"
                )
            if whole_pkg:
                for stmt in self.residual:
                    refs = ModuleSplitting._exec_time_names(wrapper, stmt) & self.subject_names
                    if refs:
                        return (
                            "module-level code evaluates "
                            + ", ".join(sorted(refs))
                            + " at import time — the package __init__ runs before the "
                            "re-exports — split by hand"
                        )
            return None

        def refuse_collisions(self, layout: ModuleSplitting._SplitLayout, package_name: str | None) -> str | None:
            """The target-collision refusals: an existing sibling file or an
            existing package dir blocks the split (the stem package needs --name
            when taken)."""
            directory = self.origin.directory
            seen_files: set[str] = set()
            for component, name in layout.members:
                if name is None:
                    for subject in component:
                        target = self._flat_target(layout, subject)
                        if target in seen_files:
                            return f"the split target '{target}' would collide — split by hand"
                        seen_files.add(target)
                        if (self.repo / target).exists():
                            return f"'{target}' already exists — split by hand"
                else:
                    package_dir = f"{directory}/{name}" if directory else name
                    # the origin file itself is being replaced — only a DIFFERENT
                    # `name.py` or an existing `name/` directory blocks the package
                    if (self.repo / package_dir).exists() or (
                        (self.repo / f"{package_dir}.py").exists() and f"{package_dir}.py" != self.origin.rel
                    ):
                        if name == self.origin.stem and package_name is None:
                            return f"a package named '{name}' already exists — pass --name <name>"
                        return f"the package name '{name}' is taken — pick another --name"
            return None

        def refuse_cycles(self, layout: ModuleSplitting._SplitLayout) -> str | None:
            """Import cycles between the produced class files (the __init__
            re-export edge is exempt by construction)."""
            if not layout.any_package:
                return None
            for component, name in layout.members:
                if name is None:
                    continue
                nodes = [subject.name.value for subject in component]
                edges = {node: set() for node in nodes}
                for subject in component:
                    for other in component:
                        if other is not subject and other.name.value in self.free_of[id(subject)]:
                            edges[subject.name.value].add(other.name.value)
                cycle = ModuleSplitting._graph_cycle(nodes, edges)
                if cycle:
                    return "the split would create an import cycle (" + " -> ".join(cycle) + ") — split by hand"
            return None

        def refuse_residual_cycles(self) -> str | None:
            """Import cycles between a moved class file and the residual origin
            module, and the `from ... import *` origin the split cannot retarget."""
            bad = [
                subject.name.value
                for subject in self.subjects
                if (self.free_of[id(subject)] & self.residual_def_names)
                and subject.name.value in self.residual_subject_refs
            ]
            if bad:
                return (
                    "the split would create an import cycle between '"
                    + bad[0]
                    + "' and the origin module — split by hand"
                )
            if ModuleSplitting._module_star_imported(self.repo, self.origin.rel, self.module):
                return "a file does 'from ... import *' on the origin — the split cannot retarget it — split by hand"
            return None

        @staticmethod
        def targets(layout: ModuleSplitting._SplitLayout) -> dict[str, list[str]]:
            """Each subject class's import path after the split: the package name,
            or the snake-cased flat sibling."""
            return {
                subject.name.value: ([name] if name is not None else [layout.snake[subject.name.value]])
                for component, name in layout.members
                for subject in component
            }

        def _flat_target(self, layout: ModuleSplitting._SplitLayout, subject: cst.ClassDef) -> str:
            filename = f"{layout.snake[subject.name.value]}.py"
            return f"{self.origin.directory}/{filename}" if self.origin.directory else filename

        def member_sources(
            self, layout: ModuleSplitting._SplitLayout, wrapper: cst.MetadataWrapper
        ) -> list[tuple[str, str]]:
            """The produced files for the moved classes: flat siblings, or a
            package's __init__ (residual + re-exports) and one file per class."""
            writes: list[tuple[str, str]] = []
            for component, name in layout.members:
                if name is None:
                    for subject in component:
                        writes.append(
                            (
                                self._flat_target(layout, subject),
                                ModuleSplitting._split_class_source(
                                    self.module,
                                    wrapper,
                                    subject,
                                    self._split_ctx(subject, None),
                                ),
                            )
                        )
                else:
                    package_dir = f"{self.origin.directory}/{name}" if self.origin.directory else name
                    init_body = list(self.residual) + [
                        cst.SimpleStatementLine(
                            body=[ModuleSplitting._from_import_node([c.name.value], [c.name.value], 1)]
                        )
                        for c in component
                    ]
                    writes.append((f"{package_dir}/__init__.py", cst.Module(body=init_body).code))
                    for subject in component:
                        writes.append(
                            (
                                f"{package_dir}/{subject.name.value}.py",
                                ModuleSplitting._split_class_source(
                                    self.module,
                                    wrapper,
                                    subject,
                                    self._split_ctx(subject, name),
                                ),
                            )
                        )
            return writes

        def _split_ctx(self, subject: cst.ClassDef, package: str | None) -> ModuleSplitting._SplitCtx:
            """The per-class rewrite context: which names move with this class,
            and the import depth its new file sees — a package member is one hop
            deeper than the package's own __init__."""
            in_package = self.origin.in_package
            return ModuleSplitting._SplitCtx(
                stem=self.origin.stem,
                subjects=self.subject_names,
                residual_def_names=self.residual_def_names,
                moved=self.moved_consts.get(id(subject), set()),
                package=package,
                origin_dots=(2 if in_package else 0) if package is not None else (1 if in_package else 0),
                member_dots=1 if package is not None else 0,
            )

        def origin_source(self, targets: dict[str, list[str]]) -> str:
            """The origin residual: the kept statements in their original order
            plus the from-imports the residual needs for the moved classes."""
            subject_ids = {id(subject) for subject in self.subjects}
            moved_def_ids = {id(stmt) for name, stmt in self.const_stmts if name in self.moved_all}
            kept = [stmt for stmt in self.module.body if id(stmt) not in subject_ids and id(stmt) not in moved_def_ids]
            additions = [
                cst.SimpleStatementLine(
                    body=[ModuleSplitting._from_import_node(targets[name], [name], 1 if self.origin.in_package else 0)]
                )
                for name in sorted(self.residual_subject_refs)
            ]
            return cst.Module(body=ModuleSplitting._insert_header_imports(kept, additions)).code

        def retarget(
            self, origin_source: str, targets: dict[str, list[str]], whole_pkg: bool
        ) -> tuple[str, dict[str, str]]:
            """Repo-wide rewrites for the layouts that move classes out of the
            reachable origin (FLAT + sub-package): from-imports and `old.Cls`
            attribute chains retarget to the class's new module."""
            if whole_pkg:
                return origin_source, {}
            rewritten: dict[str, str] = {}
            for path in SourceFiles(self.repo).py_files():
                rel = path.relative_to(self.repo).as_posix()
                text = origin_source if rel == self.origin.rel else path.read_text(encoding="utf-8")
                new = ModuleSplitting._split_rewrite_source(text, rel, self.origin.origin_path, targets)
                if new is not None:
                    rewritten[rel] = new
            if self.origin.rel in rewritten:
                origin_source = rewritten[self.origin.rel]
            return origin_source, rewritten

        def finalize(
            self,
            origin_source: str,
            writes: list[tuple[str, str]],
            rewritten: dict[str, str],
            whole_pkg: bool,
        ) -> ModuleSplitting._SplitOutcome:
            """Verify the produced files parse and no new target collides, then
            decide whether an empty origin is deleted (only when no import names
            it anywhere in the produced repo, BQ6(f))."""
            rel = self.origin.rel
            for target, src in [(rel, origin_source)] + writes:
                try:
                    cst.parse_module(src)
                except (
                    Exception
                ):  # lucidlint: ignore broad-except the split is cst output; a parse failure means refuse
                    return ModuleSplitting._SplitOutcome(
                        None,
                        [],
                        [],
                        f"the computed split for '{target}' does not parse — split by hand",
                    )
                if ModuleSplitting._split_target_exists(self.repo, target, rel, rewritten):
                    return ModuleSplitting._SplitOutcome(
                        None,
                        [],
                        [],
                        f"the split target '{target}' already exists — split by hand",
                    )
            if whole_pkg:
                return ModuleSplitting._SplitOutcome("", writes, [rel])
            final_sources = {path: source for path, source in rewritten.items()}
            final_sources.update({target: src for target, src in writes})
            if ModuleOutline.is_empty(origin_source) and not ModuleSplitting._origin_imported_anywhere(
                self.repo, final_sources, rel, self.origin.origin_path
            ):
                return ModuleSplitting._SplitOutcome("", writes, [rel])
            return ModuleSplitting._SplitOutcome(origin_source, writes, [])

        def execute(self, wrapper: cst.MetadataWrapper, package_name: str | None) -> ModuleSplitting._SplitOutcome:
            """Run the split: name the layout, refuse the unsafe shapes, build and
            verify the sources. The refusals run in the order their reasons are
            cheapest for the reader to check."""
            layout = self.layout(package_name)
            if layout.decline is not None:
                return ModuleSplitting._SplitOutcome(None, [], [], layout.decline)
            whole_pkg = self.is_whole_package(layout)
            refusals = [
                self.refuse_package(layout, wrapper, whole_pkg),
                self.refuse_collisions(layout, package_name),
                self.refuse_cycles(layout),
            ]
            if not whole_pkg:
                refusals.append(self.refuse_residual_cycles())
            for refusal in refusals:
                if refusal is not None:
                    return ModuleSplitting._SplitOutcome(None, [], [], refusal)
            targets = self.targets(layout)
            writes = self.member_sources(layout, wrapper)
            origin_source = self.origin_source(targets)
            origin_source, rewritten = self.retarget(origin_source, targets, whole_pkg)
            writes.extend((rel, src) for rel, src in rewritten.items() if rel != self.origin.rel)
            return self.finalize(origin_source, writes, rewritten, whole_pkg)

    @staticmethod
    def _from_import_node(
        module_comps: list[str], names: list[str] | list[cst.ImportAlias], dots: int
    ) -> cst.ImportFrom:
        """`from <module> import <names>` with `dots` relative dots; module
        components are a dotted Name/Attribute chain when present."""
        module: cst.BaseExpression | None = None
        if module_comps:
            module = cst.Name(module_comps[0])
            for part in module_comps[1:]:
                module = cst.Attribute(value=module, attr=cst.Name(part))
        aliases = [a if isinstance(a, cst.ImportAlias) else cst.ImportAlias(name=cst.Name(a)) for a in names]
        if not isinstance(module, (cst.Name, cst.Attribute)):
            module = cst.Name(str(module))
        return cst.ImportFrom(
            module=module,
            names=aliases,
            relative=[cst.Dot()] * dots,
            lpar=None,
            rpar=None,
        )

    @staticmethod
    def _split_class_source(
        module: cst.Module,
        wrapper: cst.MetadataWrapper,
        cls: cst.ClassDef,
        ctx: ModuleSplitting._SplitCtx,
    ) -> str:
        """One moved class's new file: the imports it needs (verbatim copies of
        the origin's imports binding its free names; a from-origin import for a
        residual member; a from-`.Class` import for a sibling subject) + the
        class itself with its constants and references rewritten."""
        free = ModuleSplitting._class_free_names(cls) - ctx.moved - Bindings.BUILTINS
        imports = Imports(module).needed(free)  # verbatim copies for import-bound names
        constructed = [
            import_node
            for n in sorted(free)
            if (import_node := ModuleSplitting._constructed_import(n, cls, ctx)) is not None
        ]
        seen: set[str] = set()
        body: list = []
        for stmt in imports + [cst.SimpleStatementLine(body=[i]) for i in constructed]:
            code = cst.Module(body=[stmt]).code
            if code in seen:
                continue
            seen.add(code)
            body.append(stmt)
        cls_node = ModuleSplitting._class_with_moved_consts(wrapper, cls, ctx.moved, cls.name.value)
        return cst.Module(body=body + [cls_node]).code

    @staticmethod
    def _constructed_import(n: str, cls: cst.ClassDef, ctx: ModuleSplitting._SplitCtx) -> cst.ImportFrom | None:
        """The import a free name needs in the moved class's file: a sibling
        subject (from its own module) or a residual member (from the origin).
        None means the name is already carried by the origin's own imports —
        one of the three cases, so no free name is left unbound."""
        if n in ctx.subjects and n != cls.name.value:
            if ctx.package is None:
                # unreachable via the cohesion edges (a reference would merge the
                # components) — emit the sibling-file form anyway
                return ModuleSplitting._from_import_node([ModuleSplitting._snake_name(n)], [n], ctx.origin_dots)
            return ModuleSplitting._from_import_node([n], [n], ctx.member_dots)
        if n in ctx.residual_def_names:
            if ctx.package is None:
                return ModuleSplitting._from_import_node([ctx.stem], [n], ctx.origin_dots)
            return ModuleSplitting._from_import_node([], [n], 1)  # from . import X
        return None

    @staticmethod
    def _dotted_expr(parts: list[str]) -> cst.Name | cst.Attribute:
        if len(parts) == 1:
            return cst.Name(parts[0])
        expr: cst.Name | cst.Attribute = cst.Name(parts[0])
        for part in parts[1:]:
            expr = cst.Attribute(value=expr, attr=cst.Name(part))
        return expr

    @staticmethod
    def _resolve_dotted(expr) -> list[str] | None:
        """A plain Name/Attribute chain's dotted components, or None."""
        parts: list[str] = []
        cur = expr
        while isinstance(cur, cst.Attribute):
            parts.append(cur.attr.value)
            cur = cur.value
        if isinstance(cur, cst.Name):
            parts.append(cur.value)
            return list(reversed(parts))
        return None

    @staticmethod
    def _module_alias_map(module: cst.Module, origin_path: list[str]) -> dict[str, list[str]]:
        """`import <origin> [as x]` bindings in one file: the bound name -> the
        origin's dotted path (for attribute-base resolution)."""
        out: dict[str, list[str]] = {}
        for stmt in module.body:
            if not isinstance(stmt, cst.SimpleStatementLine):
                continue
            for inner in stmt.body:
                if not isinstance(inner, cst.Import):
                    continue
                for alias in inner.names:
                    parts = ModuleSplitting._resolve_dotted(alias.name)
                    if parts is None:
                        continue
                    if parts != origin_path:
                        continue
                    bound = alias.asname.name if alias.asname is not None else parts[0]
                    out[str(bound)] = list(parts)
        return out

    @staticmethod
    def _alias_points_to_origin(parts: list[str], origin_path: list[str], alias_map: dict[str, list[str]]) -> bool:
        """A one-component import path that names the origin through an alias."""
        return len(parts) == 1 and parts[0] in alias_map and alias_map[parts[0]] == origin_path

    @staticmethod
    def _import_refers_to_origin(ifrom: cst.ImportFrom, importer_dir: list[str], origin_path: list[str]) -> bool:
        """Does this ImportFrom resolve to the origin module (absolute full path,
        relative dots resolving to the origin's directory)?"""
        comps = ModuleSplitting._resolve_dotted(ifrom.module) if ifrom.module is not None else []
        if comps is None:
            return False
        if ifrom.relative:
            dots = len(ifrom.relative)
            prefix = len(importer_dir) - (dots - 1)
            if prefix < 0:
                return False
            return importer_dir[:prefix] + comps == origin_path
        return comps == origin_path

    class _SplitImportRewrite(cst.CSTTransformer):
        """The FLAT/sub-package repo-wide rewrite: `from prod_mod import Cls`
        splices per subject into `from <new> import Cls` (the other aliases stay
        on the origin), and `prod_mod.Cls` attribute chains become `<new>.Cls`,
        registering the retargeted modules for `import <new>` injection."""

        def __init__(
            self,
            origin_path: list[str],
            targets: dict[str, list[str]],
            alias_map: dict[str, list[str]],
            importer_dir: list[str],
        ) -> None:
            self.origin_path = origin_path
            self.targets = targets
            self.alias_map = alias_map
            self.importer_dir = importer_dir
            self.added: set[str] = set()

        def _base_is_origin(self, parts: list[str]) -> bool:
            if parts == self.origin_path:
                return True
            return ModuleSplitting._alias_points_to_origin(parts, self.origin_path, self.alias_map)

        @override
        def leave_SimpleStatementLine(self, original_node, updated_node):
            new_inners: list[cst.BaseSmallStatement] = []
            changed = False
            for inner in updated_node.body:
                if isinstance(inner, cst.ImportFrom) and not isinstance(inner.names, cst.ImportStar):
                    spliced = self._splice_import_from(inner)
                    if spliced is not None:
                        new_inners.extend(spliced)
                        changed = True
                        continue
                new_inners.append(inner)
            if not changed:
                return updated_node
            return updated_node.with_changes(body=new_inners)

        def _splice_import_from(self, ifrom: cst.ImportFrom) -> list[cst.ImportFrom] | None:
            if not ModuleSplitting._import_refers_to_origin(ifrom, self.importer_dir, self.origin_path):
                return None
            if isinstance(ifrom.names, cst.ImportStar):
                return None
            keep: list[cst.ImportAlias] = []
            subj: list[cst.ImportAlias] = []
            for alias in ifrom.names:
                if isinstance(alias.name, cst.Name) and alias.name.value in self.targets:
                    subj.append(alias)
                else:
                    keep.append(alias)
            if not subj:
                return None
            out: list[cst.ImportFrom] = []
            if keep:
                out.append(ifrom.with_changes(names=keep))
            for alias in subj:
                if not isinstance(alias.name, cst.Name):
                    continue
                comps = self.targets[alias.name.value]
                module_comps = comps if ifrom.relative else self.origin_path[:-1] + comps
                # a FRESH alias: the original carries its comma/whitespace from the
                # multi-alias line (`User, Team`) — reusing it renders `User, ` alone
                fresh = cst.ImportAlias(name=alias.name, asname=alias.asname)
                out.append(ModuleSplitting._from_import_node(module_comps, [fresh], len(ifrom.relative)))
            return out

        @override
        def leave_Attribute(self, original_node, updated_node):
            attr = updated_node.attr.value
            if attr not in self.targets:
                return updated_node
            parts = ModuleSplitting._resolve_dotted(updated_node.value)
            if parts is None or not self._base_is_origin(parts):
                return updated_node
            full = self.origin_path[:-1] + self.targets[attr]
            self.added.add(".".join(full))
            return updated_node.with_changes(value=ModuleSplitting._dotted_expr(full))

    @staticmethod
    def _insert_header_imports(body: list, additions: list) -> list:
        """Insert the added imports after any module docstring — imports must sit
        before the statements that reference the names they bind."""
        i = 1 if body and ModuleOutline.is_docstring_stmt(body[0]) else 0
        return body[:i] + additions + body[i:]

    @staticmethod
    def _split_rewrite_source(
        text: str,
        p_rel: str,
        origin_path: list[str],
        targets: dict[str, list[str]],
    ) -> str | None:
        """One file's rewrite for the split: retargeted from-imports, retargeted
        `old.Cls` attribute chains, and the `import <new>` injection for those
        chains. None when the file is unchanged."""
        try:
            module = cst.parse_module(text)
        except Exception:  # parse errors in other files are their own findings
            return None
        alias_map = ModuleSplitting._module_alias_map(module, origin_path)
        importer_dir = p_rel.split("/")[:-1]
        tx = ModuleSplitting._SplitImportRewrite(origin_path, targets, alias_map, importer_dir)
        new = module.visit(tx).code
        if tx.added:
            existing: set[str] = set()
            for stmt in module.body:
                if not isinstance(stmt, cst.SimpleStatementLine):
                    continue
                for inner in stmt.body:
                    if isinstance(inner, cst.Import):
                        for alias in inner.names:
                            parts = ModuleSplitting._resolve_dotted(alias.name)
                            if parts is not None:
                                existing.add(".".join(parts))
            if tx.added - existing:
                new = ModuleSplitting._add_module_imports(cst.parse_module(new), tx.added - existing).code
        return new if new != text else None

    @staticmethod
    def _add_module_imports(module: cst.Module, dotted_paths: set[str]) -> cst.Module:
        """`import <dotted>` statements for the retargeted attribute-chain bases,
        inserted after the docstring — `prod_mod.User` became `user.User`, which
        needs the module binding."""
        lines = [
            cst.SimpleStatementLine(
                body=[cst.Import(names=[cst.ImportAlias(name=ModuleSplitting._dotted_expr(p.split(".")))])]
            )
            for p in sorted(dotted_paths)
        ]
        return module.with_changes(body=ModuleSplitting._insert_header_imports(list(module.body), lines))

    class _ConstImportScan(cst.CSTVisitor):
        """One file's external readers of a constant: `from <origin> import C`
        (or a star import) and `<origin>.C` attribute chains."""

        def __init__(
            self,
            importer_dir: list[str],
            alias_map: dict[str, list[str]],
            origin_path: list[str],
            name: str,
        ) -> None:
            self.importer_dir = importer_dir
            self.alias_map = alias_map
            self.origin_path = origin_path
            self.name = name
            self.hit = False

        def _refers_to_origin(self, parts: list[str]) -> bool:
            """The import path is the origin module itself, or an alias of it."""
            if parts == self.origin_path:
                return True
            return ModuleSplitting._alias_points_to_origin(parts, self.origin_path, self.alias_map)

        @override
        def visit_ImportFrom(self, node) -> None:
            if not ModuleSplitting._import_refers_to_origin(node, self.importer_dir, self.origin_path):
                return
            if isinstance(node.names, cst.ImportStar) or any(
                isinstance(a.name, cst.Name) and a.name.value == self.name for a in node.names
            ):
                self.hit = True

        @override
        def visit_Attribute(self, node) -> None:
            if node.attr.value != self.name:
                return
            parts = ModuleSplitting._resolve_dotted(node.value)
            if parts is None:
                return
            if self._refers_to_origin(parts):
                self.hit = True

    @staticmethod
    def _module_star_imported(repo: Path, rel: str, module: cst.Module) -> bool:
        """Does any other file `from <origin> import *`? The flat rewrite cannot
        retarget a star import — the moved classes would silently vanish from it."""
        origin_path = ModuleSplitting._module_parts(rel)
        for path in SourceFiles(repo).py_files():
            if path.relative_to(repo).as_posix() == rel:
                continue
            try:
                mod = cst.parse_module(path.read_text(encoding="utf-8"))
            except Exception:
                continue
            importer_dir = path.relative_to(repo).as_posix().split("/")[:-1]
            for stmt in mod.body:
                if not isinstance(stmt, cst.SimpleStatementLine):
                    continue
                for inner in stmt.body:
                    if ModuleSplitting._is_star_import_from_origin(inner, importer_dir, origin_path):
                        return True
        return False

    @staticmethod
    def _origin_imported_anywhere(repo: Path, sources: dict[str, str], rel: str, origin_path: list[str]) -> bool:
        """BQ6(f): an emptied origin is deleted only when no import statement in
        the produced repo names it anymore."""
        for p_rel, source in sources.items():
            if p_rel == rel:
                continue
            try:
                mod = cst.parse_module(source)
            except Exception:
                continue
            importer_dir = p_rel.split("/")[:-1]
            for stmt in mod.body:
                if not isinstance(stmt, cst.SimpleStatementLine):
                    continue
                for inner in stmt.body:
                    if isinstance(inner, cst.ImportFrom) and not isinstance(inner.names, cst.ImportStar):
                        if ModuleSplitting._import_refers_to_origin(inner, importer_dir, origin_path):
                            return True
                    elif isinstance(inner, cst.Import):
                        for alias in inner.names:
                            parts = ModuleSplitting._resolve_dotted(alias.name)
                            if parts == origin_path:
                                return True
        return False

    @staticmethod
    def _graph_cycle(nodes: list[str], edges: dict[str, set[str]]) -> list[str] | None:
        """A directed cycle as the node path, or None — the import-cycle refusal
        for the produced class files."""
        state: dict[str, int] = {}

        def dfs(n: str, path: list[str]) -> list[str] | None:
            state[n] = 1
            path.append(n)
            for nxt in sorted(edges[n]):
                if state.get(nxt) == 1:
                    cyc = path[path.index(nxt) :] + [nxt]
                    return cyc
                if state.get(nxt) is None:
                    r = dfs(nxt, path)
                    if r:
                        return r
            path.pop()
            state[n] = 2
            return None

        for n in nodes:
            if state.get(n) is None:
                r = dfs(n, [])
                if r:
                    return r
        return None

    @staticmethod
    def _is_single_name_assign(stmt: cst.BaseSmallStatement) -> bool:
        """`name = value` with exactly one plain-name target."""
        return isinstance(stmt, cst.Assign) and len(stmt.targets) == 1 and isinstance(stmt.targets[0].target, cst.Name)

    @staticmethod
    def _split_target_exists(repo: Path, target: str, rel: str, rewritten) -> bool:
        """The split's target path already exists on disk and is not the origin or
        a file this fix already rewrote."""
        return target != rel and target not in rewritten and (repo / target).exists()

    @staticmethod
    def _is_star_import_from_origin(
        imp: cst.BaseSmallStatement, importer_dir: list[str], origin_path: list[str]
    ) -> bool:
        """A `from ... import *` that pulls in the module being split."""
        return (
            isinstance(imp, cst.ImportFrom)
            and isinstance(imp.names, cst.ImportStar)
            and ModuleSplitting._import_refers_to_origin(imp, importer_dir, origin_path)
        )

    @staticmethod
    def _functions_collide(first, second, free_of: dict, attrs_of: dict) -> bool:
        """Two functions cannot share one moved cluster: one calls the other, or
        both touch the same state."""
        return (
            second.name.value in free_of[id(first)]
            or first.name.value in free_of[id(second)]
            or bool(attrs_of[id(first)] & attrs_of[id(second)])
        )
