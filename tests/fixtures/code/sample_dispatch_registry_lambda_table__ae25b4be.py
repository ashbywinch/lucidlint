def route(sel, n):
    if sel == "a":
        return n + 1
    if sel == "b":
        return n * 2
    if sel == "c":
        return n - 1
    return -1
