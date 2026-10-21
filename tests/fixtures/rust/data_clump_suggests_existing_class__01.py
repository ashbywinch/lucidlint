from dataclasses import dataclass


@dataclass(frozen=True)
class PageScale:
    unit: float
    zoom: float


@dataclass(frozen=True)
class Writing:
    marks: list[object]
    lines: list[object]
    scale: PageScale
    stripped: int


def read_writing(lines, unit):
    return len(lines), unit


def clean_writing(lines, unit):
    return lines, unit


def write_writing(lines, unit):
    return unit, lines