def f():
    try:
        g()
    except:
        pass
    try:
        h()
    except ValueError:
        log('x')
