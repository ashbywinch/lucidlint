def f(xs, a, b):
    for x in xs:
        if x:
            break
    else:
        return 0
    try:
        g()
    except ValueError:
        h()
    else:
        k()
    assert a and b
    match a:
        case 1:
            return 1
        case _:
            return 0
