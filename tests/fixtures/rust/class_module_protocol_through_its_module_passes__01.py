import typing_extensions as te


class ProdModLike(te.Protocol):
    """The seam, spelled through the module that carries Protocol."""

    def render(self) -> str: ...


class ProdMod(ProdModLike):
    """The namesake class — it declares the seam as a base."""

    def render(self) -> str:
        return "x"
