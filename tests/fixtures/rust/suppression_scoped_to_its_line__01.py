def f():
    try:
        g()
    except ValueError:  # lucidlint: ignore swallow this one is safe, logged
        log('a')
    try:
        h()
    except ValueError:
        log('b')
