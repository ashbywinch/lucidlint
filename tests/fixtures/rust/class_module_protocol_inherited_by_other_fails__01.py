from typing import Protocol


class ProdModLike(Protocol):
    """The DI seam — inherited by Helper, not by the class the file is named for."""

    def render(self) -> str: ...


class ProdMod:
    """The real thing."""

    def render(self) -> str:
        return "x"


class Helper(ProdModLike):
    """A double."""

    def render(self) -> str:
        return "y"
