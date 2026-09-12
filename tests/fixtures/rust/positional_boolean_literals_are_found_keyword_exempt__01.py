def f():
    retry(g(True), h(retry=True), False)
    return 1
