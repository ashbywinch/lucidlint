from typing import Protocol


class ProdModLike(Protocol):
    """The DI seam; tests satisfy it with their own doubles."""

    def render(self) -> str: ...


class ProdMod:
    """The real thing — it does NOT declare the Protocol as a base."""

    def render(self) -> str:
        return "x"
