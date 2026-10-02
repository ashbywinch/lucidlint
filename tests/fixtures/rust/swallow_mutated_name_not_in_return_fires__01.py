def f(db):
    try:
        work()
    except ValueError:
        db["status"] = "error"
    return 5