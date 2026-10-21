def f():
    try:
        g()
    except ValueError:  # lucidlint: ignore swallow this is safe, logged
        log('x')
