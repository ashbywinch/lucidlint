"""The destination a commute card names, and nothing else."""

from dataclasses import dataclass


@dataclass(frozen=True)
class CommuteCardDestination:
    """Where the card goes; the person is its own record."""

    address: str
