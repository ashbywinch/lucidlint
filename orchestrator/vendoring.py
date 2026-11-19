"""Where the release bundle's vendored dependencies live for this interpreter.

The bundle self-contains the fix engine's libcst in a sibling `deps/` dir
(review-log B8): when a `deps/` sits next to the entry module (the bundle, not
a wheel install), the fix path uses it with no pip step. A wheel or source
install has no `deps/` and uses site-packages libcst.
"""

from __future__ import annotations

import os
import sys
from collections.abc import Sequence


def vendor_path(here: str, version: Sequence[int] | None = None) -> str | None:
    """The bundled deps directory THIS interpreter can load.

    libcst's parser is a compiled extension, so one build serves one Python
    ABI. The release bundle ships one per supported version (`deps/cp312`,
    `deps/cp313`, `deps/cp314`) and this picks the match; a flat `deps/` is
    the fallback (a dev bundle, or a bundle from before the split). The
    `version` pair is the interpreter's (major, minor), defaulting to this
    process's.

    Returns None when neither exists — the fix command then reports the
    libcst requirement instead of failing at import.
    """
    major, minor = version or sys.version_info[:2]
    tagged = os.path.join(here, "deps", f"cp{major}{minor}")
    for candidate in (tagged, os.path.join(here, "deps")):
        if os.path.isdir(candidate):
            return candidate
    return None
