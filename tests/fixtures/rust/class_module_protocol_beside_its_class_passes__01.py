from typing import Protocol


class ProdModLike(Protocol):
    """The DI seam; tests satisfy it with their own doubles."""

    def render(self) -> str: ...


class ProdMod(ProdModLike):
    """The real thing."""

    def render(self) -> str:
        return "x"
