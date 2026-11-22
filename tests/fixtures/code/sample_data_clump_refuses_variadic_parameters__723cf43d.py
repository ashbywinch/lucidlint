def area(width: int, height: int, *rest) -> int:
    return width * height

def perimeter(width: int, height: int) -> int:
    return 2 * (width + height)

def diagonal(width: int, height: int) -> float:
    return (width ** 2 + height ** 2) ** 0.5
