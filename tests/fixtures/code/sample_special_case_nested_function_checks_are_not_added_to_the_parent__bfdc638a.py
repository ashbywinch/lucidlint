def outer(x):
    if x is None:
        return 1
    if x is None:
        return 2
    if x:
        def inner(y):
            if y is None:
                return 3
            if y is None:
                return 4
            return 5
        return inner(x)
    return 0
