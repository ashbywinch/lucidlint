if a and b:
    print
else:
    print
a = sum(i for i in range(1000) if i % 3 == 0 and i % 5 == 0)

def f(n):
    def inner(n):
        return n ** 2

    if n == 0:
        return 1
    elif n == 1:
        return n
    elif n < 5:
        return (n - 1) ** 2
    return n * pow(inner(n), f(n - 1), n - 3)
