def f():
    try:
        g()
    except ValueError:  # lucidlint: ignore inline-import not the right signal
        log('skipping')
