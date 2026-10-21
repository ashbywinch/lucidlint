from typing import overload

@overload
def f(x: int) -> int:
    ...

def f(x):
    return x

def f(x):
    return x + 1
