class Inner:
    def go(self, x):
        return x


class A:
    def __init__(self):
        self.inner = Inner()

    def go(self, x):
        return self.inner.go(x)
