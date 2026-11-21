"""The repo's Python files, for cross-file analysis.

The fix engine resolves call sites, sibling modules and duplicate names across
the repo; this is the one walk that answers which files belong to it.
"""

from __future__ import annotations

from pathlib import Path

_REPO_SKIP_DIRS = frozenset(
    {
        ".git",
        ".venv",
        "venv",
        "node_modules",
        "__pycache__",
        ".lucidlint-cache",
        ".ruff_cache",
        ".pytest_cache",
        ".mypy_cache",
        ".pyrefly-cache",
        "htmlcov",
        "dist",
        "build",
        "target",
        ".code-review-graph",
        ".tox",
        ".eggs",
    }
)


class SourceFiles:
    """One repo's `.py` files (vendored and generated trees skipped)."""

    def __init__(self, repo: Path) -> None:
        self.repo = repo

    def py_files(self) -> list[Path]:
        """Every .py file under the repo, skipping venvs/caches/build output."""
        repo = self.repo
        out: list[Path] = []
        stack = [repo]
        while stack:
            d = stack.pop()
            try:
                entries = list(d.iterdir())
            except OSError:
                continue
            for p in entries:
                if p.is_dir():
                    if p.name not in _REPO_SKIP_DIRS:
                        stack.append(p)
                elif p.suffix == ".py":
                    out.append(p)
        return sorted(out)
