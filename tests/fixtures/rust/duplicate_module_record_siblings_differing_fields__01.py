"""The person a commute card names, and nothing else."""

from dataclasses import dataclass


@dataclass(frozen=True)
class CommuteCardPerson:
    """Who the card names; the destination is its own record."""

    name: str
