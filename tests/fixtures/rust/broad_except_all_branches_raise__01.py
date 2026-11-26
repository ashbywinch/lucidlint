def f(op, err):
    try:
        return op()
    except Exception:
        if err:
            raise
        else:
            raise
