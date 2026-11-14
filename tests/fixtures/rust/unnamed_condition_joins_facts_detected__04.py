def nested(a, b, c, d, e):
    if a > (b or c) and d and e:
        return True
    return False


def two_operands(a, b):
    if a > b and a > 0:
        return True
    return False
