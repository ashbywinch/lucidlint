def f(lock, op):
    try:
        return op()
    except Exception:
        lock.release()
        raise
