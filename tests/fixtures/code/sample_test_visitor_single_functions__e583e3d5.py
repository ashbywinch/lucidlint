def f(a, b, c):
    if a and b == 4:
        return c ** c
    elif a and not c:
        return sum(i for i in range(41) if i & 1)
    return a + b