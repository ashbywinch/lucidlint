def f():
    try:
        g()
    except ValueError:
        log('x')
