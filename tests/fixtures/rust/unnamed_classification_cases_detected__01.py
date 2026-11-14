def kind_of(value):
    head = value.split("-")[0]
    if head == "a":
        return "alpha"
    elif head == "b":
        return "beta"
    elif head == "c":
        return "gamma"
    return "unknown"
