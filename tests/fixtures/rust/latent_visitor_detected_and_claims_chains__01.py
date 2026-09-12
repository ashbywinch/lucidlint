class A:
    pass

class B:
    pass

def op1(x):
    if isinstance(x, A):
        return 1
    elif isinstance(x, B):
        return 2
    return 0

def op2(x):
    if isinstance(x, A):
        return 3
    elif isinstance(x, B):
        return 4
    return 0
