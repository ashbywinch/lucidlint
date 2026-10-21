class RegistryMeta(type):
    pass


class Renderer(metaclass=RegistryMeta):
    def render(self, *, fast: bool = False) -> str:
        return "text"


class ThirdPartyThing:
    def render(self, *, fast: bool = False) -> str:
        return "text"


class Adapter:
    def __init__(self) -> None:
        self._inner = ThirdPartyThing()

    def render(self, *, fast: bool = False) -> str:
        return self._inner.render()
