def f(a, b):
    while a ** b:
        a, b = b, a * (b - 1)
        if a and b:
            b = 0
        else:
            b = 1
    return sum(i for i in range(b))