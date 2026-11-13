def read_config(path):
    try:
        with open(path) as fh:
            return fh.read()
    except Exception:
        pass


def minutes(value):
    return value * 60
