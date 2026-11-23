class Measurement:
    def __init__(self, value):
        self._value = value

    def _combine(self, other):
        return self._value + other

    def plus(self, other):
        return self._combine(other)
