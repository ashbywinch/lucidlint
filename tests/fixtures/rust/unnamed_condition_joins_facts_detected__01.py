def decide(user, order):
    if user.active and not user.locked and order.paid:
        return True
    return False


def shape(value):
    if isinstance(value, list) and len(value) == 2 and value[0] > value[1]:
        return True
    return False
