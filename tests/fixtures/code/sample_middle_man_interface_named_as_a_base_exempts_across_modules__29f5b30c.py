from protocols import Renderer


class ThirdPartyThing:
    def emit(self) -> str:
        return "text"


class Adapter(Renderer):
    def __init__(self) -> None:
        self._inner = ThirdPartyThing()

    def render(self) -> str:
        return self._inner.emit()
