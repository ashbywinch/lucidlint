class Expression:
    """One binary operator of the formula language."""

    def evaluate(self) -> int:
        raise NotImplementedError


class Sub(Expression):
    def __init__(self, left: int, right: int) -> None:
        self.left = left
        self.right = right

    def evaluate(self) -> int:
        return self.left - self.right

    def formula(self) -> str:
        return f"{self.left} - {self.right}"

    def to_formula_lines(self) -> list[str]:
        return [str(self.left), str(self.right)]


class Mul(Expression):
    def __init__(self, left: int, right: int) -> None:
        self.left = left
        self.right = right

    def evaluate(self) -> int:
        return self.left * self.right

    def formula(self) -> str:
        return f"{self.left} * {self.right}"

    def to_formula_lines(self) -> list[str]:
        return [str(self.left), str(self.right)]
