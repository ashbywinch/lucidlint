class Inner:
    def render(self) -> str:
        return "x"


class Wrapper:
    def __init__(self) -> None:
        self._inner = Inner()

    def render(self) -> str:
        return self._inner.render()
