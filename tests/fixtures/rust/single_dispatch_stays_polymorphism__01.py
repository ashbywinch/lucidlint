class A:
    pass

class B:
    pass

class C:
    pass

class D:
    pass

def op1(x):
    if isinstance(x, A):
        return 1
    elif isinstance(x, B):
        return 2
    elif isinstance(x, C):
        return 3
    elif isinstance(x, D):
        return 4
    return 0
