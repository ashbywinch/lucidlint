from dataclasses import dataclass


@dataclass(frozen=True)
class Provenance:
    label: str
    value: str


def ingest(label: str, sources: dict[str, Provenance]) -> None:
    """Mutate the map it was handed, through its own dict methods."""
    sources.update({"extra": Provenance(label=label, value="")})


def prune(sources: dict[str, Provenance]) -> None:
    sources.pop("extra", None)
