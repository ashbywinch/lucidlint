state: tuple[str, int, float, bool] = (mode, level, ratio, alive)
backup: tuple[str, int, float, bool] = (mode, level, ratio, alive)


def reset():
    return state
