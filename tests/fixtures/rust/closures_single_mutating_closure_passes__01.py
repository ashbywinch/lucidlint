def f():
    L = []
    def g(x):
        L.append(x)
    return g
