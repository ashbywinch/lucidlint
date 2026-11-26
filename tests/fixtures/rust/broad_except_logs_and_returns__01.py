def f(op):
    try:
        return op()
    except Exception:
        log("failed")
        return None
