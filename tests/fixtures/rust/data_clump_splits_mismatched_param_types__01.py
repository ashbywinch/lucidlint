from dataclasses import dataclass


@dataclass(frozen=True)
class Wall:
    low: int
    high: int


@dataclass(frozen=True)
class Point:
    x: int
    y: int


def combine(first: Wall, second: Wall) -> Wall:
    return Wall(min(first.low, second.low), max(first.high, second.high))


def distance(first: Point, second: Point) -> float:
    return ((first.x - second.x) ** 2 + (first.y - second.y) ** 2) ** 0.5


def ranges_touch(first: tuple[int, int], second: tuple[int, int], tolerance: int) -> bool:
    return min(first[1], second[1]) + tolerance >= max(first[0], second[0])
