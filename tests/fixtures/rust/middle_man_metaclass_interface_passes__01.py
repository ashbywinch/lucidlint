from abc import ABCMeta


class Renderer(metaclass=ABCMeta):
    def render(self, *, fast: bool = False) -> str:
        raise NotImplementedError


class ThirdPartyThing:
    def render(self, *, fast: bool = False) -> str:
        return "text"


class Adapter:
    def __init__(self) -> None:
        self._inner = ThirdPartyThing()

    def render(self, *, fast: bool = False) -> str:
        return self._inner.render()
