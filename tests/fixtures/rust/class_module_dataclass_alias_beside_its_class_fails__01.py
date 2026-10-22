from dataclasses import dataclass as dc
from typing import Protocol


@dc
class ProdModLike(Protocol):
    """A public dataclass — under an aliased decorator — that also types the seam."""

    label: str


class ProdMod(ProdModLike):
    """The namesake class."""

    def render(self) -> str:
        return self.label
