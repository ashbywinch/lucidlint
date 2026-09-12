def f():
    try:
        g()
    except ValueError:
        return 'failed'
