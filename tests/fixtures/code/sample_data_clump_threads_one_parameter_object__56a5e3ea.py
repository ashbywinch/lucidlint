def area(width: int, height: int, scale: int) -> int:
    return width * height * scale

def perimeter(width: int, height: int, scale: int) -> int:
    return 2 * (width + height) * scale

def diagonal(width: int, height: int) -> float:
    return (width ** 2 + height ** 2) ** 0.5

def report() -> str:
    return str(area(2, 3, 1) + perimeter(2, 3, 1) + diagonal(2, 3))
