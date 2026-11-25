from pathlib import Path

from school import School


def name(path: Path) -> str:
    return f"{path.parent} {path.name} {path.stem} {path.suffix}"


def identity(school: School) -> School:
    return school


def two(school: School, other: int) -> str:
    return f"{school.name}{school.postcode}{school.town}{school.region}{other}"
