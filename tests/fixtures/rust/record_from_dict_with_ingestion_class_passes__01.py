from typing import Any


class Label:
    def __init__(self, name: str) -> None:
        self.name = name

    @classmethod
    def from_dict(cls, raw: dict[str, Any]) -> "Label":
        return cls(raw["name"])


def from_dict(raw: dict[str, Any]) -> Label:
    return Label.from_dict(raw)