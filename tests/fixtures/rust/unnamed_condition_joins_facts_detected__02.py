def decide(user, order):
    if user.active and order.paid:
        return True
    return False


def eligible(user):
    return user.active and not user.locked and user.age >= 18


def bump(self):
    if self.retries and self.backoff and self.jitter:
        return 1
    return 0


def outer(a, b, c):
    def helper():
        return 1

    return a and b and c
