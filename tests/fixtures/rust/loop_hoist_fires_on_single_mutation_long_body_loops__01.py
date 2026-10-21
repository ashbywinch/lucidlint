def f(items):
    out = []
    for it in items:
        x = it * 2
        y = x - 1
        if y > 0:
            out.append(y)
    return out