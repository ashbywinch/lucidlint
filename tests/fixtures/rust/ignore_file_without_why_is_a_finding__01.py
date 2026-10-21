# lucidlint: ignore-file except
def f():
    try:
        g()
    except ValueError:
        log('x')
