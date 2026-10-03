def retry(fn):
    attempts = 0
    while attempts < 5:
        if fn():
            return True
        attempts += 1
    return False
