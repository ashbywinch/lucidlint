def f():
    try:
        g()
    # lucidlint: ignore swallow deliberate skip
    except ValueError:
        log('x')
