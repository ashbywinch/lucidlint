"""Record-shaped collection check — "never a bare dict as a record".

The rule from docs/coding-standards.md: structured data is an object with
named fields, not a bare dict, tuple, or nested list. This tool gates on
the two places records appear:

1. **Signatures** — record-shaped collections in parameters and returns:
   grab-bags (`dict[str, Any]`, bare `dict`), collections of dicts or
   tuples (`list[dict[str, str]]`, `list[tuple[A, B]]`), nested lists, and
   fixed tuples. Maps pass: `dict[str, T]` (including primitive-valued
   maps like counts) is a lookup, not a record; `list[T]` of a domain
   class is a collection. Deserializer boundaries are the one sanctioned
   bare-dict spot: a grab-bag parameter on a function that returns a
   domain class (raw JSON in, `Label` out) is exempt.
2. **Literals** — a dict literal with >= 2 keys, at least one constant
   string key, and at least one dynamic value, in a *record position*
   (assigned, returned, or yielded), is a record being built
   (`{"kind": "tool_call", ...}` — mixed or odd keys included: if it has
   a shape, it wants a class). Inline call arguments (`headers={...}`)
   are maps, not records, and pass. Lookup tables whose keys and values
   are all constant (nested structures included) pass untouched.

Function strewing (free functions sharing a leading parameter) is a
canary for the same disease; it is reported as a warning, never the gate.

Stdlib-only (this is a config repo — no deps).

    python3 check_records.py [paths...]
"""

from __future__ import annotations

import argparse
import ast
import sys
from collections import defaultdict
from pathlib import Path

# Primitive value types: a dict of these (in a signature) is a map, not a record.
PRIMITIVES = frozenset({"str", "int", "float", "bool", "bytes", "Any", "object", "None"})


class ScanResult:
    """Findings fail the gate; warnings are the strewing canary."""

    findings: list[str]
    warnings: list[str]

    def __init__(self, findings: list[str], warnings: list[str]) -> None:
        self.findings = findings
        self.warnings = warnings


class ModuleScanner:
    """All checks over one parsed module."""

    tree: ast.Module
    path: Path

    def __init__(self, tree: ast.Module, path: Path) -> None:
        self.tree = tree
        self.path = path

    @staticmethod
    def _name_of(node: ast.expr | None) -> str | None:
        if isinstance(node, ast.Name):
            return node.id
        if isinstance(node, ast.Attribute):
            return node.attr  # typing.Any -> Any
        if isinstance(node, ast.Constant):
            return str(node.value)
        return None

    @staticmethod
    def _base_name(node: ast.expr | None) -> str | None:
        """The normalized collection base name: lowercase dict/list/tuple
        for bare names and typing-qualified spellings (typing.Dict ->
        dict, Optional -> optional)."""
        if isinstance(node, ast.Name):
            return node.id.lower()
        if isinstance(node, ast.Attribute):
            return node.attr.lower()
        return None

    @classmethod
    def _unwrap(cls, node: ast.expr) -> list[ast.expr]:
        """Peel Optional[..]/Union[..]/A | B wrappers into their members;
        anything else is itself. Deliberately does NOT unwrap arbitrary
        subscripts — dict[str, Label] is a map, not a domain-class return."""
        if isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr):
            return cls._unwrap(node.left) + cls._unwrap(node.right)
        if isinstance(node, ast.Subscript) and ModuleScanner._base_name(node.value) in ("optional", "union"):
            parts = node.slice
            if isinstance(parts, ast.Tuple):
                return [e for part in parts.elts for e in cls._unwrap(part)]
            return cls._unwrap(parts)
        return [node]

    @staticmethod
    def _is_variadic_tuple(node: ast.expr) -> bool:
        """tuple[T, ...] — a homogeneous sequence like list[T], not a
        fixed-size record pair."""
        if not isinstance(node, ast.Subscript) or ModuleScanner._base_name(node.value) != "tuple":
            return False
        parts = node.slice
        return isinstance(parts, ast.Tuple) and any(
            isinstance(e, ast.Constant) and e.value is Ellipsis for e in parts.elts
        )

    @classmethod
    def _is_raw_json(cls, node: ast.expr) -> bool:
        """Raw JSON in: a bare or grab-bag dict (dict, dict[str, Any],
        dict[Any, X], dict[str, object]), or a collection whose element is
        one (list[dict[str, Any]] — bulk deserializer rows)."""
        wrapped = cls._unwrap(node)
        if len(wrapped) != 1:
            return any(cls._is_raw_json(p) for p in wrapped)
        node = wrapped[0]
        if isinstance(node, ast.Name):
            return cls._base_name(node) == "dict"
        if not isinstance(node, ast.Subscript):
            return False
        base = cls._base_name(node.value)
        if base == "dict":
            value = node.slice
            if isinstance(value, ast.Tuple):
                if len(value.elts) != 2:
                    return True  # malformed — treat as bare-ish
                _, val = value.elts
                val_parts = cls._unwrap(val)
                return any(cls._name_of(p) in ("Any", "object", "None") for p in val_parts)
            return True  # dict[X] single-arg
        if base in ("list", "tuple"):
            elt = node.slice
            if isinstance(elt, ast.Tuple):
                return False  # a fixed tuple of stuff is not raw rows
            return cls._is_raw_json(elt)
        return False

    @classmethod
    def _annotation_is_record(cls, node: ast.expr | None) -> bool:
        """True when an annotation is a record-shaped bare collection.

        Calibration: maps pass (dict[str, primitive], dict[str, Domain]);
        grab-bags fail (bare dict, dict[str, Any]); collections of dicts /
        tuples / nested lists fail; fixed tuples fail. Optional/Union/|
        wrappers are peeled first, so equivalent spellings agree.
        Variadic tuples (tuple[str, ...]) and bare list/tuple are
        collections, not records.
        """
        if node is None:
            return False
        wrapped = cls._unwrap(node)
        if len(wrapped) != 1:
            return any(cls._annotation_is_record(p) for p in wrapped)
        node = wrapped[0]
        if isinstance(node, ast.Name):
            return cls._base_name(node) == "dict"  # bare dict = grab-bag; bare list/tuple are collections
        if not isinstance(node, ast.Subscript):
            return False
        base = cls._base_name(node.value)
        if base == "dict":
            value = node.slice
            if isinstance(value, ast.Tuple):
                if len(value.elts) != 2:
                    return False  # dict[()] / dict[str, int, float] — malformed, tolerate
                key, val = value.elts
                if cls._name_of(key) not in ("str", "Any"):
                    return False
                val_parts = cls._unwrap(val)
                if len(val_parts) > 1:
                    # a union value: a record or shapeless member (Any | None)
                    # makes the whole value a record
                    return any(cls._annotation_is_record(p) for p in val_parts) or any(
                        cls._name_of(p) in ("Any", "object") for p in val_parts
                    )
                val = val_parts[0]
                val_name = cls._name_of(val)
                if val_name in ("Any", "object", "None"):
                    return True  # grab-bag: no shape
                if isinstance(val, ast.Subscript):
                    return not cls._is_variadic_tuple(val)  # collection values are records; variadic is a sequence
                if cls._base_name(val) in ("dict", "tuple", "list"):
                    return True
                return False  # dict[str, primitive | domain] = a map
            return True  # dict[X] single-arg or dict[()] — bare-ish
        if base == "tuple":
            return not cls._is_variadic_tuple(node)  # fixed-size pairs are records
        if base == "list":
            parts = cls._unwrap(node.slice)
            if len(parts) != 1:
                return any(cls._annotation_is_record(p) for p in parts)
            value = parts[0]
            if isinstance(value, ast.Subscript):
                return not cls._is_variadic_tuple(value)  # list[dict[...]] / list[tuple[...]] / list[list[...]] — records; variadic is a sequence
            return isinstance(value, ast.Name) and cls._base_name(value) in ("dict", "tuple", "list")
        return False

    @classmethod
    def _returns_domain_class(cls, node: ast.FunctionDef | ast.AsyncFunctionDef) -> bool:
        """Return annotation resolves to a domain class — a bare class
        name, Optional/Union/| wrappers, or a collection of one
        (list[Label], tuple[Label, ...]) — so the function converts raw
        JSON into domain objects (the sanctioned deserializer boundary).
        A map return (dict[str, Label]) is not a boundary."""
        r = node.returns
        if r is None:
            return False
        return any(cls._part_is_domain(p) for p in cls._unwrap(r))

    @classmethod
    def _part_is_domain(cls, node: ast.expr) -> bool:
        """One part of the return annotation that resolves to a domain
        class: a bare class name, or a collection of one."""
        if isinstance(node, ast.Name):
            return node.id not in PRIMITIVES and node.id not in ("dict", "tuple", "list")
        if isinstance(node, ast.Subscript) and cls._base_name(node.value) in ("list", "tuple"):
            elt = node.slice
            if isinstance(elt, ast.Tuple):
                parts = [e for e in elt.elts if not (isinstance(e, ast.Constant) and e.value is Ellipsis)]
                return len(parts) == 1 and cls._part_is_domain(parts[0])
            return cls._part_is_domain(elt)
        return False

    def signature_findings(self) -> list[str]:
        findings: list[str] = []
        for node in ast.walk(self.tree):
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            boundary = self._returns_domain_class(node)
            args = node.args.args + node.args.posonlyargs + node.args.kwonlyargs
            if node.args.vararg is not None:
                args = args + [node.args.vararg]
            if node.args.kwarg is not None:
                args = args + [node.args.kwarg]
            for arg in args:
                ann_node = arg.annotation
                if ann_node is None or not self._annotation_is_record(ann_node):
                    continue
                if boundary and self._is_raw_json(ann_node):
                    continue  # deserializer boundary: raw JSON in, domain class out
                ann = ast.unparse(ann_node)
                findings.append(
                    f"{self.path}: bare record collection '{ann}' in parameter "
                    + f"'{arg.arg}' of {node.name} (line {node.lineno})"
                )
            returns_node = node.returns
            if returns_node is not None and self._annotation_is_record(returns_node):
                ann = ast.unparse(returns_node)
                findings.append(
                    f"{self.path}: bare record collection '{ann}' as return type "
                    + f"of {node.name} (line {node.lineno})"
                )
        return findings

    @staticmethod
    def _is_constant_value(node: ast.expr) -> bool:
        """A literal value that cannot vary at runtime: a constant, or a
        list/tuple/dict literal whose parts are all constant (lookup tables
        may carry nested constant structures)."""
        if isinstance(node, ast.Constant):
            return True
        if isinstance(node, (ast.List, ast.Tuple)):
            return all(ModuleScanner._is_constant_value(e) for e in node.elts)
        if isinstance(node, ast.Dict):
            return all(k is not None and ModuleScanner._is_constant_value(k) for k in node.keys) and all(
                ModuleScanner._is_constant_value(v) for v in node.values
            )
        return False

    def record_literal_lines(self) -> list[int]:
        """Line numbers of dict literals building records: >= 2 keys, >= 1
        constant string key, >= 1 dynamic value, in a record position
        (assigned, returned, or yielded, including inside comprehensions
        and lambdas). Inline call arguments are maps and are not descended
        into. Lookup tables (all values constant, nested structures
        included) pass."""
        found: set[int] = set()

        def scan_expr(node: ast.expr | None) -> None:
            if node is None:
                return
            if isinstance(node, ast.Dict):
                keys = node.keys
                has_const_key = any(
                    isinstance(k, ast.Constant) and isinstance(k.value, str) for k in keys
                )
                has_dynamic_value = any(not self._is_constant_value(v) for v in node.values)
                if len(keys) >= 2 and has_const_key and has_dynamic_value:
                    found.add(node.lineno)
                for v in node.values:
                    scan_expr(v)
                return
            if isinstance(node, (ast.List, ast.Tuple)):
                for elt in node.elts:
                    scan_expr(elt)
            elif isinstance(node, ast.IfExp):
                scan_expr(node.body)
                scan_expr(node.orelse)
            elif isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp)):
                scan_expr(node.elt)
            elif isinstance(node, ast.DictComp):
                scan_expr(node.key)
                scan_expr(node.value)
            elif isinstance(node, ast.Lambda):
                scan_expr(node.body)
            # Do NOT descend into Call nodes: inline arguments (headers={...},
            # params={...}) are maps/config, not records.

        for node in ast.walk(self.tree):
            if isinstance(node, (ast.Assign, ast.AnnAssign, ast.Return, ast.Yield, ast.YieldFrom)):
                scan_expr(getattr(node, "value", None))
        return sorted(found)

    def strewing_warnings(self) -> list[str]:
        """Free module-level functions sharing a leading parameter — the canary."""
        functions: list[tuple[str, int, str]] = []
        for node in self.tree.body:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                if not node.args.args:
                    continue
                annotation = node.args.args[0].annotation
                if annotation is None:
                    continue
                functions.append((node.name, node.lineno, ast.unparse(annotation)))
        groups: dict[str, list[tuple[str, int]]] = defaultdict(list)
        for name, lineno, annotation in functions:
            groups[annotation].append((name, lineno))
        warnings: list[str] = []
        for annotation, members in sorted(groups.items()):
            if len(members) >= 2:
                names = ", ".join(f"{n} (line {ln})" for n, ln in sorted(members, key=lambda m: m[1]))
                warnings.append(
                    f"{self.path}: {len(members)} free functions share leading parameter "
                    + f"'{annotation}' — a {annotation} class is missing: {names}"
                )
        return warnings


def scan(paths: list[Path]) -> ScanResult:
    files: list[Path] = []
    for p in paths:
        if p.is_dir():
            files.extend(
                f
                for f in sorted(p.rglob("*.py"))
                # fixture dirs are intentionally non-compliant test input;
                # dot-dirs are vendored/hidden trees (site-packages etc.)
                if "fixtures" not in f.parts and not any(part.startswith(".") for part in f.parts)
            )
        elif p.is_file() and p.suffix == ".py":
            files.append(p)
    findings: list[str] = []
    warnings: list[str] = []
    for f in files:
        try:
            tree = ast.parse(f.read_text(encoding="utf-8"))
        except (SyntaxError, UnicodeDecodeError):  # code-health: ignore except an unparseable file is skipped, not a scan failure; the parse is best-effort
            continue
        scanner = ModuleScanner(tree, f)
        findings.extend(scanner.signature_findings())
        findings.extend(
            f"{f}: dict literal with constant keys is a record — make a class (line {ln})"
            for ln in scanner.record_literal_lines()
        )
        warnings.extend(scanner.strewing_warnings())
    return ScanResult(findings, warnings)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="check_records",
        description="Gate on record-shaped collections (bare dict/tuple/nested list); warn on function strewing.",
    )
    parser.add_argument("paths", nargs="*", default=["."], help="files or directories to scan (.py)")
    args = parser.parse_args(argv)
    result = scan([Path(p) for p in args.paths])
    for w in result.warnings:
        print(f"  warn: {w}")
    if result.findings:
        print("record-shaped collections found (make a class):")
        for f in result.findings:
            print(f"  {f}")
        return 1
    print(f"ok — no record-shaped collections in {len(args.paths)} path(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
