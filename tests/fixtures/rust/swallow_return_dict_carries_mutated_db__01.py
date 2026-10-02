def f(db):
    try:
        work()
    except ValueError:
        db["status"] = "error"
    return {"status": "ok", "db": db}