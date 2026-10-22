from dataclasses import dataclass
from typing import Protocol


@dataclass
class ProdModLike(Protocol):
    """A public dataclass that also types the seam — it belongs in its own module."""

    label: str


class ProdMod(ProdModLike):
    """The namesake class."""

    def render(self) -> str:
        return self.label
