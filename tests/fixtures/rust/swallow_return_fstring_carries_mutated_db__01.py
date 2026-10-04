def f(db):
    try:
        work()
    except ValueError:
        db["status"] = "error"
    return f"db is {db}"