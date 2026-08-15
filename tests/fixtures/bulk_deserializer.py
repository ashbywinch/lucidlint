from typing import Any


def from_lines(lines: list[dict[str, Any]]) -> list[Label]:
    return [Label(**line) for line in lines]
