from typing import Protocol


class ProdModLike(Protocol):
    """A seam the namesake class only partly satisfies — not its interface."""

    def render(self) -> str: ...

    def flush(self) -> None: ...


class ProdMod:
    """The namesake class — it renders, but never flushes."""

    def render(self) -> str:
        return "x"
