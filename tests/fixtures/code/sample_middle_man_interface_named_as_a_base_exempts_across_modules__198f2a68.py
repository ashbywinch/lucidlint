from typing import Protocol


class Renderer(Protocol):
    def render(self) -> str: ...
