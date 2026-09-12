def check(a):
    out = []
    if a.get(1):
        if a.get(2):
            out.append('x')
    if a.get(3):
        out.append('y')
    if a.get(4):
        out.append('z')
    return out
