def f():
    try:
        g()
    except Exception as e:
        log(e)
        return fallback
