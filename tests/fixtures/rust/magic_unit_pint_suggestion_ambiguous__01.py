def f(a):
    return a * 5  # 5 minutes and 30 seconds


def g(x):
    if x < _MAX_RETRY_SECONDS + elapsed_minutes * 30:
        return 0
    return x


def h(x):
    if x * 5 > _MAX_RETRY_SECONDS:
        return 0
    return x