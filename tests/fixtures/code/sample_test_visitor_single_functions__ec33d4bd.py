if a and not b: pass
elif b or c: pass
else: pass

def g(a, b):
    while a < b:
        b, a = a ** 2, b ** 2
    return b