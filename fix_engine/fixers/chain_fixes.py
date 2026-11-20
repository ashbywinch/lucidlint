"""Collapsing a forwarding chain to its terminal.

One fix family, one class; the class holds the request, its helpers are
members, and its libcst visitors are nested classes.
"""

from __future__ import annotations

from collections.abc import Sequence
from pathlib import Path
from typing import NamedTuple, override

import libcst as cst
from libcst.metadata import PositionProvider

from fix_engine.bindings import Bindings
from fix_engine.module_outline import ModuleOutline
from fix_engine.module_source import ModuleSource
from fix_engine.source_files import SourceFiles

# lucidlint: ignore-file record-shape the libcst layer's tuple/dict shorthands ARE its wire records —
# lucidlint: ignore-file record-shape a class per helper hop is ceremony


class ChainFixes:
    """The collapse-chain fix for one request: rewire or promote the terminal,
    never blind inline."""

    class _ForwardTarget(NamedTuple):
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
        callee: ChainFixes._ForwardTarget

    class _ChainMiddle(NamedTuple):
        """The module function the head forwards into: where it lives, the module
        it belongs to, its parameters, and where that forwards."""

        rel: str
        module: cst.Module
        params: list[str]
        callee: ChainFixes._ForwardTarget

    class _ChainTerminal(NamedTuple):
        """The method the chain reaches through its middle: D.N, plus the
        receiver parameter the head passes for it."""

        recv: str
        name: str
        rel: str
        cls: cst.ClassDef
        method: cst.FunctionDef

    class _ModuleFn(NamedTuple):
        """One module-level function resolved repo-wide: the file it lives in,
        the parsed module, and its node — the three facts a caller needs
        together."""

        rel: str
        module: cst.Module
        node: cst.FunctionDef

    class _ModuleRef(NamedTuple):
        """One module-level name in its repo: the thing the repo-wide lookups ask
        about, as one parameter instead of a (repo, name) pair everywhere."""

        repo: Path
        name: str

    def __init__(self, request) -> None:
        self.request = request

    def run(self) -> str | None:
        """Collapse a forwarding chain (#30): method M (class C) whose body is
        exactly `return F(<all params>)`, module fn F whose body is exactly
        `return N(<all F params>)`, N a method on ANOTHER class D (or a
        module fn at depth >= 2). REWIRE M to reach D.N directly and delete F
        when D is real (has state or non-forwarding members); PROMOTE N to a
        sibling method of M when D has no identity and dissolves. REFUSES
        when the modules are independent (the deliberate boundary), when F
        has other callers, on *args/**kwargs passthrough, on a
        `from mod import F` re-export, or when N has other callers — never
        blind inlining."""
        module, wrapper = ModuleSource.parse(self.request._loaded_source())
        head = ChainFixes._chain_head(wrapper, self.request.line)
        if head is None:
            return None  # stale, or the method is not a pure forwarder (R28)
        mid = ChainFixes._chain_middle(self.request.repo, head)
        if mid is None:
            return None  # F is missing or does not forward its own params
        refusal = self._chain_refusal(head, mid)
        if refusal is not None:
            self.request.decline = refusal
            return None
        if mid.callee.is_name:
            return self._collapse_to_module_fn(module, head, mid)
        return self._collapse_to_method(module, head, mid)

    def _chain_refusal(self, head: ChainFixes._ChainHead, mid: ChainFixes._ChainMiddle) -> str | None:
        """The refusals every chain shape shares: F's only caller is the head,
        no *args/**kwargs passthrough, and no `from mod import F` re-export to
        retarget."""
        f_name = head.callee.name
        if ChainFixes._call_sites(ChainFixes._ModuleRef(self.request.repo, f_name)) != 1:
            return f"'{f_name}' has more than one caller — collapse by hand"
        if any("*" in p for p in head.params) or any("*" in p for p in mid.params):
            return "the chain passes *args/**kwargs — collapse by hand"
        if ChainFixes._reexported(ChainFixes._ModuleRef(self.request.repo, f_name)):
            return f"'from ... import {f_name}' re-exports the forwarder — collapse by hand"
        return None

    def _collapse_to_module_fn(
        self, module: cst.Module, head: ChainFixes._ChainHead, mid: ChainFixes._ChainMiddle
    ) -> str | None:
        """M -> F -> G (a module function): M calls G with its own params and F
        is deleted. None when the two modules are otherwise independent — the
        chain is then the deliberate boundary between them."""
        if not ChainFixes._depends_on(self.request.repo, self.request.rel, mid.rel):
            self.request.decline = "the chain is the only coupling between the modules — a boundary; leave it"
            return None
        body = cst.SimpleStatementLine(
            body=[
                cst.Return(
                    cst.Call(
                        func=cst.Name(mid.callee.name),
                        args=[cst.Arg(cst.Name(p)) for p in head.params],
                    )
                )
            ]
        )
        origin_new = ChainFixes._replace_method_body(module, head.method, body, self.request.line).code
        # F may live in the origin itself — deleting it from a FRESH parse
        # would overwrite the rewire just applied above
        return self._drop_middle(origin_new, head, mid)

    def _method_terminal(
        self, head: ChainFixes._ChainHead, mid: ChainFixes._ChainMiddle
    ) -> ChainFixes._ChainTerminal | None:
        """The unique D.N the chain reaches through F. None when the method is
        not unique in the repo, or when the modules are independent."""
        attr_method = mid.callee.attr_method
        if attr_method is None:
            return None
        recv, n_name = attr_method
        arg_params = [p for p in mid.params if p != recv]
        matches = [
            (d_rel, d_cls, d_method)
            for d_rel, d_cls, d_method in ChainFixes._methods_named(ChainFixes._ModuleRef(self.request.repo, n_name))
            if d_cls.name.value != head.class_name
            and ChainFixes._non_receiver_param_names(d_method.params.params) == arg_params
        ]
        if len(matches) != 1:
            return None  # ambiguous — no structural certainty
        d_rel, d_cls, d_method = matches[0]
        if not ChainFixes._depends_on(self.request.repo, self.request.rel, d_rel):
            self.request.decline = "the chain is the only coupling between the modules — a boundary; leave it"
            return None
        if ChainFixes._call_sites(ChainFixes._ModuleRef(self.request.repo, n_name)) > 1:
            self.request.decline = f"'{n_name}' has other callers beyond the chain — collapse by hand"
            return None
        return ChainFixes._ChainTerminal(recv, n_name, d_rel, d_cls, d_method)

    def _collapse_to_method(
        self, module: cst.Module, head: ChainFixes._ChainHead, mid: ChainFixes._ChainMiddle
    ) -> str | None:
        """M -> F -> D.N: rewire when D has identity, promote N onto C when D
        dissolves. D's identity decides, not the caller's preference."""
        terminal = self._method_terminal(head, mid)
        if terminal is None:
            return None
        if ChainFixes._class_has_identity(terminal.cls):
            return self._rewire_method_terminal(module, head, mid, terminal)
        return self._promote_method_terminal(module, head, mid, terminal)

    def _rewire_method_terminal(
        self,
        module: cst.Module,
        head: ChainFixes._ChainHead,
        mid: ChainFixes._ChainMiddle,
        terminal: ChainFixes._ChainTerminal,
    ) -> str:
        """D keeps its identity: M reaches D.N directly and F is deleted."""
        args = [p for p in head.params if p != terminal.recv]
        body = cst.SimpleStatementLine(
            body=[
                cst.Return(
                    cst.Call(
                        func=cst.Attribute(value=cst.Name(terminal.recv), attr=cst.Name(terminal.name)),
                        args=[cst.Arg(cst.Name(p)) for p in args],
                    )
                )
            ]
        )
        origin_new = ChainFixes._replace_method_body(module, head.method, body, self.request.line).code
        return self._drop_middle(origin_new, head, mid)

    def _promote_method_terminal(
        self,
        module: cst.Module,
        head: ChainFixes._ChainHead,
        mid: ChainFixes._ChainMiddle,
        terminal: ChainFixes._ChainTerminal,
    ) -> str | None:
        """D has no identity: N becomes a sibling method of M on C, D dissolves."""
        d_name = terminal.cls.name.value
        if not ChainFixes._zero_external_refs(self.request.repo, d_name):
            self.request.decline = f"'{d_name}' still has external references — dissolve D by hand"
            return None
        if ChainFixes._method_uses_self(terminal.method):
            self.request.decline = f"'{terminal.name}' touches its own instance state — promote by hand"
            return None
        promoted = terminal.method.with_changes(name=cst.Name(terminal.name))
        new_class = head.class_node.with_changes(
            body=head.class_node.body.with_changes(body=[*head.class_node.body.body, promoted])
        )
        origin_new = ChainFixes._replace_class_body(module, self.request.line, new_class).code
        args = [p for p in head.params if p != terminal.recv]
        m_body = cst.SimpleStatementLine(
            body=[
                cst.Return(
                    cst.Call(
                        func=cst.Attribute(value=cst.Name("self"), attr=cst.Name(terminal.name)),
                        args=[cst.Arg(cst.Name(p)) for p in args],
                    )
                )
            ]
        )
        origin_new = ChainFixes._replace_method_body(
            cst.parse_module(origin_new), head.method, m_body, self.request.line
        ).code
        origin_new = self._drop_middle(origin_new, head, mid)
        if terminal.rel != self.request.rel:
            d_new = ChainFixes._remove_class(ChainFixes._module_of(self.request.repo, terminal.rel), d_name)
            if ModuleOutline.is_empty(d_new):
                self.request.deletes.append(terminal.rel)
            else:
                self.request.extra_writes.append((terminal.rel, d_new))
        else:
            # D dissolves in the origin too (zero external refs by the gate)
            origin_new = cst.parse_module(origin_new).visit(ChainFixes._RemoveClass(d_name)).code
        return origin_new

    def _drop_middle(self, origin_new: str, head: ChainFixes._ChainHead, mid: ChainFixes._ChainMiddle) -> str:
        """Delete F once the head no longer calls it: in the origin when F
        lives there, otherwise as a separate write."""
        if mid.rel == self.request.rel:
            return cst.parse_module(origin_new).visit(ChainFixes._RemoveFn(head.callee.name)).code
        self.request.extra_writes.append((mid.rel, ChainFixes._remove_fn(mid.module, head.callee.name)))
        return origin_new

    @staticmethod
    def _non_receiver_param_names(params: Sequence[cst.Param]) -> list[str]:
        """The parameter names a moved method carries, without `self`/`cls`."""
        return [p.name.value for p in params if p.name.value not in ("self", "cls")]

    class _MethodAtLine(cst.CSTVisitor):
        """The class + method whose def sits on one line — a chain's head probe."""

        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, target_line: int) -> None:
            self.target_line = target_line
            self.owner: tuple[str, cst.ClassDef, cst.FunctionDef] | None = None

        @override
        def visit_ClassDef(self, node: cst.ClassDef) -> None:
            for stmt in node.body.body:
                if not isinstance(stmt, cst.FunctionDef):
                    continue
                pos = ModuleOutline.span(self.get_metadata(PositionProvider, stmt))
                if pos.start.line == self.target_line:
                    self.owner = (node.name.value, node, stmt)
                    return

    @staticmethod
    def _chain_head(wrapper: cst.MetadataWrapper, line: int) -> ChainFixes._ChainHead | None:
        """The forwarding chain's head at `line`: a method whose body is exactly
        one call into a module function, passing its own parameters through. None
        when the line is not that shape (stale, or not a forwarder)."""
        probe = ChainFixes._MethodAtLine(line)
        wrapper.visit(probe)
        if probe.owner is None:
            return None
        class_name, class_node, method = probe.owner
        params = ChainFixes._non_receiver_param_names(method.params.params)
        callee, args = ChainFixes._pure_forward_of(method)
        if ChainFixes._breaks_forwarding_chain(callee, args, params):
            return None
        assert callee is not None
        return ChainFixes._ChainHead(class_name, class_node, method, params, callee)

    @staticmethod
    def _chain_middle(repo: Path, head: ChainFixes._ChainHead) -> ChainFixes._ChainMiddle | None:
        """The module function F the head calls, resolved repo-wide; None when F is
        missing or does not forward its own parameters on."""
        found = ChainFixes._find_module_fn(ChainFixes._ModuleRef(repo, head.callee.name))
        if found is None:
            return None
        params = [p.name.value for p in found.node.params.params]
        callee, args = ChainFixes._pure_forward_of(found.node)
        if callee is None:
            return None
        # a method callee absorbs the receiver param (`store.load(key)` passes only
        # `key`); a module-fn callee takes all params
        expected = [p for p in params if p != callee.attr_method[0]] if callee.attr_method is not None else params
        if args != expected:
            return None
        return ChainFixes._ChainMiddle(found.rel, found.module, params, callee)

    @staticmethod
    def _pure_forward_of(fn: cst.FunctionDef) -> tuple[ChainFixes._ForwardTarget | None, list[str]]:
        """The forwarder call + arg names when `fn`'s body is exactly
        `return <callee>(<arg names>)` — the #30 re-derivation of the pure
        forwarder shape (pinned by the scanner's own rule tests)."""
        if not isinstance(fn.body, cst.IndentedBlock) or len(fn.body.body) != 1:
            return None, []
        line = fn.body.body[0]
        if not isinstance(line, cst.SimpleStatementLine) or len(line.body) != 1:
            return None, []
        ret = line.body[0]
        if not isinstance(ret, cst.Return) or not isinstance(ret.value, cst.Call):
            return None, []
        call = ret.value
        argnames = []
        for a in call.args:
            if not isinstance(a.value, cst.Name):
                return None, []
            argnames.append(a.value.value)
        if isinstance(call.func, cst.Name):
            return ChainFixes._ForwardTarget(is_name=True, name=call.func.value), argnames
        if isinstance(call.func, cst.Attribute) and isinstance(call.func.value, cst.Name):
            return ChainFixes._ForwardTarget(
                is_name=False, attr_method=(call.func.value.value, call.func.attr.value)
            ), argnames
        return None, []

    @staticmethod
    def _module_fn_node(module: cst.Module, name: str) -> cst.FunctionDef | None:
        for stmt in module.body:
            if isinstance(stmt, cst.FunctionDef) and stmt.name.value == name:
                return stmt
        return None

    @staticmethod
    def _find_module_fn(ref: ChainFixes._ModuleRef) -> ChainFixes._ModuleFn | None:
        """One module-level function by name, resolved across the repo — the
        lookup returns the file, the module, and the node together."""
        for path in SourceFiles(ref.repo).py_files():
            module = cst.parse_module(path.read_text(encoding="utf-8"))
            node = ChainFixes._module_fn_node(module, ref.name)
            if node is not None:
                return ChainFixes._ModuleFn(path.relative_to(ref.repo).as_posix(), module, node)
        return None

    @staticmethod
    def _call_sites(ref: ChainFixes._ModuleRef) -> int:
        total = 0
        for path in SourceFiles(ref.repo).py_files():
            mod = cst.parse_module(path.read_text(encoding="utf-8"))

            class _Count(cst.CSTVisitor):
                def __init__(self):
                    self.n = 0

                @override
                def visit_Call(self, node):
                    if isinstance(node.func, cst.Name) and node.func.value == ref.name:
                        self.n += 1

            c = _Count()
            mod.visit(c)
            total += c.n
        return total

    @staticmethod
    def _reexported(ref: ChainFixes._ModuleRef) -> bool:
        for path in SourceFiles(ref.repo).py_files():
            mod = cst.parse_module(path.read_text(encoding="utf-8"))
            for stmt in mod.body:
                if isinstance(stmt, cst.SimpleStatementLine):
                    for inner in stmt.body:
                        if not isinstance(inner, cst.ImportFrom) or isinstance(inner.names, cst.ImportStar):
                            continue
                        for alias in inner.names:
                            if isinstance(alias.name, cst.Name) and alias.name.value == ref.name:
                                return True
        return False

    @staticmethod
    def _depends_on(repo: Path, rel: str, d_rel: str) -> bool:
        if rel == d_rel:
            return True
        mod = cst.parse_module((repo / rel).read_text(encoding="utf-8"))
        target_stem = Path(d_rel).stem
        for stmt in mod.body:
            if not isinstance(stmt, cst.SimpleStatementLine):
                continue
            for inner in stmt.body:
                if isinstance(inner, cst.ImportFrom) and inner.module is not None:
                    mod_name = (
                        inner.module
                        if isinstance(inner.module, str)
                        else (inner.module.value if hasattr(inner.module, "value") else "")
                    )
                    if str(mod_name).split(".")[-1] == target_stem:
                        return True
        return False

    @staticmethod
    def _methods_named(ref: ChainFixes._ModuleRef) -> list[tuple[str, cst.ClassDef, cst.FunctionDef]]:
        out: list[tuple[str, cst.ClassDef, cst.FunctionDef]] = []
        for path in SourceFiles(ref.repo).py_files():
            mod = cst.parse_module(path.read_text(encoding="utf-8"))
            for stmt in mod.body:
                if not isinstance(stmt, cst.ClassDef):
                    continue
                for mem in stmt.body.body:
                    if isinstance(mem, cst.FunctionDef) and mem.name.value == ref.name:
                        out.append((path.relative_to(ref.repo).as_posix(), stmt, mem))
        return out

    @staticmethod
    def _class_has_identity(cls: cst.ClassDef) -> bool:
        writes_self = False

        class _Scan(cst.CSTVisitor):
            @override
            def visit_Assign(self, node):
                nonlocal writes_self
                for t in node.targets:
                    if ModuleOutline.is_self_attribute(t.target):
                        writes_self = True

            @override
            def visit_AnnAssign(self, node):
                nonlocal writes_self
                if ModuleOutline.is_self_attribute(node.target):
                    writes_self = True

        cls.visit(_Scan())
        for stmt in cls.body.body:
            if isinstance(stmt, cst.AnnAssign):
                return True  # declared class-level field
            if isinstance(stmt, cst.FunctionDef) and ChainFixes._pure_forward_of(stmt)[0] is None:
                return True  # a non-forwarding member — D is real
        return writes_self

    @staticmethod
    def _method_uses_self(method: cst.FunctionDef) -> bool:
        uses = False

        class _Scan(cst.CSTVisitor):
            @override
            def visit_Attribute(self, node):
                nonlocal uses
                if isinstance(node.value, cst.Name) and node.value.value == "self":
                    uses = True

        method.visit(_Scan())
        return uses

    @staticmethod
    def _zero_external_refs(repo: Path, class_name: str) -> bool:
        total = 0
        for path in SourceFiles(repo).py_files():
            mod = cst.parse_module(path.read_text(encoding="utf-8"))
            for node in Bindings.module_name_nodes(mod):
                if node.value == class_name:
                    total += 1
        return total <= 1  # only the definition itself

    class _MethodBodyRewriteByLine(cst.CSTTransformer):
        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int, name: str, body: list) -> None:
            self.line = line
            self.name = name
            self.body = body

        @override
        def leave_FunctionDef(self, original_node, updated_node):
            if updated_node.name.value == self.name:
                pos = ModuleOutline.span(self.get_metadata(PositionProvider, original_node))
                if pos.start.line == self.line:
                    return updated_node.with_changes(body=updated_node.body.with_changes(body=self.body))
            return updated_node

    @staticmethod
    def _replace_method_body(
        module: cst.Module, method: cst.FunctionDef, body: cst.SimpleStatementLine, line: int | None = None
    ) -> cst.Module:
        if line is None:
            line = ModuleOutline.span(cst.MetadataWrapper(module).resolve(PositionProvider)[method]).start.line
        return cst.MetadataWrapper(module).visit(ChainFixes._MethodBodyRewriteByLine(line, method.name.value, [body]))

    class _ReplaceClassByLine(cst.CSTTransformer):
        METADATA_DEPENDENCIES = (PositionProvider,)

        def __init__(self, line: int, new_class: cst.ClassDef) -> None:
            self.line = line
            self.new_class = new_class
            self.done = False

        @override
        def leave_ClassDef(self, original_node, updated_node):
            if self.done:
                return updated_node
            pos = ModuleOutline.span(self.get_metadata(PositionProvider, original_node))
            if pos.start.line == self.line:
                self.done = True
                return self.new_class
            return updated_node

    @staticmethod
    def _replace_class_body(module: cst.Module, line: int, new_class: cst.ClassDef) -> cst.Module:
        return cst.MetadataWrapper(module).visit(ChainFixes._ReplaceClassByLine(line, new_class))

    class _RemoveFn(cst.CSTTransformer):
        def __init__(self, name: str) -> None:
            self.name = name

        @override
        def leave_FunctionDef(self, original_node, updated_node):
            if updated_node.name.value == self.name and not updated_node.decorators:
                return cst.RemoveFromParent()
            return updated_node

    @staticmethod
    def _remove_fn(module: cst.Module, name: str) -> str:
        return module.visit(ChainFixes._RemoveFn(name)).code

    class _RemoveClass(cst.CSTTransformer):
        def __init__(self, name: str) -> None:
            self.name = name

        @override
        def leave_ClassDef(self, original_node, updated_node):
            if updated_node.name.value == self.name:
                return cst.RemoveFromParent()
            return updated_node

    @staticmethod
    def _remove_class(module: cst.Module, name: str) -> str:
        return module.visit(ChainFixes._RemoveClass(name)).code

    @staticmethod
    def _module_of(repo: Path, rel: str) -> cst.Module:
        return cst.parse_module((repo / rel).read_text(encoding="utf-8"))

    @staticmethod
    def _breaks_forwarding_chain(callee, args: list, params: list) -> bool:
        """The call does not forward every parameter to a plain name — the chain's
        middle edge is broken."""
        return callee is None or args != params or not callee.is_name
