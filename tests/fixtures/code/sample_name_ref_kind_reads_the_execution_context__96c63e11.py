X = 1
def deco(fn):
    return fn
@deco
def f(a, b=X):
    for i in range(2):
        v = i
    return v
class K:
    attr = X
