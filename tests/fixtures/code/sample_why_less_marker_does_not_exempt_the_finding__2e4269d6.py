def f():
    try:
        g()
    except Exception:  # lucidlint: ignore swallow
        pass
