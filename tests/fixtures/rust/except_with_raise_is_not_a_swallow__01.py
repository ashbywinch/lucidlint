def f():
    try:
        g()
    except ValueError:
        log('bad')
        raise
