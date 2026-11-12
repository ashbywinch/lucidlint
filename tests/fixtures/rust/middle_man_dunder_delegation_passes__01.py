class Measurement:
    """A value whose arithmetic goes through one combining helper."""

    def __init__(self, value: float) -> None:
        self._value = value

    def _combine(self, other: float) -> float:
        return self._value + other

    def __add__(self, other: float) -> float:
        return self._combine(other)
