"""One entry-module binding, resolved at call time.

The composition root (`lucidlint`) owns the process module and the optional
libraries because that is the surface a consumer or the test suite substitutes
(`ch.subprocess = fake`). Capturing a binding at import would freeze it and
hide a later substitution, so `Substitutable` reads the entry module's current
value each time it is used.
"""

from __future__ import annotations

import sys
from typing import Any


class Substitutable:
    """A named binding on the entry module, resolved at call time.

    Attribute access is forwarded to the binding's current value, so the
    caller writes the same expression it would against the real module.
    """

    def __init__(self, name: str) -> None:
        self._name = name

    def get(self) -> Any:
        """The binding's current value."""
        entry = sys.modules.get("lucidlint")
        if entry is None:  # imported as a library, with no entry module
            raise AttributeError(f"{self._name}: the binding lives on the lucidlint entry module")
        return getattr(entry, self._name)

    def __call__(self, *args: Any, **kwargs: Any) -> Any:
        """Call the binding (a class or a factory) with these arguments."""
        return self.get()(*args, **kwargs)

    def __getattr__(self, attr: str) -> Any:
        return getattr(self.get(), attr)
