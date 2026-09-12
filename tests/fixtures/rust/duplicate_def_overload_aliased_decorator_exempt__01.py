from typing import overload as ov

@ov
def f(x: int) -> int:
    ...

@ov
def f(x: str) -> str:
    ...

def f(x):
    return x
