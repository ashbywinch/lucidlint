def f():
    try:
        g()
    except ValueError:  # lucidlint: ignore swallow
        log('x')
