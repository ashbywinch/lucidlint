from typing import Any


def from_json(blob: str) -> dict[str, Any]:
    return decode(blob)