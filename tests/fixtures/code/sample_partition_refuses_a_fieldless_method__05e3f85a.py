class Mixed:
    def __init__(self, left: int, right: int):
        self.left = left
        self.right = right

    def a(self) -> int:
        return self.left

    def b(self) -> int:
        return self.left + 1

    def c(self) -> int:
        return self.right

    def d(self) -> int:
        return self.right + 1

    def ping(self) -> int:
        return 1
