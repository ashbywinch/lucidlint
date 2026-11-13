"""A person on a commute card."""

from dataclasses import dataclass


@dataclass(frozen=True)
class CommuteCardPerson:
    """A person on a commute card."""

    name: str
