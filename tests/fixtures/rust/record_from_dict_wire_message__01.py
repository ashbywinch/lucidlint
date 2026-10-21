from typing import Any


def from_dict(raw: dict[str, Any]) -> Label:
    return Label(raw)


def from_json(blob: str) -> dict[str, Any]:
    return loads(blob)