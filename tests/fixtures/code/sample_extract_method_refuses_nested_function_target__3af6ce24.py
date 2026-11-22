def outer():
    def inner():
        if a:
            x = 1
        if b:
            x = 2
        return x
    return inner()
