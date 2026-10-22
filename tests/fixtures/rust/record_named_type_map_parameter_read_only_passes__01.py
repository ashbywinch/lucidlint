from dataclasses import dataclass


@dataclass(frozen=True)
class Provenance:
    label: str
    value: str


def composite(label: str, sources: dict[str, Provenance]) -> Provenance:
    """Read the map: pick the first source's value."""
    first = next(iter(sources.values()), None)
    return Provenance(label=label, value=first.value if first else "")


def total(sources: dict[str, Provenance]) -> int:
    return len(sources)
