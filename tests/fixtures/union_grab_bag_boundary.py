from typing import Any


def f(m: dict[str, Any | None]) -> Label:
    return Label(m)
