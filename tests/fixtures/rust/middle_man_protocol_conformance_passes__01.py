from typing import Protocol


class Renderer(Protocol):
    def render(self) -> str: ...


class ThirdPartyThing:
    def emit(self, *, format: str) -> str:
        return format


class Adapter:
    def __init__(self) -> None:
        self._inner = ThirdPartyThing()

    def render(self) -> str:
        return self._inner.emit(format="text")
