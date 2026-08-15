# code-health: ignore-file fakefs reading the tools' own source is the test's
# subject (the files under test), not the filesystem under test — pyfakefs
# would fake away exactly what this test inspects.

"""The tools must run anywhere: module-level imports are stdlib + siblings.

Consuming repos invoke code_health.py / check_records.py with a bare
`uv run --with radon` (or plain python3) — a third-party module-level
import would crash them. radon is imported OPTIONALLY (guarded); that
guard is what makes the complexity checks degrade gracefully.
"""

import ast
import sys
from pathlib import Path

TOOLS = ["code_health.py", "check_records.py", "check_review_posted.py"]
STDLIB = set(sys.stdlib_module_names)
SIBLINGS = {"code_health", "check_records", "check_review_posted"}


def _top_level_imports(path):
    tree = ast.parse(Path(path).read_text())
    names = set()
    for node in tree.body:
        if isinstance(node, ast.Import):
            names.update(a.name.split(".")[0] for a in node.names)
        elif isinstance(node, ast.ImportFrom) and node.level == 0:
            names.add(node.module.split(".")[0] if node.module else "")
    return names


def test_tool_imports_are_stdlib_or_sibling():
    for tool in TOOLS:
        imports = _top_level_imports(tool)
        foreign = imports - STDLIB - SIBLINGS
        assert not foreign, f"{tool} imports non-stdlib at module level: {foreign}"


def test_radon_import_is_guarded():
    """The one allowed third-party import must be optional (required=False)."""
    src = Path("code_health.py").read_text()
    assert "from radon.visitors import ComplexityVisitor  # optional dependency" in src
    assert "required=False" in src
