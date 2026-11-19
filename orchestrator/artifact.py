"""Where this artifact's own files live.

The entry module sits beside the package, so the package's parent is the
bundle root, the checkout root, or the site-packages that holds the
distribution — the one home of `deps/`, `version.txt`, `bin/` and
`lucidlint_bin/`.
"""

from __future__ import annotations

from pathlib import Path


def artifact_root() -> Path:
    """The directory the entry module (`lucidlint.py`) lives in."""
    return Path(__file__).resolve().parent.parent
