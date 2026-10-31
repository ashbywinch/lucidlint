from dataclasses import dataclass


@dataclass(frozen=True)
class Wall:
    low: int
    high: int


WallAlias = Wall


def combine(first: WallAlias, second: WallAlias) -> Wall:
    return Wall(min(first.low, second.low), max(first.high, second.high))


def union(first: Wall, second: Wall) -> Wall:
    return Wall(min(first.low, second.low), max(first.high, second.high))


def intersect(first: Wall, second: Wall) -> Wall:
    return Wall(max(first.low, second.low), min(first.high, second.high))
